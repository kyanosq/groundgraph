//! #219 — end-to-end CLI coverage for the ten analysis commands that
//! previously had zero `cargo_bin("groundgraph")` exercise:
//! `trace`, `schema-index`, `questions`, `suggest-tests`, `feature-pack`,
//! `contract` (single-graph) and `port-coverage`, `route-coverage`,
//! `graph-equiv`, `graph-diff` (dual-db).
//!
//! These guard the CLI wrapper layer (arg parse → engine call → serialize →
//! stdout) which the in-engine unit tests cannot see. The dual-db commands are
//! driven as a *self-comparison* (same graph.db on both sides) so the result is
//! deterministic: a graph is trivially equivalent / fully ported / unchanged
//! against itself.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("pixcraft_iap")
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
    let db = tmp_root.join(".groundgraph/graph.db");
    if db.exists() {
        std::fs::remove_file(&db).unwrap();
    }
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp_root)
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp_root)
        .arg("index")
        .assert()
        .success();
}

/// Run a subcommand in `cwd`, assert exit 0, and return stdout parsed as JSON.
fn run_json(cwd: &Path, args: &[&str]) -> Value {
    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(cwd)
        .args(args)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`{}` stdout is not JSON ({e}): {stdout}", args.join(" ")))
}

#[test]
fn single_graph_analysis_commands_round_trip_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let cwd = tmp.path();

    // trace: walks the call/reference closure from a search seed.
    let trace = run_json(cwd, &["trace", "applyPurchase", "--json"]);
    assert!(trace.is_object(), "trace must emit a JSON object: {trace}");

    // schema-index: indexes DB table schemas (none in this fixture → empty but
    // must still succeed and emit valid JSON stats).
    let schema = run_json(cwd, &["schema-index", "--json"]);
    assert!(schema.is_object(), "schema-index must emit JSON: {schema}");

    // questions: orphan/coverage questions report.
    let questions = run_json(cwd, &["questions", "--format", "json"]);
    assert!(
        questions["stats"].is_object(),
        "questions must emit a stats block: {questions}"
    );

    // suggest-tests: prioritised test suggestions.
    let suggest = run_json(cwd, &["suggest-tests", "--json"]);
    assert!(
        suggest.is_object(),
        "suggest-tests must emit JSON: {suggest}"
    );

    // feature-pack: scoped decision-evidence pack (JSON unless --text).
    let pack = run_json(cwd, &["feature-pack", "--path", "lib"]);
    assert!(pack.is_object(), "feature-pack must emit JSON: {pack}");

    // contract: data-contract (table schema + serialization keys) report.
    let contract = run_json(cwd, &["contract", "--json"]);
    assert!(contract.is_object(), "contract must emit JSON: {contract}");
}

#[test]
fn dual_db_comparison_commands_round_trip_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let cwd = tmp.path();
    let db = tmp.path().join(".groundgraph/graph.db");
    let db = db.to_str().unwrap();

    // port-coverage of a graph against itself: every source symbol is ported,
    // so the missing count must be exactly zero.
    let port = run_json(
        cwd,
        &[
            "port-coverage",
            "--source-db",
            db,
            "--target-db",
            db,
            "--json",
        ],
    );
    assert_eq!(
        port["stats"]["missing_names"], 0,
        "self port-coverage must have zero missing symbols: {port}"
    );

    // route-coverage of a graph against itself (no HTTP routes in fixture →
    // empty, but the CLI pipeline must still round-trip valid JSON).
    let route = run_json(
        cwd,
        &[
            "route-coverage",
            "--source-db",
            db,
            "--target-db",
            db,
            "--json",
        ],
    );
    assert!(route["stats"].is_object(), "route-coverage stats: {route}");

    // graph-equiv of a graph against itself: trivially equivalent — full name
    // coverage and zero missing names on either side.
    let equiv = run_json(
        cwd,
        &[
            "graph-equiv",
            "--source-db",
            db,
            "--target-db",
            db,
            "--json",
        ],
    );
    assert_eq!(
        equiv["names"]["missing"], 0,
        "self graph-equiv must have zero missing names: {equiv}"
    );
    assert_eq!(
        equiv["names"]["coverage"], 1.0,
        "self graph-equiv must report full name coverage: {equiv}"
    );

    // graph-diff of a graph against itself: no node/edge changes.
    let diff = run_json(
        cwd,
        &[
            "graph-diff",
            "--base-db",
            db,
            "--head-db",
            db,
            "--format",
            "json",
        ],
    );
    assert!(
        diff.is_object(),
        "graph-diff must emit a JSON object: {diff}"
    );
}
