//! P1: AI candidate links + human confirmation.
//!
//! `specslice connect` is split into two file-driven phases so the AI step
//! stays external:
//!
//! 1. [`propose_evidence`] reads the indexed graph and produces an
//!    [`EvidencePack`] — the set of facts an AI should ground itself in
//!    (requirements with their current links, orphan symbols/tests, etc).
//!    The user feeds this to whichever model they trust; the model returns a
//!    candidates YAML.
//! 2. [`apply_candidates`] loads that candidates file, validates every
//!    reference against the graph (existence, locatability), and merges
//!    accepted candidates into `.specslice/links.yaml`. Anything the resolver
//!    cannot locate is reported as rejected. Rules never invent business
//!    links — they only verify references.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{ArtifactId, EdgeKind, NodeKind};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::links_indexer::{
    strict_resolve_doc, strict_resolve_implementation, strict_resolve_test,
};

pub const EVIDENCE_SCHEMA_VERSION: u32 = 1;
pub const CANDIDATES_SCHEMA_VERSION: u32 = 1;

const PROMPT: &str = "You are SpecSlice's external candidate generator. Read the evidence pack \
and propose link candidates that connect each REQ to its implementation symbol(s) and verifying \
test case(s). Output YAML matching the `CandidatesDocument` schema. Never invent paths, names, or \
identifiers that are not present in the evidence pack. If unsure, output a clarifying question \
under `questions:` instead of a low-confidence candidate.";

