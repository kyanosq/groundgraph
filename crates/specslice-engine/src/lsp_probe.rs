//! Shared LSP "smoke launch" probe.
//!
//! ## Why this module exists
//!
//! Every language adapter (Python, TypeScript, Java, Swift, Go) needs
//! to answer the same question before driving an LSP session: *is this
//! binary actually runnable on this host?* Historically each adapter
//! answered differently:
//!
//! - Python (after P20 fixes) did a real smoke launch with timeout +
//!   stderr inspection, catching broken shebangs and missing
//!   interpreters.
//! - Swift / Go / TypeScript / Java only checked that the binary
//!   existed on PATH.
//!
//! That asymmetry produced a real failure mode on the reviewer's box
//! during the v0.2.0 close-out: `sourcekit-lsp` was on PATH but
//! crashed on init with `SOURCEKITD FATAL ERROR: Service is invalid`.
//! Swift's shallow probe said "available", the adapter started, the
//! session collapsed, and the opt-in LSP smoke went red instead of
//! soft-skipping.
//!
//! This module collapses every adapter into a single probe with the
//! same semantics so the same kind of operator-side breakage gets the
//! same kind of structured "not usable, here's why" answer regardless
//! of language.
//!
//! ## Semantics
//!
//! `probe_lsp_command(command, args, timeout)` spawns the binary,
//! drains stderr (bounded), enforces the timeout, and returns
//! [`ProbeReport`]:
//!
//! - `Runnable` — process exited 0 within the timeout and stderr does
//!   not contain any of the canonical "broken stub" markers.
//! - `Unrunnable { reason }` — process failed to spawn, timed out,
//!   exited non-zero, or stderr matched a broken-stub marker
//!   (broken shebang, missing interpreter, indexstoredb crash, etc).
//!
//! Non-zero exit is treated as *unrunnable*. This is deliberate: a
//! healthy LSP recognises `--help` (or whichever smoke args are
//! passed) and exits 0; a non-zero exit usually means we're pointed at
//! the wrong binary, or the right binary that no longer works on this
//! machine. We'd rather soft-skip than start a stdio session that
//! never finishes initialise.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default smoke arguments for an LSP binary. Almost every LSP we
/// integrate (`pyright-langserver`, `basedpyright-langserver`,
/// `pylsp`, `typescript-language-server`, `jdtls`, `sourcekit-lsp`,
/// `gopls`) recognises `--help` and exits 0 within a few hundred ms.
pub const DEFAULT_SMOKE_ARGS: &[&str] = &["--help"];

/// Default timeout. 1500 ms covers cold JVM (`jdtls`) and Node
/// (`typescript-language-server`) startups on slow disks while still
/// catching genuinely hung binaries.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Structured probe outcome. Callers turn this into either a `bool`
/// (for backwards-compat `<lang>_lsp_available`) or a human-readable
/// `sidecar_skip_reason`.
#[derive(Debug, Clone)]
pub enum ProbeReport {
    /// Process exited 0 within the timeout and stderr was clean.
    Runnable,
    /// Process did not produce evidence of a healthy LSP. `reason` is
    /// the operator-facing string we surface in
    /// `result.sidecar_skip_reason` and CLI output.
    Unrunnable { reason: String },
}

impl ProbeReport {
    pub fn is_runnable(&self) -> bool {
        matches!(self, ProbeReport::Runnable)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            ProbeReport::Runnable => None,
            ProbeReport::Unrunnable { reason } => Some(reason.as_str()),
        }
    }
}

/// Spawn `command args`, give it `timeout` to exit cleanly, and
/// classify the result.
///
/// The function never panics: spawn failures, timeouts, and broken
/// stubs all turn into `Unrunnable` with a reason that points the
/// operator at the actual diagnostic.
pub fn probe_lsp_command(command: &str, args: &[&str], timeout: Duration) -> ProbeReport {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // #68: own process group so a hung probe's grandchildren are reaped too.
    crate::proc::detach_process_group(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ProbeReport::Unrunnable {
                reason: format!("`{command}` 启动失败：{e}"),
            };
        }
    };

    let start = Instant::now();
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    // #68/#77: group kill + bounded reap.
                    crate::proc::kill_and_reap(&mut child, Duration::from_secs(2));
                    return ProbeReport::Unrunnable {
                        reason: format!("`{command} --help` 在 {timeout:?} 内未退出，疑似挂起"),
                    };
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                return ProbeReport::Unrunnable {
                    reason: format!("`{command}` 等待退出时出错：{e}"),
                };
            }
        }
    };

    let mut stderr_buf = String::new();
    if let Some(s) = child.stderr.take() {
        let _ = s.take(4096).read_to_string(&mut stderr_buf);
    }

    if let Some(marker) = broken_stub_marker(&stderr_buf) {
        return ProbeReport::Unrunnable {
            reason: format!("`{command}` smoke launch 检测到 `{marker}`，疑似 binary 不可用"),
        };
    }

    match exit {
        Some(status) if status.success() => ProbeReport::Runnable,
        Some(status) => ProbeReport::Unrunnable {
            reason: format!("`{command} --help` 退出码 {status} 非 0，疑似 binary 不可用"),
        },
        None => ProbeReport::Unrunnable {
            reason: format!("`{command}` 未能采集退出状态"),
        },
    }
}

