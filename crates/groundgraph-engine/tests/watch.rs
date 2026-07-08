//! Polling watcher policy tests.

use std::fs;
use std::time::Duration;

use groundgraph_engine::watch::{collect_watch_snapshot, diff_watch_snapshots, WatchChangeKind};

#[test]
fn watch_snapshot_ignores_generated_and_cache_directories() {
    let repo = tempfile::TempDir::new().unwrap();
    for path in [
        ".git/config",
        ".groundgraph/graph.db",
        "target/debug/app",
        "node_modules/pkg/index.js",
        "build/out.txt",
        "src/lib.rs",
    ] {
        let file = repo.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "x").unwrap();
    }

    let snapshot = collect_watch_snapshot(repo.path()).unwrap();

    assert!(snapshot.files.contains_key("src/lib.rs"));
    assert!(!snapshot.files.contains_key(".git/config"));
    assert!(!snapshot.files.contains_key(".groundgraph/graph.db"));
    assert!(!snapshot.files.contains_key("target/debug/app"));
    assert!(!snapshot.files.contains_key("node_modules/pkg/index.js"));
    assert!(!snapshot.files.contains_key("build/out.txt"));
}

#[test]
fn watch_diff_detects_added_modified_and_deleted_files() {
    let repo = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/lib.rs"), "pub fn before() {}\n").unwrap();

    let before = collect_watch_snapshot(repo.path()).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn after_change() { let _x = 1; }\n",
    )
    .unwrap();
    fs::write(repo.path().join("src/new.rs"), "pub fn new_file() {}\n").unwrap();

    let after_add_modify = collect_watch_snapshot(repo.path()).unwrap();
    let changes = diff_watch_snapshots(&before, &after_add_modify);
    assert!(changes
        .iter()
        .any(|c| c.path == "src/lib.rs" && c.kind == WatchChangeKind::Modified));
    assert!(changes
        .iter()
        .any(|c| c.path == "src/new.rs" && c.kind == WatchChangeKind::Added));

    fs::remove_file(repo.path().join("src/new.rs")).unwrap();
    let after_delete = collect_watch_snapshot(repo.path()).unwrap();
    let changes = diff_watch_snapshots(&after_add_modify, &after_delete);
    assert!(changes
        .iter()
        .any(|c| c.path == "src/new.rs" && c.kind == WatchChangeKind::Deleted));
}
