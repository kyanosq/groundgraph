use groundgraph_core::{EdgeCertainty, EdgeStatus};
use groundgraph_engine::index::{index_repository, IndexOptions};
use groundgraph_store::Store;
use serde_json::Value;
use std::path::Path;

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}
fn fixture(extra: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    write(root,".groundgraph.yaml", "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .groundgraph/graph.db\nlanguages:\n  - id: java\n    paths: [src]\nenrichment:\n  scip: false\n  analyzer: false\n  lsp: false\njava_semantics:\n  enabled: true\n  dispatch_contracts:\n    - name: synthetic_bus\n      dispatcher_methods: [demo.Bus.invoke]\n      registration_methods: [demo.Factory.command]\n      handler_type: demo.Handler\n      registration_annotation: demo.Register\n");
    write(root,"src/demo/Types.java", "package demo; interface Handler {} @interface Register {} class Factory { static Handler command(String id, Runnable run) {return null;} } class Bus { void invoke(String id) {} } class Fake { void invoke(String id) {} } class Keys { static final String ID = \"stock.\" + \"receive@1\"; }");
    write(root,"src/demo/Config.java", "package demo; class Config { @Register Handler stock() { return Factory.command(Keys.ID, () -> {}); } }");
    write(root,"src/demo/Caller.java", &format!("package demo; class Caller {{ Bus bus; Fake fake; void run(String dynamic) {{ bus.invoke(Keys.ID); bus.invoke(Keys.ID); bus.invoke(dynamic); fake.invoke(Keys.ID); }} }} {extra}"));
    d
}
fn index(root: &Path) -> Store {
    index_repository(IndexOptions::all(root)).unwrap();
    Store::open(root.join(".groundgraph/graph.db")).unwrap()
}
fn dispatch_edges(store: &Store) -> Vec<groundgraph_core::EdgeAssertion> {
    store
        .list_all_edges()
        .unwrap()
        .into_iter()
        .filter(|e| e.indexer.as_deref() == Some("java_dispatch"))
        .collect()
}
fn diagnostics(store: &Store) -> String {
    store
        .list_all_nodes()
        .unwrap()
        .iter()
        .filter_map(|n| n.metadata_json.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}
#[test]
fn constant_ids_link_to_registration_as_candidates_without_fake_receivers() {
    let d = fixture("");
    let s = index(d.path());
    let e = dispatch_edges(&s);
    assert_eq!(
        e.len(),
        2,
        "one candidate per repeated real bus site; dynamic and fake receiver must not link"
    );
    assert_ne!(e[0].id, e[1].id);
    for edge in e {
        assert_eq!(edge.certainty, EdgeCertainty::Candidate);
        assert_eq!(edge.status, EdgeStatus::Proposed);
        assert!(edge.to_id.as_str().contains("Config.stock"));
        let meta: Value = serde_json::from_str(edge.evidence_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta["capability_id"], "stock.receive@1");
        assert_eq!(meta["runtime_binding_not_resolved"], true);
        assert!(meta["registration"]["path"].is_string());
    }
    assert!(diagnostics(&s).contains("dynamic_dispatch_id"));
}
#[test]
fn duplicate_registrations_are_ambiguous_not_arbitrarily_selected() {
    let d = fixture(
        "class Other { @Register Handler other() { return Factory.command(Keys.ID, () -> {}); } }",
    );
    let s = index(d.path());
    let e = dispatch_edges(&s);
    assert_eq!(e.len(), 4);
    for edge in e {
        let meta: Value = serde_json::from_str(edge.evidence_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta["registration_count"], 2);
    }
    assert!(diagnostics(&s).contains("ambiguous_dispatch_registration"));
}
#[test]
fn unregistered_ids_do_not_create_a_runtime_target_and_reindex_removes_old_edges() {
    let d = fixture("");
    let s = index(d.path());
    assert_eq!(dispatch_edges(&s).len(), 2);
    drop(s);
    write(d.path(),"src/demo/Config.java","package demo; class Config { Handler stock() { return Factory.command(Keys.ID, () -> {}); } }");
    let s = index(d.path());
    assert!(dispatch_edges(&s).is_empty());
    assert!(diagnostics(&s).contains("dispatch_registration_missing"));
}

#[test]
fn a_factory_call_not_returned_as_handler_is_not_a_registration() {
    let d = fixture("");
    write(d.path(),"src/demo/Config.java","package demo; class Config { @Register Handler stock() { Factory.command(Keys.ID, () -> {}); return null; } }");
    let s = index(d.path());
    assert!(dispatch_edges(&s).is_empty());
    assert!(diagnostics(&s).contains("dispatch_registration_missing"));
}
#[test]
fn dynamic_registration_is_reported_and_not_invented() {
    let d = fixture("");
    write(d.path(),"src/demo/Config.java","package demo; class Config { @Register Handler stock(String id) { return Factory.command(id, () -> {}); } }");
    let s = index(d.path());
    assert!(dispatch_edges(&s).is_empty());
    assert!(diagnostics(&s).contains("dynamic_registration_id"));
}
#[test]
fn without_opt_in_contracts_no_dispatch_edges_are_projected() {
    let d = fixture("");
    let path = d.path().join(".groundgraph.yaml");
    let cfg = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, cfg.split("  dispatch_contracts:").next().unwrap()).unwrap();
    let s = index(d.path());
    assert!(dispatch_edges(&s).is_empty());
}
