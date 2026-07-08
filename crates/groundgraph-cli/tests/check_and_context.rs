//! CLI e2e tests for `groundgraph check` and `groundgraph context`.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("flutter_watermark_app")
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

fn init_and_index(tmp: &Path) {
    copy_dir(&fixture_path(), tmp);
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp)
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp)
        .arg("index")
        .assert()
        .success();
}

#[test]
fn check_on_clean_fixture_reports_no_findings() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_and_index(tmp.path());
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["check"])
        .assert()
        .success()
        .stdout(contains("0 findings"));
}

#[test]
fn check_reports_broken_manifest_link() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_and_index(tmp.path());
    let links_path = tmp.path().join(".groundgraph/links.yaml");
    let original = std::fs::read_to_string(&links_path).unwrap();
    let edited = original.replace("AutoPlacementService", "MissingService");
    std::fs::write(&links_path, edited).unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["check"])
        .assert()
        .failure()
        .stdout(contains("broken_link"))
        .stdout(contains("MissingService"));
}

#[test]
fn check_json_is_machine_readable() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_and_index(tmp.path());
    let out = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["check", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let raw = String::from_utf8(out).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(parsed["findings"].as_array().unwrap().is_empty());
}

#[test]
fn context_pack_json_has_slice_and_snippets() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_and_index(tmp.path());
    let out = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["context", "REQ-WATERMARK-001", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let raw = String::from_utf8(out).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["requirement_id"], "REQ-WATERMARK-001");
    assert!(!parsed["docs_snippets"].as_array().unwrap().is_empty());
    assert!(!parsed["impl_snippets"].as_array().unwrap().is_empty());
}
