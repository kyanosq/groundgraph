//! Spring Data JPA inherited CRUD must retain exact entity evidence and remain
//! a runtime candidate. Shared CrudRepository names are not a JPA store proof.
use groundgraph_core::{EdgeCertainty, EdgeKind, EdgeStatus, NodeKind};
use groundgraph_engine::index::{index_repository, IndexOptions};
use groundgraph_store::Store;
use serde_json::Value;
use std::path::Path;

fn write(root: &Path, path: &str, source: &str) {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, source).unwrap();
}

fn fixture(root: &Path, namespace: &str) {
    write(root, ".groundgraph.yaml", "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .groundgraph/graph.db\nlanguages:\n  - id: java\n    paths: [src]\nenrichment:\n  scip: false\n  analyzer: false\n  lsp: false\njava_semantics:\n  enabled: true\n");
    for (name, body) in [
        ("Entity", "String name() default \"\";"),
        (
            "Table",
            "String name() default \"\"; String schema() default \"\";",
        ),
    ] {
        write(
            root,
            &format!("src/{}/{name}.java", namespace.replace('.', "/")),
            &format!("package {namespace}; public @interface {name} {{ {body} }}"),
        );
    }
    write(root, "src/org/springframework/data/repository/CrudRepository.java", "package org.springframework.data.repository; public interface CrudRepository<T, ID> { <S extends T> S save(S entity); <S extends T> Iterable<S> saveAll(Iterable<S> entities); java.util.Optional<T> findById(ID id); long count(); void deleteById(ID id); }");
    write(root, "src/org/springframework/data/jpa/repository/JpaRepository.java", "package org.springframework.data.jpa.repository; public interface JpaRepository<T, ID> extends org.springframework.data.repository.CrudRepository<T, ID> { <S extends T> S saveAndFlush(S entity); void deleteAllInBatch(); T getReferenceById(ID id); void flush(); void saveCustom(T entity); }");
    write(root, "src/org/springframework/data/mongodb/repository/MongoRepository.java", "package org.springframework.data.mongodb.repository; public interface MongoRepository<T, ID> extends org.springframework.data.repository.CrudRepository<T, ID> {}");
    write(root, "src/p/Ledger.java", &format!("package p;\nimport {namespace}.Entity;\nimport {namespace}.Table;\n@Entity\n@Table(name=\"ledger_rows\")\npublic class Ledger {{ private Long id; }}\nclass Other {{}}\n"));
}

fn index(root: &Path) -> Store {
    index_repository(IndexOptions::all(root)).unwrap();
    Store::open(root.join(".groundgraph/graph.db")).unwrap()
}

#[test]
fn jpa_inherited_crud_resolved_calls_produce_candidate_read_upsert_delete_effects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, "jakarta.persistence");
    write(root, "src/p/Repo.java", "package p; interface Base<E> extends org.springframework.data.jpa.repository.JpaRepository<E, Long> {} interface Repo extends Base<Ledger> {}");
    write(root, "src/p/Service.java", "package p; class Service { Repo repo; void write(Ledger e) { repo.save(e); repo.saveAll(java.util.List.of(e)); repo.saveAndFlush(e); } void read() { repo.findById(1L); repo.count(); repo.getReferenceById(1L); } void remove() { repo.deleteById(1L); repo.deleteAllInBatch(); } void unrelated(Ledger e) { repo.flush(); repo.saveCustom(e); } }");
    let store = index(root);
    let edges: Vec<_> = store
        .list_edges_by_kind(EdgeKind::PersistsTo)
        .unwrap()
        .into_iter()
        .filter(|e| e.from_id.as_str().contains("Service."))
        .collect();
    assert_eq!(
        edges.len(),
        8,
        "inherited JPA CRUD must reach its entity table"
    );
    let mut operations = Vec::new();
    for edge in &edges {
        assert_eq!(
            edge.to_id.as_str(),
            "db_table::src/p/Ledger.java::ledger_rows"
        );
        assert_eq!(edge.certainty, EdgeCertainty::Candidate);
        assert_eq!(edge.status, EdgeStatus::Proposed);
        assert!(edge.confidence.get() > 0.0 && edge.confidence.get() < 1.0);
        let meta: Value = serde_json::from_str(edge.metadata_json.as_deref().unwrap()).unwrap();
        operations.push(meta["operation"].as_str().unwrap().to_string());
        let evidence: Value = serde_json::from_str(edge.evidence_json.as_deref().unwrap()).unwrap();
        assert_eq!(evidence["resolver"], "spring_data_jpa_inherited_crud");
        assert_eq!(evidence["compiler_resolution"], "resolved");
        assert_eq!(evidence["entity"], "p.Ledger");
        assert!(evidence["call_site"].is_string());
        assert!(evidence["line"].as_u64().unwrap() > 0);
    }
    operations.sort();
    assert_eq!(
        operations,
        ["delete", "delete", "read", "read", "read", "upsert", "upsert", "upsert"]
    );
    let writers = groundgraph_engine::effects::writers_with_store(&store, "ledger_rows").unwrap();
    assert_eq!(
        writers.write_edges, 5,
        "upsert is a possible writer, read is not"
    );
    let mut options = groundgraph_engine::trace::TraceOptions::new(root, "Service.write");
    options.max_seeds = 1;
    let trace = groundgraph_engine::trace::run_trace_with_store(&store, options).unwrap();
    let card = groundgraph_engine::effects::effects_from_trace(&trace);
    assert_eq!(card.tables.len(), 3);
    assert!(card.tables.iter().all(|t| t.certainty == "candidate"));
    assert_eq!(card.confirmed_effect_ratio, 0.0);
}

