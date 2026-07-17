//! Lightweight polling support for `groundgraph watch`.
//!
//! This module deliberately exposes the pure snapshot/diff pieces so CLI tests
//! and embedders can verify watcher policy without spawning a long-running
//! process.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};
use std::time::UNIX_EPOCH;

use anyhow::Context;
use ignore::WalkBuilder;

use crate::error::EngineResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSnapshot {
    pub files: BTreeMap<String, WatchFileState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchFileState {
    pub len: u64,
    pub modified_unix_nanos: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchChange {
    pub path: String,
    pub kind: WatchChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchChangeKind {
    Added,
    Modified,
    Deleted,
}

pub fn collect_watch_snapshot(repo_root: &Path) -> EngineResult<WatchSnapshot> {
    let mut files = BTreeMap::new();
    let mut builder = WalkBuilder::new(repo_root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| !is_ignored_watch_path(entry.path()));

    for entry in builder.build() {
        let entry = entry.with_context(|| {
            format!(
                "walking repository while collecting watch snapshot under {}",
                repo_root.display()
            )
        })?;
        let path = entry.path();
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        if is_ignored_watch_path(path) {
            continue;
        }
        let rel = match path.strip_prefix(repo_root) {
            Ok(rel) => rel,
            Err(_) => continue,
        };
        if is_ignored_relative_path(rel) {
            continue;
        }
        let metadata = fs::metadata(path)
            .with_context(|| format!("reading metadata for watched file {}", path.display()))?;
        files.insert(
            normalise_relative_path(rel),
            WatchFileState {
                len: metadata.len(),
                modified_unix_nanos: modified_unix_nanos(&metadata),
            },
        );
    }

    Ok(WatchSnapshot { files })
}

pub fn diff_watch_snapshots(before: &WatchSnapshot, after: &WatchSnapshot) -> Vec<WatchChange> {
    let mut paths = BTreeSet::new();
    paths.extend(before.files.keys().cloned());
    paths.extend(after.files.keys().cloned());

    let mut changes = Vec::new();
    for path in paths {
        match (before.files.get(&path), after.files.get(&path)) {
            (None, Some(_)) => changes.push(WatchChange {
                path,
                kind: WatchChangeKind::Added,
            }),
            (Some(_), None) => changes.push(WatchChange {
                path,
                kind: WatchChangeKind::Deleted,
            }),
            (Some(old), Some(new)) if old != new => changes.push(WatchChange {
                path,
                kind: WatchChangeKind::Modified,
            }),
            _ => {}
        }
    }
    changes
}

pub fn is_ignored_watch_path(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        matches!(
            name.to_string_lossy().as_ref(),
            ".git"
                | ".groundgraph"
                | ".dart_tool"
                | ".gradle"
                | ".cache"
                | "target"
                | "node_modules"
                | "build"
                | "dist"
                | "DerivedData"
                | "Pods"
        )
    })
}

fn is_ignored_relative_path(path: &Path) -> bool {
    is_ignored_watch_path(path)
}

fn normalise_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn modified_unix_nanos(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}
