//! Public-surface error classification for the engine (#166).
//!
//! These tests pin the *typed* contract the CLI (#233 exit-code contract) and
//! the MCP server (`INVALID_PARAMS` vs `INTERNAL_ERROR`) consume: each
//! failure source lands in a distinct [`EngineError`] variant with a matching
//! [`ErrorKind`], and the human-readable message does not regress relative to
//! the pre-#166 `anyhow` surface.

use std::path::Path;

use groundgraph_engine::{
    run_search, slice_from_store, slice_requirement, EngineError, ErrorKind, SearchOptions,
    SearchQuery, SliceOptions,
};
use groundgraph_store::Store;

fn search_opts(repo: &Path) -> SearchOptions {
    SearchOptions {
        repo_root: repo.to_path_buf(),
        query: SearchQuery::Keywords("anything".to_string()),
        depth: 0,
        kinds: Vec::new(),
        limit: 1,
        include_noise: false,
    }
}

/// No `.groundgraph.yaml` is a *user* error — the caller must `init` first,
/// not an internal/store failure.
#[test]
fn run_search_without_workspace_is_a_user_error_no_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let err = run_search(search_opts(tmp.path())).unwrap_err();
    assert!(
        matches!(err, EngineError::NoWorkspace { .. }),
        "got {err:?}"
    );
    assert_eq!(err.kind(), ErrorKind::UserInput);
}

/// The no-workspace message must keep the actionable `init` hint that the
/// pre-#166 `anyhow::bail!` carried — classifying the error must not strip it.
#[test]
fn no_workspace_message_keeps_the_init_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let err = run_search(search_opts(tmp.path())).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("groundgraph init"), "{msg}");
}

/// A present-but-malformed config is still a user error, just a different
/// variant (`Config`, not `NoWorkspace`).
#[test]
fn run_search_malformed_config_is_a_config_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".groundgraph.yaml"),
        "repo:\n  root: [unclosed\n",
    )
    .unwrap();
    let err = run_search(search_opts(tmp.path())).unwrap_err();
    assert!(matches!(err, EngineError::Config { .. }), "got {err:?}");
    assert_eq!(err.kind(), ErrorKind::UserInput);
}

/// `slice_requirement` shares the config-load prelude, so it surfaces the same
/// no-workspace class as `run_search`.
#[test]
fn slice_requirement_without_workspace_is_a_user_error() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = SliceOptions {
        repo_root: tmp.path().to_path_buf(),
        requirement: "req:DoesNotExist".to_string(),
        ..Default::default()
    };
    let err = slice_requirement(opts).unwrap_err();
    assert!(
        matches!(err, EngineError::NoWorkspace { .. }),
        "got {err:?}"
    );
}

/// A requirement id absent from a *healthy* migrated store is a `NotFound`,
/// distinct from a store failure: the db is fine, the target just is not there.
#[test]
fn slice_unknown_requirement_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("graph.db");
    let mut store = Store::open(&db).unwrap();
    store.migrate().unwrap();
    let err = slice_from_store(&store, "req:DoesNotExist").unwrap_err();
    assert!(matches!(err, EngineError::NotFound { .. }), "got {err:?}");
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

/// A db the store genuinely cannot open (unwritable location) surfaces as a
/// `Store` variant — an operational error, not user-input.
#[test]
fn run_search_unopenable_db_is_a_store_error() {
    let tmp = tempfile::tempdir().unwrap();
    // Absolute `storage.path` is honoured verbatim; a path whose ancestor
    // cannot be created makes `Store::open` fail before any query runs.
    std::fs::write(
        tmp.path().join(".groundgraph.yaml"),
        "storage:\n  path: /groundgraph_nonexistent_root/sub/graph.db\n",
    )
    .unwrap();
    let err = run_search(search_opts(tmp.path())).unwrap_err();
    assert!(matches!(err, EngineError::Store(_)), "got {err:?}");
    assert_eq!(err.kind(), ErrorKind::Operational);
}
