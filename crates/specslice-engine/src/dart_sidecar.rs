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

/// Override the sidecar wall-clock budget (seconds, default 600). A hung
/// analyzer otherwise blocked `specslice index` forever (issues2.md #48).
pub const ENV_TIMEOUT_SECS: &str = "SPECSLICE_DART_ANALYZER_TIMEOUT_SECS";

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
    tests: Vec<serde_json::Value>,
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
    let probes = probe_locations(repo_root);
    let mut cmd = match resolve_command_with(repo_root, &probes) {
        Some(cmd) => cmd,
        None => {
            let tried = probes
                .iter()
                .map(|p| format!("    - {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            return SidecarOutcome::Skipped {
                reason: format!(
                    "could not locate sidecar — high-precision Dart analyzer is OFF. \
                     Set {ENV_BIN}=/path/to/specslice_dart_analyzer.dart, or place the \
                     sidecar source at one of:\n{tried}"
                ),
            };
        }
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // #68: own process group so a timeout kill also reaps the analyzer
    // subprocesses `dart run` forks (analysis_server, build tools).
    crate::proc::detach_process_group(&mut cmd);

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

    // Reader threads keep both pipes drained (a full pipe would deadlock
    // the child) while the main thread enforces a wall-clock budget — a
    // hung analyzer must not hang `specslice index` forever
    // (issues2.md #48).
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout_pipe {
            use std::io::Read;
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr_pipe {
            use std::io::Read;
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });

    let budget = sidecar_timeout();
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > budget {
                    // #68/#77: take the whole group down (reaping forked
                    // analyzer subprocesses) and bound the reap.
                    crate::proc::kill_and_reap(&mut child, std::time::Duration::from_secs(2));
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return SidecarOutcome::Skipped {
                        reason: format!(
                            "sidecar exceeded the {}s budget and was killed \
                             (override with {ENV_TIMEOUT_SECS})",
                            budget.as_secs()
                        ),
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return SidecarOutcome::Skipped {
                    reason: format!("wait sidecar: {e}"),
                };
            }
        }
    };
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    if !status.success() {
        return SidecarOutcome::Skipped {
            reason: format!(
                "sidecar exited with {:?} stderr={}",
                status.code(),
                String::from_utf8_lossy(&stderr)
                    .chars()
                    .take(200)
                    .collect::<String>()
            ),
        };
    }
    parse_response(&stdout)
}

/// Wall-clock budget for one sidecar run. Large Flutter repos legitimately
/// take minutes (cold analyzer); the default only has to stop *hangs*.
fn sidecar_timeout() -> std::time::Duration {
    let secs = std::env::var(ENV_TIMEOUT_SECS)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(600);
    std::time::Duration::from_secs(secs)
}

fn is_enabled() -> bool {
    // P2 — sidecar is now the default high-precision path. The opt-out
    // ladder is:
    // - `SPECSLICE_DART_ANALYZER=0` / `false` / `off` / `no` -> disabled.
    // - `SPECSLICE_DART_ANALYZER=1` / `true` / `yes` / `on`  -> enabled.
    // - unset                                                -> enabled
    //   (callers without a Dart SDK still get a silent fallback because
    //    [`resolve_command`] returns `None` and [`try_run`] reports a
    //    skip reason instead of crashing).
    match std::env::var(ENV_ENABLE) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "off" | "no" => false,
            "" => true,
            _ => true,
        },
        Err(_) => true,
    }
}

