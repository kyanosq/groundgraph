//! Exercise every CLI command's human-readable output. JSON-only tests live
//! alongside their command tests; this file makes sure the non-JSON branches
//! also see coverage.

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
    StdCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
}

fn setup_full(tmp: &Path) {
    copy_dir(&fixture_path(), tmp);
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp)
        .arg("init")
        .assert()
        .success();
    run_git(tmp, &["init", "-q", "-b", "main"]);
    run_git(tmp, &["config", "user.email", "t@t"]);
    run_git(tmp, &["config", "user.name", "T"]);
    std::fs::write(tmp.join(".gitignore"), ".specslice/\n").unwrap();
    run_git(tmp, &["add", "."]);
    run_git(tmp, &["commit", "-q", "-m", "baseline"]);
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp)
        .arg("index")
        .assert()
        .success();
}

#[test]
fn context_human_output_lists_snippets() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup_full(tmp.path());
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["context", "REQ-WATERMARK-001"])
        .assert()
        .success()
        .stdout(contains("Context Pack: REQ-WATERMARK-001"))
        .stdout(contains("Snippets included"));
}

#[test]
fn context_human_without_snippets_skips_summary() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup_full(tmp.path());
    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["context", "REQ-WATERMARK-001", "--no-snippets"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(!out.contains("Snippets included"), "got: {out}");
}

#[test]
fn impact_human_output_lists_changed_files_and_warnings() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup_full(tmp.path());
    let impl_path = tmp
        .path()
        .join("lib/domain/watermark/auto_placement_service.dart");
    let original = std::fs::read_to_string(&impl_path).unwrap();
    std::fs::write(
        &impl_path,
        original.replace(
            "candidates.sort((a, b) => b.score.compareTo(a.score));",
            "candidates.sort((a, b) => b.score.compareTo(a.score) * 1);",
        ),
    )
    .unwrap();
    run_git(tmp.path(), &["add", "."]);
    run_git(tmp.path(), &["commit", "-q", "-m", "edit impl"]);

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["impact", "--base", "HEAD~1", "--head", "HEAD"])
        .assert()
        .success()
        .stdout(contains("Affected requirements:"))
        .stdout(contains("Linked tests:"))
        .stdout(contains("Warnings:"));
}

#[test]
fn impact_human_output_handles_no_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup_full(tmp.path());
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["impact", "--base", "HEAD", "--head", "HEAD"])
        .assert()
        .success()
        .stdout(contains("(none)"));
}

#[test]
fn export_human_output_reports_bundle_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup_full(tmp.path());
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["export", "--format", "jsonl"])
        .assert()
        .success();
}

#[test]
fn fail_on_warning_returns_exit_one_for_orphan_requirement() {
    let tmp = tempfile::TempDir::new().unwrap();
    copy_dir(&fixture_path(), tmp.path());
    // Strip the implementation so the requirement has no tests/impl.
    std::fs::remove_dir_all(tmp.path().join("lib")).unwrap();
    std::fs::remove_dir_all(tmp.path().join("test")).unwrap();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["check", "--fail-on-warning"])
        .assert()
        .failure();
}

#[test]
fn cli_run_returns_error_with_friendly_message_when_workspace_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .failure()
        .stderr(contains("specslice init"));
}

#[test]
fn init_human_output_reports_paths() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(contains(".specslice"));
}

#[test]
fn slice_human_output_handles_missing_implementation_risk() {
    let tmp = tempfile::TempDir::new().unwrap();
    copy_dir(&fixture_path(), tmp.path());
    // Remove the implementation file so slice has no impl.
    std::fs::remove_dir_all(tmp.path().join("lib")).unwrap();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["slice", "REQ-WATERMARK-001"])
        .assert()
        .success()
        .stdout(contains("Risks:"));
}

#[test]
fn check_human_output_includes_error_severity_marker() {
    let tmp = tempfile::TempDir::new().unwrap();
    copy_dir(&fixture_path(), tmp.path());
    // Break the trace target.
    let impl_path = tmp
        .path()
        .join("lib/domain/watermark/auto_placement_service.dart");
    let s = std::fs::read_to_string(&impl_path).unwrap();
    std::fs::write(
        &impl_path,
        s.replace("@implements REQ-WATERMARK-001", "@implements REQ-NOPE"),
    )
    .unwrap();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("check")
        .assert()
        .failure()
        .stdout(contains("[ERROR]"));
}

#[test]
fn check_human_output_includes_warn_severity_marker() {
    let tmp = tempfile::TempDir::new().unwrap();
    copy_dir(&fixture_path(), tmp.path());
    // Remove the impl + test to make REQ-WATERMARK-001 an orphan (warning).
    std::fs::remove_dir_all(tmp.path().join("lib")).unwrap();
    std::fs::remove_dir_all(tmp.path().join("test")).unwrap();
    // Also strip the `## Related` section so the doc's `symbol://`/`test://`
    // references do not surface as `broken_related` errors. We want this
    // test to focus on the orphan-requirement warning path.
    let doc = tmp.path().join("docs/watermark.md");
    let body = std::fs::read_to_string(&doc).unwrap();
    let trimmed = body.split("## Related").next().unwrap_or(&body).to_string();
    std::fs::write(&doc, trimmed).unwrap();

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("check")
        .assert()
        .success()
        .stdout(contains("[WARN]"));
}
