//! P1: AI candidate links + human confirmation.
//!
//! `groundgraph connect` is split into two file-driven phases so the AI step
//! stays external:
//!
//! 1. [`propose_evidence`] reads the indexed graph and produces an
//!    [`EvidencePack`] — the set of facts an AI should ground itself in
//!    (requirements with their current links, orphan symbols/tests, etc).
//!    The user feeds this to whichever model they trust; the model returns a
//!    candidates YAML.
//! 2. [`apply_candidates`] loads that candidates file, validates every
//!    reference against the graph (existence, locatability), and merges
//!    accepted candidates into `.groundgraph/links.yaml`. Anything the resolver
//!    cannot locate is reported as rejected. Rules never invent business
//!    links — they only verify references.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use groundgraph_core::{ArtifactId, EdgeKind, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::config::{resolve_storage_path, EngineConfig};
use crate::error::EngineResult;
use crate::links_indexer::{
    strict_resolve_doc, strict_resolve_implementation, strict_resolve_test,
};

pub const EVIDENCE_SCHEMA_VERSION: u32 = 1;
pub const CANDIDATES_SCHEMA_VERSION: u32 = 1;

const PROMPT: &str = "You are GroundGraph's external candidate generator. Read the evidence pack \
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

pub fn propose_evidence(repo_root: &Path) -> EngineResult<EvidencePack> {
    let config = load_config(repo_root)?;
    let db_path = resolve_storage_path(repo_root, &config)?;
    let store = Store::open(&db_path)?;

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
    // Collapse the per-requirement `list_edges_to` N+1 (issues.md #158) into
    // a single `list_edges_by_kinds` query, bucketed in memory by `to_id`.
    // Only the three evidence kinds are fetched, and `list_edges_by_kinds`
    // returns rows ordered by id — the same order the per-requirement scan
    // produced — so each bucket's sort + dedup is byte-identical to before.
    let evidence_kinds = [
        EdgeKind::Documents,
        EdgeKind::DeclaresImplementation,
        EdgeKind::DeclaresVerification,
    ];
    let mut by_req: BTreeMap<ArtifactId, Vec<(EdgeKind, ArtifactId)>> = BTreeMap::new();
    for edge in store.list_edges_by_kinds(&evidence_kinds)? {
        by_req
            .entry(edge.to_id.clone())
            .or_default()
            .push((edge.kind, edge.from_id.clone()));
    }

    let mut out = Vec::new();
    for req in store.list_nodes_by_kind(NodeKind::Requirement)? {
        let mut linked_docs = Vec::new();
        let mut linked_impls = Vec::new();
        let mut linked_tests = Vec::new();
        if let Some(edges) = by_req.get(&req.id) {
            for (kind, from_id) in edges {
                let Some(spec) = node_spec_for_edge(store, from_id)? else {
                    continue;
                };
                match kind {
                    EdgeKind::Documents => linked_docs.push(spec),
                    EdgeKind::DeclaresImplementation => linked_impls.push(spec),
                    EdgeKind::DeclaresVerification => linked_tests.push(spec),
                    _ => {}
                }
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

fn line_range(node: &groundgraph_core::Node) -> Option<(u32, u32)> {
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

/// Resolve the links-manifest path from (untrusted) `.groundgraph.yaml`, confined
/// to the workspace. `links.path` is controlled by the *target* repo, so an
/// absolute path or one escaping via `..` would let a poisoned config make
/// `connect apply` create/overwrite files anywhere (e.g. `/etc/cron.d/...`,
/// #199). Refuse those; a relative in-repo path is the only legitimate case.
fn confine_manifest_path(repo_root: &Path, links_path: &str) -> Result<PathBuf> {
    let rel = Path::new(links_path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!(
            "config `links.path` ({links_path}) must be a workspace-relative path without `..`; \
             absolute paths and parent-dir escapes are refused so a poisoned .groundgraph.yaml \
             cannot write outside the repo"
        );
    }
    Ok(repo_root.join(rel))
}

pub fn apply_candidates(options: ApplyOptions) -> EngineResult<ApplyOutcome> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config)?;
    let store = Store::open(&db_path)?;

    let raw = std::fs::read_to_string(&options.candidates_path)
        .with_context(|| format!("reading candidates {}", options.candidates_path.display()))?;
    let doc: CandidatesDocument = serde_norway::from_str(&raw).with_context(|| {
        format!(
            "parsing candidates file {}",
            options.candidates_path.display()
        )
    })?;

    let manifest_abs = confine_manifest_path(&options.repo_root, &config.links.path)?;

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

    let manifest_path_rel = config.links.path.clone();
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
    let parsed: WriteManifest = serde_norway::from_str(&raw)
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
    let yaml = serde_norway::to_string(&doc)
        .with_context(|| format!("serialising manifest {}", path.display()))?;
    crate::atomic_write::write_atomic(path, &yaml)
        .with_context(|| format!("writing manifest {}", path.display()))?;
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

fn load_config(repo_root: &Path) -> crate::error::EngineResult<EngineConfig> {
    crate::config::load_config(repo_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confine_manifest_path_rejects_traversal_and_accepts_relative() {
        let root = Path::new("/work/repo");
        // The legitimate default — a workspace-relative manifest.
        assert_eq!(
            confine_manifest_path(root, ".groundgraph/links.yaml").unwrap(),
            root.join(".groundgraph/links.yaml")
        );
        // Absolute path from a poisoned config → refused (#199).
        assert!(confine_manifest_path(root, "/etc/cron.d/payload").is_err());
        // `..` escape → refused, even though it stays "relative".
        assert!(confine_manifest_path(root, "../../etc/passwd").is_err());
        assert!(confine_manifest_path(root, ".groundgraph/../../escape").is_err());
    }

    use groundgraph_core::{EdgeAssertion, EdgeSource, Node};

    fn empty_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn requirement(store: &mut Store, stable_key: &str, name: &str) -> ArtifactId {
        let aid = ArtifactId::new(format!("req::{stable_key}"));
        let mut node = Node::new(aid.clone(), NodeKind::Requirement);
        node.stable_key = Some(stable_key.to_string());
        node.name = Some(name.to_string());
        store.upsert_node(&node).unwrap();
        aid
    }

    /// Insert a node whose `node_spec_for_edge` serialises to `{path}#{name}`.
    fn evidence_symbol(store: &mut Store, id: &str, path: &str, name: &str) -> ArtifactId {
        let aid = ArtifactId::new(id.to_string());
        let mut node = Node::new(aid.clone(), NodeKind::DocSection);
        node.path = Some(path.to_string());
        node.name = Some(name.to_string());
        store.upsert_node(&node).unwrap();
        aid
    }

    fn link(store: &mut Store, from: &ArtifactId, to: &ArtifactId, kind: EdgeKind) {
        let e = EdgeAssertion::fact(from.clone(), to.clone(), kind, EdgeSource::LanguageAdapter);
        store.upsert_edge(&e).unwrap();
    }

    /// issues.md #158: pin the per-requirement evidence buckets so the
    /// N+1 → single-query refactor keeps `linked_docs` / `linked_implementations`
    /// / `linked_tests` byte-identical, including sort + dedup and the
    /// exclusion of unrelated edge kinds (e.g. `Calls`).
    #[test]
    fn collect_requirements_buckets_three_evidence_kinds_correctly() {
        let (mut store, _dir) = empty_store();
        let r1 = requirement(&mut store, "REQ-1", "Login");
        let _r2 = requirement(&mut store, "REQ-2", "Search");

        let doc = evidence_symbol(&mut store, "doc::auth", "docs/auth.md", "Login");
        let impl1 = evidence_symbol(&mut store, "py::auth", "src/auth.py", "login");
        let impl2 = evidence_symbol(&mut store, "ts::auth", "src/auth.ts", "login");
        let test = evidence_symbol(&mut store, "test::auth", "tests/auth_test.py", "test_login");

        link(&mut store, &doc, &r1, EdgeKind::Documents);
        link(&mut store, &impl1, &r1, EdgeKind::DeclaresImplementation);
        link(&mut store, &impl2, &r1, EdgeKind::DeclaresImplementation);
        link(&mut store, &impl1, &r1, EdgeKind::DeclaresImplementation); // dup → deduped
        link(&mut store, &test, &r1, EdgeKind::DeclaresVerification);
        // An unrelated Calls edge on R1 must NOT leak into any bucket.
        link(&mut store, &impl2, &r1, EdgeKind::Calls);

        let reqs = collect_requirements(&store).unwrap();
        let r1e = reqs
            .iter()
            .find(|r| r.id == "REQ-1")
            .expect("REQ-1 present");
        let r2e = reqs
            .iter()
            .find(|r| r.id == "REQ-2")
            .expect("REQ-2 present");

        assert_eq!(r1e.linked_docs, vec!["docs/auth.md#Login"]);
        assert_eq!(
            r1e.linked_implementations,
            vec!["src/auth.py#login", "src/auth.ts#login"]
        );
        assert_eq!(r1e.linked_tests, vec!["tests/auth_test.py#test_login"]);
        assert!(!r1e.missing_docs);
        assert!(!r1e.missing_implementations);
        assert!(!r1e.missing_tests);

        assert!(r2e.linked_docs.is_empty());
        assert!(r2e.linked_implementations.is_empty());
        assert!(r2e.linked_tests.is_empty());
        assert!(r2e.missing_docs);
        assert!(r2e.missing_implementations);
        assert!(r2e.missing_tests);
    }
}
