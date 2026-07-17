//! Subprocess lifecycle helpers shared by every external tool GroundGraph spawns
//! (LSP servers, SCIP indexers, the Dart sidecar, LSP probes).
//!
//! Two robustness guarantees, both learned the hard way (issues.md #68, #77):
//!
//! 1. **Process groups.** `dart run …`, `sourcekit-lsp`, `gopls` and friends
//!    routinely fork grandchildren (analysis servers, build tools). std's
//!    [`std::process::Child::kill`] signals only the *direct* child, so the
//!    grandchildren are orphaned — they keep burning CPU and holding SDK/index
//!    locks after a Ctrl+C or a timeout, blocking the next `groundgraph index`.
//!    We put each child in its own process group at spawn
//!    ([`detach_process_group`]) and signal the whole group on teardown
//!    ([`kill_tree`]).
//! 2. **Bounded reaping.** A plain `child.wait()` after `kill()` blocks forever
//!    if the kill failed (e.g. a privileged server). [`reap_within`] polls
//!    `try_wait` against a deadline so teardown can never hang the indexer.
//!
//! The workspace forbids `unsafe`, so the group signal is delivered via the
//! POSIX `kill` binary (`kill -KILL -<pgid>`) rather than a raw `libc::kill`
//! FFI call — no new dependency, no `unsafe`, and it degrades gracefully to the
//! std single-PID `kill` when the helper binary is unavailable.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Put the child in its own process group (pgid == child pid on unix) so the
/// whole tree can be signalled later via [`kill_tree`]. No-op on non-unix
/// (Windows job objects are out of scope; we fall back to single-PID kills).
pub(crate) fn detach_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// Terminate `child` **and every process it spawned**. On unix the child is its
/// own group leader (see [`detach_process_group`]), so we `SIGKILL` the whole
/// group via `kill -KILL -<pgid>`; we also issue the std single-PID kill as a
/// belt-and-braces fallback (and the only path on non-unix).
///
/// **Contract:** call this only on a child you have **not yet reaped**. Once a
/// child is reaped its pid (= pgid) may be recycled by the OS, and a later
/// group `SIGKILL` would land on an unrelated group (#253). Every caller honours
/// this: struct-held children are removed with `Option::take` before teardown so
/// a second shutdown/Drop sees `None`, and locally-scoped children are killed
/// exactly once while still running (the timeout / error paths).
pub(crate) fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // `child.id()` is the pgid because we spawned it with
        // `process_group(0)`. `kill -<sig> -<pgid>` targets the whole group.
        let pgid = child.id();
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(format!("-{pgid}"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// Wait up to `budget` for `child` to exit, polling `try_wait` so a failed
/// `kill` can never wedge the caller. Returns `true` iff the child exited (and
/// was reaped — no zombie) within the budget.
pub(crate) fn reap_within(child: &mut Child, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
}

/// Best-effort: [`kill_tree`] then [`reap_within`] a short budget. The default
/// teardown for force-kill / Drop paths.
pub(crate) fn kill_and_reap(child: &mut Child, budget: Duration) -> bool {
    kill_tree(child);
    reap_within(child, budget)
}

// ---- #217: subprocess retry executor ----------------------------------------
//
// Every external tool GroundGraph spawns (SCIP indexers, LSP servers, the
// Dart sidecar) can flake on a *first* run for reasons unrelated to the repo:
// a JVM cold-start OOM, a Node ESM resolution race, a PATH race while
// `rustup` / `dart pub` finishes installing a tool. Previously one such flake
// demoted that language's whole precision layer to Failed for the entire
// index. The helpers below give every spawn site a shared "try, classify,
// back off, retry once" loop, retrying only failures plausibly transient.

/// Knobs for [`retry_transient_subprocess`]: how many total attempts we make
/// (the first try plus any retries) and the base delay grown exponentially
/// between attempts. Read from the environment via
/// [`subprocess_retry_policy`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryPolicy {
    /// Total attempts including the first try (2 ⇒ one retry).
    pub max_attempts: u32,
    pub base_backoff: Duration,
}

/// `GROUNDGRAPH_SUBPROCESS_RETRY_ATTEMPTS` — total attempts (first try +
/// retries). Default 2 (one retry). Must be ≥ 1.
const RETRY_ATTEMPTS_ENV: &str = "GROUNDGRAPH_SUBPROCESS_RETRY_ATTEMPTS";
/// `GROUNDGRAPH_SUBPROCESS_RETRY_BACKOFF_MS` — base backoff in ms, doubled per
/// retry. Default 200.
const RETRY_BACKOFF_ENV: &str = "GROUNDGRAPH_SUBPROCESS_RETRY_BACKOFF_MS";

const DEFAULT_RETRY_ATTEMPTS: u32 = 2;
const DEFAULT_BACKOFF_MS: u64 = 200;
/// Cap a single backoff (and the parsed override) so a misconfigured env knob
/// can never stall `groundgraph index` for minutes between retries. Kept in
/// ms so the parser can clamp without a u128→u64 cast.
const MAX_BACKOFF_MILLIS: u64 = 30_000;
const MAX_BACKOFF: Duration = Duration::from_millis(MAX_BACKOFF_MILLIS);

/// Read the retry policy from the environment, falling back to the defaults
/// (one retry, 200ms base). Parsing is split into the pure helpers below so it
/// can be unit-tested without touching process-global `std::env`.
pub(crate) fn subprocess_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: parse_retry_attempts(std::env::var(RETRY_ATTEMPTS_ENV).ok().as_deref()),
        base_backoff: parse_retry_backoff(std::env::var(RETRY_BACKOFF_ENV).ok().as_deref()),
    }
}

