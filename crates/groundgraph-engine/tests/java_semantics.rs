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
    assert!(store
        .list_nodes_by_kind(NodeKind::ExternalEffect)
        .unwrap()
        .iter()
        .any(|n| n
            .metadata_json
            .as_deref()
            .unwrap_or("")
            .contains("\"client\":\"Feign\"")));
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
fn broken_argument_keeps_unique_interface_dispatch_as_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(
        root,
        "src/shop/Port.java",
        "package shop; interface Port { void send(long id); }",
    );
    write(
        root,
        "src/shop/RealPort.java",
        "package shop; class RealPort implements Port { public void send(long id) {} }",
    );
    write(
        root,
        "src/shop/Other.java",
        "package shop; interface Other { void send(long id); }",
    );
    write(
        root,
        "src/shop/Order.java",
        "package shop; class Order { /* generated getter absent */ }",
    );
    write(root, "src/shop/Action.java", "package shop; class Action { Port port; void run(Order order) { port.send(order.getId()); } }");
    let store = index(root);
    let edges = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::Calls)
        .unwrap();
    let edge = edges
        .iter()
        .find(|e| {
            e.from_id.as_str().ends_with("Action.run(Order)")
                && e.to_id.as_str().ends_with("Port.send(long)")
        })
        .expect("candidate edge to exact field type method");
    assert_eq!(edge.certainty.as_str(), "candidate");
    assert!(!edges
        .iter()
        .any(|e| e.from_id.as_str().ends_with("Action.run(Order)")
            && e.to_id.as_str().ends_with("Other.send(long)")));
    assert!(store
        .list_edges_by_kind(groundgraph_core::EdgeKind::DeclaresImplementation)
        .unwrap()
        .iter()
        .any(|e| e.from_id.as_str().ends_with("Port.send(long)")
            && e.to_id.as_str().ends_with("RealPort.send(long)")));
}

#[test]
fn broken_argument_keeps_private_same_class_call_as_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/shop/Order.java", "package shop; class Order {}");
    write(root, "src/shop/Action.java", "package shop; class Action { void run(Order order) { finish(order.missing()); } private void finish(long id) {} }");
    let store = index(root);
    let calls = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::Calls)
        .unwrap();
    let link = calls
        .iter()
        .find(|e| {
            e.from_id.as_str().ends_with("Action.run(Order)")
                && e.to_id.as_str().ends_with("Action.finish(long)")
        })
        .expect("private call retained");
    assert_eq!(link.certainty.as_str(), "candidate");
}

#[test]
fn jdbc_update_in_private_method_reaches_table_but_unused_sql_does_not() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/com/baomidou/mybatisplus/annotation/TableName.java", "package com.baomidou.mybatisplus.annotation; public @interface TableName { String value(); }");
    write(root, "src/shop/Order.java", "package shop;\nimport com.baomidou.mybatisplus.annotation.TableName;\n@TableName(\"t_order\")\nclass Order { long id; }");
    write(
        root,
        "src/shop/Jdbc.java",
        "package shop; class Jdbc { int update(String sql) { return 1; } }",
    );
    write(root, "src/shop/Review.java", "package shop; class Review { Jdbc jdbc; void audit() { sync(); } private void sync() { String sql = \"UPDATE t_order SET id = 1\"; jdbc.update(sql); } void unused() { String sql = \"UPDATE t_order SET id = 2\"; } }");
    let store = index(root);
    let edges = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::PersistsTo)
        .unwrap();
    assert!(
        edges
            .iter()
            .any(|e| e.from_id.as_str().ends_with("Review.sync()")
                && e.to_id.as_str().ends_with("::t_order")),
        "edges={edges:#?} tables={:#?} methods={:#?}",
        store.list_nodes_by_kind(NodeKind::DbTable).unwrap(),
        store
            .list_nodes_by_kind(NodeKind::JavaMethod)
            .unwrap()
            .iter()
            .map(|n| (&n.id, n.start_line, n.end_line))
            .collect::<Vec<_>>()
    );
    assert!(!edges
        .iter()
        .any(|e| e.from_id.as_str().ends_with("Review.unused()")));
    let write = edges
        .iter()
        .find(|e| e.from_id.as_str().ends_with("Review.sync()"))
        .unwrap();
    let metadata: Value = serde_json::from_str(write.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["operation"], "update");
}

