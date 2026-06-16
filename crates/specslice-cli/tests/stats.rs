//! CLI e2e tests for `specslice stats` (P26).
//!
//! Every invocation appends one record to `.specslice/stats.jsonl`; `stats`
//! aggregates that ledger. We drive the real binary so the timing wrapper,
//! metric collection and exit-code path stay covered.

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

fn run(tmp_root: &Path, args: &[&str]) {
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp_root)
        .args(args)
        .assert()
        .success();
}

fn stats_json(tmp_root: &Path) -> serde_json::Value {
    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp_root)
        .args(["stats", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(&stdout).expect("valid stats JSON")
}

fn find_command<'a>(summary: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    summary["commands"]
        .as_array()?
        .iter()
        .find(|c| c["command"] == name)
}

#[test]
fn stats_records_invocations_and_search_metrics() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    // Start from a clean ledger so assertions are deterministic.
    run(tmp.path(), &["stats", "--reset"]);
    run(tmp.path(), &["search", "watermark"]);
    run(tmp.path(), &["search", "apply"]);

    let summary = stats_json(tmp.path());

    // Two search calls must be aggregated under a single "search" row.
    let search = find_command(&summary, "search").expect("search recorded");
    assert_eq!(search["calls"], 2, "summary={summary:#}");
    assert_eq!(search["errors"], 0);
    // search always emits its result counts, even when zero, so the metric
    // keys are always present — proving "返回了多少" is captured.
    assert!(
        search["metrics"]["hits"].is_number(),
        "missing hits metric: {summary:#}"
    );
    assert!(
        search["metrics"]["subgraph_nodes"].is_number(),
        "missing subgraph_nodes metric: {summary:#}"
    );

    // The reset itself is also a recorded "stats" invocation.
    assert!(
        find_command(&summary, "stats").is_some(),
        "stats command should record itself: {summary:#}"
    );
    assert!(summary["total_calls"].as_u64().unwrap() >= 3);
}

#[test]
fn stats_reset_clears_ledger() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    run(tmp.path(), &["search", "watermark"]);
    run(tmp.path(), &["stats", "--reset"]);

    // After reset the ledger holds only the reset's own record, so the next
    // read sees exactly one call (the reset) before appending itself.
    let summary = stats_json(tmp.path());
    assert_eq!(summary["total_calls"], 1, "summary={summary:#}");
    let only = &summary["commands"][0];
    assert_eq!(only["command"], "stats");
}

#[test]
fn stats_reset_json_emits_machine_readable_output() {
    // `--reset --json` must honour `--json` (not swallow it), so CI can parse
    // the outcome instead of a localized human string (issues2.md #92).
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["stats", "--reset", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stats --reset --json must emit JSON");
    assert_eq!(v["reset"], serde_json::Value::Bool(true), "stdout={stdout}");
    // bootstrap (init + index) wrote ledger records, so it existed.
    assert_eq!(
        v["existed"],
        serde_json::Value::Bool(true),
        "stdout={stdout}"
    );
}

#[test]
fn stats_reset_reports_when_ledger_absent() {
    // Resetting a non-existent ledger must not claim it was cleared (#92).
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let ledger = tmp.path().join(".specslice").join("stats.jsonl");
    if ledger.exists() {
        std::fs::remove_file(&ledger).unwrap();
    }
    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["stats", "--reset", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["existed"],
        serde_json::Value::Bool(false),
        "stdout={stdout}"
    );
}

#[test]
fn stats_index_run_records_symbol_counts() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    run(tmp.path(), &["stats", "--reset"]);
    run(tmp.path(), &["index"]);

    let summary = stats_json(tmp.path());
    let index = find_command(&summary, "index").expect("index recorded");
    // The watermark fixture has Dart symbols, so the indexer must report a
    // positive symbol count via the stats collector.
    assert!(
        index["metrics"]["symbols"].as_i64().unwrap_or(0) > 0,
        "index should record symbol count: {summary:#}"
    );
}