#[test]
fn jpa_shared_crud_without_jpa_receiver_and_same_file_non_entity_produce_no_edges() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, "jakarta.persistence");
    write(root, "src/p/Service.java", "package p; interface Mongo extends org.springframework.data.mongodb.repository.MongoRepository<Ledger,Long> {} interface Common extends org.springframework.data.repository.CrudRepository<Ledger,Long> {} interface NotEntity extends org.springframework.data.jpa.repository.JpaRepository<Other,Long> {} class Service { Mongo mongo; Common common; NotEntity other; void write(Ledger e, Other o) { mongo.save(e); common.save(e); other.save(o); } }");
    let store = index(root);
    assert!(store
        .list_edges_by_kind(EdgeKind::PersistsTo)
        .unwrap()
        .iter()
        .all(|e| !e.from_id.as_str().contains("Service.")));
}

#[test]
fn jpa_same_named_non_jpa_annotations_do_not_create_repository_table_effects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, "other.persistence");
    write(root, "src/p/Service.java", "package p; interface Repo extends org.springframework.data.jpa.repository.JpaRepository<Ledger,Long> {} class Service { Repo repo; void write(Ledger e) { repo.save(e); } }");
    let store = index(root);
    assert!(store
        .list_edges_by_kind(EdgeKind::PersistsTo)
        .unwrap()
        .iter()
        .all(|e| !e.from_id.as_str().contains("Service.")));
}

#[test]
fn jpa_legacy_annotations_reindex_removes_candidate_when_entity_mapping_disappears() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, "javax.persistence");
    write(root, "src/p/Service.java", "package p; interface Repo extends org.springframework.data.jpa.repository.JpaRepository<Ledger,Long> {} class Service { Repo repo; void write(Ledger e) { repo.save(e); } }");
    let store = index(root);
    assert_eq!(
        store
            .list_edges_by_kind(EdgeKind::PersistsTo)
            .unwrap()
            .len(),
        1
    );
    let before = store.list_edges_by_kind(EdgeKind::PersistsTo).unwrap();
    drop(store);
    let store = index(root);
    assert_eq!(
        store.list_edges_by_kind(EdgeKind::PersistsTo).unwrap(),
        before
    );
    drop(store);
    write(
        root,
        "src/p/Ledger.java",
        "package p; public class Ledger {} class Other {}",
    );
    let store = index(root);
    assert!(store
        .list_edges_by_kind(EdgeKind::PersistsTo)
        .unwrap()
        .is_empty());
    assert!(store
        .list_nodes_by_kind(NodeKind::DbTable)
        .unwrap()
        .is_empty());
}

#[test]
fn jpa_compiler_recovery_preserves_candidate_resolution_in_effect_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, "jakarta.persistence");
    write(root, "src/p/Service.java", "package p; interface Repo extends org.springframework.data.jpa.repository.JpaRepository<Ledger,Long> {} class Service { Repo repo; void write() { repo.save(missing); repo.unknown(); } }");
    let store = index(root);
    let edges = store.list_edges_by_kind(EdgeKind::PersistsTo).unwrap();
    assert_eq!(
        edges.len(),
        1,
        "unknown methods must not gain inferred effects"
    );
    let edge = &edges[0];
    let evidence: Value = serde_json::from_str(edge.evidence_json.as_deref().unwrap()).unwrap();
    assert_eq!(evidence["compiler_resolution"], "candidate");
    assert_eq!(edge.certainty, EdgeCertainty::Candidate);
    assert!(edge.confidence.get() < 0.8);
}

#[test]
fn jpa_method_reference_and_custom_override_do_not_imply_inherited_crud_execution() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, "jakarta.persistence");
    write(root, "src/p/Service.java", "package p; interface Repo extends org.springframework.data.jpa.repository.JpaRepository<Ledger,Long> {} interface Custom extends Repo { <S extends Ledger> S save(S e); } class Service { Repo repo; Custom custom; void callback(Ledger e) { java.util.function.Function<Ledger,Ledger> callback=repo::save; custom.save(e); repo.flush(); } }");
    let store = index(root);
    assert!(store
        .list_edges_by_kind(EdgeKind::PersistsTo)
        .unwrap()
        .is_empty());
}

#[test]
fn jpa_implicit_or_schema_qualified_mapping_is_not_guessed_as_unqualified_table() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, "jakarta.persistence");
    write(root, "src/p/Ledger.java", "package p;\nimport jakarta.persistence.Entity;\nimport jakarta.persistence.Table;\n@Entity\n@Table(name=\"ledger_rows\", schema=\"tenant\")\npublic class Ledger {}\n");
    write(root, "src/p/Implicit.java", "package p;\nimport jakarta.persistence.Entity;\nimport jakarta.persistence.Table;\n@Entity\n@Table\nclass Implicit {}\n");
    write(root, "src/p/Service.java", "package p; interface Repo extends org.springframework.data.jpa.repository.JpaRepository<Ledger,Long> {} interface DefaultRepo extends org.springframework.data.jpa.repository.JpaRepository<Implicit,Long> {} class Service { Repo repo; DefaultRepo implicit; void write(Ledger e, Implicit i) { repo.save(e); implicit.save(i); } }");
    let store = index(root);
    assert!(store
        .list_edges_by_kind(EdgeKind::PersistsTo)
        .unwrap()
        .is_empty());
}
