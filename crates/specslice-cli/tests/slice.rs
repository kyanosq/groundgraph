//! CLI e2e tests for `specslice slice`.

use std::path::PathBuf;

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

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
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

fn init_and_index(tmp: &std::path::Path) {
    copy_dir(&fixture_path(), tmp);
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp)
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp)
        .arg("index")
        .assert()
        .success();
}

#[test]
fn slice_outputs_docs_impl_and_tests_human() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_and_index(tmp.path());

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["slice", "REQ-WATERMARK-001"])
        .assert()
        .success()
        .stdout(contains("Feature Slice: REQ-WATERMARK-001"))
        .stdout(contains("docs/watermark.md"))
        .stdout(contains("auto_placement_service.dart"))
        .stdout(contains("auto_placement_service_test.dart"));
}

#[test]
fn slice_json_is_machine_readable() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_and_index(tmp.path());

    let output = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["slice", "REQ-WATERMARK-001", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let raw = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["requirement_id"], "REQ-WATERMARK-001");
    assert!(!parsed["docs"].as_array().unwrap().is_empty());
}

#[test]
fn slice_unknown_requirement_fails_with_message() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_and_index(tmp.path());

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["slice", "REQ-NOPE"])
        .assert()
        .failure()
        .stderr(contains("REQ-NOPE"));
}
