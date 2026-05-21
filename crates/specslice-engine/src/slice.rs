//! Feature slice engine.
//!
//! MVP-3 (PRD §3 / implementation plan §MVP-3): given a Requirement ID,
//! walk only confirmed/declared edges to assemble docs, declared
//! implementation symbols, linked tests and risks.

use std::collections::BTreeSet;
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
    /// P14 — symbols reached one or more hops along forward
    /// `EdgeKind::Calls` / `EdgeKind::References` edges from
    /// [`Self::implementation`]. Empty when `call_depth = 0` or no
    /// implementation produces fact-edges. Order is stable (id-sorted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub code_fanout: Vec<SliceItem>,
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
    /// P14 — how many hops to follow `EdgeKind::Calls` /
    /// `EdgeKind::References` from each declared implementation symbol.
    /// Defaults to `1` so reviewers see the immediate callees a
    /// requirement touches; set to `0` to recover the pre-P14 manifest-
    /// only slice.
    pub fanout: SliceFanoutOptions,
}

impl Default for SliceOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::new(),
            requirement: String::new(),
            fanout: SliceFanoutOptions::default(),
        }
    }
}

/// P14 — knobs for the slice fact-edge fan-out. Kept in its own struct
/// (rather than inline on [`SliceOptions`]) so the CLI / MCP layer can
/// pass through future toggles (`include_synthetic`, `skip_noise`, …)
/// without breaking call-sites that only care about `call_depth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceFanoutOptions {
    /// BFS depth on outgoing `Calls` / `References` edges from each
    /// implementation node. `0` disables propagation entirely.
    pub call_depth: usize,
}

impl Default for SliceFanoutOptions {
    fn default() -> Self {
        Self { call_depth: 1 }
    }
}

/// Hard cap on the fan-out result size so a noisy graph does not blow
/// up `slice` JSON output.
const SLICE_FANOUT_MAX: usize = 256;

pub fn slice_requirement(options: SliceOptions) -> Result<FeatureSlice> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    slice_from_store_with_options(&store, &options.requirement, options.fanout)
}

pub fn slice_from_store(store: &Store, requirement: &str) -> Result<FeatureSlice> {
    slice_from_store_with_options(store, requirement, SliceFanoutOptions::default())
}

pub fn slice_from_store_with_options(
    store: &Store,
    requirement: &str,
    fanout: SliceFanoutOptions,
) -> Result<FeatureSlice> {
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
        code_fanout: Vec::new(),
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

    // P14 — fact-edge fan-out. We seed with the implementation symbol
    // ids and walk outward via `Calls` / `References` so the slice
    // reflects what the requirement's code actually exercises today,
    // not just what the manifest claims.
    fanout_calls_and_references(store, &mut slice, fanout.call_depth)?;
    sort_items(&mut slice.code_fanout);

    if slice.linked_tests.is_empty() && !slice.implementation.is_empty() {
        slice.risks.push(
            "Requirement has linked implementation but no linked verification tests.".to_string(),
        );
    }
    if !slice.linked_tests.is_empty() {
        slice
            .risks
            .push("Verification is linked, not proven by coverage.".to_string());
    }
    if slice.implementation.is_empty() {
        slice
            .risks
            .push("No linked implementation found for this requirement.".to_string());
    }

    Ok(slice)
}

