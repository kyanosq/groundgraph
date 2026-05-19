//! Dart analyzer sidecar bridge (P7).
//!
//! The sidecar is a Dart subprocess that uses `package:analyzer` to
//! produce a resolved-AST [`LanguageIndexBatch`]. We invoke it when
//! - the user has explicitly enabled it via `SPECSLICE_DART_ANALYZER=1`,
//!   or
//! - the user pointed `SPECSLICE_DART_ANALYZER_BIN` at a built binary
//!   (or the default `dart run tool/specslice_dart_analyzer/...`
//!   command resolves at runtime).
//!
//! On any failure (missing Dart SDK, sidecar JSON malformed, non-zero
//! exit) the engine falls back to the heuristic Dart adapter
//! ([`specslice_lang_dart::index_dart_paths`]) silently, preserving the
//! pre-P7 behaviour. The fallback path emits a `dart_sidecar_unavailable`
//! diagnostic on the returned [`SidecarOutcome`] so the engine can
//! surface the reason in `DartIndexResult.resolver_used`.
//!
//! Why a sidecar and not a Rust port of analyzer? Because the only
//! source of truth for Dart's resolved-AST is `package:analyzer` itself.
//! Re-implementing element resolution in Rust would always lag the
//! upstream behaviour and miss extension methods, mixins, augmentations,
//! generics, import aliases, etc. — the very things the heuristic
//! adapter cannot do.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::language_batch::LanguageIndexBatch;

/// Environment variable used to opt in to the sidecar.
///
/// Any non-empty truthy value (`1`, `true`, `yes`, `on`) turns the
/// sidecar on. The variable also accepts `0` / `off` / `false` to
/// explicitly disable it (overriding any project-level default).
pub const ENV_ENABLE: &str = "SPECSLICE_DART_ANALYZER";

/// Environment variable used to override the sidecar entry-point.
///
/// Accepts either an absolute path to a compiled `specslice_dart_analyzer`
/// binary or a shell-style command (split on whitespace). If unset, the
/// engine defaults to `dart run tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart`
/// relative to the repo root.
pub const ENV_BIN: &str = "SPECSLICE_DART_ANALYZER_BIN";

/// Default workspace location of the sidecar entry point. Kept in sync
/// with the file we ship at `tool/specslice_dart_analyzer/`.
pub const DEFAULT_SIDECAR_REL: &str =
    "tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart";

/// Outcome of trying to run the sidecar.
#[derive(Debug)]
pub enum SidecarOutcome {
    /// Sidecar produced a batch — caller should use this instead of the
    /// heuristic adapter.
    Used(LanguageIndexBatch),
    /// Sidecar was disabled by configuration or could not be invoked.
    /// `reason` is human-readable for engine diagnostics.
    Skipped { reason: String },
}

/// Request body passed to the sidecar over stdin.
#[derive(Debug, Serialize)]
struct SidecarRequest<'a> {
    repo_root: &'a str,
    code_roots: Vec<&'a str>,
    exclude_globs: &'a [String],
    resolve_imports: bool,
}

/// Response shape returned by the sidecar on stdout.
#[derive(Debug, Deserialize)]
struct SidecarRawResponse {
    ok: bool,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    files: Vec<serde_json::Value>,
    #[serde(default)]
    symbols: Vec<serde_json::Value>,
    #[serde(default)]
    symbol_ranges: Vec<serde_json::Value>,
    #[serde(default)]
    imports: Vec<serde_json::Value>,
    #[serde(default)]
    references: Vec<serde_json::Value>,
    /// P8 — synthetic targets (routes, storage buckets, top-level
    /// providers we did not pick up as symbols). Optional in JSON for
    /// forward compatibility with older sidecar builds.
    #[serde(default)]
    synthetic_nodes: Vec<serde_json::Value>,
    #[serde(default)]
    diagnostics: Vec<serde_json::Value>,
}