#[test]
fn unresolved_wrapper_update_keeps_entity_and_update_operation_as_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/com/baomidou/mybatisplus/annotation/TableName.java", "package com.baomidou.mybatisplus.annotation; public @interface TableName { String value(); }");
    write(root, "src/com/baomidou/mybatisplus/core/mapper/BaseMapper.java", "package com.baomidou.mybatisplus.core.mapper; public interface BaseMapper<T> { int update(Object wrapper); }");
    write(root, "src/shop/Order.java", "package shop;\nimport com.baomidou.mybatisplus.annotation.TableName;\n@TableName(\"t_order\")\nclass Order { long id; }");
    write(root, "src/shop/Stock.java", "package shop;\nimport com.baomidou.mybatisplus.annotation.TableName;\n@TableName(\"t_stock\")\nclass Stock { long id; }");
    write(root, "src/shop/OrderMapper.java", "package shop; interface OrderMapper extends com.baomidou.mybatisplus.core.mapper.BaseMapper<Order> {}");
    write(root, "src/shop/Action.java", "package shop; class Action { OrderMapper mapper; void run() { mapper.update(new Object().missing()); } }");
    let store = index(root);
    let edges = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::PersistsTo)
        .unwrap();
    let edge = edges
        .iter()
        .find(|e| {
            e.from_id.as_str().ends_with("Action.run()") && e.to_id.as_str().ends_with("::t_order")
        })
        .expect("candidate update reaches declared entity");
    assert_eq!(edge.certainty.as_str(), "candidate");
    let meta: Value = serde_json::from_str(edge.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta["operation"], "update");
    assert!(!edges
        .iter()
        .any(|e| e.from_id.as_str().ends_with("Action.run()")
            && e.to_id.as_str().ends_with("::t_stock")));
}

#[test]
fn wrapper_builder_is_not_a_write_but_ambiguous_mapper_update_is_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/com/baomidou/mybatisplus/annotation/TableName.java", "package com.baomidou.mybatisplus.annotation; public @interface TableName { String value(); }");
    write(root, "src/com/baomidou/mybatisplus/core/mapper/BaseMapper.java", "package com.baomidou.mybatisplus.core.mapper; public interface BaseMapper<T> { int update(Object wrapper); int update(String wrapper); }");
    write(root, "src/com/baomidou/mybatisplus/core/conditions/update/UpdateWrapper.java", "package com.baomidou.mybatisplus.core.conditions.update; public class UpdateWrapper<T> { public UpdateWrapper<T> setSql(String sql) { return this; } }");
    write(root, "src/shop/Order.java", "package shop;\nimport com.baomidou.mybatisplus.annotation.TableName;\n@TableName(\"t_order\")\nclass Order { long id; }");
    write(root, "src/shop/OrderMapper.java", "package shop; interface OrderMapper extends com.baomidou.mybatisplus.core.mapper.BaseMapper<Order> {}");
    write(root, "src/shop/Action.java", "package shop; class Action { OrderMapper mapper; void run() { mapper.update(new com.baomidou.mybatisplus.core.conditions.update.UpdateWrapper<Order>().setSql(\"id=1\").broken()); } }");
    let store = index(root);
    let writes: Vec<_> = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::PersistsTo)
        .unwrap()
        .into_iter()
        .filter(|e| e.from_id.as_str().ends_with("Action.run()"))
        .collect();
    assert_eq!(writes.len(), 1, "{writes:#?}");
    assert_eq!(
        serde_json::from_str::<Value>(writes[0].metadata_json.as_deref().unwrap()).unwrap()
            ["operation"],
        "update"
    );
    assert_eq!(writes[0].certainty.as_str(), "candidate");
}

