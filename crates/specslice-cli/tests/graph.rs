//! CLI e2e tests for `specslice graph` (P6).
//!
//! All tests share one fixture: the bundled watermark Flutter app, which
//! already ships with a fully-linked `.specslice/links.yaml`. The CLI is
//! exercised through the real `specslice` binary so flag parsing and exit
//! codes stay covered.

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
    // Drop the pre-baked SQLite database so we re-index inside the temp dir.
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
fn graph_json_prints_view_model() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(v["schema_version"], 2);
    assert_eq!(v["view"], "overview");
    assert!(v["nodes"].as_array().unwrap().len() >= 4);
    assert!(!v["edges"].as_array().unwrap().is_empty());
    let kinds: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"requirement"), "{kinds:?}");
    assert!(kinds.contains(&"doc_section"), "{kinds:?}");
    assert!(kinds.contains(&"dart_class"), "{kinds:?}");
    assert!(v["stats"]["business_logic"].as_u64().unwrap() >= 1);
}

#[test]
fn graph_json_focus_filters_to_focused_neighbourhood() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--focus", "REQ-WATERMARK-001"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["focus"], "REQ-WATERMARK-001");
    let ids: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"req::REQ-WATERMARK-001"), "{ids:?}");
}

#[test]
fn graph_json_writes_to_out_path_when_given() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let out = tmp.path().join("graph.json");

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--out"])
        .arg(&out)
        .assert()
        .success();
    assert!(out.exists());
    let body = std::fs::read_to_string(&out).unwrap();
    let _v: serde_json::Value = serde_json::from_str(&body).expect("file is JSON");
}

#[test]
fn graph_mermaid_prints_flowchart() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "mermaid"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.starts_with("flowchart LR"), "got: {stdout}");
    assert!(stdout.contains("-->"), "no edges: {stdout}");
    // Edge labels include the edge kind from the engine view.
    assert!(
        stdout.contains("declares_implementation") || stdout.contains("documents"),
        "no edge label: {stdout}"
    );
    // Aliases keep raw artifact ids out of the diagram body.
    assert!(!stdout.contains("dart_class::"), "raw id leaked: {stdout}");
}

#[test]
fn graph_html_writes_self_contained_file_to_default_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "html"])
        .assert()
        .success();

    let out = tmp.path().join(".specslice/export/graph.html");
    assert!(
        out.exists(),
        "default export path missing: {}",
        out.display()
    );
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.starts_with("<!doctype html>"), "missing doctype");
    assert!(body.contains("SpecSlice Graph"));
    assert!(body.contains("<script id=\"specslice-data\""));
    assert!(body.contains("REQ-WATERMARK-001"));
    // Offline-only: no remote dependencies allowed.
    assert!(!body.contains("https://"), "remote https URL leaked");
    assert!(!body.contains("http://"), "remote http URL leaked");
    assert!(!body.contains("cdn."), "CDN reference leaked");
    // The renderer JS must distinguish layers visually.
    assert!(
        body.contains("layer-confirmed"),
        "missing confirmed CSS class"
    );
    assert!(body.contains("layer-fact"), "missing fact CSS class");
}

#[test]
fn graph_html_supports_explicit_out_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let out = tmp.path().join("custom/graph.html");

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "html", "--out"])
        .arg(&out)
        .assert()
        .success();
    assert!(out.exists());
}

#[test]
fn graph_json_default_visibility_only_marks_top_level_modules_in_overview() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let visible: Vec<&serde_json::Value> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["default_visible"].as_bool().unwrap_or(false))
        .collect();
    assert!(!visible.is_empty(), "no visible nodes in overview");
    for n in &visible {
        assert_eq!(n["kind"], "module", "non-module visible by default: {n}");
        assert!(n["parent_id"].is_null());
    }
}

#[test]
fn graph_json_business_view_emits_no_business_finding_when_pixcraft_style() {
    // Bootstrap fixture but wipe links.yaml so no requirements exist.
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let manifest = tmp.path().join(".specslice/links.yaml");
    std::fs::write(&manifest, "requirements: {}\n").unwrap();
    // Re-index to reflect the change.
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--view", "business"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["view"], "business");
    let codes: Vec<&str> = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"no_business_logic"),
        "missing no_business_logic finding: {codes:?}"
    );
}

#[test]
fn graph_html_renders_three_pane_explorer_with_tree_canvas_detail() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "html"])
        .assert()
        .success();
    let html = std::fs::read_to_string(tmp.path().join(".specslice/export/graph.html")).unwrap();
    assert!(html.contains("class=\"tree\""));
    assert!(html.contains("class=\"canvas\""));
    assert!(html.contains("class=\"detail\""));
    // The view selector and tree-item / node-card CSS classes are the
    // explorer's contract surface.
    assert!(html.contains("id=\"view\""));
    assert!(html.contains("tree-item"));
    assert!(html.contains("node-card"));
    // module aggregator must be embedded (proves new contract).
    assert!(html.contains("\"kind\":\"module\""));
    assert!(!html.contains("https://"));
    assert!(!html.contains("http://"));
}

#[test]
fn graph_html_business_view_embeds_no_business_finding_when_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    std::fs::write(
        tmp.path().join(".specslice/links.yaml"),
        "requirements: {}\n",
    )
    .unwrap();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "html", "--view", "business"])
        .assert()
        .success();
    let html = std::fs::read_to_string(tmp.path().join(".specslice/export/graph.html")).unwrap();
    assert!(html.contains("no_business_logic"));
    assert!(html.contains("\"view\":\"business\""));
}

#[test]
fn graph_json_max_nodes_emits_truncation_finding() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--max-nodes", "2"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let codes: Vec<&str> = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"graph_truncated"), "{codes:?}");
    assert!(v["nodes"].as_array().unwrap().len() <= 2);
}
