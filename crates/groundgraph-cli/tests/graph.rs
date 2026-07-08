//! CLI e2e tests for `groundgraph graph` (P6).
//!
//! All tests share one fixture: the bundled watermark Flutter app, which
//! already ships with a fully-linked `.groundgraph/links.yaml` manifest. The CLI is
//! exercised through the real `groundgraph` binary so flag parsing and exit
//! codes stay covered.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use assert_cmd::Command;

static PIXCRAFT_BOOTSTRAP_LOCK: Mutex<()> = Mutex::new(());

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

#[test]
fn graph_json_prints_view_model() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
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

    let assert = Command::cargo_bin("groundgraph")
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

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--out"])
        .arg(&out)
        .assert()
        .success();
    assert!(out.exists());
    let body = std::fs::read_to_string(&out).unwrap();
    let _v: serde_json::Value = serde_json::from_str(&body).expect("file is JSON");
    // #111: when output goes to a file, stdout must stay empty so a piped
    // `--out` invocation never mixes the "wrote …" status line into data.
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "status message must go to stderr, not stdout: {stdout:?}"
    );
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("wrote"),
        "status line should be on stderr: {stderr:?}"
    );
}

#[test]
fn graph_mermaid_prints_flowchart() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    // Default (overview) mermaid honours the view's visible surface: a bounded
    // set of top-level module boxes, not a dump of the whole graph. It must
    // still be a valid flowchart with aliased ids (no raw artifact ids).
    let overview = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "mermaid"])
        .assert()
        .success();
    let overview = String::from_utf8(overview.get_output().stdout.clone()).unwrap();
    assert!(overview.starts_with("flowchart LR"), "got: {overview}");
    assert!(overview.contains("n0"), "no nodes rendered: {overview}");
    assert!(
        !overview.contains("dart_class::"),
        "raw id leaked: {overview}"
    );

    // `--view business` is the relationship surface: requirements plus their
    // one-hop evidence, so the diagram has labelled edges.
    let business = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "mermaid", "--view", "business"])
        .assert()
        .success();
    let business = String::from_utf8(business.get_output().stdout.clone()).unwrap();
    assert!(business.starts_with("flowchart LR"), "got: {business}");
    assert!(business.contains("-->"), "no edges: {business}");
    // Edge labels include the edge kind from the engine view.
    assert!(
        business.contains("declares_implementation") || business.contains("documents"),
        "no edge label: {business}"
    );
    // Aliases keep raw artifact ids out of the diagram body.
    assert!(
        !business.contains("dart_class::"),
        "raw id leaked: {business}"
    );
}

#[test]
fn graph_html_writes_self_contained_file_to_default_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "html"])
        .assert()
        .success();

    let out = tmp.path().join(".groundgraph/export/graph.html");
    assert!(
        out.exists(),
        "default export path missing: {}",
        out.display()
    );
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.starts_with("<!doctype html>"), "missing doctype");
    assert!(body.contains("GroundGraph Graph"));
    assert!(body.contains("<script id=\"groundgraph-data\""));
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

    Command::cargo_bin("groundgraph")
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

    let assert = Command::cargo_bin("groundgraph")
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
    let manifest = tmp.path().join(".groundgraph/links.yaml");
    std::fs::write(&manifest, "requirements: {}\n").unwrap();
    // Re-index to reflect the change.
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    let assert = Command::cargo_bin("groundgraph")
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
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "html"])
        .assert()
        .success();
    let html = std::fs::read_to_string(tmp.path().join(".groundgraph/export/graph.html")).unwrap();
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
        tmp.path().join(".groundgraph/links.yaml"),
        "requirements: {}\n",
    )
    .unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "html", "--view", "business"])
        .assert()
        .success();
    let html = std::fs::read_to_string(tmp.path().join(".groundgraph/export/graph.html")).unwrap();
    assert!(html.contains("no_business_logic"));
    assert!(html.contains("\"view\":\"business\""));
}

// ---------------------------------------------------------------------------
// P6.2: code-fact graph regression tests against the pixcraft IAP fixture.
// ---------------------------------------------------------------------------

fn pixcraft_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("pixcraft_iap")
}

fn sidecar_entrypoint() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tool")
        .join("groundgraph_dart_analyzer")
        .join("bin")
        .join("groundgraph_dart_analyzer.dart")
}

