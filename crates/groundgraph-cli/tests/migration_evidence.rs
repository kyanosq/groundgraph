//! Migration evidence must not turn a name match into a correctness claim.
use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

fn index(root: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(".groundgraph.yaml"), "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .groundgraph/graph.db\nlanguages:\n  - id: java\n    paths: [src]\nenrichment:\n  scip: false\n  analyzer: false\n  lsp: false\n").unwrap();
    for (name, source) in files {
        std::fs::write(root.join("src").join(name), source).unwrap();
    }
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(root)
        .arg("index")
        .assert()
        .success();
}

fn json(root: &Path, args: &[&str]) -> Value {
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(root)
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn java_dispatch_requires_a_declaration_not_an_impl_suffix() {
    let dir = tempfile::tempdir().unwrap();
    index(dir.path(), &[
        ("Price.java", "public class Price { public int quote(int n) { return n * 2; } }"),
        ("PriceImpl.java", "public class PriceImpl { public int quote(int n) { return n * 200; } }"),
        ("Calculator.java", "package billing; public interface Calculator { int calculate(int n); }"),
        ("OtherCalculator.java", "package other; public interface Calculator { int calculate(int n); }"),
        ("Standard.java", "package billing; public class Standard implements Calculator { public int calculate(int n) { return n * 2; } }"),
    ]);
    let pack = json(dir.path(), &["feature-pack", "--path", "src"]);
    let edges: Vec<_> = pack["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "declares_implementation")
        .collect();
    assert_eq!(
        edges.len(),
        1,
        "only the explicitly declared billing interface is linked: {edges:?}"
    );
    assert!(edges[0]["from"]
        .as_str()
        .unwrap()
        .contains("src/Calculator.java"));
    assert!(edges[0]["to"]
        .as_str()
        .unwrap()
        .contains("Standard.calculate"));
    assert!(edges[0]["assertion"]["evidence_json"].is_string());
}

#[test]
fn feature_pack_and_trace_keep_assertion_provenance() {
    let dir = tempfile::tempdir().unwrap();
    index(dir.path(), &[("Price.java", "public class Price { public int quote(int n) { return helper(n); } public int helper(int n) { return n * 2; } }")]);
    for result in [
        json(dir.path(), &["feature-pack", "--path", "src"]),
        json(dir.path(), &["trace", "quote", "--seeds", "1", "--json"]),
    ] {
        let edge = result["edges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "calls")
            .unwrap();
        assert!(
            edge["assertion"]["confidence"].is_number(),
            "confidence was lost: {edge}"
        );
        assert!(edge["assertion"]["source"].is_string());
        assert!(edge["assertion"].get("evidence_json").is_some());
    }
}

#[test]
fn identical_names_never_claim_behavioral_equivalence() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    index(
        &source,
        &[(
            "Price.java",
            "public class Price { public int quote(int n) { return n * 2; } }",
        )],
    );
    index(
        &target,
        &[(
            "Price.java",
            "public class Price { public int quote(int n) { return n * 200; } }",
        )],
    );
    for command in ["port-coverage", "graph-equiv"] {
        let result = json(
            &source,
            &[
                command,
                "--source-db",
                source.join(".groundgraph/graph.db").to_str().unwrap(),
                "--target-db",
                target.join(".groundgraph/graph.db").to_str().unwrap(),
                "--json",
            ],
        );
        assert_eq!(result["behavioral_equivalence"], "not_evaluated");
        assert_eq!(result["assessment"], "structural_only");
    }
}

#[test]
fn unavailable_source_and_bounded_evidence_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    index(
        dir.path(),
        &[(
            "Price.java",
            "public class Price { public int quote(int n) { return n * 2; } }",
        )],
    );
    std::fs::remove_file(dir.path().join("src/Price.java")).unwrap();
    let pack = json(dir.path(), &["feature-pack", "--path", "src"]);
    assert_eq!(pack["omitted_symbols"].as_array().unwrap().len(), 2);
    assert!(pack["evidence_limit_per_symbol"].is_number());
    assert!(!pack["limitations"].as_array().unwrap().is_empty());
}

#[test]
fn invalid_workspace_config_cannot_reset_a_different_stats_ledger() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".groundgraph")).unwrap();
    let ledger = dir.path().join(".groundgraph/stats.jsonl");
    std::fs::write(&ledger, "must survive\n").unwrap();
    std::fs::write(dir.path().join(".groundgraph.yaml"), "storage: [invalid").unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(dir.path())
        .args(["stats", "--reset"])
        .assert()
        .failure();
    assert_eq!(std::fs::read_to_string(ledger).unwrap(), "must survive\n");
}

#[test]
fn compiler_calls_keep_static_evidence_and_unknown_calls_do_not_prove_purity() {
    let dir = tempfile::tempdir().unwrap();
    index(dir.path(), &[("Price.java", "class Price { int quote(int n) { return helper(n); } int helper(int n) { return n * 2; } int external() { return mapper.selectCount(); } }")]);
    let pack = json(dir.path(), &["feature-pack", "--path", "src"]);
    let call = pack["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "calls")
        .unwrap();
    assert_eq!(call["assertion"]["certainty"], "fact");
    assert_eq!(call["assertion"]["status"], "confirmed");
    let evidence: Value =
        serde_json::from_str(call["assertion"]["evidence_json"].as_str().unwrap()).unwrap();
    assert_eq!(evidence["resolver"], "javac");
    assert_eq!(evidence["static_target_only"], true);
    let unknown = pack["java_analysis"]["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["expression"] == "mapper.selectCount()")
        .unwrap();
    assert_eq!(unknown["resolution"], "unresolved");
    assert!(unknown["target"].is_null());
    let method = pack["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "external")
        .unwrap();
    assert_eq!(method["purity"], "unknown");
}

#[test]
fn web_export_honors_explicit_caps_and_contains_no_external_assets() {
    let dir = tempfile::tempdir().unwrap();
    index(
        dir.path(),
        &[("Price.java", "class Price { int quote() { return 2; } }")],
    );
    let output = dir.path().join("viewer.html");
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(dir.path())
        .args(["graph", "--format", "web", "--max-nodes", "1", "--out"])
        .arg(&output)
        .assert()
        .success();
    let html = std::fs::read_to_string(output).unwrap();
    let start =
        html.find("type=\"application/json\">").unwrap() + "type=\"application/json\">".len();
    let end = html[start..].find("</script>").unwrap() + start;
    let data: Value = serde_json::from_str(&html[start..end]).unwrap();
    assert_eq!(data["nodes"].as_array().unwrap().len(), 1);
    assert!(!html.contains("<script src="));
    assert!(!html.contains("<link rel=\"stylesheet\""));
    assert!(!html.contains("unsafe-eval"));
}
