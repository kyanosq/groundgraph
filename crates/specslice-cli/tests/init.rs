//! CLI e2e tests for `specslice init`.

use assert_cmd::Command;
use assert_fs::prelude::*;
use predicates::str::contains;

/// MVP-0 acceptance: running `specslice init` in an empty directory
/// generates the `.specslice.yaml` config file and the
/// `.specslice/graph.db` SQLite database.
#[test]
fn init_creates_config_and_graph_db_in_empty_directory() {
    let repo = assert_fs::TempDir::new().expect("create tempdir");

    let mut cmd = Command::cargo_bin("specslice").expect("locate specslice binary");
    cmd.current_dir(repo.path())
        .arg("init")
        .assert()
        .success()
        .stdout(contains(".specslice.yaml"))
        .stdout(contains(".specslice/links.yaml"))
        .stdout(contains(".specslice/graph.db"));

    repo.child(".specslice.yaml")
        .assert(predicates::path::is_file());
    repo.child(".specslice").assert(predicates::path::is_dir());
    repo.child(".specslice/graph.db")
        .assert(predicates::path::is_file());
    repo.child(".specslice/links.yaml")
        .assert(predicates::path::is_file());
}

/// MVP-0 acceptance: repeatedly running `specslice init` must not clobber an
/// existing config nor lose existing data in the graph database.
#[test]
fn init_is_idempotent_and_preserves_existing_config() {
    let repo = assert_fs::TempDir::new().expect("create tempdir");

    let custom_config = "\
repo:
  root: .
  default_branch: trunk
storage:
  path: .specslice/graph.db
";
    repo.child(".specslice.yaml")
        .write_str(custom_config)
        .expect("seed existing config");

    let mut cmd = Command::cargo_bin("specslice").expect("locate specslice binary");
    cmd.current_dir(repo.path())
        .arg("init")
        .assert()
        .success()
        .stdout(contains("kept"));

    repo.child(".specslice.yaml")
        .assert(predicates::str::contains("default_branch: trunk"));
    repo.child(".specslice/graph.db")
        .assert(predicates::path::is_file());
}
