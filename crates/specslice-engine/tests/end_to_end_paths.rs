//! End-to-end exercise of every path-based engine entrypoint. This is the
//! companion test that drives coverage for `init_repository`, `index_repository`,
//! `slice_requirement`, `run_impact`, `run_checks`, `build_context` and `export`.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use specslice_engine::index::index_repository;
use specslice_engine::{
    build_context, export, init_repository, run_checks, run_impact, slice_requirement,
    CheckOptions, ContextOptions, ExportFormat, ExportOptions, ImpactOptions, IndexOptions,
    InitOptions, SliceOptions,
};
use tempfile::TempDir;

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

fn run_git(repo: &Path, args: &[&str]) {
    StdCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
}

fn setup_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    copy_dir(&fixture_path(), tmp.path());
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    run_git(tmp.path(), &["init", "-q", "-b", "main"]);
    run_git(tmp.path(), &["config", "user.email", "t@t"]);
    run_git(tmp.path(), &["config", "user.name", "T"]);
    std::fs::write(
        tmp.path().join(".gitignore"),
        ".specslice/graph.db\n.specslice/export/\n",
    )
    .unwrap();
    run_git(tmp.path(), &["add", "."]);
    run_git(tmp.path(), &["commit", "-q", "-m", "baseline"]);
    tmp
}

#[test]
fn init_is_idempotent_via_engine_api() {
    let tmp = TempDir::new().unwrap();
    let first = init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    assert!(!first.config_already_existed);
    assert!(!first.links_already_existed);
    assert!(!first.graph_db_already_existed);
    let second = init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    assert!(second.config_already_existed);
    assert!(second.links_already_existed);
    assert!(second.graph_db_already_existed);
}

#[test]
fn index_then_slice_path_entrypoint_returns_fixture_artifacts() {
    let tmp = setup_repo();
    index_repository(IndexOptions::all(tmp.path().to_path_buf())).unwrap();
    let slice = slice_requirement(SliceOptions {
        repo_root: tmp.path().into(),
        requirement: "REQ-WATERMARK-001".into(),
    })
    .unwrap();
    assert_eq!(slice.requirement_id, "REQ-WATERMARK-001");
    assert!(!slice.implementation.is_empty());
}

#[test]
fn index_docs_only_via_path_entrypoint() {
    let tmp = setup_repo();
    let result = index_repository(IndexOptions::docs_only(tmp.path().to_path_buf())).unwrap();
    let docs = result.docs.expect("docs result");
    assert_eq!(docs.files, 1);
    assert_eq!(docs.requirements, 0);
    assert!(docs.doc_sections >= 1);

    let checks = run_checks(CheckOptions {
        repo_root: tmp.path().into(),
        impact: None,
    })
    .unwrap();
    // Docs-only indexing is physical evidence only. It should not create
    // business logic nodes that checks can treat as orphan requirements.
    assert!(checks.findings.is_empty(), "{:?}", checks.findings);
}

#[test]
fn run_impact_path_entrypoint_reindexes_and_reports() {
    let tmp = setup_repo();
    // Edit the impl and commit it.
    let impl_path = tmp
        .path()
        .join("lib/domain/watermark/auto_placement_service.dart");
    let original = std::fs::read_to_string(&impl_path).unwrap();
    std::fs::write(
        &impl_path,
        original.replace(
            "candidates.sort((a, b) => b.score.compareTo(a.score));",
            "candidates.sort((a, b) => b.score.compareTo(a.score) * 1);",
        ),
    )
    .unwrap();
    run_git(tmp.path(), &["add", "."]);
    run_git(tmp.path(), &["commit", "-q", "-m", "edit impl"]);

    let report = run_impact(ImpactOptions {
        repo_root: tmp.path().into(),
        base_ref: "HEAD~1".into(),
        head_ref: "HEAD".into(),
        reindex: true,
    })
    .unwrap();
    assert!(report
        .affected_requirements
        .iter()
        .any(|r| r.id == "req::REQ-WATERMARK-001"));
}

