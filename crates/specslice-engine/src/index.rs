//! Top-level orchestration for `specslice index`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::docs_indexer::{index_docs, DocsIndexOptions, DocsIndexResult, DOCS_INDEXER_NAME};

#[derive(Debug, Clone)]
pub struct IndexOptions {
    pub repo_root: PathBuf,
    pub include_docs: bool,
    pub include_code: bool,
}

impl IndexOptions {
    pub fn all(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            include_docs: true,
            include_code: true,
        }
    }

    pub fn docs_only(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            include_docs: true,
            include_code: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IndexResult {
    pub docs: Option<DocsIndexResult>,
    pub code: Option<crate::dart_indexer::DartIndexResult>,
}

pub fn index_repository(options: IndexOptions) -> Result<IndexResult> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let mut store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    store
        .migrate()
        .with_context(|| format!("running migrations on {}", db_path.display()))?;

    let mut result = IndexResult::default();

    if options.include_docs {
        store
            .clear_indexer_outputs(DOCS_INDEXER_NAME)
            .context("clearing previous docs index outputs")?;
        let docs_options = DocsIndexOptions {
            repo_root: options.repo_root.clone(),
            doc_roots: vec![
                PathBuf::from("docs"),
                PathBuf::from("specs"),
                PathBuf::from("adr"),
            ],
        };
        let docs = index_docs(&mut store, &docs_options).context("indexing docs")?;
        result.docs = Some(docs);
    }

    if options.include_code {
        store
            .clear_indexer_outputs(crate::dart_indexer::DART_INDEXER_NAME)
            .context("clearing previous dart index outputs")?;
        let code = crate::dart_indexer::index_dart(
            &mut store,
            &crate::dart_indexer::DartIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
            },
        )
        .context("indexing Dart sources")?;
        result.code = Some(code);
    }

    Ok(result)
}

fn load_config(repo_root: &Path) -> Result<EngineConfig> {
    let path = repo_root.join(DEFAULT_CONFIG_FILE_NAME);
    if !path.exists() {
        anyhow::bail!(
            "no SpecSlice workspace at {}: run `specslice init` first",
            repo_root.display()
        );
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let cfg: EngineConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("parsing config {}", path.display()))?;
    Ok(cfg)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = Path::new(&config.storage.path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    }
}