#[test]
fn mapper_update_with_unavailable_overload_remains_a_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/com/baomidou/mybatisplus/annotation/TableName.java", "package com.baomidou.mybatisplus.annotation; public @interface TableName { String value(); }");
    write(root, "src/com/baomidou/mybatisplus/core/mapper/BaseMapper.java", "package com.baomidou.mybatisplus.core.mapper; public interface BaseMapper<T> { int update(T entity, Object wrapper); }");
    write(root, "src/shop/Order.java", "package shop;\nimport com.baomidou.mybatisplus.annotation.TableName;\n@TableName(\"t_order\")\nclass Order { long id; }");
    write(root, "src/shop/OrderMapper.java", "package shop; interface OrderMapper extends com.baomidou.mybatisplus.core.mapper.BaseMapper<Order> {}");
    write(root, "src/shop/Action.java", "package shop; class Action { OrderMapper mapper; void run() { mapper.update(new Object()); } }");
    let store = index(root);
    let source = store
        .list_nodes_by_kind(NodeKind::File)
        .unwrap()
        .into_iter()
        .find(|n| n.id.as_str().ends_with("Action.java"))
        .unwrap();
    let source: Value = serde_json::from_str(source.metadata_json.as_deref().unwrap()).unwrap();
    let update = source["java_analysis"]["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            c["expression"]
                .as_str()
                .unwrap_or("")
                .starts_with("mapper.update")
        })
        .unwrap();
    assert_eq!(update["target"], Value::Null);
    assert_eq!(
        update["external_target"],
        "com.baomidou.mybatisplus.core.mapper.BaseMapper.update(?)"
    );
    assert_eq!(
        update["reason"],
        "framework_crud_overload_missing_from_classpath"
    );
    let writes: Vec<_> = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::PersistsTo)
        .unwrap()
        .into_iter()
        .filter(|e| e.from_id.as_str().ends_with("Action.run()"))
        .collect();
    assert_eq!(writes.len(), 1, "{writes:#?}");
    assert_eq!(writes[0].certainty.as_str(), "candidate");
    assert_eq!(
        serde_json::from_str::<Value>(writes[0].metadata_json.as_deref().unwrap()).unwrap()
            ["operation"],
        "update"
    );
}

#[test]
fn external_client_calls_become_effect_nodes_without_matching_unrelated_types() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/org/springframework/web/client/RestTemplate.java", "package org.springframework.web.client; public class RestTemplate { public Object postForEntity(String url, Object body, Class<?> type) { return null; } }");
    write(root, "src/org/springframework/context/ApplicationEventPublisher.java", "package org.springframework.context; public interface ApplicationEventPublisher { void publishEvent(Object event); }");
    write(root, "src/shop/RestTemplate.java", "package shop; class RestTemplate { void postForEntity(String url, Object body, Class<?> type) {} }");
    write(root, "src/shop/Action.java", "package shop; class Action { org.springframework.web.client.RestTemplate http; org.springframework.context.ApplicationEventPublisher events; RestTemplate local; void run() { http.postForEntity(\"/orders\", null, Object.class); events.publishEvent(new Object()); local.postForEntity(\"local\", null, Object.class); } }");
    let store = index(root);
    let effects = store.list_nodes_by_kind(NodeKind::ExternalEffect).unwrap();
    assert_eq!(effects.len(), 2, "{effects:#?}");
    let edges = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::Calls)
        .unwrap();
    assert!(effects.iter().all(|n| edges.iter().any(|e| e
        .from_id
        .as_str()
        .ends_with("Action.run()")
        && e.to_id == n.id)));
    assert!(effects.iter().any(|n| n
        .metadata_json
        .as_deref()
        .unwrap_or("")
        .contains("\"effect\":\"http\"")));
    assert!(effects.iter().any(|n| n
        .metadata_json
        .as_deref()
        .unwrap_or("")
        .contains("\"effect\":\"event\"")));
}

