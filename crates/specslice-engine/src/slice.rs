//! Feature slice engine.
//!
//! MVP-3 (PRD §3 / implementation plan §MVP-3): given a Requirement ID,
//! walk only confirmed/declared edges to assemble docs, declared
//! implementation symbols, linked tests and risks.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::artifact_id::requirement_id;
use specslice_core::{ArtifactId, EdgeKind, NodeKind};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureSlice {
    pub requirement_id: String,
    pub title: Option<String>,
    pub docs: Vec<SliceItem>,
    pub implementation: Vec<SliceItem>,
    pub linked_tests: Vec<SliceItem>,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceItem {
    pub id: String,
    pub kind: String,
    pub path: Option<String>,
    pub name: Option<String>,
    pub line_range: Option<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct SliceOptions {
    pub repo_root: PathBuf,
    pub requirement: String,
}

pub fn slice_requirement(options: SliceOptions) -> Result<FeatureSlice> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    slice_from_store(&store, &options.requirement)
}

pub fn slice_from_store(store: &Store, requirement: &str) -> Result<FeatureSlice> {
    let req_id = requirement_id(requirement);
    let req_node = store
        .find_node(&req_id)?
        .with_context(|| format!("requirement {requirement} not found in graph"))?;

    let mut slice = FeatureSlice {
        requirement_id: requirement.to_string(),
        title: req_node.name.clone(),
        docs: Vec::new(),
        implementation: Vec::new(),
        linked_tests: Vec::new(),
        risks: Vec::new(),
    };

    let docs_edges = store.list_edges_to(&req_id)?;
    for edge in &docs_edges {
        match edge.kind {
            EdgeKind::Documents => {
                if let Some(node) = store.find_node(&edge.from_id)? {
                    slice.docs.push(node_to_item(&node));
                }
            }
            EdgeKind::DeclaresImplementation => {
                if let Some(node) = store.find_node(&edge.from_id)? {
                    slice.implementation.push(node_to_item(&node));
                }
            }
            EdgeKind::DeclaresVerification => {
                if let Some(node) = store.find_node(&edge.from_id)? {
                    slice.linked_tests.push(node_to_item(&node));
                }
            }
            _ => {}
        }
    }

    sort_items(&mut slice.docs);
    sort_items(&mut slice.implementation);
    sort_items(&mut slice.linked_tests);

    if slice.linked_tests.is_empty() && !slice.implementation.is_empty() {
        slice.risks.push(
            "Requirement has declared implementation but no linked tests (missing @verifies)."
                .to_string(),
        );
    }
    if !slice.linked_tests.is_empty() {
        slice
            .risks
            .push("Verification is declared, not proven by coverage.".to_string());
    }
    if slice.implementation.is_empty() {
        slice
            .risks
            .push("No declared implementation found for this requirement.".to_string());
    }

    Ok(slice)
}

fn node_to_item(node: &specslice_core::Node) -> SliceItem {
    SliceItem {
        id: node.id.to_string(),
        kind: node.kind.as_str().to_string(),
        path: node.path.clone(),
        name: node.name.clone(),
        line_range: match (node.start_line, node.end_line) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => None,
        },
    }
}

fn sort_items(items: &mut [SliceItem]) {
    items.sort_by(|a, b| a.id.cmp(&b.id));
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

/// True for nodes whose kind matches "implementation" (Dart class, method,
/// function, constructor). Useful for downstream Impact and Context modules.
pub fn is_implementation_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::DartClass
            | NodeKind::DartMethod
            | NodeKind::DartFunction
            | NodeKind::DartConstructor
    )
}

/// Helper for downstream modules: collect all requirements an artifact
/// declares implementation/verification of.
pub fn declared_requirements_for(store: &Store, from: &ArtifactId) -> Result<Vec<ArtifactId>> {
    let mut out = Vec::new();
    for edge in store.list_edges_from(from)? {
        if matches!(
            edge.kind,
            EdgeKind::DeclaresImplementation | EdgeKind::DeclaresVerification
        ) {
            out.push(edge.to_id);
        }
    }
    Ok(out)
}
