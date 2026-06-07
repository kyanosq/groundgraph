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
        cwd: repo_root.to_path_buf(),
        out,
        writes_cwd_index: spec.writes_cwd_index,
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
    let scip_dir = repo_root.join(".specslice").join("scip");
    let mut outcomes = Vec::new();
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for language in languages {
        if !seen.insert(language.as_str()) {
            continue;
        }
        let plan = plan_with(language, repo_root, &scip_dir, &|b| binary_on_path(b));
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
}