#[test]
fn transaction_annotations_include_class_level_propagation_and_async_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/org/springframework/transaction/annotation/Propagation.java", "package org.springframework.transaction.annotation; public enum Propagation { REQUIRED, REQUIRES_NEW }");
    write(root, "src/org/springframework/transaction/annotation/Transactional.java", "package org.springframework.transaction.annotation; public @interface Transactional { Propagation propagation() default Propagation.REQUIRED; Class<?>[] noRollbackFor() default {}; }");
    write(
        root,
        "src/org/springframework/scheduling/annotation/Async.java",
        "package org.springframework.scheduling.annotation; public @interface Async {}",
    );
    write(root, "src/shop/Service.java", "package shop; import org.springframework.transaction.annotation.*; import org.springframework.scheduling.annotation.Async; @Transactional class Service { void outer() { inner(); } @Transactional(propagation=Propagation.REQUIRES_NEW, noRollbackFor=IllegalArgumentException.class) void inner() {} @Async void later() {} }");
    let store = index(root);
    let methods = store.list_nodes_by_kind(NodeKind::JavaMethod).unwrap();
    let framework = |name: &str| {
        let n = methods
            .iter()
            .find(|n| n.id.as_str().ends_with(name))
            .unwrap();
        let v: Value = serde_json::from_str(n.metadata_json.as_deref().unwrap()).unwrap();
        v["java"]["framework"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    assert!(framework("Service.outer()")
        .iter()
        .any(|v| v["role"] == "transaction" && v["class_level"] == true));
    assert!(framework("Service.inner()")
        .iter()
        .any(|v| v["role"] == "transaction"
            && v["args"].as_str().unwrap_or("").contains("REQUIRES_NEW")));
    assert!(framework("Service.later()")
        .iter()
        .any(|v| v["role"] == "async"));
}

#[test]
fn self_invocation_does_not_open_transaction_but_cross_bean_call_does() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/org/springframework/transaction/annotation/Propagation.java", "package org.springframework.transaction.annotation; public enum Propagation { REQUIRED, REQUIRES_NEW }");
    write(root, "src/org/springframework/transaction/annotation/Transactional.java", "package org.springframework.transaction.annotation; public @interface Transactional { Propagation propagation() default Propagation.REQUIRED; }");
    write(root, "src/shop/Other.java", "package shop; import org.springframework.transaction.annotation.*; class Other { @Transactional(propagation=Propagation.REQUIRES_NEW) void write() {} }");
    write(root, "src/shop/Action.java", "package shop; import org.springframework.transaction.annotation.*; class Action { Other other; void run() { inner(); other.write(); } @Transactional void inner() {} }");
    let _store = index(root);
    let trace = groundgraph_engine::trace::run_trace(groundgraph_engine::trace::TraceOptions::new(
        root,
        "Action.run",
    ))
    .unwrap();
    let tx = groundgraph_engine::effects::analyze_transactions(&trace);
    assert!(
        tx.groups
            .iter()
            .any(|g| g.owner.ends_with("Other.write()") && g.propagation == "REQUIRES_NEW"),
        "{tx:#?}"
    );
    assert!(
        !tx.groups
            .iter()
            .any(|g| g.owner.ends_with("Action.inner()")),
        "{tx:#?}"
    );
}

#[test]
fn nested_transactions_and_external_risks_keep_propagation_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/org/springframework/transaction/annotation/Propagation.java", "package org.springframework.transaction.annotation; public enum Propagation { REQUIRED, REQUIRES_NEW }");
    write(root, "src/org/springframework/transaction/annotation/Transactional.java", "package org.springframework.transaction.annotation; public @interface Transactional { Propagation propagation() default Propagation.REQUIRED; Class<?>[] noRollbackFor() default {}; }");
    write(root, "src/org/springframework/web/client/RestTemplate.java", "package org.springframework.web.client; public class RestTemplate { public Object postForEntity(String url, Object body, Class<?> type) { return null; } }");
    write(root, "src/shop/Writer.java", "package shop; import org.springframework.transaction.annotation.*; class Writer { @Transactional(propagation=Propagation.REQUIRES_NEW, noRollbackFor=IllegalArgumentException.class) void save() {} }");
    write(root, "src/shop/Action.java", "package shop; import org.springframework.transaction.annotation.*; class Action { Writer writer; org.springframework.web.client.RestTemplate http; @Transactional void run() { writer.save(); http.postForEntity(\"/orders\", null, Object.class); } void outside() { http.postForEntity(\"/outside\", null, Object.class); } }");
    let _store = index(root);
    let trace = groundgraph_engine::trace::run_trace(groundgraph_engine::trace::TraceOptions::new(
        root,
        "Action.run",
    ))
    .unwrap();
    let tx = groundgraph_engine::effects::analyze_transactions(&trace);
    assert_eq!(tx.groups.len(), 2, "{tx:#?}");
    assert!(tx.groups.iter().any(|g| g.owner.ends_with("Writer.save()")
        && g.propagation == "REQUIRES_NEW"
        && g.no_rollback_for
            .as_deref()
            .unwrap_or("")
            .contains("IllegalArgumentException")));
    assert!(tx.risks.iter().any(|r| r.kind == "external_in_transaction"));
    let outside = groundgraph_engine::trace::run_trace(
        groundgraph_engine::trace::TraceOptions::new(root, "Action.outside"),
    )
    .unwrap();
    assert!(!groundgraph_engine::effects::analyze_transactions(&outside)
        .risks
        .iter()
        .any(|r| r.kind == "external_in_transaction"));
}