fn fanout_calls_and_references(
    store: &Store,
    slice: &mut FeatureSlice,
    depth: usize,
) -> Result<()> {
    if depth == 0 || slice.implementation.is_empty() {
        return Ok(());
    }
    let seeds: BTreeSet<ArtifactId> = slice
        .implementation
        .iter()
        .map(|item| ArtifactId::new(item.id.clone()))
        .collect();
    let mut visited: BTreeSet<ArtifactId> = seeds.clone();
    let mut frontier: Vec<ArtifactId> = seeds.iter().cloned().collect();
    let mut truncated = false;
    'outer: for _ in 0..depth {
        let mut next: Vec<ArtifactId> = Vec::new();
        for id in &frontier {
            for edge in store.list_edges_from(id)? {
                if !matches!(edge.kind, EdgeKind::Calls | EdgeKind::References) {
                    continue;
                }
                let target = edge.to_id;
                if !visited.insert(target.clone()) {
                    continue;
                }
                if slice.code_fanout.len() >= SLICE_FANOUT_MAX {
                    truncated = true;
                    break 'outer;
                }
                if let Some(node) = store.find_node(&target)? {
                    slice.code_fanout.push(node_to_item(&node));
                }
                next.push(target);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    if truncated {
        slice.risks.push(format!(
            "slice: 调用 / 引用 fanout 达到上限 {SLICE_FANOUT_MAX}，结果已截断"
        ));
    }
    Ok(())
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

/// True for nodes whose kind represents an "implementation" symbol that
/// can carry behaviour — Dart classes/methods/functions/constructors plus
/// the Swift and Go equivalents emitted by the LSP-backed adapters in P11.
/// Routes / providers / storage are deliberately excluded because they
/// are synthetic anchors, not bodies of code.
pub fn is_implementation_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::DartClass
            | NodeKind::DartMethod
            | NodeKind::DartFunction
            | NodeKind::DartConstructor
            | NodeKind::SwiftClass
            | NodeKind::SwiftStruct
            | NodeKind::SwiftEnum
            | NodeKind::SwiftProtocol
            | NodeKind::SwiftMethod
            | NodeKind::SwiftFunction
            | NodeKind::SwiftInitializer
            | NodeKind::GoStruct
            | NodeKind::GoInterface
            | NodeKind::GoMethod
            | NodeKind::GoFunction
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

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn is_implementation_kind_matches_all_dart_symbol_kinds() {
        assert!(is_implementation_kind(NodeKind::DartClass));
        assert!(is_implementation_kind(NodeKind::DartMethod));
        assert!(is_implementation_kind(NodeKind::DartFunction));
        assert!(is_implementation_kind(NodeKind::DartConstructor));
        assert!(!is_implementation_kind(NodeKind::Requirement));
        assert!(!is_implementation_kind(NodeKind::DocSection));
        assert!(!is_implementation_kind(NodeKind::File));
    }

    #[test]
    fn resolve_storage_path_returns_absolute_when_already_absolute() {
        let cfg = EngineConfig {
            storage: crate::config::StorageConfig {
                path: "/tmp/a/b.db".into(),
            },
            ..EngineConfig::default()
        };
        let resolved = resolve_storage_path(Path::new("/elsewhere"), &cfg);
        assert_eq!(resolved.to_string_lossy(), "/tmp/a/b.db");
    }

    #[test]
    fn declared_requirements_for_collects_implements_and_verifies_edges() {
        use specslice_core::{artifact_id::dart_class_id, EdgeAssertion, EdgeSource};
        let tmp = tempfile::TempDir::new().unwrap();
        let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let from = dart_class_id("lib/x.dart", "X");
        store
            .upsert_edge(&EdgeAssertion::declared(
                from.clone(),
                requirement_id("REQ-A"),
                EdgeKind::DeclaresImplementation,
                EdgeSource::ExternalManifest,
            ))
            .unwrap();
        store
            .upsert_edge(&EdgeAssertion::declared(
                from.clone(),
                requirement_id("REQ-B"),
                EdgeKind::DeclaresVerification,
                EdgeSource::ExternalManifest,
            ))
            .unwrap();
        store
            .upsert_edge(&EdgeAssertion::fact(
                from.clone(),
                requirement_id("REQ-C"),
                EdgeKind::Imports,
                EdgeSource::ExternalManifest,
            ))
            .unwrap();
        let reqs = declared_requirements_for(&store, &from).unwrap();
        assert_eq!(reqs.len(), 2);
    }
}