/// Substring markers that indicate the binary started but was clearly
/// broken (wrapper script with a stale interpreter, missing native
/// dylib, crashed indexstoredb, etc). Match is case-insensitive.
fn broken_stub_marker(stderr: &str) -> Option<&'static str> {
    let lower = stderr.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "bad interpreter",
        "no such file or directory",
        "no module named",
        "cannot execute",
        "command not found",
        // sourcekit-lsp: indexstoredb / sourcekitd crashes on init.
        "sourcekitd fatal error",
        "could not load",
        // JVM bootstrap failure — `jdtls` wraps `java`.
        "no java runtime",
        "java_home is not set",
        // Node bootstrap failure — `typescript-language-server` is a
        // node script with a `#!/usr/bin/env node` shebang.
        "node: command not found",
    ];
    MARKERS.iter().copied().find(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn runnable_for_well_behaved_command() {
        // `true` exits 0 immediately on every POSIX box we ship to.
        let report = probe_lsp_command("true", &[], DEFAULT_TIMEOUT);
        assert!(report.is_runnable(), "got {report:?}");
        assert!(report.reason().is_none());
    }

    #[test]
    fn unrunnable_when_binary_missing() {
        let report = probe_lsp_command(
            "specslice-this-binary-should-not-exist-9z",
            DEFAULT_SMOKE_ARGS,
            DEFAULT_TIMEOUT,
        );
        assert!(!report.is_runnable());
        let reason = report.reason().expect("expected reason");
        assert!(
            reason.contains("启动失败"),
            "expected spawn-failure reason, got `{reason}`"
        );
    }

    #[test]
    fn unrunnable_when_shebang_points_at_missing_interpreter() {
        // Build the exact failure mode the reviewer hit with `pylsp`
        // on their box: a shell wrapper whose shebang resolves to a
        // deleted interpreter. The kernel reports `no such file or
        // directory` on stderr; our marker matcher must catch it.
        let dir = tempdir().unwrap();
        let stub = dir.path().join("broken-stub-langserver");
        std::fs::write(
            &stub,
            "#!/path/that/does/not/exist/python\nprint('unreachable')\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let report = probe_lsp_command(stub.to_str().unwrap(), DEFAULT_SMOKE_ARGS, DEFAULT_TIMEOUT);
        assert!(!report.is_runnable(), "expected unrunnable, got {report:?}");
        let reason = report.reason().expect("expected reason");
        // macOS converts the broken-shebang to ENOENT at spawn time;
        // Linux runs the child and prints "bad interpreter" to
        // stderr. Either branch must produce an operator-readable
        // reason that mentions the missing or unusable interpreter.
        assert!(
            reason.contains("启动失败") || reason.contains("不可用") || reason.contains("退出码"),
            "expected diagnosable reason, got `{reason}`"
        );
    }

    #[test]
    fn unrunnable_when_command_times_out() {
        // Use `sleep` (POSIX) as a stand-in for a hung LSP that
        // accepts launch but never exits.
        let report = probe_lsp_command("sleep", &["10"], Duration::from_millis(200));
        assert!(!report.is_runnable(), "got {report:?}");
        assert!(report.reason().unwrap().contains("未退出，疑似挂起"));
    }

    #[test]
    fn unrunnable_when_command_exits_nonzero() {
        // `false` exits 1 with no stderr; we still treat that as
        // unrunnable so that wrong-binary slips can't masquerade as
        // a real LSP.
        let report = probe_lsp_command("false", &[], DEFAULT_TIMEOUT);
        assert!(!report.is_runnable());
        assert!(report.reason().unwrap().contains("退出码"));
    }

    #[test]
    fn broken_stub_marker_recognises_sourcekit_fatal() {
        assert_eq!(
            broken_stub_marker("error: SOURCEKITD FATAL ERROR: Service is invalid"),
            Some("sourcekitd fatal error")
        );
    }

    #[test]
    fn broken_stub_marker_recognises_missing_jvm() {
        assert_eq!(
            broken_stub_marker("No Java runtime present, requesting install."),
            Some("no java runtime")
        );
    }

    #[test]
    fn broken_stub_marker_ignores_clean_help_output() {
        assert!(broken_stub_marker("usage: pyright-langserver [options]\n").is_none());
    }
}
