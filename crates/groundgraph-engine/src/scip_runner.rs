//! Auto-invoke installed SCIP indexers (ADR-0001 R1/R2).
//!
//! When `groundgraph index` runs, for every indexed language whose SCIP indexer
//! binary is on `PATH`, invoke it **once** — a one-shot batch process, not a
//! long-running LSP server — to regenerate `.groundgraph/scip/<lang>.scip`. The
//! [`scip_overlay`](crate::scip_overlay) then ingests that file as the
//! high-confidence `Calls`/`References` precision layer.
//!
//! This is what lets SCIP be the *single* precision source without sacrificing
//! the near-zero-config experience: the operator does not have to remember to
//! run `rust-analyzer scip` / `scip-go` / … by hand. A missing indexer binary
//! is a **silent skip** (with a recorded reason, surfaced by the CLI), so a
//! machine without the toolchain simply gets the structure-only graph — never
//! an error. Every byte we write lands under `.groundgraph/` (D1 non-invasive).
//!
//! The PATH probe is injected ([`plan_with`]) so the planning logic is unit
//! tested deterministically; the real run ([`run_indexers`]) uses the live
//! `PATH` and executes the subprocess.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Static knowledge of one language's SCIP indexer.
#[derive(Debug, Clone, Copy)]
struct IndexerSpec {
    /// Canonical language id (matches [`crate::config::canonical_language_id`]).
    language: &'static str,
    /// Default executable probed on `PATH` (overridable per language via the
    /// `GROUNDGRAPH_SCIP_<LANG>_BIN` env var).
    binary: &'static str,
    /// argv passed after the program. `{root}` expands to the repo root and
    /// `{out}` to the destination `.scip` path.
    args: &'static [&'static str],
    /// Some indexers ignore an output flag and always emit `index.scip` into
    /// the working directory; when `true` the runner moves that file to the
    /// destination after a successful run.
    writes_cwd_index: bool,
}

/// The indexer table. Languages absent here have no auto-invoke path yet and
/// fall back to "ingest a hand-placed `.scip`" (e.g. Java/C++ need build-system
/// context). Order is irrelevant; lookup is by language id.
const SPECS: &[IndexerSpec] = &[
    IndexerSpec {
        language: "rust",
        binary: "rust-analyzer",
        args: &["scip", "{root}", "--output", "{out}"],
        writes_cwd_index: false,
    },
    IndexerSpec {
        language: "go",
        binary: "scip-go",
        args: &["index", "--output", "{out}"],
        writes_cwd_index: false,
    },
    IndexerSpec {
        language: "typescript",
        binary: "scip-typescript",
        args: &["index", "--infer-tsconfig", "--output", "{out}"],
        writes_cwd_index: false,
    },
    // `--project-name`/`--project-version` are pinned to stable placeholders so
    // scip-python never falls back to the git revision (absent in non-git or
    // vendored trees) and crashes on `normalizeNameOrVersion(undefined)`. These
    // only appear inside scip-python symbol ids, which the overlay never
    // surfaces (it maps edges by document path + range), so fixed values are
    // harmless and keep the run deterministic.
    IndexerSpec {
        language: "python",
        binary: "scip-python",
        args: &[
            "index",
            "--cwd",
            "{root}",
            "--project-name",
            "groundgraph-local",
            "--project-version",
            "0",
            "--output",
            "{out}",
        ],
        writes_cwd_index: false,
    },
    // Dart's `scip_dart` ignores `--output` and writes `index.scip` into the
    // cwd, so `writes_cwd_index` moves it to the destination. NOTE: the caller
    // (`index.rs::indexed_languages`) only asks for "dart" when the analyzer
    // sidecar is disabled — the sidecar is Dart's authoritative precision
    // source (richer than generic SCIP), so scip_dart only fills the gap when
    // it is off (ADR-0001 §8.8 (f)).
    IndexerSpec {
        language: "dart",
        binary: "scip_dart",
        args: &["{root}"],
        writes_cwd_index: true,
    },
];

/// Per-language env override for the indexer binary, e.g.
/// `GROUNDGRAPH_SCIP_GO_BIN=/opt/scip-go`.
fn env_override_for(language: &str) -> String {
    format!("GROUNDGRAPH_SCIP_{}_BIN", language.to_ascii_uppercase())
}

fn spec_for(language: &str) -> Option<&'static IndexerSpec> {
    SPECS.iter().find(|s| s.language == language)
}

/// A resolved decision for one language: run it, skip it, or unsupported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScipRunPlan {
    /// No auto-invoke spec for this language (ingest-only).
    Unsupported { language: String },
    /// Indexer binary not found / not configured — structure-only this run.
    Skipped { language: String, reason: String },
    /// Ready to execute.
    Runnable {
        language: String,
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        out: PathBuf,
        writes_cwd_index: bool,
    },
}

impl ScipRunPlan {
    pub fn language(&self) -> &str {
        match self {
            ScipRunPlan::Unsupported { language }
            | ScipRunPlan::Skipped { language, .. }
            | ScipRunPlan::Runnable { language, .. } => language,
        }
    }
}

