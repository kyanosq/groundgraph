use groundgraph_core::NodeKind;
use groundgraph_engine::index::{index_repository, IndexOptions};
use groundgraph_store::Store;
use serde_json::Value;
use std::path::Path;

fn write(root: &Path, path: &str, source: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, source).unwrap();
}

fn config(root: &Path, extra: &str) {
    write(root, ".groundgraph.yaml", &format!("repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .groundgraph/graph.db\nlanguages:\n  - id: java\n    paths: [src]\nenrichment:\n  scip: false\n  analyzer: false\n  lsp: false\n{extra}"));
}

fn index(root: &Path) -> Store {
    index_repository(IndexOptions::all(root)).unwrap();
    Store::open(root.join(".groundgraph/graph.db")).unwrap()
}

fn calls(store: &Store) -> Vec<Value> {
    store
        .list_nodes_by_kind(NodeKind::File)
        .unwrap()
        .iter()
        .filter_map(|n| n.metadata_json.as_deref())
        .map(|m| serde_json::from_str::<Value>(m).unwrap())
        .flat_map(|v| {
            v["java_analysis"]["calls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn java_calls_keep_overloads_repetition_recursion_and_unknowns() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "java_semantics:\n  enabled: true\n");
    write(
        root,
        "src/p/Service.java",
        "package p; class Service { void save(int x) {} void save(String x) {} }",
    );
    write(root, "src/p/Caller.java", "package p; class Caller { Service service; void run() { service.save(1); service.save(2); service.save(\"x\"); run(); missing.whatever(); } }");
    let store = index(root);
    let methods = store.list_nodes_by_kind(NodeKind::JavaMethod).unwrap();
    assert!(methods
        .iter()
        .any(|n| n.id.as_str().ends_with("Service.save(int)")));
    assert!(methods
        .iter()
        .any(|n| n.id.as_str().ends_with("Service.save(String)")));
    let sites = calls(&store);
    assert_eq!(
        sites.len(),
        5,
        "every source invocation is retained: {sites:#?}"
    );
    assert_eq!(
        sites
            .iter()
            .filter(|s| s["resolution"] == "resolved")
            .count(),
        4,
        "{sites:#?}"
    );
    assert_eq!(
        sites
            .iter()
            .filter(|s| s["resolution"] == "unresolved")
            .count(),
        1
    );
    let ids: std::collections::BTreeSet<_> =
        sites.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids.len(),
        5,
        "repeated calls on the same line must not collapse"
    );
    let targets: Vec<_> = sites.iter().filter_map(|s| s["target"].as_str()).collect();
    assert_eq!(
        targets
            .iter()
            .filter(|t| t.ends_with("Service.save(int)"))
            .count(),
        2
    );
    assert!(targets.iter().any(|t| t.ends_with("Caller.run()")));
    let before = sites;
    let again = calls(&index(root));
    assert_eq!(
        before, again,
        "stable calls on an unchanged source snapshot"
    );
}

#[test]
fn missing_compiler_preserves_inventory_and_exposes_failure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(
        root,
        "java_semantics:\n  enabled: true\n  java_command: groundgraph-nonexistent-java-for-test\n",
    );
    write(
        root,
        "src/A.java",
        "class A { Object x = create(); void go() { unknown.call(); } }",
    );
    let store = index(root);
    let sites = calls(&store);
    assert_eq!(sites.len(), 2);
    assert!(sites.iter().all(|s| s["resolution"] == "unresolved"));
    assert!(store
        .list_nodes_by_kind(NodeKind::File)
        .unwrap()
        .iter()
        .any(|n| n
            .metadata_json
            .as_deref()
            .unwrap_or("")
            .contains("compiler_unavailable")));
}

#[test]
fn implicit_constructor_is_not_misbound_to_a_class_declaration() {
    let temp = tempfile::tempdir().unwrap();
    config(temp.path(), "");
    write(
        temp.path(),
        "src/A.java",
        "class A { Object make() { new B(); return new A(); } } class B { B() { this(1); } B(int n) { super(); } }",
    );
    let store = index(temp.path());
    let sites = calls(&store);
    assert_eq!(sites.len(), 4);
    let implicit = sites.iter().find(|c| c["expression"] == "new A()").unwrap();
    assert_eq!(implicit["resolution"], "unresolved");
    assert!(
        implicit["target"].is_null(),
        "synthetic constructor has no source callable: {sites:?}"
    );
    for expected in ["B.B()", "B.B(int)"] {
        assert!(
            sites
                .iter()
                .any(|c| c["target"].as_str().is_some_and(|t| t.ends_with(expected))),
            "{sites:?}"
        );
    }
    assert_eq!(
        sites
            .iter()
            .filter(|c| c["resolution"] == "resolved")
            .count(),
        3
    );
}

#[test]
fn feign_links_are_service_and_verb_scoped_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "java_semantics:\n  enabled: true\n  services:\n    orders: [src/server]\n    unrelated: [src/other]\n");
    write(root, "src/client/Client.java", "package client; @FeignClient(name=\"orders\", path=\"/api\") interface Client { @GetMapping(\"/order/{id}\") String get(String id); }");
    write(root, "src/server/Controller.java", "package server; @RestController @RequestMapping(\"/api\") class Controller { @GetMapping(\"/order/{key}\") String get(String key) { return key; } @PostMapping(\"/order/{key}\") void post(String key) {} }");
    write(root, "src/other/Controller.java", "package other; @RestController @RequestMapping(\"/api\") class Controller { @GetMapping(\"/order/{key}\") String get(String key) { return key; } }");
    let store = index(root);
    let edges: Vec<_> = store
        .list_all_edges()
        .unwrap()
        .into_iter()
        .filter(|e| e.indexer.as_deref() == Some("java_feign"))
        .collect();
    assert_eq!(edges.len(), 1, "{edges:#?}");
    assert!(edges[0]
        .to_id
        .as_str()
        .contains("src/server/Controller.java"));
    assert_eq!(edges[0].certainty.as_str(), "candidate");
    assert!(edges[0]
        .evidence_json
        .as_deref()
        .unwrap()
        .contains("orders"));
}

