//! Auto-invoke installed SCIP indexers (ADR-0001 R1/R2).
//!
//! When `specslice index` runs, for every indexed language whose SCIP indexer
//! binary is on `PATH`, invoke it **once** — a one-shot batch process, not a
//! long-running LSP server — to regenerate `.specslice/scip/<lang>.scip`. The
//! [`scip_overlay`](crate::scip_overlay) then ingests that file as the
//! high-confidence `Calls`/`References` precision layer.
//!
//! This is what lets SCIP be the *single* precision source without sacrificing
//! the near-zero-config experience: the operator does not have to remember to
//! run `rust-analyzer scip` / `scip-go` / … by hand. A missing indexer binary
//! is a **silent skip** (with a recorded reason, surfaced by the CLI), so a
//! machine without the toolchain simply gets the structure-only graph — never
//! an error. Every byte we write lands under `.specslice/` (D1 non-invasive).
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
    /// `SPECSLICE_SCIP_<LANG>_BIN` env var).
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
    IndexerSpec {
        language: "python",
        binary: "scip-python",
        args: &["index", "--cwd", "{root}", "--output", "{out}"],
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
/// `SPECSLICE_SCIP_GO_BIN=/opt/scip-go`.
fn env_override_for(language: &str) -> String {
    format!("SPECSLICE_SCIP_{}_BIN", language.to_ascii_uppercase())
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
/// is written (`<repo>/.specslice/scip`).
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
                "未在 PATH 找到 `{binary}`（{language} 的 SCIP indexer）；安装后 `specslice index` 会自动生成 .specslice/scip/{language}.scip，或设 {env_key} 指定路径",
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
                "未在 PATH 找到 `{binary}`（go 的 SCIP indexer）；安装后 `specslice index` 会自动生成 .specslice/scip/go.scip，或设 {env_key} 指定路径",
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
        let is_output = name == exact
            || (name.starts_with(&numbered)
                && name.ends_with(".scip")
                && name[numbered.len()..name.len() - ".scip".len()]
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
/// writing `.specslice/scip/<lang>.scip`. Uses the live `PATH`. Never errors:
/// a missing or failing indexer degrades to a recorded `Skipped`/`Failed`
/// outcome so the structural graph still indexes.
pub fn run_indexers(repo_root: &Path, languages: &[String]) -> Vec<ScipRunOutcome> {
    // Absolutize the root for subprocess use: Go's `scip-go` runs in a *module*
    // subdir (its `cwd` ≠ repo root), so a relative `--output ./.specslice/...`
    // would resolve against that subdir and fail ("no such file or directory").
    // Canonicalizing here keeps both `{out}` and `{root}` independent of where
    // each indexer is spawned. This only affects the subprocess argv/paths — the
    // graph and the overlay carry their own (caller-supplied) root.
    let repo_root = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let repo_root = repo_root.as_path();
    let scip_dir = repo_root.join(".specslice").join("scip");
    let mut outcomes = Vec::new();
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for language in languages {
        if !seen.insert(language.as_str()) {
            continue;
        }
        let plans = plan_language(language, repo_root, &scip_dir, &|b| binary_on_path(b));
        // Clear this language's prior outputs before regenerating, but only when
        // we actually have a runnable plan — an absent indexer (all-Skipped)
        // leaves any previously generated index untouched.
        if plans
            .iter()
            .any(|p| matches!(p, ScipRunPlan::Runnable { .. }))
        {
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
                    execute(&program, &args, &cwd, &out, writes_cwd_index, language)
                }
            };
            outcomes.push(outcome);
        }
    }
    outcomes
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
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd);
    match cmd.output() {
        Ok(o) if o.status.success() => {
            if writes_cwd_index {
                let produced = cwd.join("index.scip");
                if produced.exists() {
                    if let Err(e) = std::fs::rename(&produced, out) {
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
        Ok(o) => {
            let tail = String::from_utf8_lossy(&o.stderr);
            let tail = tail.lines().last().unwrap_or("").trim().to_string();
            ScipRunOutcome {
                language,
                status: ScipRunStatus::Failed(format!("退出码 {}: {tail}", o.status)),
                output: None,
            }
        }
        Err(e) => ScipRunOutcome {
            language,
            status: ScipRunStatus::Failed(format!("无法启动 `{program}`: {e}")),
            output: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always_found(path: &str) -> Box<dyn Fn(&str) -> Option<PathBuf>> {
        let path = path.to_string();
        Box::new(move |_bin: &str| Some(PathBuf::from(&path)))
    }

    fn never_found() -> Box<dyn Fn(&str) -> Option<PathBuf>> {
        Box::new(|_bin: &str| None)
    }

    #[test]
    fn rust_plan_is_runnable_when_binary_present() {
        let repo = PathBuf::from("/repo");
        let scip = repo.join(".specslice/scip");
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
        let scip = repo.join(".specslice/scip");
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
        let scip = repo.join(".specslice/scip");
        let plan = plan_with("haskell", &repo, &scip, &*always_found("/x"));
        assert!(matches!(plan, ScipRunPlan::Unsupported { .. }));
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
        let scip = repo.join(".specslice/scip");

        let plans = plan_language("go", repo, &scip, &*always_found("/usr/bin/scip-go"));
        assert_eq!(plans.len(), 1, "one module → one plan");
        match &plans[0] {
            ScipRunPlan::Runnable {
                cwd, out, args, ..
            } => {
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
        let scip = repo.join(".specslice/scip");

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
        let scip = repo.join(".specslice/scip");

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
        let scip = repo.join(".specslice/scip");

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
        let scip = repo.join(".specslice/scip");

        let plans = plan_language("python", repo, &scip, &*always_found("/usr/bin/scip-python"));
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
        let scip = repo.join(".specslice/scip");

        let plans = plan_language("go", repo, &scip, &*never_found());
        assert_eq!(plans.len(), 1);
        assert!(matches!(plans[0], ScipRunPlan::Skipped { .. }));
    }
}