fn bootstrap_pixcraft(tmp_root: &Path) {
    let _guard = PIXCRAFT_BOOTSTRAP_LOCK
        .lock()
        .expect("pixcraft bootstrap mutex poisoned");
    copy_dir(&pixcraft_fixture_path(), tmp_root);
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
    let index = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp_root)
        .env("GROUNDGRAPH_DART_ANALYZER", "1")
        .env(
            "GROUNDGRAPH_DART_ANALYZER_BIN",
            format!("dart run {}", sidecar_entrypoint().display()),
        )
        .arg("index")
        .assert()
        .success();
    let stdout = String::from_utf8(index.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("Resolver: dart_analyzer"),
        "pixcraft graph tests require analyzer references/calls, got index output:\n{stdout}"
    );
}

#[test]
fn graph_focus_on_iap_module_returns_file_and_class_nodes() {
    // Mirrors the user's complaint: `--focus lib/core/iap` previously
    // returned a single module node with `edges: []`. After P6.2 it must
    // include at least the file and the `IapProductIds` class.
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap_pixcraft(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--focus", "lib/core/iap"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let ids: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"module::lib/core/iap"), "{ids:?}");
    assert!(
        ids.contains(&"file::lib/core/iap/iap_constants.dart"),
        "{ids:?}"
    );
    assert!(
        ids.contains(&"dart_class::lib/core/iap/iap_constants.dart#IapProductIds"),
        "{ids:?}"
    );
}

#[test]
fn graph_focus_on_apply_purchase_emits_calls_and_references_chain() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap_pixcraft(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "graph",
            "--format",
            "json",
            "--focus",
            "dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    let edges: Vec<(&str, &str, &str)> = v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap(),
                e["from"].as_str().unwrap(),
                e["to"].as_str().unwrap(),
            )
        })
        .collect();

    // Outgoing references to IapProductIds.
    assert!(
        edges.iter().any(|(k, f, t)| *k == "references"
            && *f == "dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase"
            && *t == "dart_class::lib/core/iap/iap_constants.dart#IapProductIds"),
        "missing references edge in focus output: {edges:?}",
    );
    // Incoming calls from PaywallScreen.listenToPurchaseUpdates.
    assert!(
        edges
            .iter()
            .any(|(k, f, t)| *k == "calls"
                && *f
                    == "dart_method::lib/features/paywall/paywall_screen.dart#PaywallScreen.listenToPurchaseUpdates"
                && *t == "dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase"),
        "missing calls edge from listener: {edges:?}",
    );
}

#[test]
fn graph_json_default_view_includes_calls_and_references_edge_kinds() {
    // Confirms that the unfiltered `graph --format json` no longer reports
    // only `contains` / `imports` for a code-only repo — the new fact
    // edges must show up.
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap_pixcraft(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let kinds: std::collections::BTreeSet<&str> = v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains("calls"), "{kinds:?}");
    assert!(kinds.contains("references"), "{kinds:?}");
}

#[test]
fn graph_json_max_nodes_emits_truncation_finding() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
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

#[test]
fn graph_json_pretty_flag_emits_indented_output() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--pretty"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    // Pretty JSON contains newlines and 2-space indentation; compact does
    // neither.
    assert!(
        stdout.contains("\n  \""),
        "expected pretty-printed indentation, got: {stdout}"
    );
}

#[test]
fn graph_mermaid_writes_to_out_path_when_given() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let out = tmp.path().join("graph.mmd");

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "mermaid", "--out"])
        .arg(&out)
        .assert()
        .success();
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.starts_with("flowchart LR"), "got: {body}");
}

#[test]
fn graph_focus_unknown_id_emits_focus_not_found_finding() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--focus", "no-such-id-here"])
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
    assert!(codes.contains(&"focus_not_found"), "{codes:?}");
    // Unknown focus clears nodes/edges.
    assert!(v["nodes"].as_array().unwrap().is_empty());
}

#[test]
fn graph_command_fails_when_no_groundgraph_workspace_present() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Do NOT init or index — there's no .groundgraph/ directory.

    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "expected non-zero exit code");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("no GroundGraph workspace") && stderr.contains("groundgraph init"),
        "stderr did not explain missing workspace: {stderr}"
    );
}

#[test]
fn graph_view_code_default_visible_is_modules_only() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["graph", "--format", "json", "--view", "code"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["view"], "code");
}
