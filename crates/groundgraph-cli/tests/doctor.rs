//! #116 — `groundgraph doctor` probes git / SCIP indexers / Dart / graph.db /
//! config and reports ✓ / ✗ per check, exiting 2 when a required probe fails.

use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn doctor_exits_2_when_workspace_missing() {
    // An empty dir has no .groundgraph.yaml and no graph.db → required probes
    // fail → exit 2 (the #233 user-error code).
    let tmp = TempDir::new().unwrap();
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("doctor")
        .assert()
        .failure()
        .code(2);
    let combined = format!(
        "{}{}",
        String::from_utf8(output.get_output().stdout.clone()).unwrap(),
        String::from_utf8(output.get_output().stderr.clone()).unwrap()
    );
    assert!(
        combined.contains("Doctor:"),
        "should print a summary: {combined}"
    );
    assert!(
        combined.contains(".groundgraph.yaml"),
        "should flag the missing config: {combined}"
    );
    assert!(
        combined.contains("graph.db"),
        "should flag the missing graph store: {combined}"
    );
}

#[test]
fn doctor_reports_all_checks_in_initialised_repo() {
    // After `init` the required probes (git on PATH, config, graph.db) pass;
    // optional probes (SCIP, Dart) are reported but never fail the run.
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
        .arg("doctor")
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("git"), "should check git: {stdout}");
    assert!(
        stdout.contains("graph.db"),
        "should check graph.db: {stdout}"
    );
    assert!(
        stdout.contains(".groundgraph.yaml"),
        "should check config: {stdout}"
    );
    assert!(
        stdout.contains("SCIP"),
        "should check SCIP indexers: {stdout}"
    );
    assert!(
        stdout.contains("Doctor:"),
        "should print a summary: {stdout}"
    );
}