/// All locations we try in order to find the sidecar, from highest to
/// lowest priority. Exposed for diagnostics so the skip reason can
/// quote every path we already tried.
fn probe_locations(repo_root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    out.push(repo_root.join(DEFAULT_SIDECAR_REL));
    // Co-located alongside the `specslice` binary — works for a
    // pre-packaged install where the sidecar source is shipped next to
    // the executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            out.push(parent.join(DEFAULT_SIDECAR_REL));
            out.push(parent.join("specslice_dart_analyzer.dart"));
            // …/target/<profile>/specslice → walk back to the workspace
            // root so a developer running `cargo run -- index` from any
            // subdirectory still finds the source tree.
            if let Some(grandparent) = parent.parent() {
                out.push(grandparent.join(DEFAULT_SIDECAR_REL));
                if let Some(great) = grandparent.parent() {
                    out.push(great.join(DEFAULT_SIDECAR_REL));
                }
            }
        }
    }
    // User-scoped install location for ad-hoc setups.
    if let Some(home) = home_dir() {
        out.push(
            home.join(".specslice")
                .join("dart_analyzer")
                .join("bin")
                .join("specslice_dart_analyzer.dart"),
        );
    }
    out
}

fn resolve_command_with(_repo_root: &Path, locations: &[PathBuf]) -> Option<Command> {
    // 1. Explicit override always wins.
    if let Ok(custom) = std::env::var(ENV_BIN) {
        if !custom.trim().is_empty() {
            return Some(command_from_str(&custom));
        }
    }
    // 2. Probe known locations and use the first hit.
    for candidate in locations {
        if candidate.exists() {
            let mut cmd = Command::new("dart");
            cmd.arg("run").arg(candidate);
            return Some(cmd);
        }
    }
    None
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Split a shell-style command into program + args, honouring quotes and
/// backslash escapes so a binary path containing spaces
/// (`'/opt/my dart/dart' run ...`) is parsed as one argument. Falls back to
/// whitespace splitting if the string has unbalanced quotes (never panics).
fn split_command(raw: &str) -> Vec<String> {
    match shlex::split(raw) {
        Some(parts) if !parts.is_empty() => parts,
        // `shlex` returns `None` on unbalanced quotes and `Some([])` when it
        // treats the whole value as a comment (a leading `#`, e.g. a
        // `#!`-shebang path). For a non-blank override, falling through to bare
        // `dart` would silently mask the misconfiguration — so split on
        // whitespace instead, letting the user's value reach `Command` (and
        // fail loudly if it cannot exec). (#258)
        _ if !raw.trim().is_empty() => raw.split_whitespace().map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

fn command_from_str(raw: &str) -> Command {
    let parts = split_command(raw);
    let mut cmd = Command::new(parts.first().map(String::as_str).unwrap_or("dart"));
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
    // Partial recovery (issues2.md #48): one malformed record (sidecar /
    // engine version skew) must drop *that record*, not the whole batch —
    // "9 999 of 10 000 symbols" beats "Dart index missing entirely".
    // Dropped rows surface as an [`AdapterDiagnostic`] per record family.
    let mut dropped: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    fn collect<T: serde::de::DeserializeOwned>(
        rows: Vec<serde_json::Value>,
        family: &'static str,
        out: &mut Vec<T>,
        dropped: &mut std::collections::BTreeMap<&'static str, usize>,
    ) {
        for v in rows {
            match serde_json::from_value(v) {
                Ok(parsed) => out.push(parsed),
                Err(_) => *dropped.entry(family).or_insert(0) += 1,
            }
        }
    }
    collect(response.files, "file", &mut batch.files, &mut dropped);
    collect(response.symbols, "symbol", &mut batch.symbols, &mut dropped);
    collect(response.tests, "test", &mut batch.tests, &mut dropped);
    collect(
        response.symbol_ranges,
        "symbol_range",
        &mut batch.symbol_ranges,
        &mut dropped,
    );
    collect(response.imports, "import", &mut batch.imports, &mut dropped);
    collect(
        response.references,
        "reference",
        &mut batch.references,
        &mut dropped,
    );
    collect(
        response.synthetic_nodes,
        "synthetic_node",
        &mut batch.synthetic_nodes,
        &mut dropped,
    );
    for (family, count) in dropped {
        batch
            .diagnostics
            .push(specslice_core::language_batch::AdapterDiagnostic {
                path: String::new(),
                message: format!("sidecar response: dropped {count} invalid {family} row(s)"),
            });
    }
    // Sidecar-side diagnostics are informational only — we surface them via
    // the outcome's caller, not the batch.
    let _ = response.diagnostics;

    SidecarOutcome::Used(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env_lock<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().expect("env test mutex poisoned");
        f();
    }

    #[test]
    fn split_command_honours_quotes_and_falls_back_safely() {
        // A quoted binary path with spaces stays a single argv[0].
        assert_eq!(
            split_command("'/opt/my dart/dart' run tool/x.dart"),
            vec!["/opt/my dart/dart", "run", "tool/x.dart"]
        );
        // Plain whitespace commands are unchanged.
        assert_eq!(
            split_command("dart run x.dart"),
            vec!["dart", "run", "x.dart"]
        );
        // Unbalanced quotes must not panic — fall back to whitespace split.
        assert_eq!(split_command("dart 'unclosed"), vec!["dart", "'unclosed"]);
    }

    #[test]
    fn split_command_does_not_swallow_hash_led_override() {
        // `shlex` treats `#` as a comment start and returns an *empty* vec, so
        // a `#!`-shebang-like override silently collapsed to bare `dart`. A
        // non-blank override must still reach argv. (#258)
        assert_eq!(
            split_command("#!/opt/dart"),
            vec!["#!/opt/dart".to_string()]
        );
        assert_eq!(
            split_command("/opt/dart # note"),
            vec!["/opt/dart".to_string()],
            "a trailing comment is fine as long as the program survives"
        );
        // A truly blank value still yields no tokens (caller falls back to the
        // default `dart` only when the override is unset/blank).
        assert!(split_command("   ").is_empty());
    }

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
    fn is_enabled_recognises_truthy_values_and_defaults_to_on() {
        with_env_lock(|| {
            for v in ["1", "true", "TRUE", "yes", "on"] {
                with_env_var(ENV_ENABLE, Some(v), || {
                    assert!(is_enabled(), "{v} should enable the sidecar");
                });
            }
            // P2 — empty / unset values now mean "use the sidecar".
            for v in ["", " "] {
                with_env_var(ENV_ENABLE, Some(v), || {
                    assert!(
                        is_enabled(),
                        "{v:?} should default to the sidecar (P2 default-on)",
                    );
                });
            }
            with_env_var(ENV_ENABLE, None, || {
                assert!(
                    is_enabled(),
                    "unset env var should default to the sidecar (P2 default-on)",
                );
            });
            // Opt-outs still work — power users can disable explicitly.
            for v in ["0", "false", "off", "no"] {
                with_env_var(ENV_ENABLE, Some(v), || {
                    assert!(!is_enabled(), "{v} should disable the sidecar");
                });
            }
        });
    }

    #[test]
    fn try_run_returns_skipped_when_env_explicitly_disabled() {
        with_env_lock(|| {
            with_env_var(ENV_ENABLE, Some("0"), || {
                let tmp = tempfile::TempDir::new().unwrap();
                let outcome = try_run(tmp.path(), &[PathBuf::from("lib")], &[]);
                match outcome {
                    SidecarOutcome::Skipped { reason } => {
                        assert!(reason.contains(ENV_ENABLE), "{reason}");
                    }
                    _ => panic!("expected Skipped, got {outcome:?}"),
                }
            });
        });
    }

    #[test]
    fn try_run_returns_skipped_when_sidecar_path_missing() {
        with_env_lock(|| {
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
        });
    }

    #[test]
    fn skip_reason_lists_every_probed_path() {
        // Reviewer asked: real target repos rarely have
        // `tool/specslice_dart_analyzer/...` at the repo root, so the
        // sidecar silently skips. Skip reason must now name every
        // location we tried, so the operator knows where to drop the
        // sidecar source (or which env var to set).
        with_env_lock(|| {
            with_env_var(ENV_ENABLE, Some("1"), || {
                with_env_var(ENV_BIN, None, || {
                    let tmp = tempfile::TempDir::new().unwrap();
                    let outcome = try_run(tmp.path(), &[PathBuf::from("lib")], &[]);
                    match outcome {
                        SidecarOutcome::Skipped { reason } => {
                            assert!(reason.contains(ENV_BIN), "{reason}");
                            assert!(
                                reason.contains(&format!(
                                    "{}",
                                    tmp.path().join(DEFAULT_SIDECAR_REL).display()
                                )),
                                "skip reason should quote repo-root probe path:\n{reason}"
                            );
                            assert!(
                                reason.contains("high-precision Dart analyzer is OFF"),
                                "operator-facing message expected:\n{reason}"
                            );
                        }
                        _ => panic!("expected Skipped, got {outcome:?}"),
                    }
                });
            });
        });
    }

    #[test]
    fn probe_locations_includes_repo_root_and_binary_neighbour_and_home() {
        // Defence-in-depth: even when the repo doesn't ship the
        // sidecar source, an operator who dropped it next to the
        // `specslice` binary OR under `~/.specslice/dart_analyzer/...`
        // should hit one of these probes without setting any env var.
        let tmp = tempfile::TempDir::new().unwrap();
        let probes = probe_locations(tmp.path());
        let as_str: Vec<String> = probes.iter().map(|p| p.display().to_string()).collect();
        let joined = as_str.join("\n");
        assert!(
            joined.contains(&format!(
                "{}",
                tmp.path().join(DEFAULT_SIDECAR_REL).display()
            )),
            "must probe repo root:\n{joined}"
        );
        assert!(
            joined.contains(".specslice/dart_analyzer"),
            "must probe ~/.specslice/dart_analyzer:\n{joined}"
        );
        // current_exe() is platform-dependent in tests; instead of asserting
        // its specific value we just require that at least one probe is rooted
        // outside the repo path (i.e. came from current_exe / HOME chain).
        assert!(
            probes.iter().any(|p| !p.starts_with(tmp.path())),
            "must probe at least one path outside repo root:\n{joined}"
        );
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
    fn parse_response_accepts_test_rows() {
        let raw = br#"{
          "ok": true,
          "tests": [
            {
              "id": "dart_test::test/iap/iap_constants_test.dart#exposes-monthly-yearly-lifetime-ids",
              "kind": "test_case",
              "path": "test/iap/iap_constants_test.dart",
              "name": "exposes monthly/yearly/lifetime ids",
              "start_line": 2,
              "end_line": 2,
              "parent_symbol_id": null
            }
          ]
        }"#;
        match parse_response(raw) {
            SidecarOutcome::Used(batch) => {
                assert_eq!(batch.tests.len(), 1);
                assert_eq!(batch.tests[0].name, "exposes monthly/yearly/lifetime ids");
            }
            other => panic!("expected Used, got {other:?}"),
        }
    }

    /// issues2.md #48: one malformed record must not throw away the whole
    /// batch. 9 999 good symbols + 1 incompatible row = a 9 999-symbol
    /// batch with a diagnostic, not a sidecar skip.
    #[test]
    fn parse_response_recovers_partially_from_invalid_rows() {
        let raw = br#"{
          "ok": true,
          "symbols": [
            {
              "id": "dart_class::lib/a.dart#Good",
              "kind": "dart_class",
              "path": "lib/a.dart",
              "name": "Good",
              "qualified_name": "Good",
              "start_line": 1,
              "end_line": 3,
              "parent_symbol_id": null,
              "metadata_json": null
            },
            { "id": "dart_class::lib/b.dart#Bad", "kind": 42 }
          ]
        }"#;
        match parse_response(raw) {
            SidecarOutcome::Used(batch) => {
                assert_eq!(batch.symbols.len(), 1, "good row survives");
                assert_eq!(batch.symbols[0].name, "Good");
                assert!(
                    batch
                        .diagnostics
                        .iter()
                        .any(|d| d.message.contains("invalid") || d.message.contains("symbol")),
                    "dropped rows must surface as a diagnostic: {:?}",
                    batch.diagnostics
                );
            }
            other => panic!("expected partial Used, got {other:?}"),
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
