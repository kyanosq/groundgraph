use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use groundgraph_engine::watch::{collect_watch_snapshot, diff_watch_snapshots};

use super::index;

#[derive(Debug, Clone)]
pub struct WatchRunArgs {
    pub interval: Duration,
    pub debounce: Duration,
    pub once: bool,
    pub initial_index: bool,
    pub docs_only: bool,
}

pub fn run(repo_root: &Path, args: WatchRunArgs) -> Result<()> {
    if args.initial_index {
        println!("Initial index:");
        index::run(repo_root, args.docs_only, false)?;
    }

    let mut snapshot = collect_watch_snapshot(repo_root)?;
    println!("Watch snapshot: {} files", snapshot.files.len());
    if args.once {
        return Ok(());
    }

    println!(
        "Watching {} (interval={}ms, debounce={}ms)",
        repo_root.display(),
        args.interval.as_millis(),
        args.debounce.as_millis()
    );
    loop {
        thread::sleep(args.interval);
        let next = collect_watch_snapshot(repo_root)?;
        let changes = diff_watch_snapshots(&snapshot, &next);
        if changes.is_empty() {
            snapshot = next;
            continue;
        }

        println!(
            "Detected {} file change(s); re-indexing after {}ms debounce.",
            changes.len(),
            args.debounce.as_millis()
        );
        thread::sleep(args.debounce);
        index::run(repo_root, args.docs_only, false)?;
        snapshot = collect_watch_snapshot(repo_root)?;
    }
}
