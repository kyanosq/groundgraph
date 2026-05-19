//! CLI e2e tests for `specslice index`.

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

fn copy_fixture(dest: &std::path::Path) {
    let fixture = fixture_path();
    copy_dir(&fixture, dest);
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

#[test]
fn index_docs_reports_doc_sections_without_rule_requirements() {
    let tmp = tempfile::TempDir::new().unwrap();
    copy_fixture(tmp.path());

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["index", "--docs-only"])
        .assert()
        .success()
        .stdout(contains("Requirements: 0"))
        .stdout(contains("DocSections: 1"));
}

#[test]
fn index_full_reports_dart_symbols_tests_and_manifest_links() {
    let tmp = tempfile::TempDir::new().unwrap();
    copy_fixture(tmp.path());

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["index"])
        .assert()
        .success()
        .stdout(contains("Requirements: 1"))
        .stdout(contains("TestCases: 1"))
        .stdout(contains("Links index:"))
        .stdout(contains("Implementations: 1"))
        .stdout(contains("Tests: 1"));
}
