//! #232 — `groundgraph index` surfaces partial indexer failures (parse
//! timeouts, SCIP failures, schema skips) and exits non-zero by default so CI
//! does not mistake an index with gaps for a fully successful one. The
//! `--fail-on-partial` flag (default true) toggles the exit-code behaviour.

use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

/// Build a repo whose tree-sitter python pass is enabled, plus a python file
/// large enough to blow a 1ms parse budget — forcing a parse timeout, which
/// is exactly the "partial failure" #232 wants visible.
fn setup_partial_repo(tmp: &Path) {
    let mut big = String::new();
    for i in 0..3000 {
        big.push_str(&format!(
            "def func_{i}(a, b, c):\n    if a > {i} and b < {i}:\n        return [j*a + k*b for j in range(c) for k in range(a)]\n    return None\n"
        ));
    }
    std::fs::write(tmp.join("big.py"), big).unwrap();

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp)
        .arg("init")
        .assert()
        .success();

    // Enable the tree-sitter python pass (off by default) and turn enrichment
    // off so the test isolates the parse-timeout partial failure.
    let cfg_path = tmp.join(".groundgraph.yaml");
    let cfg = std::fs::read_to_string(&cfg_path).unwrap();
    let cfg = cfg
        .replace(
            "treesitter:\n  enabled: false\n  languages: []",
            "treesitter:\n  enabled: true\n  languages: [python]",
        )
        .replace(
            "  analyzer: true\n  scip: true",
            "  analyzer: false\n  scip: false",
        );
    std::fs::write(&cfg_path, cfg).unwrap();
}

#[test]
fn index_exits_2_on_parse_timeout_partial_failure() {
    let tmp = TempDir::new().unwrap();
    setup_partial_repo(tmp.path());
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .env("GROUNDGRAPH_PARSE_BUDGET_MS", "1")
        .arg("index")
        .assert()
        .failure()
        .code(2);
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("partial failure"),
        "stderr should report the partial failure: {stderr}"
    );
    assert!(
        stderr.contains("tree-sitter-python"),
        "stderr should name the timed-out indexer: {stderr}"
    );
}

#[test]
fn index_fail_on_partial_false_exits_zero() {
    let tmp = TempDir::new().unwrap();
    setup_partial_repo(tmp.path());
    // Same partial failure, but the operator opts out — exit 0 (the index
    // still completed; the gap is only warned about).
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .env("GROUNDGRAPH_PARSE_BUDGET_MS", "1")
        .args(["index", "--fail-on-partial=false"])
        .assert()
        .success();
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("partial failure"),
        "even when tolerated the failure should still be reported: {stderr}"
    );
}

#[test]
fn index_clean_repo_exits_zero() {
    // Happy path: an empty repo has no partial failures → exit 0 (no
    // regression from the new default-fail behaviour).
    let tmp = TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();
}

#[test]
fn index_stdout_carries_report_not_progress_noise() {
    // #231 — progress (the indicatif spinner) routes to stderr / hides under
    // CI; stdout must carry only the human-readable report. Guards against a
    // future regression that points the draw target at stdout.
    let tmp = TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("Docs index:"),
        "stdout should carry the report: {stdout}"
    );
    assert!(
        !stdout.contains("index →"),
        "stdout must not leak indicatif spinner state: {stdout}"
    );
}

#[test]
fn index_quiet_suppresses_warn_level_partial_failure_message() {
    // #230 — diagnostics now route through tracing. `-q` (error level) must
    // suppress the warn-level partial-failure message. Uses
    // `--fail-on-partial=false` so the run exits 0 and the only "partial
    // failure" text on stderr is the warn-level diagnostic (not a fatal
    // error line, which `-q` must still show). Before the tracing migration
    // the message was a bare `eprintln!` that ignored `-q`.
    let tmp = TempDir::new().unwrap();
    setup_partial_repo(tmp.path());
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .env("GROUNDGRAPH_PARSE_BUDGET_MS", "1")
        .args(["-q", "index", "--fail-on-partial=false"])
        .assert()
        .success();
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains("partial failure"),
        "-q should suppress the warn-level partial failure message now that it is a tracing event: {stderr}"
    );
}