#[test]
fn compiler_handles_declared_types_generics_inheritance_and_dispatch_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(
        root,
        "src/p/All.java",
        r#"package p;
interface Contract { void save(int n); }
abstract class Base implements Contract { }
class First extends Base { public void save(int n) {} }
class Second implements Contract { public void save(int n) {} }
class Box<T> { T get() { return null; } }
class All {
  Contract injected; Box<First> box;
  void run(Second local) { injected.save(1); local.save(2); box.get().save(3); Runnable r = this::runAgain; }
  void runAgain() {}
}"#,
    );
    let store = index(root);
    let sites = calls(&store);
    let targets: Vec<_> = sites.iter().filter_map(|s| s["target"].as_str()).collect();
    for expected in [
        "Contract.save(int)",
        "Second.save(int)",
        "First.save(int)",
        "Box.get()",
        "All.runAgain()",
    ] {
        assert!(
            targets.iter().any(|t| t.ends_with(expected)),
            "missing {expected}: {sites:#?}"
        );
    }
    let edges = store.list_all_edges().unwrap();
    let method_reference = edges
        .iter()
        .find(|e| {
            e.indexer.as_deref() == Some("java_semantics")
                && e.to_id.as_str().ends_with("All.runAgain()")
        })
        .unwrap();
    assert_eq!(method_reference.kind.as_str(), "references");
    let implementations: Vec<_> = edges
        .iter()
        .filter(|e| {
            e.kind.as_str() == "declares_implementation"
                && e.from_id.as_str().ends_with("Contract.save(int)")
        })
        .collect();
    assert_eq!(implementations.len(), 2, "{implementations:#?}");
    assert!(implementations
        .iter()
        .all(|e| e.certainty.as_str() == "candidate"));
}

