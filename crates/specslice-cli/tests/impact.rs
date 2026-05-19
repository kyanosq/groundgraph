//! CLI e2e tests for `specslice impact`. Uses a temp git repository.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

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

fn run_git(repo: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("invoke git");
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(tmp: &Path) {
    copy_dir(&fixture_path(), tmp);
    run_git(tmp, &["init", "-q", "-b", "main"]);
    run_git(tmp, &["config", "user.email", "test@example.com"]);
    run_git(tmp, &["config", "user.name", "Test"]);
    // Ignore the SpecSlice runtime so commits stay clean across `init` runs.
    std::fs::write(tmp.join(".gitignore"), ".specslice/\n").unwrap();
    run_git(tmp, &["add", "."]);
    run_git(tmp, &["commit", "-q", "-m", "baseline"]);
}

#[test]
fn impact_reports_changed_method_and_affected_requirement() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_repo(tmp.path());

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    // Edit the implementation method so its line range contains a +/- diff.
    let impl_path = tmp
        .path()
        .join("lib/domain/watermark/auto_placement_service.dart");
    let original = std::fs::read_to_string(&impl_path).unwrap();
    let edited = original.replace(
        "candidates.sort((a, b) => b.score.compareTo(a.score));",
        "candidates.sort((a, b) => b.score.compareTo(a.score) * -1 * -1);",
    );
    std::fs::write(&impl_path, edited).unwrap();
    run_git(tmp.path(), &["add", "."]);
    run_git(tmp.path(), &["commit", "-q", "-m", "edit impl"]);

    let output = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["impact", "--base", "HEAD~1", "--head", "HEAD", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let raw = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();

    let reqs = parsed["affected_requirements"].as_array().unwrap();
    assert!(reqs.iter().any(|r| r["id"] == "req::REQ-WATERMARK-001"));
    let tests = parsed["linked_tests"].as_array().unwrap();
    assert!(tests.iter().any(|t| t["path"]
        .as_str()
        .map(|p| p.ends_with("auto_placement_service_test.dart"))
        .unwrap_or(false)));
}

#[test]
fn impact_reports_changed_doc_section_and_linked_implementation() {
    let tmp = tempfile::TempDir::new().unwrap();
    init_repo(tmp.path());

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    let doc = tmp.path().join("docs/watermark.md");
    let original = std::fs::read_to_string(&doc).unwrap();
    let edited = original.replace("用户导入图片后", "用户上传图片后");
    std::fs::write(&doc, edited).unwrap();
    run_git(tmp.path(), &["add", "."]);
    run_git(tmp.path(), &["commit", "-q", "-m", "edit doc"]);

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["impact", "--base", "HEAD~1", "--head", "HEAD"])
        .assert()
        .success()
        .stdout(contains("REQ-WATERMARK-001"))
        .stdout(contains("docs/watermark.md"))
        .stdout(contains("auto_placement_service_test.dart"));
}