/// Pure policy for [`RetryPolicy::max_attempts`]. A missing, non-numeric, or
/// zero value falls back to the default (at least one attempt is mandatory).
fn parse_retry_attempts(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(DEFAULT_RETRY_ATTEMPTS)
}

/// Pure policy for [`RetryPolicy::base_backoff`], capped at [`MAX_BACKOFF`].
fn parse_retry_backoff(raw: Option<&str>) -> Duration {
    let ms = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&m| m > 0)
        .unwrap_or(DEFAULT_BACKOFF_MS)
        .min(MAX_BACKOFF_MILLIS);
    Duration::from_millis(ms)
}

/// A failed subprocess attempt, in the shape [`is_transient_failure`] can
/// classify without the caller re-deriving the rules.
#[derive(Debug)]
pub(crate) enum SubprocessFailure {
    /// `Command::spawn` (or the wait) returned an io error.
    Spawn(std::io::Error),
    /// The child ran but exited non-zero.
    Exited { code: Option<i32>, stderr: Vec<u8> },
}

/// Whether a failed attempt is worth retrying. Only failures plausibly caused
/// by a *transient* condition are retried:
///
/// - **`Spawn`** — retried for any io error **except** [`NotFound`] (the
///   binary genuinely isn't on disk) and [`TimedOut`] (the indexer already
///   burned its whole budget; retrying just pays it again). fd exhaustion, an
///   `EAGAIN` during fork, or a PATH race while a tool installs ⇒ retry.
/// - **`Exited`** — retried for any non-zero exit **except** the deterministic
///   ones: `2` (CLI usage/arg error), `127` (shell "command not found"), or a
///   stderr naming a missing interpreter/binary. A JVM OOM (`137`), a Node
///   ESM race (`1`), or a signal kill (no code) ⇒ retry.
///
/// [`NotFound`]: std::io::ErrorKind::NotFound
/// [`TimedOut`]: std::io::ErrorKind::TimedOut
pub(crate) fn is_transient_failure(failure: &SubprocessFailure) -> bool {
    match failure {
        SubprocessFailure::Spawn(e) => !matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::TimedOut
        ),
        SubprocessFailure::Exited { code, stderr } => match *code {
            // exit 2 = usage/arg error; 127 = shell "command not found".
            Some(2) | Some(127) => false,
            _ => {
                let lower = String::from_utf8_lossy(stderr).to_ascii_lowercase();
                !(lower.contains("command not found")
                    || lower.contains("no such file or directory"))
            }
        },
    }
}

/// Exponential backoff before retry #`retry_index` (0-indexed: 0 = before the
/// first retry, 1 = before the second, …): `base * 2^retry_index`, capped at
/// [`MAX_BACKOFF`].
pub(crate) fn backoff_for_retry(retry_index: u32, policy: RetryPolicy) -> Duration {
    let shift = retry_index.min(6);
    let scaled = policy.base_backoff.saturating_mul(1u32 << shift);
    scaled.min(MAX_BACKOFF)
}

