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
    pub links_path: PathBuf,
    pub links_already_existed: bool,
    pub graph_db_path: PathBuf,
    pub graph_db_already_existed: bool,
    /// P23.9 — `.specslice/requirements/` (Markdown requirements).
    pub requirements_dir: PathBuf,
    pub requirements_already_existed: bool,
}

/// Chinese-first starter file written into `.specslice/requirements/` on a
/// fresh init so users have a copyable template.
/// Scaffolded `README.md` explaining the Markdown requirements format. It is
/// intentionally *not* indexed as a requirement (the indexer skips `README.md`)
/// so a fresh `init` leaves the graph empty — mirroring the empty `links.yaml`
/// manifest. The example lives inside a code fence so it never parses as a live
/// requirement.
const SAMPLE_REQUIREMENT_MD: &str = "# SpecSlice 需求映射（Markdown）\n\n\
在本目录新增 `*.md` 文件，声明“需求 → 文档 / 实现 / 测试”的映射。\n\
索引时 SpecSlice 会读取它们并写入图谱；本目录非侵入，只属于 `.specslice/`，不改业务代码。\n\
（`README.md` 仅为说明文档，不会被当作需求解析。）\n\n\
## 文件格式\n\n\
- 每个需求以一级标题开头：`# <编号> <标题>`，`<编号>` 为首个空白前的标记（如 `REQ-001`）。\n\
- 三个可选小节，中英皆可：`## 文档` / `## 实现` / `## 测试`。\n\
- 每个小节是 `路径#片段` 列表；片段可为符号名、类名、`类型.方法` 或文档小节标题；省略 `#片段` 表示整文件。\n\n\
## 示例\n\n\
复制下面代码块内容到新文件（如 `0001-示例.md`）即可：\n\n\
```markdown\n\
# REQ-001 示例需求标题\n\n\
用一句话描述需求意图（可多行），将作为 Requirement 节点的描述。\n\n\
## 文档\n\
- docs/example.md#示例小节\n\n\
## 实现\n\
- lib/example.dart#ExampleClass\n\n\
## 测试\n\
- test/example_test.dart#示例用例\n\
```\n";

/// Initialise a SpecSlice workspace at `options.repo_root`.
///
/// Behaviour:
/// - If `.specslice.yaml` is missing, write a default config. Existing config
///   files are left untouched (idempotent re-init).
/// - Ensure `.specslice/` exists and open the SQLite database. The database
///   file is created if it is missing.
/// - Ensure the external links manifest exists. This is the only place where
///   users declare requirement-to-code/test relationships.
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

    let links_path = resolve_links_path(&repo_root, &config);
    let links_already_existed = links_path.exists();
    if !links_already_existed {
        if let Some(parent) = links_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating links directory {}", parent.display()))?;
        }
        std::fs::write(&links_path, "requirements: {}\n")
            .with_context(|| format!("writing links manifest {}", links_path.display()))?;
    }

    // P23.9 — scaffold the Markdown requirements directory with a template.
    let requirements_dir = repo_root.join(crate::requirements_md_indexer::DEFAULT_REQUIREMENTS_DIR);
    let requirements_already_existed = requirements_dir.exists();
    if !requirements_already_existed {
        std::fs::create_dir_all(&requirements_dir).with_context(|| {
            format!(
                "creating requirements directory {}",
                requirements_dir.display()
            )
        })?;
        let sample = requirements_dir.join("README.md");
        std::fs::write(&sample, SAMPLE_REQUIREMENT_MD)
            .with_context(|| format!("writing requirements README {}", sample.display()))?;
    }

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
        links_path,
        links_already_existed,
        graph_db_path,
        graph_db_already_existed,
        requirements_dir,
        requirements_already_existed,
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

fn resolve_links_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = Path::new(&config.links.path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    }
}