#[test]
fn swallowed_external_failure_is_risk_but_rethrow_is_not() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "src/org/springframework/web/client/RestTemplate.java", "package org.springframework.web.client; public class RestTemplate { public Object postForEntity(String url, Object body, Class<?> type) { return null; } }");
    write(root, "src/shop/Action.java", "package shop; class Action { org.springframework.web.client.RestTemplate http; void swallow() { try { http.postForEntity(\"/orders\", null, Object.class); } catch (RuntimeException ex) { } } void rethrow() { try { http.postForEntity(\"/orders\", null, Object.class); } catch (RuntimeException ex) { throw ex; } } }");
    let _store = index(root);
    for (method, expected) in [("Action.swallow", true), ("Action.rethrow", false)] {
        let trace = groundgraph_engine::trace::run_trace(
            groundgraph_engine::trace::TraceOptions::new(root, method),
        )
        .unwrap();
        let tx = groundgraph_engine::effects::analyze_transactions(&trace);
        assert_eq!(
            tx.risks
                .iter()
                .any(|r| r.kind == "swallowed_external_failure"),
            expected,
            "{method}: {tx:#?}"
        );
    }
}

#[test]
fn async_and_transactional_event_listener_are_visible_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(
        root,
        "src/org/springframework/scheduling/annotation/Async.java",
        "package org.springframework.scheduling.annotation; public @interface Async {}",
    );
    write(root, "src/org/springframework/transaction/event/TransactionalEventListener.java", "package org.springframework.transaction.event; public @interface TransactionalEventListener {}");
    write(root, "src/shop/Listener.java", "package shop; import org.springframework.transaction.event.TransactionalEventListener; class Listener { @TransactionalEventListener void receive() {} }");
    write(root, "src/shop/Worker.java", "package shop; import org.springframework.scheduling.annotation.Async; class Worker { @Async void work() {} }");
    write(
        root,
        "src/shop/Action.java",
        "package shop; class Action { Worker worker; void run() { worker.work(); } }",
    );
    let _store = index(root);
    let trace = groundgraph_engine::trace::run_trace(groundgraph_engine::trace::TraceOptions::new(
        root,
        "Action.run",
    ))
    .unwrap();
    assert!(groundgraph_engine::effects::analyze_transactions(&trace)
        .risks
        .iter()
        .any(|r| r.kind == "async_boundary"));
    let listener = groundgraph_engine::trace::run_trace(
        groundgraph_engine::trace::TraceOptions::new(root, "Listener.receive"),
    )
    .unwrap();
    assert!(groundgraph_engine::effects::analyze_transactions(&listener)
        .risks
        .iter()
        .any(|r| r.kind == "transactional_event_listener"));
}

#[test]
fn effects_card_and_writers_use_write_edges_and_show_unresolved_calls() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(root, "schema.sql", "CREATE TABLE t_stock (id BIGINT);");
    write(
        root,
        "src/org/springframework/scheduling/annotation/Scheduled.java",
        "package org.springframework.scheduling.annotation; public @interface Scheduled {}",
    );
    write(
        root,
        "src/org/springframework/kafka/annotation/KafkaListener.java",
        "package org.springframework.kafka.annotation; public @interface KafkaListener {}",
    );
    write(
        root,
        "src/shop/StockMapper.java",
        "package shop; interface StockMapper { void write(); void read(); }",
    );
    write(root, "src/shop/StockMapper.xml", "<mapper namespace=\"shop.StockMapper\"><update id=\"write\">UPDATE t_stock SET id=1</update><select id=\"read\">SELECT * FROM t_stock</select></mapper>");
    write(root, "src/shop/First.java", "package shop; class First { StockMapper mapper; void run() { mapper.write(); missing.call(); } }");
    write(root, "src/shop/Second.java", "package shop; import org.springframework.scheduling.annotation.Scheduled; class Second { StockMapper mapper; @Scheduled void tick() { mapper.write(); } void onlyRead() { mapper.read(); } }");
    write(root, "src/shop/Consumer.java", "package shop; import org.springframework.kafka.annotation.KafkaListener; class Consumer { StockMapper mapper; @KafkaListener void onMessage() { mapper.write(); } }");
    let _store = index(root);
    let card = groundgraph_engine::effects::run_effects(
        groundgraph_engine::trace::TraceOptions::new(root, "First.run"),
    )
    .unwrap();
    assert!(card
        .tables
        .iter()
        .any(|e| e.table == "t_stock" && e.operation == "update"));
    assert!(
        card.unresolved_count >= 1
            && card
                .breakpoints
                .iter()
                .any(|b| b.expression.contains("missing.call"))
    );
    let writers = groundgraph_engine::effects::run_writers(root, "t_stock").unwrap();
    assert!(writers
        .entries
        .iter()
        .any(|e| e.id.ends_with("First.run()")));
    assert!(writers
        .entries
        .iter()
        .any(|e| e.id.ends_with("Second.tick()") && e.kind == "scheduled"));
    assert!(writers
        .entries
        .iter()
        .any(|e| e.id.ends_with("Consumer.onMessage()") && e.kind == "MQ"));
    assert!(!writers
        .entries
        .iter()
        .any(|e| e.id.ends_with("Second.onlyRead()")));
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

