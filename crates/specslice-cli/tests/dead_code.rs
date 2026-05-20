//! P7 — `specslice dead-code` CLI integration test.
//!
//! Uses the watermark fixture (no Dart sidecar required) to exercise
//! the new command end-to-end. The lightweight indexer captures
//! enough nodes/edges for the analyzer to find at least one
//! candidate; we assert shape, ordering and JSON contract here so
//! flag parsing + IO stay covered.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

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

fn bootstrap(tmp_root: &Path) {
    copy_dir(&fixture_path(), tmp_root);
    let db = tmp_root.join(".specslice/graph.db");
    if db.exists() {
        std::fs::remove_file(&db).unwrap();
    }
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
fn dead_code_default_text_output_has_chinese_header_and_stats() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["dead-code"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SpecSlice dead-code"));
    assert!(stdout.contains("总符号"));
    assert!(stdout.contains("入口点"));
    assert!(stdout.contains("最低置信度"));
}

#[test]
fn dead_code_json_emits_schema_v1_and_sorted_candidates() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["dead-code", "--json", "--min-confidence", "low"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["schema_version"], serde_json::json!(1));
    assert!(v["stats"]["total_code_symbols"].is_number());
    assert!(v["stats"]["entrypoints"].is_number());
    let candidates = v["candidates"].as_array().unwrap();
    // Confidence must descend (high → medium → low). Convert to a
    // numeric rank so the assertion is robust.
    let rank = |s: &str| match s {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    };
    let ranks: Vec<i32> = candidates
        .iter()
        .map(|c| rank(c["confidence"].as_str().unwrap_or("")))
        .collect();
    let mut sorted = ranks.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        ranks, sorted,
        "candidates must be ordered by confidence desc"
    );
    // Every candidate must carry reasons + an id.
    for c in candidates {
        assert!(c["id"].is_string());
        assert!(c["kind"].is_string());
        assert!(c["reasons"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false));
    }
}

#[test]
fn dead_code_min_confidence_high_filters_lower_buckets() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["dead-code", "--json", "--min-confidence", "high"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    for c in v["candidates"].as_array().unwrap() {
        assert_eq!(
            c["confidence"].as_str(),
            Some("high"),
            "--min-confidence high must drop medium/low candidates"
        );
    }
}
