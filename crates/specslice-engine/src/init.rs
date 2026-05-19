//! `specslice init` behaviour.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME, DEFAULT_STORAGE_DIR};

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub repo_root: PathBuf,
}

impl InitOptions {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }
}

/// The on-disk artefacts produced by `specslice init`.
#[derive(Debug, Clone)]
pub struct InitOutcome {
    pub config_path: PathBuf,
    pub config_already_existed: bool,
    pub graph_db_path: PathBuf,
    pub graph_db_already_existed: bool,
}

/// Initialise a SpecSlice workspace at `options.repo_root`.
///
/// Behaviour:
/// - If `.specslice.yaml` is missing, write a default config. Existing config
///   files are left untouched (idempotent re-init).
/// - Ensure `.specslice/` exists and open the SQLite database. The database
///   file is created if it is missing.
pub fn init_repository(options: InitOptions) -> Result<InitOutcome> {
    let repo_root = options.repo_root;
    let config_path = repo_root.join(DEFAULT_CONFIG_FILE_NAME);
    let config_already_existed = config_path.exists();

    let config = if config_already_existed {
        load_existing_config(&config_path)?
    } else {
        let cfg = EngineConfig::default();
        let yaml = serde_yaml::to_string(&cfg).context("serialising default config to YAML")?;
        std::fs::write(&config_path, yaml)
            .with_context(|| format!("writing default config to {}", config_path.display()))?;
        cfg
    };

    let storage_dir = repo_root.join(DEFAULT_STORAGE_DIR);
    std::fs::create_dir_all(&storage_dir)
        .with_context(|| format!("creating storage directory {}", storage_dir.display()))?;

    let graph_db_path = resolve_storage_path(&repo_root, &config);
    let graph_db_already_existed = graph_db_path.exists();

    let mut store = specslice_store::Store::open(&graph_db_path)
        .with_context(|| format!("opening SQLite database at {}", graph_db_path.display()))?;
    store
        .migrate()
        .with_context(|| format!("running migrations on {}", graph_db_path.display()))?;
    drop(store);

    Ok(InitOutcome {
        config_path,
        config_already_existed,
        graph_db_path,
        graph_db_already_existed,
    })
}

fn load_existing_config(path: &Path) -> Result<EngineConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading existing config {}", path.display()))?;
    serde_yaml::from_str::<EngineConfig>(&contents)
        .with_context(|| format!("parsing existing config {}", path.display()))
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = Path::new(&config.storage.path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    }
}