#[test]
fn framework_annotations_keep_exact_methods_dynamic_gaps_and_entrypoints() {
    use groundgraph_core::EdgeKind;
    use groundgraph_engine::dead_code::{analyze_dead_code, DeadCodeOptions};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "");
    write(
        root,
        "src/p/Mapper.java",
        r#"package p;
import org.apache.ibatis.annotations.Select;
import org.apache.ibatis.annotations.SelectProvider;
interface Mapper {
  String TABLE = "orders";
  @Select({"select *", "from " + TABLE, "where id = #{id}"}) Object find(int id);
  @Select("select * from archived_orders") Object find(String id);
  @Select("select * from ${table}") Object dynamic();
  @SelectProvider(type=Object.class, method="sql") Object provider();
}
"#,
    );
    write(
        root,
        "src/p/Jobs.java",
        r#"package p;
import org.springframework.scheduling.annotation.Scheduled;
import org.springframework.context.event.EventListener;
class Jobs {
  @Scheduled(fixedRate=1000) void tick() {}
  @EventListener void event(Object event) {}
}
"#,
    );
    write(
        root,
        "src/q/Custom.java",
        r#"package q;
@interface Select { String value(); }
@interface Scheduled {}
class Custom {
  @Select("select * from fake_table") Object find() { return null; }
  @Scheduled void custom() {}
}
"#,
    );
    write(root, "src/w/Wild.java", "package w; import org.apache.ibatis.annotations.*; interface Wild { @Select(\"select * from uncertain_table\") Object find(); }");
    write(
        root,
        "src/Local.java",
        r#"@interface Select { String value(); } class Local { @Select("select * from fake") void local() {} }"#,
    );
    let store = index(root);
    let stmts = store.list_nodes_by_kind(NodeKind::SqlMapperStmt).unwrap();
    assert_eq!(stmts.len(), 2, "{stmts:#?}");
    let edges = store.list_edges_by_kind(EdgeKind::References).unwrap();
    for (signature, table) in [
        ("Mapper.find(int)", "orders"),
        ("Mapper.find(String)", "archived_orders"),
    ] {
        let edge = edges
            .iter()
            .find(|e| {
                e.from_id.as_str().ends_with(signature) && stmts.iter().any(|s| s.id == e.to_id)
            })
            .unwrap();
        assert_eq!(edge.certainty, groundgraph_core::EdgeCertainty::Candidate);
        assert!(edge
            .evidence_json
            .as_deref()
            .unwrap()
            .contains("annotation_start"));
        let stmt = stmts.iter().find(|s| s.id == edge.to_id).unwrap();
        let meta: Value = serde_json::from_str(stmt.metadata_json.as_deref().unwrap()).unwrap();
        assert!(meta["sql"].as_str().unwrap().contains(table));
        assert!(store
            .list_edges_by_kind(EdgeKind::PersistsTo)
            .unwrap()
            .iter()
            .any(|e| e.from_id == stmt.id && e.to_id.as_str().ends_with(table)));
    }
    let methods = store.list_nodes_by_kind(NodeKind::JavaMethod).unwrap();
    let local = methods
        .iter()
        .find(|m| m.id.as_str().ends_with("Local.local()"))
        .unwrap();
    let local: Value = serde_json::from_str(local.metadata_json.as_deref().unwrap()).unwrap();
    assert!(local["java"]["framework"].is_null());
    for signature in ["Mapper.dynamic()", "Mapper.provider()", "Wild.find()"] {
        let method = methods
            .iter()
            .find(|m| m.id.as_str().ends_with(signature))
            .unwrap();
        let meta: Value = serde_json::from_str(method.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta["java"]["framework"][0]["resolution"], "unresolved");
    }
    let report = analyze_dead_code(DeadCodeOptions {
        repo_root: root.into(),
        ..Default::default()
    })
    .unwrap();
    for name in ["Jobs.tick()", "Jobs.event(Object)"] {
        assert!(
            !report.candidates.iter().any(|c| c.id.ends_with(name)),
            "{report:#?}"
        );
    }
    assert!(report
        .candidates
        .iter()
        .any(|c| c.id.ends_with("Custom.custom()")));
    let facts = groundgraph_engine::symbol_facts::analyze_symbol_facts(
        groundgraph_engine::symbol_facts::SymbolFactsOptions {
            repo_root: root.into(),
            ..Default::default()
        },
    )
    .unwrap();
    let job = facts
        .facts
        .iter()
        .find(|f| f.id.ends_with("Jobs.tick()"))
        .unwrap();
    let metadata: Value = serde_json::from_str(job.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["java"]["framework"][0]["role"], "entrypoint");
    let graph = groundgraph_engine::network::network_from_graph(
        "test",
        &store.list_all_nodes().unwrap(),
        &[],
        true,
    );
    let graph = serde_json::to_value(graph).unwrap();
    let job = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"].as_str().unwrap().ends_with("Jobs.tick()"))
        .unwrap();
    assert!(job["metadata_json"]
        .as_str()
        .is_some_and(|s| s.contains("entrypoint")));
    let trace = groundgraph_engine::trace::run_trace_with_store(
        &store,
        groundgraph_engine::trace::TraceOptions::new(root, "Jobs.tick"),
    )
    .unwrap();
    let trace = serde_json::to_value(trace).unwrap();
    assert!(trace["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["metadata_json"]
            .as_str()
            .is_some_and(|s| s.contains("entrypoint"))));
    drop(store);
    write(
        root,
        "src/p/Mapper.java",
        "package p; interface Mapper { Object find(int id); }",
    );
    write(
        root,
        "src/p/Jobs.java",
        "package p; class Jobs { void tick() {} }",
    );
    let store = index(root);
    assert!(store
        .list_nodes_by_kind(NodeKind::SqlMapperStmt)
        .unwrap()
        .is_empty());
    let report = analyze_dead_code(DeadCodeOptions {
        repo_root: root.into(),
        ..Default::default()
    })
    .unwrap();
    assert!(report
        .candidates
        .iter()
        .any(|c| c.id.ends_with("Jobs.tick()")));
}

