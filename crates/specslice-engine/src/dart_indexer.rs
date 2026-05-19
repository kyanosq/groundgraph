//! Bridge between the Dart lightweight adapter and the SpecSlice store.
//!
//! The implementation is filled in MVP-2. MVP-1 only needs the surface so
//! that `index_repository` compiles when callers opt out of the code index.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use specslice_store::Store;

pub const DART_INDEXER_NAME: &str = "dart_lightweight";

#[derive(Debug, Clone)]
pub struct DartIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DartIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub declared_implementations: usize,
    pub declared_verifications: usize,
}

/// Real implementation lands in MVP-2. The MVP-1 stub honours the contract by
/// returning an empty result.
pub fn index_dart(_store: &mut Store, _options: &DartIndexOptions) -> Result<DartIndexResult> {
    Ok(DartIndexResult::default())
}