/// Drive up to `policy.max_attempts` tries of `attempt`, retrying only the
/// transient failures (see [`is_transient_failure`]) after an exponential
/// backoff (see [`backoff_for_retry`]). A non-transient failure — or the final
/// attempt's failure — is returned as-is, so the caller keeps its existing
/// degradation semantics and no new panic path is introduced.
pub(crate) fn retry_transient_subprocess<T, F>(
    policy: RetryPolicy,
    mut attempt: F,
) -> Result<T, SubprocessFailure>
where
    F: FnMut() -> Result<T, SubprocessFailure>,
{
    let mut last = None;
    for attempt_index in 0..policy.max_attempts {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(failure) => {
                let transient = is_transient_failure(&failure);
                last = Some(failure);
                // Stop on a deterministic failure, or once the attempt budget
                // is spent.
                if !transient || attempt_index + 1 >= policy.max_attempts {
                    break;
                }
                std::thread::sleep(backoff_for_retry(attempt_index, policy));
            }
        }
    }
    Err(last.expect("max_attempts >= 1 guarantees at least one attempt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[cfg(unix)]
    fn is_alive(pid: i32) -> bool {
        // `kill -0` probes for the process without signalling it; exit 0 ⇒ alive.
        Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    #[test]
    fn reap_within_returns_true_once_the_child_exits() {
        let mut child = Command::new("sleep").arg("0.1").spawn().unwrap();
        assert!(
            reap_within(&mut child, Duration::from_secs(5)),
            "a fast-exiting child must be reaped within budget"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reap_within_times_out_for_a_live_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let started = Instant::now();
        assert!(
            !reap_within(&mut child, Duration::from_millis(200)),
            "a live child must not be reported as reaped"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(kill_and_reap(&mut child, Duration::from_secs(5)));
    }

    /// #68: killing the parent must take its grandchildren with it. We spawn a
    /// shell (its own group leader) that backgrounds a `sleep 60` grandchild
    /// and prints the grandchild PID; after `kill_tree` the grandchild must be
    /// gone. Without the group kill, the bare `child.kill()` would reap only
    /// the shell and orphan the sleeper.
    #[cfg(unix)]
    #[test]
    fn kill_tree_reaps_an_orphaned_grandchild() {
        use std::io::Read;
        use std::process::Stdio;

        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 60 & echo $!; wait"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        detach_process_group(&mut cmd);
        let mut child = cmd.spawn().unwrap();

        // Read the grandchild PID (first line the shell prints).
        let mut out = child.stdout.take().unwrap();
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while out.read(&mut byte).map(|n| n == 1).unwrap_or(false) {
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        let grandchild: i32 = String::from_utf8_lossy(&buf).trim().parse().unwrap();
        assert!(
            is_alive(grandchild),
            "grandchild should be alive before the kill"
        );

        assert!(kill_and_reap(&mut child, Duration::from_secs(5)));

        // Give the OS a beat to deliver SIGKILL to the whole group.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !is_alive(grandchild),
            "grandchild {grandchild} survived the group kill — it was orphaned"
        );
    }

    // ------------------------------------------------------------------
    // #217: shared subprocess retry executor (spawn + exponential backoff).
    // Used by scip_runner / lsp_indexer / dart_sidecar so a one-off flake
    // (JVM cold-start OOM, a Node ESM resolution race, a PATH race while a
    // tool installs) no longer demotes a whole language's precision layer
    // to Failed on the first try.
    // ------------------------------------------------------------------

    #[test]
    fn retry_policy_defaults_to_one_retry_and_a_small_backoff() {
        let policy = RetryPolicy {
            max_attempts: parse_retry_attempts(None),
            base_backoff: parse_retry_backoff(None),
        };
        assert_eq!(policy.max_attempts, 2, "default = first try + one retry");
        assert_eq!(policy.base_backoff, Duration::from_millis(200));
    }

    #[test]
    fn parse_retry_attempts_honours_overrides_and_rejects_garbage() {
        assert_eq!(parse_retry_attempts(None), 2);
        assert_eq!(parse_retry_attempts(Some("")), 2);
        assert_eq!(parse_retry_attempts(Some("nope")), 2);
        assert_eq!(
            parse_retry_attempts(Some("0")),
            2,
            "zero attempts is nonsensical"
        );
        assert_eq!(parse_retry_attempts(Some("3")), 3);
        assert_eq!(parse_retry_attempts(Some("  5 ")), 5);
    }

    #[test]
    fn parse_retry_backoff_honours_overrides_and_rejects_garbage() {
        assert_eq!(parse_retry_backoff(None), Duration::from_millis(200));
        assert_eq!(parse_retry_backoff(Some("0")), Duration::from_millis(200));
        assert_eq!(parse_retry_backoff(Some("abc")), Duration::from_millis(200));
        assert_eq!(parse_retry_backoff(Some("750")), Duration::from_millis(750));
    }

    #[test]
    fn parse_retry_backoff_caps_a_runaway_override() {
        // A misconfigured 10-minute backoff must never stall `groundgraph index`.
        let parsed = parse_retry_backoff(Some("600000"));
        assert!(
            parsed <= Duration::from_secs(60),
            "backoff is capped: got {parsed:?}"
        );
    }

    #[test]
    fn transient_spawn_io_errors_retry_but_missing_binary_and_timeout_do_not() {
        // NotFound: the binary genuinely isn't on disk — retrying can't help.
        assert!(!is_transient_failure(&SubprocessFailure::Spawn(
            std::io::Error::from(std::io::ErrorKind::NotFound,)
        )));
        // TimedOut: the indexer already burned its whole budget — retrying
        // just pays it again.
        assert!(!is_transient_failure(&SubprocessFailure::Spawn(
            std::io::Error::from(std::io::ErrorKind::TimedOut,)
        )));
        // Resource exhaustion / an EAGAIN during fork / a PATH race: transient.
        assert!(is_transient_failure(&SubprocessFailure::Spawn(
            std::io::Error::from(std::io::ErrorKind::ResourceBusy,)
        )));
    }

    #[test]
    fn transient_nonzero_exits_retry_but_arg_and_notfound_exits_do_not() {
        // exit 2 = CLI usage/arg error; 127 = shell "command not found".
        assert!(!is_transient_failure(&SubprocessFailure::Exited {
            code: Some(2),
            stderr: vec![]
        }));
        assert!(!is_transient_failure(&SubprocessFailure::Exited {
            code: Some(127),
            stderr: vec![]
        }));
        // A stderr that names a missing interpreter is deterministic too.
        assert!(!is_transient_failure(&SubprocessFailure::Exited {
            code: Some(127),
            stderr: b"bash: rust-analyzer: command not found\n".to_vec(),
        }));
        // JVM OOM (137), a Node ESM race (1): transient.
        assert!(is_transient_failure(&SubprocessFailure::Exited {
            code: Some(1),
            stderr: vec![]
        }));
        assert!(is_transient_failure(&SubprocessFailure::Exited {
            code: Some(137),
            stderr: vec![]
        }));
        // Killed by a signal (no exit code) — could be a one-off.
        assert!(is_transient_failure(&SubprocessFailure::Exited {
            code: None,
            stderr: vec![]
        }));
    }

    #[test]
    fn backoff_grows_exponentially_and_is_capped() {
        let policy = RetryPolicy {
            max_attempts: 4,
            base_backoff: Duration::from_millis(100),
        };
        assert_eq!(backoff_for_retry(0, policy), Duration::from_millis(100));
        assert_eq!(backoff_for_retry(1, policy), Duration::from_millis(200));
        assert_eq!(backoff_for_retry(2, policy), Duration::from_millis(400));
        let huge = backoff_for_retry(40, policy);
        assert!(
            huge <= Duration::from_secs(30),
            "backoff is capped: got {huge:?}"
        );
    }

    #[test]
    fn retry_driver_succeeds_on_first_try_without_retrying() {
        let calls = AtomicU32::new(0);
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff: Duration::ZERO,
        };
        let outcome: Result<u32, SubprocessFailure> = retry_transient_subprocess(policy, || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            Ok(n)
        });
        assert_eq!(outcome.unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retry_driver_retries_a_transient_failure_then_succeeds() {
        let calls = AtomicU32::new(0);
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff: Duration::ZERO,
        };
        let outcome: Result<&str, SubprocessFailure> = retry_transient_subprocess(policy, || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(SubprocessFailure::Exited {
                    code: Some(1),
                    stderr: vec![],
                })
            } else {
                Ok("ok")
            }
        });
        assert_eq!(outcome.unwrap(), "ok");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "exactly one retry after a transient failure"
        );
    }

    #[test]
    fn retry_driver_does_not_retry_a_deterministic_failure() {
        let calls = AtomicU32::new(0);
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff: Duration::ZERO,
        };
        let outcome: Result<(), SubprocessFailure> = retry_transient_subprocess(policy, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(SubprocessFailure::Exited {
                code: Some(2),
                stderr: vec![],
            })
        });
        assert!(matches!(
            outcome,
            Err(SubprocessFailure::Exited { code: Some(2), .. })
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "deterministic failures are not retried"
        );
    }

    #[test]
    fn retry_driver_exhausts_attempts_and_returns_the_last_failure() {
        let calls = AtomicU32::new(0);
        let policy = RetryPolicy {
            max_attempts: 2,
            base_backoff: Duration::ZERO,
        };
        let outcome: Result<(), SubprocessFailure> = retry_transient_subprocess(policy, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(SubprocessFailure::Spawn(std::io::Error::from(
                std::io::ErrorKind::ResourceBusy,
            )))
        });
        assert!(matches!(outcome, Err(SubprocessFailure::Spawn(_))));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "must try exactly max_attempts times"
        );
    }

    /// First try flakes (exit 1), second succeeds — counted via a sidecar
    /// file. The driver must ride the first failure to the second attempt.
    #[cfg(unix)]
    #[test]
    fn retry_driver_retries_a_real_transient_subprocess_until_it_succeeds() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("calls.txt");
        std::fs::write(&counter, "0").unwrap();
        let stub = dir.path().join("flake.sh");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             d=$(dirname \"$0\")\n\
             n=$(cat \"$d/calls.txt\")\n\
             n=$((n+1))\n\
             echo \"$n\" > \"$d/calls.txt\"\n\
             [ \"$n\" = \"1\" ] && exit 1\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff: Duration::from_millis(1),
        };
        let mut attempts = 0u32;
        let outcome: Result<(std::process::ExitStatus, Vec<u8>), SubprocessFailure> =
            retry_transient_subprocess(policy, || {
                attempts += 1;
                let out = std::process::Command::new(&stub)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .output();
                match out {
                    Ok(o) if o.status.success() => Ok((o.status, o.stderr)),
                    Ok(o) => Err(SubprocessFailure::Exited {
                        code: o.status.code(),
                        stderr: o.stderr,
                    }),
                    Err(e) => Err(SubprocessFailure::Spawn(e)),
                }
            });
        assert!(
            outcome.is_ok(),
            "retry must ride the first flake to success: {outcome:?}"
        );
        assert_eq!(attempts, 2, "exactly one retry");
        assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "2");
    }

    /// exit 2 (arg error) is deterministic — the driver must NOT retry.
    #[cfg(unix)]
    #[test]
    fn retry_driver_does_not_retry_a_real_arg_error_subprocess() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("calls.txt");
        std::fs::write(&counter, "0").unwrap();
        let stub = dir.path().join("argerr.sh");
        std::fs::write(
            &stub,
            "#!/bin/sh\nd=$(dirname \"$0\")\nn=$(cat \"$d/calls.txt\")\nn=$((n+1))\n\
             echo \"$n\" > \"$d/calls.txt\"\nexit 2\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff: Duration::from_millis(1),
        };
        let outcome: Result<(), SubprocessFailure> = retry_transient_subprocess(policy, || {
            let out = std::process::Command::new(&stub)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output();
            match out {
                Ok(o) if o.status.success() => Ok(()),
                Ok(o) => Err(SubprocessFailure::Exited {
                    code: o.status.code(),
                    stderr: o.stderr,
                }),
                Err(e) => Err(SubprocessFailure::Spawn(e)),
            }
        });
        assert!(matches!(
            outcome,
            Err(SubprocessFailure::Exited { code: Some(2), .. })
        ));
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().trim(),
            "1",
            "arg error must not retry"
        );
    }

    /// A missing binary (NotFound) must not be retried.
    #[test]
    fn retry_driver_does_not_retry_a_missing_binary() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff: Duration::from_millis(1),
        };
        let calls = AtomicU32::new(0);
        let outcome: Result<(), SubprocessFailure> = retry_transient_subprocess(policy, || {
            calls.fetch_add(1, Ordering::SeqCst);
            let out = std::process::Command::new("groundgraph-no-such-binary-9z")
                .stdin(std::process::Stdio::null())
                .output();
            match out {
                Ok(o) if o.status.success() => Ok(()),
                Ok(o) => Err(SubprocessFailure::Exited {
                    code: o.status.code(),
                    stderr: o.stderr,
                }),
                Err(e) => Err(SubprocessFailure::Spawn(e)),
            }
        });
        assert!(matches!(outcome, Err(SubprocessFailure::Spawn(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "NotFound must not retry");
    }
}