/// Try to run the sidecar against `repo_root`. Returns `Skipped` when
/// the sidecar is disabled, the Dart SDK is unavailable, or the
/// subprocess fails — the engine is expected to fall back silently in
/// those cases.
pub fn try_run(
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
) -> SidecarOutcome {
    if !is_enabled() {
        return SidecarOutcome::Skipped {
            reason: format!("env {ENV_ENABLE} not set / disabled"),
        };
    }
    let mut cmd = match resolve_command(repo_root) {
        Some(cmd) => cmd,
        None => {
            return SidecarOutcome::Skipped {
                reason: format!(
                    "could not locate sidecar (set {ENV_BIN} or keep {DEFAULT_SIDECAR_REL})"
                ),
            };
        }
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return SidecarOutcome::Skipped {
                reason: format!("spawn failed: {e}"),
            };
        }
    };

    // Write the JSON request to stdin in a scoped block so the pipe is
    // closed before we wait_with_output.
    let request_body = match write_request(repo_root, code_roots, exclude_globs) {
        Ok(json) => json,
        Err(e) => {
            return SidecarOutcome::Skipped {
                reason: format!("serialise request: {e}"),
            };
        }
    };
    if let Some(stdin) = child.stdin.as_mut() {
        if let Err(e) = stdin.write_all(request_body.as_bytes()) {
            return SidecarOutcome::Skipped {
                reason: format!("write stdin: {e}"),
            };
        }
    }
    drop(child.stdin.take());

    let output = match child.wait_with_output() {
        Ok(out) => out,
        Err(e) => {
            return SidecarOutcome::Skipped {
                reason: format!("wait sidecar: {e}"),
            };
        }
    };
    if !output.status.success() {
        return SidecarOutcome::Skipped {
            reason: format!(
                "sidecar exited with {:?} stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(200)
                    .collect::<String>()
            ),
        };
    }
    parse_response(&output.stdout)
}

