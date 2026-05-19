//! Workspace configuration stored in `.specslice.yaml`.
//!
//! MVP-0 only needs `storage.path` so that downstream commands know where the
//! SQLite database lives. The shape stays forward-compatible with the richer
//! schema described in PRD §8: we keep field names stable and accept unknown
//! keys without erroring.

use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_FILE_NAME: &str = ".specslice.yaml";
pub const DEFAULT_STORAGE_DIR: &str = ".specslice";
pub const DEFAULT_DB_FILENAME: &str = "graph.db";

/// The full engine configuration as serialised to `.specslice.yaml`.
///
/// Only `repo` and `storage` are populated in MVP-0. Other sections will be
/// added by later phases (docs/code/slice/impact/checks) but the file format
/// already reserves the keys so users do not need to rewrite their config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    pub repo: RepoConfig,
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub root: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub path: String,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            repo: RepoConfig {
                root: ".".to_string(),
                default_branch: "main".to_string(),
            },
            storage: StorageConfig {
                path: format!("{DEFAULT_STORAGE_DIR}/{DEFAULT_DB_FILENAME}"),
            },
        }
    }
}