/// Resolve the run plan for one language, using `probe` to look a binary up on
/// `PATH` (injected for deterministic tests). `scip_dir` is where the `.scip`
/// is written (`<repo>/.groundgraph/scip`).
pub fn plan_with(
    language: &str,
    repo_root: &Path,
    scip_dir: &Path,
    probe: &dyn Fn(&str) -> Option<PathBuf>,
) -> ScipRunPlan {
    let Some(spec) = spec_for(language) else {
        return ScipRunPlan::Unsupported {
            language: language.to_string(),
        };
    };
    let env_key = env_override_for(language);
    let configured = std::env::var(&env_key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let binary = configured.unwrap_or_else(|| spec.binary.to_string());

    let Some(program) = probe(&binary) else {
        return ScipRunPlan::Skipped {
            language: language.to_string(),
            reason: format!(
                "未在 PATH 找到 `{binary}`（{language} 的 SCIP indexer）；安装后 `groundgraph index` 会自动生成 .groundgraph/scip/{language}.scip，或设 {env_key} 指定路径",
            ),
        };
    };

    let out = scip_dir.join(format!("{language}.scip"));
    plan_in(spec, language, &program, repo_root, repo_root, out)
}

/// Build a [`ScipRunPlan::Runnable`] for `spec` with an explicit working
/// directory and output path. `{root}` in the argv still expands to `repo_root`
/// (rust/python/dart pass the scan root as an argument), while `cwd` is where the
/// subprocess actually runs — they differ for Go, whose `go.mod` may live in a
/// sub-module so `scip-go` must run *there* even though the graph is rooted at
/// `repo_root`. See [`go_module_dirs`].
fn plan_in(
    spec: &IndexerSpec,
    language: &str,
    program: &Path,
    repo_root: &Path,
    cwd: &Path,
    out: PathBuf,
) -> ScipRunPlan {
    let root = repo_root.to_string_lossy();
    let out_str = out.to_string_lossy();
    let args = spec
        .args
        .iter()
        .map(|a| a.replace("{root}", &root).replace("{out}", &out_str))
        .collect();
    ScipRunPlan::Runnable {
        language: language.to_string(),
        program: program.to_string_lossy().into_owned(),
        args,
        cwd: cwd.to_path_buf(),
        out,
        writes_cwd_index: spec.writes_cwd_index,
    }
}

/// Every plan needed to cover `language` this run. All languages but Go yield a
/// single plan rooted at the repo. **Go** yields one plan per `go.mod` directory
/// (a repo may nest its module under `asc-cli/`, or hold several modules), each
/// writing a distinct `go.scip` / `go-<n>.scip` the overlay then path-rebases by
/// `metadata.project_root`. Without this, `scip-go` run at a module-less repo
/// root indexes nothing and the precise Go call graph silently vanishes.
pub fn plan_language(
    language: &str,
    repo_root: &Path,
    scip_dir: &Path,
    probe: &dyn Fn(&str) -> Option<PathBuf>,
) -> Vec<ScipRunPlan> {
    if language != "go" {
        return vec![plan_with(language, repo_root, scip_dir, probe)];
    }
    let Some(spec) = spec_for("go") else {
        return vec![ScipRunPlan::Unsupported {
            language: "go".to_string(),
        }];
    };
    let env_key = env_override_for("go");
    let configured = std::env::var(&env_key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let binary = configured.unwrap_or_else(|| spec.binary.to_string());
    let Some(program) = probe(&binary) else {
        return vec![ScipRunPlan::Skipped {
            language: "go".to_string(),
            reason: format!(
                "未在 PATH 找到 `{binary}`（go 的 SCIP indexer）；安装后 `groundgraph index` 会自动生成 .groundgraph/scip/go.scip，或设 {env_key} 指定路径",
            ),
        }];
    };
    let mut dirs = go_module_dirs(repo_root);
    if dirs.is_empty() {
        dirs.push(repo_root.to_path_buf());
    }
    dirs.iter()
        .enumerate()
        .map(|(i, dir)| {
            let out = if i == 0 {
                scip_dir.join("go.scip")
            } else {
                scip_dir.join(format!("go-{i}.scip"))
            };
            plan_in(spec, "go", &program, repo_root, dir, out)
        })
        .collect()
}

/// Directories under `repo_root` that hold a `go.mod` (each a separate Go
/// module), sorted and deduped, pruning vendored/cache/hidden subtrees the same
/// way the structural walk does ([`crate::treesitter::ALWAYS_SKIP_DIRS`]). A
/// repo whose module sits at the root yields just `[repo_root]`; one nested
/// under `asc-cli/` yields `[repo_root/asc-cli]`.
fn go_module_dirs(repo_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for entry in walkdir::WalkDir::new(repo_root)
        .into_iter()
        .filter_entry(|e| !is_skipped_module_scan_dir(e))
        .flatten()
    {
        if entry.file_type().is_file() && entry.file_name() == "go.mod" {
            if let Some(parent) = entry.path().parent() {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Prune a directory from the `go.mod` scan: hidden dirs below the root (build /
/// cache / agent worktrees) and the always-skip set (`vendor`, `node_modules`,
/// `.git`, `DerivedData`, …) — so a vendored module's `go.mod` never spawns an
/// extra `scip-go` run.
fn is_skipped_module_scan_dir(e: &walkdir::DirEntry) -> bool {
    if !e.file_type().is_dir() {
        return false;
    }
    let name = e.file_name().to_str().unwrap_or("");
    if e.depth() > 0 && name.starts_with('.') {
        return true;
    }
    // An embedded git repo (depth>0 dir with its own `.git/`) is a different
    // project; its `go.mod` is not ours, so never spawn a `scip-go` there.
    if e.depth() > 0 && e.path().join(".git").is_dir() {
        return true;
    }
    crate::treesitter::ALWAYS_SKIP_DIRS.contains(&name)
}

/// Remove a language's prior `.scip` outputs (`<lang>.scip` and `<lang>-<n>.scip`)
/// before regenerating, so a changed Go module layout (fewer modules than last
/// run) never leaves an orphaned index for the overlay to ingest.
fn clear_language_outputs(scip_dir: &Path, language: &str) {
    let Ok(entries) = std::fs::read_dir(scip_dir) else {
        return;
    };
    let exact = format!("{language}.scip");
    let numbered = format!("{language}-");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // `.scip` outputs plus their `.scip.inputs` staleness sidecars.
        let stem = name.strip_suffix(".inputs").unwrap_or(name);
        let is_output = stem == exact
            || (stem.starts_with(&numbered)
                && stem.ends_with(".scip")
                && stem[numbered.len()..stem.len() - ".scip".len()]
                    .chars()
                    .all(|c| c.is_ascii_digit()));
        if is_output {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Locate an executable. An override that already contains a path separator is
/// honoured verbatim (when it exists); a bare name is searched on each `PATH`
/// entry. Mirrors a minimal `which` — good enough for indexer discovery.
fn binary_on_path(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 || candidate.is_absolute() {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let full = dir.join(name);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

/// Outcome of one indexer invocation, surfaced by the CLI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ScipRunStatus {
    /// The indexer ran and wrote its `.scip`.
    Generated,
    /// Sources unchanged since the last successful run — the existing
    /// `.scip` was reused without spawning the indexer. SCIP indexers are
    /// full type-checkers (scip-python on django: ~4 min / 3.9 GB), so this
    /// is the difference between "re-index is free" and "re-index hurts".
    UpToDate,
    /// Binary absent / not configured — structure-only for this language.
    Skipped(String),
    /// No auto-invoke spec (ingest a hand-placed `.scip` instead).
    Unsupported,
    /// Binary present but the run failed (non-zero exit / spawn error).
    Failed(String),
}

/// One language's auto-invoke result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScipRunOutcome {
    pub language: String,
    pub status: ScipRunStatus,
    pub output: Option<PathBuf>,
}

/// Auto-invoke the SCIP indexer for each language in `languages` (deduped),
/// writing `.groundgraph/scip/<lang>.scip`. Uses the live `PATH`. Never errors:
/// a missing or failing indexer degrades to a recorded `Skipped`/`Failed`
/// outcome so the structural graph still indexes.
pub fn run_indexers(repo_root: &Path, languages: &[String]) -> Vec<ScipRunOutcome> {
    run_indexers_with(repo_root, languages, &|b| binary_on_path(b))
}

/// [`run_indexers`] with an injectable binary resolver. Tests pass a stub
/// `probe` so they never have to mutate the process environment to point at a
/// fake indexer (process-wide `set_var` races with every other test thread that
/// reads `std::env`, which is UB — #269). Production uses the real `PATH` probe.
pub(crate) fn run_indexers_with(
    repo_root: &Path,
    languages: &[String],
    probe: &dyn Fn(&str) -> Option<PathBuf>,
) -> Vec<ScipRunOutcome> {
    // Absolutize the root for subprocess use: Go's `scip-go` runs in a *module*
    // subdir (its `cwd` != repo root), so a relative `--output ./.groundgraph/...`
    // would resolve against that subdir and fail ("no such file or directory").
    // Canonicalizing here keeps both `{out}` and `{root}` independent of where
    // each indexer is spawned. This only affects the subprocess argv/paths — the
    // graph and the overlay carry their own (caller-supplied) root.
    let repo_root = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let repo_root = repo_root.as_path();
    let scip_dir = crate::config::workspace_dir_for_repo(repo_root).join("scip");
    let mut outcomes = Vec::new();
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for language in languages {
        if !seen.insert(language.as_str()) {
            continue;
        }
        let plans = plan_language(language, repo_root, &scip_dir, probe);
        // Incremental gate: when EVERY runnable plan's source digest matches
        // the digest recorded by the last successful run (and its `.scip` is
        // still on disk), reuse the whole language's outputs without spawning
        // anything. Mixed staleness re-runs everything for the language —
        // half-old half-new outputs would be confusing to debug.
        let runnable: Vec<&ScipRunPlan> = plans
            .iter()
            .filter(|p| matches!(p, ScipRunPlan::Runnable { .. }))
            .collect();
        let all_up_to_date = !runnable.is_empty()
            && runnable.iter().all(|p| {
                let ScipRunPlan::Runnable { cwd, out, .. } = p else {
                    return false;
                };
                out.is_file()
                    && stored_inputs_digest(out)
                        .is_some_and(|prev| prev == inputs_digest(cwd, language))
            });
        if all_up_to_date {
            for plan in plans {
                if let ScipRunPlan::Runnable { language, out, .. } = plan {
                    outcomes.push(ScipRunOutcome {
                        language,
                        status: ScipRunStatus::UpToDate,
                        output: Some(out),
                    });
                }
            }
            continue;
        }
        // Clear this language's prior outputs before regenerating, but only when
        // we actually have a runnable plan — an absent indexer (all-Skipped)
        // leaves any previously generated index untouched.
        if !runnable.is_empty() {
            clear_language_outputs(&scip_dir, language);
        }
        for plan in plans {
            let outcome = match plan {
                ScipRunPlan::Unsupported { language } => ScipRunOutcome {
                    language,
                    status: ScipRunStatus::Unsupported,
                    output: None,
                },
                ScipRunPlan::Skipped { language, reason } => ScipRunOutcome {
                    language,
                    status: ScipRunStatus::Skipped(reason),
                    output: None,
                },
                ScipRunPlan::Runnable {
                    language,
                    program,
                    args,
                    cwd,
                    out,
                    writes_cwd_index,
                } => {
                    if let Err(e) = std::fs::create_dir_all(&scip_dir) {
                        outcomes.push(ScipRunOutcome {
                            language,
                            status: ScipRunStatus::Failed(format!(
                                "创建 {} 失败: {e}",
                                scip_dir.display()
                            )),
                            output: None,
                        });
                        continue;
                    }
                    let digest = inputs_digest(&cwd, &language);
                    let outcome = execute(&program, &args, &cwd, &out, writes_cwd_index, language);
                    if matches!(outcome.status, ScipRunStatus::Generated) {
                        store_inputs_digest(&out, digest);
                    }
                    outcome
                }
            };
            outcomes.push(outcome);
        }
    }
    outcomes
}

/// File extensions whose content feeds each language's SCIP indexer. Only
/// these participate in the staleness digest.
fn language_source_exts(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &["rs"],
        "go" => &["go", "mod", "sum"],
        "typescript" => &["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "json"],
        "python" => &["py", "pyi", "toml", "cfg"],
        "dart" => &["dart", "yaml"],
        _ => &[],
    }
}

/// Digest of every relevant source file under `cwd` for `language`:
/// `(relative path, length, mtime)` tuples hashed in sorted order — the same
/// staleness contract `make`/`ninja` use. Content is NOT read: a digest of a
/// 3 000-file tree costs milliseconds. Touch-without-change only causes a
/// redundant re-run. The converse (same length AND same mtime after a real
/// edit — needs a deliberate `touch -r` or a 1-second-resolution FS) can
/// keep a stale overlay, like every mtime-based build system; `--force`
/// re-runs unconditionally (issues2.md #33: accepted trade-off).
fn inputs_digest(cwd: &Path, language: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let exts = language_source_exts(language);
    let mut entries: Vec<(String, u64, u128)> = Vec::new();
    for entry in walkdir::WalkDir::new(cwd)
        .into_iter()
        .filter_entry(|e| !is_skipped_module_scan_dir(e))
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !exts.contains(&ext.to_ascii_lowercase().as_str()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let rel = path
            .strip_prefix(cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        entries.push((rel, meta.len(), mtime));
    }
    entries.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    language.hash(&mut hasher);
    entries.hash(&mut hasher);
    hasher.finish()
}

/// A cheap change-detector for a file: `(mtime, len)`. `None` when the file is
/// absent. Used to tell a freshly written `index.scip` from a pre-existing one
/// (#268); the `len` guards the rare case of a same-instant overwrite.
fn file_signature(p: &Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// `<out>.inputs` sidecar holding the digest of the run that produced `out`.
fn inputs_sidecar(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(".inputs");
    PathBuf::from(s)
}

fn stored_inputs_digest(out: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(inputs_sidecar(out)).ok()?;
    u64::from_str_radix(text.trim(), 16).ok()
}

fn store_inputs_digest(out: &Path, digest: u64) {
    // Best-effort: a failed write only means the next run is not skipped.
    let _ = std::fs::write(inputs_sidecar(out), format!("{digest:x}"));
}

/// Run one indexer subprocess and classify the result.
fn execute(
    program: &str,
    args: &[String],
    cwd: &Path,
    out: &Path,
    writes_cwd_index: bool,
    language: String,
) -> ScipRunOutcome {
    // `writes_cwd_index` indexers (Dart `scip_dart`) ignore `--output` and emit
    // a fixed `index.scip` into the cwd (= repo root). Snapshot any pre-existing
    // file *before* the run so we only ever move one this run actually wrote:
    // otherwise a user's own committed `index.scip` gets moved aside, or a stale
    // leftover from a prior run is ingested as if fresh. (#268)
    let cwd_index = cwd.join("index.scip");
    let pre = if writes_cwd_index {
        file_signature(&cwd_index)
    } else {
        None
    };
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd);
    match run_with_capped_stderr(&mut cmd) {
        Ok((status, _)) if status.success() => {
            if writes_cwd_index {
                let post = file_signature(&cwd_index);
                let fresh = match (&pre, &post) {
                    (_, None) => false,           // nothing at index.scip
                    (None, Some(_)) => true,      // created this run
                    (Some(a), Some(b)) => a != b, // overwritten this run
                };
                if post.is_some() && !fresh {
                    return ScipRunOutcome {
                        language,
                        status: ScipRunStatus::Failed(
                            "scip_dart 成功退出但未生成新的 index.scip（仓库根已存在同名旧文件，已保留未触碰）"
                                .to_string(),
                        ),
                        output: None,
                    };
                }
                if fresh {
                    if let Err(e) = std::fs::rename(&cwd_index, out) {
                        return ScipRunOutcome {
                            language,
                            status: ScipRunStatus::Failed(format!("移动 index.scip 失败: {e}")),
                            output: None,
                        };
                    }
                }
            }
            if out.is_file() {
                ScipRunOutcome {
                    language,
                    status: ScipRunStatus::Generated,
                    output: Some(out.to_path_buf()),
                }
            } else {
                ScipRunOutcome {
                    language,
                    status: ScipRunStatus::Failed("indexer 成功退出但未产出 .scip".to_string()),
                    output: None,
                }
            }
        }
        Ok((status, stderr)) => {
            let summary = summarize_stderr(&stderr);
            let mut msg = format!("退出码 {status}: {summary}");
            if let Some(hint) = scip_failure_hint(program, &summary) {
                msg.push_str(&format!("（{hint}）"));
            }
            ScipRunOutcome {
                language,
                status: ScipRunStatus::Failed(msg),
                output: None,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => ScipRunOutcome {
            language,
            // A hung indexer is non-fatal: the structural graph still indexed,
            // only the precision overlay is missing (#77).
            status: ScipRunStatus::Failed(format!(
                "`{program}` 超时被终止（{e}）；可设 {SCIP_TIMEOUT_ENV} 调整预算，结构图不受影响"
            )),
            output: None,
        },
        Err(e) => ScipRunOutcome {
            language,
            status: ScipRunStatus::Failed(format!("无法启动 `{program}`: {e}")),
            output: None,
        },
    }
}

/// Keep at most this much of an indexer's stderr in memory. We only need the
/// leading error line for the summary; verbose indexers on big monorepos can
/// emit hundreds of MB (issues.md #14).
const STDERR_CAP_BYTES: usize = 64 * 1024;

/// Run `cmd`, discarding stdout entirely (the index artefact goes to a file,
/// never to stdout) and retaining only a capped prefix of stderr. The pipe is
/// drained to EOF past the cap so the child can never block on a full pipe.
/// `GROUNDGRAPH_SCIP_TIMEOUT_SECS` overrides the per-indexer wall-clock budget
/// (#77). Generous default: `rust-analyzer scip` / `scip-python` on a large
/// monorepo legitimately runs for minutes, so we only guard against a *hung*
/// indexer (one that never closes stderr / never exits), not a slow one.
const SCIP_TIMEOUT_ENV: &str = "GROUNDGRAPH_SCIP_TIMEOUT_SECS";
const DEFAULT_SCIP_TIMEOUT_SECS: u64 = 600;

fn scip_timeout() -> std::time::Duration {
    let raw = std::env::var(SCIP_TIMEOUT_ENV).ok();
    parse_scip_timeout(raw.as_deref())
}

/// Pure policy for [`scip_timeout`] — kept env-free so it is unit-testable
/// without mutating process-global state (cf. #65). A missing, non-numeric, or
/// zero value falls back to the default budget.
fn parse_scip_timeout(raw: Option<&str>) -> std::time::Duration {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map_or_else(
            || std::time::Duration::from_secs(DEFAULT_SCIP_TIMEOUT_SECS),
            std::time::Duration::from_secs,
        )
}

fn run_with_capped_stderr(
    cmd: &mut Command,
) -> std::io::Result<(std::process::ExitStatus, Vec<u8>)> {
    run_with_capped_stderr_budget(cmd, scip_timeout())
}

/// Run `cmd` under a wall-clock `budget` (#77). A reader thread drains stderr
/// to EOF (capped) so the child can never block on a full pipe, while the main
/// thread polls `try_wait`; if the budget is exceeded the child is killed and
/// `ErrorKind::TimedOut` is returned. Previously this blocked on
/// `stderr.read()` to EOF with no timeout, so a hung indexer (rust-analyzer
/// stuck on a bad toolchain, a sidecar that never exits) hung `groundgraph index`
/// indefinitely. The Dart sidecar already had this guard; SCIP did not.
fn run_with_capped_stderr_budget(
    cmd: &mut Command,
    budget: std::time::Duration,
) -> std::io::Result<(std::process::ExitStatus, Vec<u8>)> {
    use std::io::Read;
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // #68: own process group so a budget kill also reaps any subprocesses the
    // indexer forked (rust-analyzer / scip-* tooling).
    crate::proc::detach_process_group(cmd);
    let mut child = cmd.spawn()?;
    // `Stdio::piped()` makes this Some in practice, but surface an error instead
    // of panicking if a future refactor (or fd exhaustion) breaks that invariant.
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("stderr pipe missing despite Stdio::piped()"))?;
    let reader = std::thread::spawn(move || -> Vec<u8> {
        let mut pipe = stderr;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() < STDERR_CAP_BYTES {
                        let take = n.min(STDERR_CAP_BYTES - buf.len());
                        buf.extend_from_slice(&chunk[..take]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        buf
    });
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let buf = reader.join().unwrap_or_default();
                return Ok((status, buf));
            }
            Ok(None) => {
                if started.elapsed() > budget {
                    // #68/#77: take the whole group down, bound the reap.
                    crate::proc::kill_and_reap(&mut child, std::time::Duration::from_secs(2));
                    let _ = reader.join();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("indexer exceeded the {}s budget", budget.as_secs()),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                crate::proc::kill_and_reap(&mut child, std::time::Duration::from_secs(2));
                let _ = reader.join();
                return Err(e);
            }
        }
    }
}

/// Distil an indexer's stderr into one actionable line. Indexers print the real
/// cause first and then a stack trace, so the *last* line is often a useless
/// frame (`9: _main`); prefer the first line that mentions an error, falling
/// back to the first non-empty line. Capped so a wall of output never floods the
/// `index` summary.
fn summarize_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let pick = text
        .lines()
        .map(str::trim)
        .find(|l| {
            let low = l.to_ascii_lowercase();
            low.starts_with("error") || low.contains("error:")
        })
        .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    const MAX: usize = 240;
    if pick.chars().count() > MAX {
        let truncated: String = pick.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        pick.to_string()
    }
}

/// A targeted, actionable hint for well-known indexer failures, appended to the
/// summary. Today: the rustup `rust-analyzer` proxy failing because the repo's
/// pinned toolchain lacks the component — common in Rust repos with a
/// `rust-toolchain.toml`. The structural graph is unaffected either way.
fn scip_failure_hint(program: &str, summary: &str) -> Option<String> {
    let low = summary.to_ascii_lowercase();
    if program.contains("rust-analyzer")
        && (low.contains("unknown binary")
            || low.contains("toolchain")
            || low.contains("no such")
            || low.contains("not found"))
    {
        return Some(
            "精确层可选：`rustup component add rust-analyzer`，或装独立 rust-analyzer 后设 \
             GROUNDGRAPH_SCIP_RUST_BIN 指向它；结构图不受影响"
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub binary resolver used by the planner tests.
    type BinFinder = Box<dyn Fn(&str) -> Option<PathBuf>>;

    #[cfg(unix)]
    #[test]
    fn run_with_capped_stderr_drains_oversized_output_without_buffering_it_all() {
        // ~2 MiB of stderr — far beyond the cap. The runner must
        // (a) not deadlock on a full pipe, (b) bound the buffer, and
        // (c) still report the exit status (issues.md #14).
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "i=0; while [ $i -lt 2048 ]; do printf '%01024d' 0 >&2; i=$((i+1)); done",
        ]);
        let (status, stderr) = run_with_capped_stderr(&mut cmd).expect("spawn sh");
        assert!(status.success());
        assert!(
            stderr.len() <= STDERR_CAP_BYTES,
            "stderr buffer must be capped: got {} bytes",
            stderr.len()
        );
        assert!(!stderr.is_empty(), "capped prefix must still be kept");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_capped_stderr_budget_kills_a_hung_indexer() {
        // #77: a child that never exits (and never closes stderr) must not hang
        // `index` — the budget kills it and reports a timeout promptly.
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30"]);
        let started = std::time::Instant::now();
        let err = run_with_capped_stderr_budget(&mut cmd, std::time::Duration::from_millis(300))
            .expect_err("a hung child must time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "must return ~immediately after the budget, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn parse_scip_timeout_defaults_and_honours_positive_overrides() {
        let default = std::time::Duration::from_secs(DEFAULT_SCIP_TIMEOUT_SECS);
        assert_eq!(parse_scip_timeout(None), default, "unset → default");
        assert_eq!(parse_scip_timeout(Some("nope")), default, "non-numeric");
        assert_eq!(parse_scip_timeout(Some("0")), default, "zero rejected");
        assert_eq!(parse_scip_timeout(Some("")), default, "empty rejected");
        assert_eq!(
            parse_scip_timeout(Some("  5 ")),
            std::time::Duration::from_secs(5),
            "trimmed positive override honoured"
        );
    }

    #[test]
    fn summarize_stderr_prefers_the_error_line_over_a_backtrace_tail() {
        // rust-analyzer prints the cause first, then a panic backtrace whose
        // last line (`9: _main`) is useless — the old `lines().last()` surfaced
        // exactly that.
        let stderr = b"error: could not find Cargo.toml\n   1: foo\n   9: _main\n";
        let s = summarize_stderr(stderr);
        assert!(s.contains("could not find Cargo.toml"), "got: {s}");
        assert!(
            !s.contains("_main"),
            "must not surface the backtrace tail: {s}"
        );
    }

    #[test]
    fn rust_analyzer_toolchain_failure_gets_an_actionable_hint() {
        let summary = "error: Unknown binary 'rust-analyzer' in official toolchain '1.96.0'.";
        let hint = scip_failure_hint("/home/u/.cargo/bin/rust-analyzer", summary)
            .expect("rust-analyzer toolchain error must yield a hint");
        assert!(hint.contains("rustup component add rust-analyzer"));
        // An unrelated indexer error gets no (misleading) hint.
        assert!(scip_failure_hint("/usr/bin/scip-go", "error: boom").is_none());
    }

    fn always_found(path: &str) -> BinFinder {
        let path = path.to_string();
        Box::new(move |_bin: &str| Some(PathBuf::from(&path)))
    }

    #[test]
    fn second_run_with_unchanged_sources_is_up_to_date_and_does_not_respawn() {
        // scip-python on django costs ~4 minutes / 3.9 GB; an unchanged tree
        // must reuse the previous `.scip` instead of paying that again.
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        std::fs::write(repo.join("a.py"), "def f():\n    return 1\n").unwrap();
        // Fake indexer: appends one line to `calls.log`, writes the output
        // file given as the last argv (mirrors `--output <out>`).
        let fake = repo.join("fake-scip.sh");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho run >> \"$(dirname \"$0\")/calls.log\"\n\
             for last; do :; done\necho scip > \"$last\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Inject the fake via a probe instead of `GROUNDGRAPH_SCIP_PYTHON_BIN`:
        // a process-wide `set_var` races with every other test thread reading
        // `std::env` (UB). (#269)
        let fake_path = fake.clone();
        let probe = move |b: &str| (b == "scip-python").then(|| fake_path.clone());

        let calls = || {
            std::fs::read_to_string(repo.join("calls.log"))
                .map(|s| s.lines().count())
                .unwrap_or(0)
        };
        let r1 = run_indexers_with(repo, &["python".to_string()], &probe);
        assert!(
            matches!(r1[0].status, ScipRunStatus::Generated),
            "first run generates: {:?}",
            r1[0].status
        );
        assert_eq!(calls(), 1);

        let r2 = run_indexers_with(repo, &["python".to_string()], &probe);
        assert!(
            matches!(r2[0].status, ScipRunStatus::UpToDate),
            "unchanged tree reuses the output: {:?}",
            r2[0].status
        );
        assert_eq!(calls(), 1, "indexer must not respawn on an unchanged tree");
        assert!(r2[0].output.as_ref().is_some_and(|p| p.is_file()));

        // Touching a source invalidates the digest and re-runs the indexer.
        std::fs::write(repo.join("a.py"), "def f():\n    return 2\n").unwrap();
        let r3 = run_indexers_with(repo, &["python".to_string()], &probe);
        assert!(
            matches!(r3[0].status, ScipRunStatus::Generated),
            "changed tree regenerates: {:?}",
            r3[0].status
        );
        assert_eq!(calls(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn execute_does_not_consume_preexisting_cwd_index() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let cwd = dir.path();
        // A user's own (or stale) index.scip already sits at the repo root.
        std::fs::write(cwd.join("index.scip"), "PREEXISTING-STALE").unwrap();
        // Fake scip_dart that "succeeds" but writes no index.scip.
        let fake = cwd.join("noop.sh");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = cwd.join("dart.scip");
        let outcome = execute(
            fake.to_str().unwrap(),
            &[],
            cwd,
            &out,
            true,
            "dart".to_string(),
        );
        assert!(
            matches!(outcome.status, ScipRunStatus::Failed(_)),
            "a pre-existing/stale index.scip must not be consumed: {:?}",
            outcome.status
        );
        assert!(!out.exists(), "must not move the stale file to the output");
        assert_eq!(
            std::fs::read_to_string(cwd.join("index.scip")).unwrap(),
            "PREEXISTING-STALE",
            "the pre-existing file must be left untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_moves_freshly_written_cwd_index() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let cwd = dir.path();
        // Fake scip_dart that writes a fresh index.scip into the cwd.
        let fake = cwd.join("gen.sh");
        std::fs::write(&fake, "#!/bin/sh\necho FRESH-INDEX > index.scip\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = cwd.join("dart.scip");
        let outcome = execute(
            fake.to_str().unwrap(),
            &[],
            cwd,
            &out,
            true,
            "dart".to_string(),
        );
        assert!(
            matches!(outcome.status, ScipRunStatus::Generated),
            "a freshly written index.scip is moved to the output: {:?}",
            outcome.status
        );
        assert!(out.is_file());
        assert!(
            !cwd.join("index.scip").exists(),
            "the fresh file is moved, not copied"
        );
    }

    fn never_found() -> BinFinder {
        Box::new(|_bin: &str| None)
    }

    #[test]
    fn rust_plan_is_runnable_when_binary_present() {
        let repo = PathBuf::from("/repo");
        let scip = repo.join(".groundgraph/scip");
        let plan = plan_with(
            "rust",
            &repo,
            &scip,
            &*always_found("/usr/bin/rust-analyzer"),
        );
        match plan {
            ScipRunPlan::Runnable {
                program,
                args,
                cwd,
                out,
                writes_cwd_index,
                ..
            } => {
                assert_eq!(program, "/usr/bin/rust-analyzer");
                assert_eq!(
                    args,
                    vec![
                        "scip".to_string(),
                        "/repo".to_string(),
                        "--output".to_string(),
                        scip.join("rust.scip").to_string_lossy().into_owned(),
                    ]
                );
                assert_eq!(cwd, repo);
                assert_eq!(out, scip.join("rust.scip"));
                assert!(!writes_cwd_index);
            }
            other => panic!("expected Runnable, got {other:?}"),
        }
    }

    #[test]
    fn absent_binary_yields_skip_with_actionable_reason() {
        let repo = PathBuf::from("/repo");
        let scip = repo.join(".groundgraph/scip");
        let plan = plan_with("go", &repo, &scip, &*never_found());
        match plan {
            ScipRunPlan::Skipped { language, reason } => {
                assert_eq!(language, "go");
                assert!(
                    reason.contains("scip-go"),
                    "reason should name the missing binary: {reason}"
                );
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn unknown_language_is_unsupported() {
        let repo = PathBuf::from("/repo");
        let scip = repo.join(".groundgraph/scip");
        let plan = plan_with("haskell", &repo, &scip, &*always_found("/x"));
        assert!(matches!(plan, ScipRunPlan::Unsupported { .. }));
    }

    #[test]
    fn python_plan_passes_project_version_to_avoid_undefined_crash() {
        // scip-python defaults `--project-version` to the git revision; in a repo
        // with no `.git` and no `pyproject.toml` that version is `undefined` and it
        // crashes (`normalizeNameOrVersion` reads `indexOf` of undefined). Always
        // passing an explicit version keeps the indexer from aborting the whole
        // Python layer over an upstream NPE.
        let repo = PathBuf::from("/repo");
        let scip = repo.join(".groundgraph/scip");
        let plan = plan_with(
            "python",
            &repo,
            &scip,
            &*always_found("/usr/bin/scip-python"),
        );
        match plan {
            ScipRunPlan::Runnable { args, .. } => {
                let v = args.iter().position(|a| a == "--project-version");
                assert!(
                    v.is_some_and(|i| args.get(i + 1).is_some_and(|s| !s.is_empty())),
                    "python plan must pass a non-empty --project-version: {args:?}"
                );
            }
            other => panic!("expected Runnable, got {other:?}"),
        }
    }

    fn runnable_cwds_outs(plans: &[ScipRunPlan]) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut cwds = Vec::new();
        let mut outs = Vec::new();
        for p in plans {
            if let ScipRunPlan::Runnable { cwd, out, .. } = p {
                cwds.push(cwd.clone());
                outs.push(out.clone());
            }
        }
        (cwds, outs)
    }

    #[test]
    fn go_plan_runs_in_nested_module_dir_not_repo_root() {
        // The defect: a repo whose `go.mod` lives under `asc-cli/` (not the root)
        // must have `scip-go` run *in* `asc-cli/`, or it resolves no module and
        // indexes nothing. The graph stays rooted at the repo (the overlay
        // rebases the module-relative paths by `project_root`).
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("asc-cli")).unwrap();
        std::fs::write(repo.join("asc-cli/go.mod"), "module x\n").unwrap();
        let scip = repo.join(".groundgraph/scip");

        let plans = plan_language("go", repo, &scip, &*always_found("/usr/bin/scip-go"));
        assert_eq!(plans.len(), 1, "one module → one plan");
        match &plans[0] {
            ScipRunPlan::Runnable { cwd, out, args, .. } => {
                assert_eq!(cwd, &repo.join("asc-cli"), "scip-go runs in the module dir");
                assert_eq!(out, &scip.join("go.scip"));
                assert_eq!(
                    args,
                    &vec![
                        "index".to_string(),
                        "--output".to_string(),
                        scip.join("go.scip").to_string_lossy().into_owned(),
                    ]
                );
            }
            other => panic!("expected Runnable, got {other:?}"),
        }
    }

    #[test]
    fn go_plan_covers_every_module_and_skips_vendor() {
        // Several modules each get their own run with a distinct output; vendored
        // / node_modules `go.mod` files are pruned (never first-party).
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        for sub in ["svc-a", "svc-b", "vendor/dep", "node_modules/pkg"] {
            std::fs::create_dir_all(repo.join(sub)).unwrap();
            std::fs::write(repo.join(sub).join("go.mod"), "module x\n").unwrap();
        }
        let scip = repo.join(".groundgraph/scip");

        let plans = plan_language("go", repo, &scip, &*always_found("/usr/bin/scip-go"));
        let (cwds, outs) = runnable_cwds_outs(&plans);
        assert_eq!(
            cwds,
            vec![repo.join("svc-a"), repo.join("svc-b")],
            "only first-party modules, sorted; vendor/node_modules pruned"
        );
        assert_eq!(
            outs,
            vec![scip.join("go.scip"), scip.join("go-1.scip")],
            "distinct output per module"
        );
    }

    #[test]
    fn go_plan_falls_back_to_repo_root_when_no_module() {
        // No `go.mod` anywhere → a single run at the repo root (legacy behavior;
        // harmless, since there is no module to index).
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        let scip = repo.join(".groundgraph/scip");

        let plans = plan_language("go", repo, &scip, &*always_found("/usr/bin/scip-go"));
        assert_eq!(plans.len(), 1);
        let (cwds, outs) = runnable_cwds_outs(&plans);
        assert_eq!(cwds, vec![repo.to_path_buf()]);
        assert_eq!(outs, vec![scip.join("go.scip")]);
    }

    #[test]
    fn go_plan_module_at_repo_root_is_unchanged() {
        // The common case (go.mod at the repo root, e.g. platform-go) must keep
        // running at the root writing `go.scip` — no behavior change.
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        std::fs::write(repo.join("go.mod"), "module x\n").unwrap();
        let scip = repo.join(".groundgraph/scip");

        let plans = plan_language("go", repo, &scip, &*always_found("/usr/bin/scip-go"));
        assert_eq!(plans.len(), 1);
        let (cwds, outs) = runnable_cwds_outs(&plans);
        assert_eq!(cwds, vec![repo.to_path_buf()]);
        assert_eq!(outs, vec![scip.join("go.scip")]);
    }

    #[test]
    fn non_go_language_runs_once_at_repo_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        // A nested go.mod must not affect a non-Go language's single root run.
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(repo.join("sub/go.mod"), "module x\n").unwrap();
        let scip = repo.join(".groundgraph/scip");

        let plans = plan_language(
            "python",
            repo,
            &scip,
            &*always_found("/usr/bin/scip-python"),
        );
        assert_eq!(plans.len(), 1);
        let (cwds, outs) = runnable_cwds_outs(&plans);
        assert_eq!(cwds, vec![repo.to_path_buf()]);
        assert_eq!(outs, vec![scip.join("python.scip")]);
    }

    #[test]
    fn go_plan_skipped_when_binary_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("asc-cli")).unwrap();
        std::fs::write(repo.join("asc-cli/go.mod"), "module x\n").unwrap();
        let scip = repo.join(".groundgraph/scip");

        let plans = plan_language("go", repo, &scip, &*never_found());
        assert_eq!(plans.len(), 1);
        assert!(matches!(plans[0], ScipRunPlan::Skipped { .. }));
    }
}
