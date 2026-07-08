//! CLI e2e tests for `groundgraph init`.

use assert_cmd::Command;
use assert_fs::prelude::*;
use predicates::str::contains;

/// Running `groundgraph init` in an empty directory generates the GroundGraph
/// config file and graph database under the primary `.groundgraph` namespace.
#[test]
fn init_creates_config_and_graph_db_in_empty_directory() {
    let repo = assert_fs::TempDir::new().expect("create tempdir");

    let mut cmd = Command::cargo_bin("groundgraph").expect("locate groundgraph binary");
    cmd.current_dir(repo.path())
        .arg("init")
        .assert()
        .success()
        .stdout(contains(".groundgraph.yaml"))
        .stdout(contains(".groundgraph/links.yaml"))
        .stdout(contains(".groundgraph/graph.db"));

    repo.child(".groundgraph.yaml")
        .assert(predicates::path::is_file());
    repo.child(".groundgraph")
        .assert(predicates::path::is_dir());
    repo.child(".groundgraph/graph.db")
        .assert(predicates::path::is_file());
    repo.child(".groundgraph/links.yaml")
        .assert(predicates::path::is_file());
}

/// MVP-0 acceptance: repeatedly running `groundgraph init` must not clobber an
/// existing config nor lose existing data in the graph database.
#[test]
fn init_is_idempotent_and_preserves_existing_config() {
    let repo = assert_fs::TempDir::new().expect("create tempdir");

    let custom_config = "\
repo:
  root: .
  default_branch: trunk
storage:
  path: .groundgraph/graph.db
";
    repo.child(".groundgraph.yaml")
        .write_str(custom_config)
        .expect("seed existing config");

    let mut cmd = Command::cargo_bin("groundgraph").expect("locate groundgraph binary");
    cmd.current_dir(repo.path())
        .arg("init")
        .assert()
        .success()
        .stdout(contains("kept"));

    repo.child(".groundgraph.yaml")
        .assert(predicates::str::contains("default_branch: trunk"));
    repo.child(".groundgraph/graph.db")
        .assert(predicates::path::is_file());
}
