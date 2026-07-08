//! CLI e2e tests for `groundgraph export --format jsonl`.

use assert_cmd::Command;
use assert_fs::prelude::*;
use predicates::str::contains;

/// MVP-0 acceptance: even on an empty graph, `groundgraph export --format
/// jsonl` must produce a valid JSONL bundle (the directory and the per-table
/// files must exist; lines, if any, must parse as JSON).
#[test]
fn export_jsonl_emits_valid_files_for_empty_graph() {
    let repo = assert_fs::TempDir::new().expect("create tempdir");

    Command::cargo_bin("groundgraph")
        .expect("locate groundgraph binary")
        .current_dir(repo.path())
        .arg("init")
        .assert()
        .success();

    Command::cargo_bin("groundgraph")
        .expect("locate groundgraph binary")
        .current_dir(repo.path())
        .args(["export", "--format", "jsonl"])
        .assert()
        .success()
        .stdout(contains(".groundgraph/export"));

    let export_dir = repo.child(".groundgraph/export");
    export_dir.assert(predicates::path::is_dir());

    for table in ["nodes", "edge_assertions", "evidence"] {
        let file = export_dir.child(format!("{table}.jsonl"));
        file.assert(predicates::path::is_file());

        let raw = std::fs::read_to_string(file.path())
            .unwrap_or_else(|e| panic!("read {}: {e}", file.path().display()));

        for (idx, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
                panic!(
                    "expected JSONL line in {}, line {}, got error {e}: {line:?}",
                    file.path().display(),
                    idx + 1,
                )
            });
        }
    }
}