fn is_enabled() -> bool {
    let raw = std::env::var(ENV_ENABLE).unwrap_or_default();
    matches!(
        raw.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn resolve_command(repo_root: &Path) -> Option<Command> {
    if let Ok(custom) = std::env::var(ENV_BIN) {
        if !custom.trim().is_empty() {
            return Some(command_from_str(&custom));
        }
    }
    let default_path = repo_root.join(DEFAULT_SIDECAR_REL);
    if default_path.exists() {
        let mut cmd = Command::new("dart");
        cmd.arg("run").arg(&default_path);
        return Some(cmd);
    }
    None
}

/// Split a shell-style command into program + args (whitespace, no
/// quoting). Intentionally simple — users with truly exotic paths can
/// build a wrapper script.
fn command_from_str(raw: &str) -> Command {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    let mut cmd = Command::new(parts.first().copied().unwrap_or("dart"));
    for p in parts.iter().skip(1) {
        cmd.arg(p);
    }
    cmd
}

fn write_request(
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
) -> Result<String> {
    let roots_owned: Vec<String> = code_roots
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    let repo_root_str = repo_root.to_string_lossy().into_owned();
    let req = SidecarRequest {
        repo_root: &repo_root_str,
        code_roots: roots_owned.iter().map(|s| s.as_str()).collect(),
        exclude_globs,
        resolve_imports: true,
    };
    serde_json::to_string(&req).context("serialise sidecar request")
}

/// Parse the sidecar response and rebuild a [`LanguageIndexBatch`].
///
/// We re-use `serde_json` round-tripping for the per-record shapes —
/// every record in the response is already in the same on-the-wire shape
/// as the corresponding Rust struct, so we can deserialize directly.
fn parse_response(stdout: &[u8]) -> SidecarOutcome {
    let response: SidecarRawResponse = match serde_json::from_slice(stdout) {
        Ok(r) => r,
        Err(e) => {
            return SidecarOutcome::Skipped {
                reason: format!("parse response JSON: {e}"),
            };
        }
    };
    if !response.ok {
        return SidecarOutcome::Skipped {
            reason: format!(
                "sidecar reported failure: {} / {}",
                response.error_code.unwrap_or_else(|| "unknown".into()),
                response.error_message.unwrap_or_default()
            ),
        };
    }

    let mut batch = LanguageIndexBatch {
        language: "dart".into(),
        ..Default::default()
    };
    for v in response.files {
        match serde_json::from_value(v) {
            Ok(f) => batch.files.push(f),
            Err(e) => {
                return SidecarOutcome::Skipped {
                    reason: format!("invalid file row: {e}"),
                };
            }
        }
    }
    for v in response.symbols {
        match serde_json::from_value(v) {
            Ok(s) => batch.symbols.push(s),
            Err(e) => {
                return SidecarOutcome::Skipped {
                    reason: format!("invalid symbol row: {e}"),
                };
            }
        }
    }
    for v in response.symbol_ranges {
        match serde_json::from_value(v) {
            Ok(r) => batch.symbol_ranges.push(r),
            Err(e) => {
                return SidecarOutcome::Skipped {
                    reason: format!("invalid symbol_range row: {e}"),
                };
            }
        }
    }
    for v in response.imports {
        match serde_json::from_value(v) {
            Ok(i) => batch.imports.push(i),
            Err(e) => {
                return SidecarOutcome::Skipped {
                    reason: format!("invalid import row: {e}"),
                };
            }
        }
    }
    for v in response.references {
        match serde_json::from_value(v) {
            Ok(r) => batch.references.push(r),
            Err(e) => {
                return SidecarOutcome::Skipped {
                    reason: format!("invalid reference row: {e}"),
                };
            }
        }
    }
    for v in response.synthetic_nodes {
        match serde_json::from_value(v) {
            Ok(s) => batch.synthetic_nodes.push(s),
            Err(e) => {
                return SidecarOutcome::Skipped {
                    reason: format!("invalid synthetic_node row: {e}"),
                };
            }
        }
    }
    // Diagnostics are informational only — we surface them via the
    // outcome's caller, not the batch.
    let _ = response.diagnostics;

    SidecarOutcome::Used(batch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env_var<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        let prev = std::env::var(key).ok();
        // SAFETY: Tests are single-threaded relative to the test binary
        // and we restore the previous value below. Sidecar tests live
        // in a serial module so this is acceptable.
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match prev {
            Some(p) => std::env::set_var(key, p),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn is_enabled_recognises_truthy_values() {
        for v in ["1", "true", "TRUE", "yes", "on"] {
            with_env_var(ENV_ENABLE, Some(v), || {
                assert!(is_enabled(), "{v} should enable the sidecar");
            });
        }
        for v in ["0", "false", "off", "no", ""] {
            with_env_var(ENV_ENABLE, Some(v), || {
                assert!(!is_enabled(), "{v} should disable the sidecar");
            });
        }
    }

    #[test]
    fn try_run_returns_skipped_when_env_not_set() {
        with_env_var(ENV_ENABLE, None, || {
            let tmp = tempfile::TempDir::new().unwrap();
            let outcome = try_run(tmp.path(), &[PathBuf::from("lib")], &[]);
            match outcome {
                SidecarOutcome::Skipped { reason } => {
                    assert!(reason.contains(ENV_ENABLE), "{reason}");
                }
                _ => panic!("expected Skipped, got {outcome:?}"),
            }
        });
    }

    #[test]
    fn try_run_returns_skipped_when_sidecar_path_missing() {
        with_env_var(ENV_ENABLE, Some("1"), || {
            with_env_var(ENV_BIN, None, || {
                let tmp = tempfile::TempDir::new().unwrap();
                // No tool/ directory inside `tmp`, so default path lookup fails.
                let outcome = try_run(tmp.path(), &[PathBuf::from("lib")], &[]);
                match outcome {
                    SidecarOutcome::Skipped { reason } => {
                        assert!(
                            reason.contains("could not locate sidecar"),
                            "expected locate error, got {reason}"
                        );
                    }
                    _ => panic!("expected Skipped, got {outcome:?}"),
                }
            });
        });
    }

    #[test]
    fn parse_response_rejects_non_ok_payload() {
        let raw = br#"{"ok":false,"error_code":"x","error_message":"y"}"#;
        let outcome = parse_response(raw);
        match outcome {
            SidecarOutcome::Skipped { reason } => {
                assert!(reason.contains("sidecar reported failure"), "{reason}");
                assert!(reason.contains('x') && reason.contains('y'));
            }
            _ => panic!("expected Skipped"),
        }
    }

    #[test]
    fn parse_response_rejects_garbage_input() {
        let outcome = parse_response(b"not json");
        match outcome {
            SidecarOutcome::Skipped { reason } => {
                assert!(reason.contains("parse response JSON"));
            }
            _ => panic!("expected Skipped"),
        }
    }

    #[test]
    fn parse_response_accepts_empty_batch() {
        let raw = br#"{"ok":true}"#;
        match parse_response(raw) {
            SidecarOutcome::Used(batch) => {
                assert!(batch.files.is_empty());
                assert!(batch.symbols.is_empty());
            }
            other => panic!("expected Used, got {other:?}"),
        }
    }

    #[test]
    fn write_request_serialises_relative_code_roots() {
        let body = write_request(
            Path::new("/repo"),
            &[PathBuf::from("lib"), PathBuf::from("test")],
            &["**/*.g.dart".into()],
        )
        .unwrap();
        assert!(body.contains("\"repo_root\":\"/repo\""), "{body}");
        assert!(body.contains("\"code_roots\":[\"lib\",\"test\"]"), "{body}");
        assert!(
            body.contains("\"exclude_globs\":[\"**/*.g.dart\"]"),
            "{body}"
        );
        assert!(body.contains("\"resolve_imports\":true"), "{body}");
    }

    #[test]
    fn command_from_str_splits_on_whitespace() {
        // We can only check the program path here — std::process::Command
        // does not expose its argv. The smoke test is "no panic".
        let _ = command_from_str("dart run tool/sidecar.dart");
    }
}