fn mybatis_plus_stubs(root: &Path) {
    write(root, "src/com/baomidou/mybatisplus/annotation/TableName.java", "package com.baomidou.mybatisplus.annotation; public @interface TableName { String value(); }");
    write(root, "src/com/baomidou/mybatisplus/core/mapper/BaseMapper.java", "package com.baomidou.mybatisplus.core.mapper; public interface BaseMapper<T> { int insert(T e); java.util.List<T> selectList(Object w); }");
    write(root, "src/com/baomidou/mybatisplus/extension/service/IService.java", "package com.baomidou.mybatisplus.extension.service; public interface IService<T> { boolean save(T e); }");
    write(root, "src/com/baomidou/mybatisplus/extension/service/impl/ServiceImpl.java", "package com.baomidou.mybatisplus.extension.service.impl; public class ServiceImpl<M extends com.baomidou.mybatisplus.core.mapper.BaseMapper<T>, T> implements com.baomidou.mybatisplus.extension.service.IService<T> { public boolean save(T e) { return true; } }");
}

#[test]
fn mybatis_plus_inherited_crud_reaches_the_entity_table() {
    // 继承自 BaseMapper / IService 的 CRUD 没有 XML 语句，只有实体上的 @TableName；
    // 以前 trace 在这里断掉，业务方法看起来不落库。
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    config(root, "java_semantics:\n  enabled: true\n");
    mybatis_plus_stubs(root);
    write(root, "src/p/Order.java", "package p;\nimport com.baomidou.mybatisplus.annotation.TableName;\n@TableName(\"t_order\")\npublic class Order { private Long id; }");
    write(root, "src/p/Item.java", "package p;\nimport com.baomidou.mybatisplus.annotation.TableName;\n@TableName(\"t_item\")\npublic class Item { private Long id; }");
    write(root, "src/p/OrderMapper.java", "package p; public interface OrderMapper extends com.baomidou.mybatisplus.core.mapper.BaseMapper<Order> {}");
    write(root, "src/p/ItemMapper.java", "package p; public interface ItemMapper extends com.baomidou.mybatisplus.core.mapper.BaseMapper<Item> {}");
    write(root, "src/p/OrderService.java", "package p; public class OrderService extends com.baomidou.mybatisplus.extension.service.impl.ServiceImpl<OrderMapper, Order> { ItemMapper items; void create(Order o) { save(o); items.insert(new Item()); } void list() { items.selectList(null); } }");
    let store = index(root);
    let mut links: Vec<(String, String)> = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::PersistsTo)
        .unwrap()
        .into_iter()
        .filter(|e| e.from_id.as_str().contains("OrderService"))
        .map(|e| {
            let from = e.from_id.as_str().rsplit("::").next().unwrap().to_string();
            (from, e.to_id.as_str().to_string())
        })
        .collect();
    links.sort();
    assert_eq!(
        links,
        vec![
            (
                "OrderService.create(Order)".into(),
                "db_table::src/p/Item.java::t_item".into()
            ),
            (
                "OrderService.create(Order)".into(),
                "db_table::src/p/Order.java::t_order".into()
            ),
            (
                "OrderService.list()".into(),
                "db_table::src/p/Item.java::t_item".into()
            ),
        ]
    );
    let operations: Vec<_> = store
        .list_edges_by_kind(groundgraph_core::EdgeKind::PersistsTo)
        .unwrap()
        .into_iter()
        .filter(|e| e.from_id.as_str().contains("OrderService"))
        .map(|e| {
            let meta: Value =
                serde_json::from_str(e.metadata_json.as_deref().unwrap_or("{}")).unwrap();
            (
                e.from_id.to_string(),
                e.to_id.to_string(),
                meta["operation"].clone(),
            )
        })
        .collect();
    assert!(
        operations.iter().any(
            |(from, to, op)| from.ends_with("OrderService.create(Order)")
                && to.ends_with("::t_item")
                && op == "insert"
        ),
        "{operations:?}"
    );
    assert!(
        operations
            .iter()
            .any(|(from, to, op)| from.ends_with("OrderService.list()")
                && to.ends_with("::t_item")
                && op == "read"),
        "{operations:?}"
    );
}

