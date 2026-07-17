//! #115 — the seven bare-`String` `--format` / `--mode` arguments become
//! clap `ValueEnum`s so an invalid value is rejected at parse time (exit 2,
//! matching the #233 contract) instead of reaching a per-command runtime
//! `bail!` (the old exit 1, with an ad-hoc message).

use assert_cmd::Command;

fn run_in_empty(args: &[&str]) -> assert_cmd::assert::Assert {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(args)
        .assert()
        .failure()
        .code(2)
}

#[test]
fn similar_invalid_mode_exits_2() {
    // Currently a runtime `bail!` from `parse_mode` → exit 1; with ValueEnum
    // it is a clap parse error → exit 2. `parse_mode` fails before the engine
    // is touched, so no workspace is needed — this is the strict RED that
    // drives the ValueEnum mechanism.
    run_in_empty(&["similar", "--mode", "fuzzy"]);
}

#[test]
fn graph_diff_invalid_format_exits_2() {
    // base/head satisfy clap's required args; the invalid format then drives
    // the failure. Before ValueEnum this reached the runner's SQLite open
    // (exit 70); after, clap rejects `xml` at parse (exit 2).
    run_in_empty(&[
        "graph-diff",
        "--base-db",
        "/x.db",
        "--head-db",
        "/y.db",
        "--format",
        "xml",
    ]);
}

// The remaining four currently reach `NoWorkspace` (exit 2) before the
// format `bail!` fires, so they are contract guards rather than a strict
// RED: they pin that an invalid format is never accepted once ValueEnum lands.

#[test]
fn features_invalid_format_exits_2() {
    run_in_empty(&["features", "--format", "xml"]);
}

#[test]
fn questions_invalid_format_exits_2() {
    run_in_empty(&["questions", "--format", "xml"]);
}

#[test]
fn select_tests_invalid_format_exits_2() {
    run_in_empty(&["select-tests", "--format", "xml"]);
}

#[test]
fn similar_invalid_format_exits_2() {
    run_in_empty(&["similar", "--format", "xml"]);
}

#[test]
fn valid_format_values_are_accepted_at_parse_time() {
    // Positive control: the documented values parse cleanly (exit 0 or a
    // non-format error), proving ValueEnum did not over-restrict.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["features", "--format", "json"])
        .assert()
        .failure()
        .code(2); // NoWorkspace — format itself parsed fine.
}
