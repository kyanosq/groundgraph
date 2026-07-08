//! CLI e2e tests for `groundgraph watch`.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn watch_once_collects_snapshot_without_running_forever() {
    let repo = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/lib.rs"), "pub fn hello() {}\n").unwrap();

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(repo.path())
        .args(["watch", "--once", "--no-initial-index"])
        .assert()
        .success()
        .stdout(contains("Watch snapshot"))
        .stdout(contains("files"));
}