#[test]
fn mapper_namespaces_do_not_cross_link_identical_simple_names() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(
        root,
        "src/a/Mapper.java",
        "package a; interface Mapper { void save(int x); }",
    );
    write(
        root,
        "src/b/Mapper.java",
        "package b; interface Mapper { void save(int x); }",
    );
    write(
        root,
        "src/mapper.xml",
        r#"<mapper namespace="a.Mapper"><insert id="save">INSERT INTO orders(id) VALUES(1)</insert></mapper>"#,
    );
    let store = index(root);
    let links: Vec<_> = store
        .list_all_edges()
        .unwrap()
        .into_iter()
        .filter(|e| e.to_id.as_str().starts_with("sql_mapper::"))
        .collect();
    assert_eq!(links.len(), 1, "{links:#?}");
    assert!(links[0].from_id.as_str().contains("src/a/Mapper.java"));
}

#[test]
fn source_sets_allow_declared_dependencies_but_not_unrelated_modules() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root,"java_semantics:\n  source_sets:\n    app:\n      roots: [src/app]\n      dependencies: [shared]\n    shared:\n      roots: [src/shared]\n    other:\n      roots: [src/other]\n");
    write(
        root,
        "src/shared/p/Shared.java",
        "package p; public class Shared { public static void yes() {} }",
    );
    write(
        root,
        "src/other/p/Other.java",
        "package p; public class Other { public static void no() {} }",
    );
    write(
        root,
        "src/app/p/App.java",
        "package p; class App { void run() { Shared.yes(); Other.no(); } }",
    );
    let sites = calls(&index(root));
    assert_eq!(
        sites
            .iter()
            .find(|c| c["expression"] == "Shared.yes()")
            .unwrap()["resolution"],
        "resolved"
    );
    assert_eq!(
        sites
            .iter()
            .find(|c| c["expression"] == "Other.no()")
            .unwrap()["resolution"],
        "unresolved"
    );
}

#[test]
fn utf8_junit_initializers_and_varargs_are_preserved_in_exports() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/p/Test.java", "package p; @interface Test {}");
    write(
        root,
        "src/p/App.java",
        r#"package p; class App { String 文 = "😀"; Object field = make(); Object make() { return null; } @Test void verify() { send("x","y"); } void send(String... values) {} }"#,
    );
    let store = index(root);
    let sites = calls(&store);
    assert_eq!(sites.len(), 2, "{sites:#?}");
    assert!(
        sites.iter().all(|s| s["resolution"] == "resolved"),
        "{sites:#?}"
    );
    assert!(sites
        .iter()
        .any(|s| s["caller"].as_str().unwrap().ends_with("App.verify()")));
    let pack = groundgraph_engine::feature_pack::build_feature_pack_with_store(
        &store,
        &groundgraph_engine::feature_pack::FeaturePackOptions {
            repo_root: root.into(),
            selector: groundgraph_engine::feature_pack::FeaturePackSelector::Path("src".into()),
            max_evidence_per_symbol: 10,
        },
    )
    .unwrap();
    assert_eq!(pack.java_analysis.calls.len(), 2);
    let network = groundgraph_engine::network::build_network_graph(
        groundgraph_engine::network::NetworkOptions {
            repo_root: root.into(),
            keep_isolated: true,
        },
    )
    .unwrap();
    assert_eq!(network.java_analysis.calls.len(), 2);
}

#[test]
fn wildcard_import_does_not_create_an_instance_receiver_and_partial_scip_keeps_calls() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(
        root,
        "src/q/Contract.java",
        "package q; public interface Contract { void save(); }",
    );
    write(
        root,
        "src/p/App.java",
        "package p; import q.*; class App { void run() { save(); run(); } }",
    );
    let mut store = index(root);
    let sites = calls(&store);
    assert_eq!(
        sites.iter().find(|s| s["expression"] == "save()").unwrap()["resolution"],
        "unresolved"
    );
    let before = store
        .list_all_edges()
        .unwrap()
        .into_iter()
        .filter(|e| e.indexer.as_deref() == Some("java_semantics"))
        .count();
    assert_eq!(before, 1);
    store
        .delete_precision_edges_for_files_except(&["src/p/App.java".into()], "scip")
        .unwrap();
    assert_eq!(
        store
            .list_all_edges()
            .unwrap()
            .into_iter()
            .filter(|e| e.indexer.as_deref() == Some("java_semantics"))
            .count(),
        before
    );
}
