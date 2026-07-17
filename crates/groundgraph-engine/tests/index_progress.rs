//! #231 — `index_repository_with_progress` forwards a coarse phase sequence
//! to a `ProgressSink`, so the CLI can render an `indicatif` spinner without
//! the engine depending on a terminal library. This test pins the contract:
//! every index run emits at least the `docs` → … → `fulltext` → `commit`
//! phase boundaries, in order.

use groundgraph_engine::progress::RecordingSink;
use groundgraph_engine::{index_repository_with_progress, InitOptions};

use tempfile::TempDir;

#[test]
fn index_reports_phase_sequence_to_progress_sink() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    groundgraph_engine::init_repository(InitOptions {
        repo_root: root.to_path_buf(),
    })
    .unwrap();

    let mut sink = RecordingSink::default();
    index_repository_with_progress(
        groundgraph_engine::IndexOptions::all(root.to_path_buf()),
        &mut sink,
    )
    .expect("index must succeed on an initialised empty repo");

    // `docs` is the very first phase (include_docs=true in IndexOptions::all).
    assert_eq!(
        sink.phases.first().map(String::as_str),
        Some("docs"),
        "docs should be the first reported phase, got: {:?}",
        sink.phases
    );
    // The content layer runs LAST, immediately before commit.
    assert!(
        sink.phases.iter().any(|p| p == "fulltext"),
        "expected a fulltext phase, got: {:?}",
        sink.phases
    );
    // commit is the final phase, always marked on a successful run.
    assert!(
        sink.phases.iter().any(|p| p == "commit"),
        "expected a commit phase, got: {:?}",
        sink.phases
    );
    // Ordering: docs precedes fulltext precedes commit.
    let pos = |name: &str| sink.phases.iter().position(|p| p == name);
    let (d, f, c) = (pos("docs"), pos("fulltext"), pos("commit"));
    assert!(
        d.unwrap() < f.unwrap(),
        "docs before fulltext: {:?}",
        sink.phases
    );
    assert!(
        f.unwrap() < c.unwrap(),
        "fulltext before commit: {:?}",
        sink.phases
    );
}

#[test]
fn index_repository_default_uses_noop_sink() {
    // The public `index_repository` delegates to `index_repository_with_progress`
    // with a NoopSink — this only asserts it still compiles/links and returns
    // Ok on an empty repo (the no-op default must not break the legacy path).
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    groundgraph_engine::init_repository(InitOptions {
        repo_root: root.to_path_buf(),
    })
    .unwrap();
    groundgraph_engine::index_repository(groundgraph_engine::IndexOptions::all(root.to_path_buf()))
        .expect("legacy index_repository must still succeed");
}