// ---------------------------------------------------------------------------
// Evidence pack types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct EvidencePack {
    pub schema_version: u32,
    pub repo_root: String,
    pub requirements: Vec<EvidenceRequirement>,
    pub orphan_doc_sections: Vec<EvidenceDocSection>,
    pub orphan_symbols: Vec<EvidenceSymbol>,
    pub orphan_tests: Vec<EvidenceTest>,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRequirement {
    pub id: String,
    pub title: Option<String>,
    pub path: Option<String>,
    pub linked_docs: Vec<String>,
    pub linked_implementations: Vec<String>,
    pub linked_tests: Vec<String>,
    pub missing_docs: bool,
    pub missing_implementations: bool,
    pub missing_tests: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceDocSection {
    pub path: String,
    pub name: String,
    pub slug: String,
    pub line_range: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSymbol {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub name: String,
    pub qualified_name: Option<String>,
    pub line_range: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceTest {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub name: String,
    pub line_range: Option<(u32, u32)>,
}

// ---------------------------------------------------------------------------
// Candidates types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CandidatesDocument {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub candidates: Vec<LinkCandidate>,
    #[serde(default)]
    pub questions: Vec<ClarifyingQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LinkCandidate {
    pub requirement: String,
    #[serde(default)]
    pub docs: Vec<String>,
    #[serde(default, alias = "implementation")]
    pub implementations: Vec<String>,
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClarifyingQuestion {
    pub target: String,
    pub question: String,
}

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub repo_root: PathBuf,
    pub candidates_path: PathBuf,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ApplyOutcome {
    pub accepted: Vec<AcceptedCandidate>,
    pub rejected: Vec<RejectedCandidate>,
    pub manifest_path: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptedCandidate {
    pub requirement: String,
    pub docs: Vec<String>,
    pub implementations: Vec<String>,
    pub tests: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectedCandidate {
    pub requirement: String,
    pub reason: String,
    pub raw: LinkCandidate,
}

// ---------------------------------------------------------------------------
// propose_evidence
// ---------------------------------------------------------------------------

pub fn propose_evidence(repo_root: &Path) -> Result<EvidencePack> {
    let config = load_config(repo_root)?;
    let db_path = resolve_storage_path(repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;

    let requirements = collect_requirements(&store)?;
    let orphan_doc_sections = collect_orphan_doc_sections(&store)?;
    let orphan_symbols = collect_orphan_symbols(&store)?;
    let orphan_tests = collect_orphan_tests(&store)?;

    Ok(EvidencePack {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        repo_root: repo_root.to_string_lossy().into_owned(),
        requirements,
        orphan_doc_sections,
        orphan_symbols,
        orphan_tests,
        prompt: PROMPT.to_string(),
    })
}

fn collect_requirements(store: &Store) -> Result<Vec<EvidenceRequirement>> {
    let mut out = Vec::new();
    for req in store.list_nodes_by_kind(NodeKind::Requirement)? {
        let mut linked_docs = Vec::new();
        let mut linked_impls = Vec::new();
        let mut linked_tests = Vec::new();
        for edge in store.list_edges_to(&req.id)? {
            let Some(spec) = node_spec_for_edge(store, &edge.from_id)? else {
                continue;
            };
            match edge.kind {
                EdgeKind::Documents => linked_docs.push(spec),
                EdgeKind::DeclaresImplementation => linked_impls.push(spec),
                EdgeKind::DeclaresVerification => linked_tests.push(spec),
                _ => {}
            }
        }
        linked_docs.sort();
        linked_docs.dedup();
        linked_impls.sort();
        linked_impls.dedup();
        linked_tests.sort();
        linked_tests.dedup();

        out.push(EvidenceRequirement {
            id: req.stable_key.clone().unwrap_or_else(|| req.id.to_string()),
            title: req.name.clone(),
            path: req.path.clone(),
            missing_docs: linked_docs.is_empty(),
            missing_implementations: linked_impls.is_empty(),
            missing_tests: linked_tests.is_empty(),
            linked_docs,
            linked_implementations: linked_impls,
            linked_tests,
        });
    }
    Ok(out)
}

fn collect_orphan_doc_sections(store: &Store) -> Result<Vec<EvidenceDocSection>> {
    let mut out = Vec::new();
    for node in store.list_nodes_by_kind(NodeKind::DocSection)? {
        let edges_out = store.list_edges_from(&node.id)?;
        if edges_out.iter().any(|e| e.kind == EdgeKind::Documents) {
            continue;
        }
        let (Some(path), Some(name)) = (node.path.clone(), node.name.clone()) else {
            continue;
        };
        let lines = line_range(&node);
        out.push(EvidenceDocSection {
            path,
            name,
            slug: node.stable_key.unwrap_or_default(),
            line_range: lines,
        });
    }
    Ok(out)
}

fn collect_orphan_symbols(store: &Store) -> Result<Vec<EvidenceSymbol>> {
    let mut out = Vec::new();
    for kind in [
        NodeKind::DartClass,
        NodeKind::DartMethod,
        NodeKind::DartFunction,
        NodeKind::DartConstructor,
        NodeKind::SwiftClass,
        NodeKind::SwiftStruct,
        NodeKind::SwiftEnum,
        NodeKind::SwiftProtocol,
        NodeKind::SwiftMethod,
        NodeKind::SwiftFunction,
        NodeKind::SwiftInitializer,
        NodeKind::GoStruct,
        NodeKind::GoInterface,
        NodeKind::GoMethod,
        NodeKind::GoFunction,
        NodeKind::PythonModule,
        NodeKind::PythonClass,
        NodeKind::PythonFunction,
        NodeKind::PythonMethod,
    ] {
        for node in store.list_nodes_by_kind(kind)? {
            let edges_out = store.list_edges_from(&node.id)?;
            if edges_out
                .iter()
                .any(|e| e.kind == EdgeKind::DeclaresImplementation)
            {
                continue;
            }
            let (Some(path), Some(name)) = (node.path.clone(), node.name.clone()) else {
                continue;
            };
            let lines = line_range(&node);
            out.push(EvidenceSymbol {
                id: node.id.to_string(),
                kind: kind.as_str().to_string(),
                path,
                name,
                qualified_name: node.stable_key,
                line_range: lines,
            });
        }
    }
    Ok(out)
}

fn collect_orphan_tests(store: &Store) -> Result<Vec<EvidenceTest>> {
    let mut out = Vec::new();
    for kind in [NodeKind::TestCase, NodeKind::TestGroup] {
        for node in store.list_nodes_by_kind(kind)? {
            let edges_out = store.list_edges_from(&node.id)?;
            if edges_out
                .iter()
                .any(|e| e.kind == EdgeKind::DeclaresVerification)
            {
                continue;
            }
            let (Some(path), Some(name)) = (node.path.clone(), node.name.clone()) else {
                continue;
            };
            out.push(EvidenceTest {
                id: node.id.to_string(),
                kind: kind.as_str().to_string(),
                path,
                name,
                line_range: line_range(&node),
            });
        }
    }
    Ok(out)
}

fn line_range(node: &specslice_core::Node) -> Option<(u32, u32)> {
    match (node.start_line, node.end_line) {
        (Some(s), Some(e)) => Some((s, e)),
        _ => None,
    }
}

fn node_spec_for_edge(store: &Store, id: &ArtifactId) -> Result<Option<String>> {
    let Some(node) = store.find_node(id)? else {
        return Ok(None);
    };
    let Some(path) = node.path else {
        return Ok(None);
    };
    let name = node
        .name
        .or(node.stable_key)
        .unwrap_or_else(|| node.id.to_string());
    Ok(Some(format!("{path}#{name}")))
}

// ---------------------------------------------------------------------------
// apply_candidates
// ---------------------------------------------------------------------------

pub fn apply_candidates(options: ApplyOptions) -> Result<ApplyOutcome> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;

    let raw = std::fs::read_to_string(&options.candidates_path)
        .with_context(|| format!("reading candidates {}", options.candidates_path.display()))?;
    let doc: CandidatesDocument = serde_yaml::from_str(&raw).with_context(|| {
        format!(
            "parsing candidates file {}",
            options.candidates_path.display()
        )
    })?;

    let manifest_rel = config.links.path.clone();
    let manifest_abs = if Path::new(&manifest_rel).is_absolute() {
        PathBuf::from(&manifest_rel)
    } else {
        options.repo_root.join(&manifest_rel)
    };

    let mut manifest = load_existing_manifest(&manifest_abs)?;
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();

    for candidate in doc.candidates {
        match validate_candidate(&store, &candidate)? {
            Validated::Ok {
                docs,
                implementations,
                tests,
            } => {
                let entry = manifest.entry(candidate.requirement.clone()).or_default();
                merge_unique(&mut entry.docs, &docs);
                merge_unique(&mut entry.implementations, &implementations);
                merge_unique(&mut entry.tests, &tests);
                accepted.push(AcceptedCandidate {
                    requirement: candidate.requirement,
                    docs,
                    implementations,
                    tests,
                });
            }
            Validated::Rejected(reason) => {
                rejected.push(RejectedCandidate {
                    requirement: candidate.requirement.clone(),
                    reason,
                    raw: candidate,
                });
            }
        }
    }

    let manifest_path_rel = manifest_rel.clone();
    if !accepted.is_empty() && !options.dry_run {
        write_manifest(&manifest_abs, &manifest)?;
    }

    Ok(ApplyOutcome {
        accepted,
        rejected,
        manifest_path: manifest_path_rel,
        dry_run: options.dry_run,
    })
}

enum Validated {
    Ok {
        docs: Vec<String>,
        implementations: Vec<String>,
        tests: Vec<String>,
    },
    Rejected(String),
}

fn validate_candidate(store: &Store, candidate: &LinkCandidate) -> Result<Validated> {
    let mut reasons = Vec::new();
    let docs = validate_refs(store, &candidate.docs, RefKind::Doc, &mut reasons)?;
    let implementations = validate_refs(
        store,
        &candidate.implementations,
        RefKind::Impl,
        &mut reasons,
    )?;
    let tests = validate_refs(store, &candidate.tests, RefKind::Test, &mut reasons)?;
    if !reasons.is_empty() {
        return Ok(Validated::Rejected(reasons.join("; ")));
    }
    if docs.is_empty() && implementations.is_empty() && tests.is_empty() {
        return Ok(Validated::Rejected(
            "candidate carries no docs/implementations/tests".into(),
        ));
    }
    Ok(Validated::Ok {
        docs,
        implementations,
        tests,
    })
}

enum RefKind {
    Doc,
    Impl,
    Test,
}

fn validate_refs(
    store: &Store,
    refs: &[String],
    kind: RefKind,
    reasons: &mut Vec<String>,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for spec in refs {
        let resolved = match kind {
            RefKind::Doc => strict_resolve_doc(store, spec)?,
            RefKind::Impl => strict_resolve_implementation(store, spec)?,
            RefKind::Test => strict_resolve_test(store, spec)?,
        };
        if resolved.is_some() {
            out.push(spec.clone());
        } else {
            reasons.push(format!("cannot resolve `{}`", spec));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Manifest IO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct WriteManifest {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    requirements: BTreeMap<String, WriteRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct WriteRequirement {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    docs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    implementations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tests: Vec<String>,
}

fn load_existing_manifest(path: &Path) -> Result<BTreeMap<String, WriteRequirement>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading existing manifest {}", path.display()))?;
    let parsed: WriteManifest = serde_yaml::from_str(&raw)
        .with_context(|| format!("parsing existing manifest {}", path.display()))?;
    Ok(parsed.requirements)
}

fn write_manifest(path: &Path, manifest: &BTreeMap<String, WriteRequirement>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent for {}", path.display()))?;
    }
    let doc = WriteManifest {
        requirements: manifest.clone(),
    };
    let yaml = serde_yaml::to_string(&doc)
        .with_context(|| format!("serialising manifest {}", path.display()))?;
    std::fs::write(path, yaml).with_context(|| format!("writing manifest {}", path.display()))?;
    Ok(())
}

fn merge_unique(dst: &mut Vec<String>, src: &[String]) {
    for item in src {
        if !dst.iter().any(|existing| existing == item) {
            dst.push(item.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Config helpers (duplicated locally to keep connect.rs self-contained)
// ---------------------------------------------------------------------------

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