#[test]
fn build_context_path_entrypoint_returns_snippets() {
    let tmp = setup_repo();
    index_repository(IndexOptions::all(tmp.path().to_path_buf())).unwrap();
    let pack = build_context(ContextOptions {
        repo_root: tmp.path().into(),
        requirement: "REQ-WATERMARK-001".into(),
        include_snippets: true,
    })
    .unwrap();
    assert!(!pack.docs_snippets.is_empty());
    let pack_without = build_context(ContextOptions {
        repo_root: tmp.path().into(),
        requirement: "REQ-WATERMARK-001".into(),
        include_snippets: false,
    })
    .unwrap();
    assert!(pack_without.docs_snippets.is_empty());
}

#[test]
fn export_jsonl_writes_real_rows_for_indexed_fixture() {
    let tmp = setup_repo();
    index_repository(IndexOptions::all(tmp.path().to_path_buf())).unwrap();
    let outcome = export(ExportOptions {
        repo_root: tmp.path().into(),
        format: ExportFormat::Jsonl,
    })
    .unwrap();
    let nodes_path = outcome.bundle_dir.join("nodes.jsonl");
    let edges_path = outcome.bundle_dir.join("edge_assertions.jsonl");
    let nodes = std::fs::read_to_string(&nodes_path).unwrap();
    let edges = std::fs::read_to_string(&edges_path).unwrap();
    assert!(nodes.lines().any(|l| l.contains("REQ-WATERMARK-001")));
    assert!(edges.lines().any(|l| l.contains("declares_implementation")));
}

#[test]
fn slice_requirement_missing_workspace_errors_with_message() {
    let tmp = TempDir::new().unwrap();
    let err = slice_requirement(SliceOptions {
        repo_root: tmp.path().into(),
        requirement: "REQ-X".into(),
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("specslice init"), "err = {err}");
}

#[test]
fn run_checks_missing_workspace_errors_with_message() {
    let tmp = TempDir::new().unwrap();
    let err = run_checks(CheckOptions {
        repo_root: tmp.path().into(),
        impact: None,
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("specslice init"), "err = {err}");
}

#[test]
fn build_context_missing_workspace_errors_with_message() {
    let tmp = TempDir::new().unwrap();
    let err = build_context(ContextOptions {
        repo_root: tmp.path().into(),
        requirement: "REQ-X".into(),
        include_snippets: false,
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("specslice init"), "err = {err}");
}

#[test]
fn index_repository_missing_workspace_errors_with_message() {
    let tmp = TempDir::new().unwrap();
    let err = index_repository(IndexOptions::all(tmp.path().to_path_buf()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("specslice init"), "err = {err}");
}

#[test]
fn init_rejects_corrupted_existing_config() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".specslice.yaml"), "not: [valid").unwrap();
    let err = init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("parsing existing config"), "err = {err}");
}

#[test]
fn index_rejects_corrupted_config() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".specslice.yaml"), "not: [valid").unwrap();
    let err = index_repository(IndexOptions::all(tmp.path().to_path_buf()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("parsing config"), "err = {err}");
}

#[test]
fn export_writes_blob_columns_as_jsonl_arrays() {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    // Insert a node with a BLOB metadata_json column to exercise the
    // sqlite_value_to_json BLOB branch.
    let db_path = tmp.path().join(".specslice/graph.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO nodes (id, kind, metadata_json) VALUES ('blob_node', 'file', ?1)",
        rusqlite::params![&[1u8, 2u8, 3u8][..]],
    )
    .unwrap();
    drop(conn);

    let outcome = export(ExportOptions {
        repo_root: tmp.path().into(),
        format: ExportFormat::Jsonl,
    })
    .unwrap();
    let nodes = std::fs::read_to_string(outcome.bundle_dir.join("nodes.jsonl")).unwrap();
    assert!(nodes.contains("[1,2,3]"));
}

#[test]
fn run_impact_missing_workspace_errors_with_message() {
    let tmp = TempDir::new().unwrap();
    let err = run_impact(ImpactOptions {
        repo_root: tmp.path().into(),
        base_ref: "HEAD~1".into(),
        head_ref: "HEAD".into(),
        reindex: false,
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("specslice init"), "err = {err}");
}
