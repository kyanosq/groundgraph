//! Subprocess lifecycle helpers shared by every external tool SpecSlice spawns
//! (LSP servers, SCIP indexers, the Dart sidecar, LSP probes).
//!
//! Two robustness guarantees, both learned the hard way (issues.md #68, #77):
//!
//! 1. **Process groups.** `dart run …`, `sourcekit-lsp`, `gopls` and friends
//!    routinely fork grandchildren (analysis servers, build tools). std's
//!    [`std::process::Child::kill`] signals only the *direct* child, so the
//!    grandchildren are orphaned — they keep burning CPU and holding SDK/index
//!    locks after a Ctrl+C or a timeout, blocking the next `specslice index`.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
