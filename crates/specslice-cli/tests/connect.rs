//! CLI e2e tests for `specslice connect`.

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

fn copy_fixture_without_manifest(dest: &std::path::Path) {
    let fixture = fixture_path();
    copy_dir(&fixture, dest);
    // The fixture ships with a pre-baked links manifest so that other
    // commands have something to inspect. P1 verifies the *generation* of
    // that manifest, so we wipe it before exercising connect.
    let manifest = dest.join(".specslice/links.yaml");
    if manifest.exists() {
        std::fs::remove_file(&manifest).unwrap();
    }
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

fn bootstrap(tmp_root: &std::path::Path) {
    copy_fixture_without_manifest(tmp_root);
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp_root)
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp_root)
        .arg("index")
        .assert()
        .success();
}

#[test]
fn connect_propose_emits_evidence_pack_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["connect", "propose"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let pack: serde_json::Value = serde_json::from_str(&stdout).expect("propose output is JSON");
    assert_eq!(pack["schema_version"], 1);
    assert!(pack["requirements"].as_array().unwrap().is_empty());
    assert!(!pack["orphan_doc_sections"].as_array().unwrap().is_empty());
    assert!(!pack["orphan_symbols"].as_array().unwrap().is_empty());
}

#[test]
fn connect_propose_writes_to_file_when_out_given() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let out = tmp.path().join("evidence.json");

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["connect", "propose", "--out"])
        .arg(&out)
        .arg("--pretty")
        .assert()
        .success();
    assert!(out.exists());
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("\"schema_version\": 1"));
}

#[test]
fn connect_apply_writes_links_manifest_from_validated_candidates() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let candidates = tmp.path().join("candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n    tests:\n      - test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region\n",
    )
    .unwrap();

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["connect", "apply", "--candidates"])
        .arg(&candidates)
        .assert()
        .success()
        .stdout(contains("1 accepted"));

    let manifest = std::fs::read_to_string(tmp.path().join(".specslice/links.yaml")).unwrap();
    assert!(manifest.contains("REQ-WATERMARK-001"));
    assert!(manifest.contains("auto_placement_service.dart#AutoPlacementService"));
}

#[test]
fn connect_apply_dry_run_emits_json_outcome_without_writing() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let candidates = tmp.path().join("candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["connect", "apply", "--dry-run", "--json", "--candidates"])
        .arg(&candidates)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["accepted"].as_array().unwrap().len(), 1);

    let manifest = std::fs::read_to_string(tmp.path().join(".specslice/links.yaml")).unwrap();
    assert!(!manifest.contains("AutoPlacementService"));
}

#[test]
fn connect_apply_mixed_outcome_exits_nonzero_and_prints_rejected_section() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let candidates = tmp.path().join("candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n  - requirement: REQ-NOPE-001\n    implementations:\n      - lib/domain/watermark/ghost.dart#Ghost\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["connect", "apply", "--candidates"])
        .arg(&candidates)
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("1 accepted, 1 rejected"), "{stdout}");
    assert!(stdout.contains("Rejected:"), "{stdout}");
    assert!(stdout.contains("REQ-NOPE-001"), "{stdout}");

    // Accepted REQ landed despite the rejected sibling.
    let manifest = std::fs::read_to_string(tmp.path().join(".specslice/links.yaml")).unwrap();
    assert!(manifest.contains("REQ-WATERMARK-001"));
}

#[test]
fn connect_apply_exits_nonzero_when_every_candidate_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let candidates = tmp.path().join("candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    implementations:\n      - lib/domain/watermark/ghost.dart#Ghost\n",
    )
    .unwrap();

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["connect", "apply", "--candidates"])
        .arg(&candidates)
        .assert()
        .failure();
}