#[test]
fn configured_annotation_processors_run_without_writing_into_the_repo() {
    // Lombok 这类处理器补出的成员不跑处理器就解析不了，还会把外层调用一起拖成错误；
    // 只跑显式配置的处理器，生成物不得落进仓库。
    let temp = tempfile::tempdir().unwrap();
    let proc_dir = temp.path().join("proc");
    write(
        &proc_dir,
        "src/gen/Proc.java",
        r#"package gen;
import javax.annotation.processing.*; import javax.lang.model.SourceVersion; import javax.lang.model.element.TypeElement; import java.util.Set;
@SupportedAnnotationTypes("p.Gen") public class Proc extends AbstractProcessor {
  boolean done;
  public SourceVersion getSupportedSourceVersion() { return SourceVersion.latestSupported(); }
  public boolean process(Set<? extends TypeElement> a, RoundEnvironment r) {
    if (done || a.isEmpty()) return false; done = true;
    try (java.io.Writer w = processingEnv.getFiler().createSourceFile("p.Generated").openWriter()) { w.write("package p; public class Generated { public static int hello() { return 1; } }"); }
    catch (java.io.IOException e) { throw new RuntimeException(e); }
    return false; } }"#,
    );
    write(
        &proc_dir,
        "classes/META-INF/services/javax.annotation.processing.Processor",
        "gen.Proc\n",
    );
    let status = std::process::Command::new("javac")
        .arg("-d")
        .arg(proc_dir.join("classes"))
        .arg(proc_dir.join("src/gen/Proc.java"))
        .status()
        .unwrap();
    assert!(status.success());
    let root = &temp.path().join("repo");
    config(
        root,
        &format!(
            "java_semantics:\n  enabled: true\n  annotation_processor_path: [{}]\n",
            proc_dir.join("classes").display()
        ),
    );
    write(
        root,
        "src/p/Gen.java",
        "package p; public @interface Gen {}",
    );
    write(
        root,
        "src/p/Caller.java",
        "package p; @Gen class Caller { Other o; void run() { o.take(Generated.hello()); } }",
    );
    write(
        root,
        "src/p/Other.java",
        "package p; class Other { void take(int x) {} }",
    );
    let store = index(root);
    let take = calls(&store)
        .into_iter()
        .find(|c| c["expression"].as_str().unwrap().starts_with("o.take"))
        .unwrap();
    assert_eq!(take["resolution"], "resolved", "{take:#}");
    let stray: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("Generated"))
        .collect();
    assert!(
        stray.is_empty(),
        "processor output leaked into the repo: {stray:?}"
    );
}
