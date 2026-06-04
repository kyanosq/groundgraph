//! P24 — Business evidence pack (`specslice propose`).
//!
//! The product goal is *building business documentation from code*. The
//! P9 layer already defines the on-disk shape for AI-authored business
//! claims (`.specslice/candidates/business_logic.yaml`) and the human
//! review loop (`specslice candidate review`), but the *generation* of
//! those candidates was manual: an analyst had to trawl the graph by
//! hand. `connect propose` (P1) only emits a flat, link-oriented pack
//! (orphan symbols/tests) and is too slow on real repos to be usable
//! for this purpose.
//!
//! This module closes that gap. [`propose_business_pack`] reads the
//! indexed graph and produces a **per-business-module evidence pack**:
//!
//! - It segments code/test symbols into *business modules* from the
//!   **code graph itself** — deterministic Louvain community detection
//!   over the call/import coupling (see [`crate::feature_cluster`]). The
//!   target repo is *not* assumed to be tidily foldered: a feature whose
//!   files are scattered across `lib/models`, `lib/services`, `lib/ui`
//!   still clusters together because its symbols call each other densely.
//!   Directory convention (`lib/features/<x>`) is used only to *name* a
//!   community and as a fallback bucket for files with no edges.
//! - For each module it rolls up the **business signals** already on the
//!   graph: framework roles (routes/tasks/CLI), Riverpod providers read,
//!   storage written, navigation routes, stream subscriptions, the
//!   representative entry-point symbols, the related docs and tests, and
//!   the cross-module dependencies (imports/calls that cross a module
//!   boundary).
//! - It emits a Chinese prompt instructing an external AI to turn the
//!   pack into `business_logic.yaml` candidates — grounded *only* in the
//!   evidence ids present in the pack, never inventing paths/names.
//!
//! The pack is the input to the existing P9 → human-confirmation loop:
//!
//! ```text
//! specslice propose            (this module: code facts -> evidence pack + prompt)
//!   -> AI writes business_logic.yaml   (grounded in the pack)
//!   -> specslice candidate review      (human confirms / rejects)
//!   -> confirmed business graph + business doc export
//! ```
//!
//! The whole pass is a single in-memory load of the graph (like
//! `specslice features` after its P23.13 fix), so it stays sub-second on
//! large repos where `connect propose` timed out.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::language_traits;
use specslice_core::{ArtifactId, EdgeAssertion, EdgeKind, Node, NodeKind};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::feature_cluster::detect_communities;

pub const BUSINESS_PACK_SCHEMA_VERSION: u32 = 1;

/// Directory names that *contain* business modules — the module name is
/// the path segment immediately after one of these.
const FEATURE_MARKERS: &[&str] = &["features", "feature", "modules"];

/// Source-root directory names — when no explicit feature marker is
/// present, the module name is the segment immediately after one of
/// these (e.g. `lib/<module>`, `src/<module>`).
const SOURCE_ROOTS: &[&str] = &[
    "lib", "src", "app", "pkg", "packages", "internal", "crates", "backend",
];

/// Top-level bucket slugs that are *not* business modules — test
/// scaffolding, docs trees, build tooling, examples. They are still
/// counted in the totals (their symbols/tests/docs are real) but never
/// reported as a business module, so the AI never invents a "Test"
/// business candidate.
const NON_BUSINESS_BUCKETS: &[&str] = &[
    "test",
    "tests",
    "testing",
    "integration_test",
    "test_driver",
    "spec",
    "specs",
    "__tests__",
    "docs",
    "doc",
    "documentation",
    "tool",
    "tools",
    "script",
    "scripts",
    "bin",
    "build",
    "dist",
    "out",
    "target",
    "coverage",
    "example",
    "examples",
    "demo",
    "demos",
    "node_modules",
    "vendor",
    "third_party",
];

/// Callable names that carry no business meaning — framework lifecycle /
/// object plumbing. Demoted out of the entry-point ranking.
const NOISE_METHODS: &[&str] = &[
    "build",
    "dispose",
    "initstate",
    "tostring",
    "hashcode",
    "==",
    "nosuchmethod",
    "createstate",
    "didchangedependencies",
    "deactivate",
    "setstate",
    "main",
    "new",
    "default",
];

/// Symbol-name fragments that mark a *business entry point* — blocs,
/// use cases, repositories, screens, API clients, etc. Lower-cased
/// substring match against the symbol name.
const ENTRY_POINT_KEYWORDS: &[&str] = &[
    "bloc",
    "cubit",
    "notifier",
    "controller",
    "usecase",
    "use_case",
    "repository",
    "service",
    "screen",
    "page",
    "view",
    "widget",
    "provider",
    "apiclient",
    "api_client",
    "client",
    "manager",
    "handler",
    "mapper",
    "serializer",
    "interactor",
    "facade",
    "gateway",
];

#[derive(Debug, Clone)]
pub struct BusinessPackOptions {
    pub repo_root: PathBuf,
    /// Maximum number of business modules to report (highest signal first).
    pub max_modules: usize,
    /// Maximum representative entry-point symbols per module.
    pub max_entry_points: usize,
    /// Maximum sample values per signal list (routes/providers/storage/...).
    pub max_signal_samples: usize,
}

impl Default for BusinessPackOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            max_modules: 40,
            max_entry_points: 8,
            max_signal_samples: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessPack {
    pub schema_version: u32,
    pub repo_root: String,
    pub stats: BusinessPackStats,
    pub modules: Vec<ModuleEvidence>,
    pub module_dependencies: Vec<ModuleDependency>,
    /// Chinese instructions for the external AI that turns this pack into
    /// `business_logic.yaml` candidates.
    pub prompt: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessPackStats {
    pub total_modules: usize,
    pub modules_reported: usize,
    pub total_symbols: usize,
    pub assigned_symbols: usize,
    pub unassigned_symbols: usize,
    pub total_docs: usize,
    pub total_tests: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleEvidence {
    /// Stable slug, valid as a `business_candidate::<id>` id.
    pub id: String,
    /// Human-readable module name (the feature folder, prettified).
    pub name: String,
    /// Repo-relative path prefix that anchors this module.
    pub path_prefix: String,
    pub file_count: usize,
    pub symbol_count: usize,
    pub test_count: usize,
    /// Heuristic "this module looks like a coherent business surface"
    /// score; drives ordering. Higher = stronger signal.
    pub signal_score: u32,
    /// Graph cohesion in `0.0..=1.0`: share of this module's structural
    /// coupling that stays *inside* the module (internal / (internal +
    /// external)). High = self-contained feature; low = leaky / entangled.
    /// Derived from the call/import community, not the directory layout.
    pub cohesion: f64,
    /// Representative business symbols (blocs / use cases / repositories
    /// / screens / framework entry points), strongest first.
    pub entry_points: Vec<EvidenceSymbol>,
    /// Navigation routes reached from this module (`navigates_to`).
    pub routes: Vec<String>,
    /// Riverpod providers read by this module (`reads_provider`).
    pub providers: Vec<String>,
    /// Storage buckets written by this module (`persists_to`).
    pub storage: Vec<String>,
    /// Number of `subscribes_stream` edges originating in this module.
    pub stream_subscriptions: usize,
    /// Framework families detected on this module's symbols.
    pub framework_roles: Vec<String>,
    /// Documentation sections that describe this module.
    pub docs: Vec<EvidenceRef>,
    /// Tests that verify this module.
    pub tests: Vec<EvidenceRef>,
    /// Other module ids this module depends on (outgoing imports/calls).
    pub depends_on: Vec<String>,
    /// Curated node-id evidence list, ready to paste into a
    /// `business_logic.yaml` candidate's `evidence:` field.
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSymbol {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub line_range: Option<(u32, u32)>,
    /// Framework families detected on this symbol (may be empty).
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub id: String,
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleDependency {
    pub from: String,
    pub to: String,
    pub weight: usize,
}

/// Open the store from `.specslice.yaml` and build the pack.
pub fn propose_business_pack(options: BusinessPackOptions) -> Result<BusinessPack> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    propose_business_pack_with_store(&store, &options)
}

pub fn propose_business_pack_with_store(
    store: &Store,
    options: &BusinessPackOptions,
) -> Result<BusinessPack> {
    let nodes = store.list_all_nodes().context("listing nodes")?;
    let edges = store.list_all_edges().context("listing edges")?;
    Ok(build_pack(&nodes, &edges, options))
}

fn build_pack(
    nodes: &[Node],
    edges: &[EdgeAssertion],
    options: &BusinessPackOptions,
) -> BusinessPack {
    let nodes_by_id: HashMap<&ArtifactId, &Node> = nodes.iter().map(|n| (&n.id, n)).collect();

    // ---- 0. partition files into business modules FROM THE CODE GRAPH ----
    // Communities are detected on the call/import coupling, not the folder
    // layout (see `partition_modules`). This is the key correctness change:
    // a repo organised by layer (`lib/models`, `lib/services`) or one that
    // is simply messy still yields feature-shaped modules, because a
    // feature's symbols call each other far more than they call other
    // features'. Paths are consulted only to *name* a community.
    let partition = partition_modules(nodes, edges);

    // ---- 1. assign every code symbol / test / file to its module --------
    // Docs are matched to a module separately (normalised segment match)
    // because doc trees often sit under `docs/feature-docs/<x>` rather than
    // the source layout.
    let mut module_of_node: HashMap<&ArtifactId, String> = HashMap::new();
    let mut modules: BTreeMap<String, ModuleAcc> = BTreeMap::new();

    let mut total_symbols = 0usize;
    let mut assigned_symbols = 0usize;
    let mut total_tests = 0usize;
    let mut total_docs = 0usize;

    for node in nodes {
        let is_symbol =
            language_traits::is_callable(node.kind) || language_traits::is_type(node.kind);
        let is_test = node.kind.is_test();
        if is_symbol {
            total_symbols += 1;
        }
        if is_test {
            total_tests += 1;
        }
        if node.kind == NodeKind::DocSection {
            total_docs += 1;
            continue; // docs handled in a later pass
        }
        let Some(path) = node.path.as_deref() else {
            continue;
        };
        let Some(cidx) = partition.community_of(path) else {
            continue;
        };
        let slug = partition.slug[cidx].clone();
        let acc = modules.entry(slug.clone()).or_insert_with(|| ModuleAcc {
            slug: slug.clone(),
            name: partition.name[cidx].clone(),
            path_prefix: partition.prefix[cidx].clone(),
            cohesion: partition.cohesion[cidx],
            ..Default::default()
        });
        module_of_node.insert(&node.id, slug);

        if node.kind == NodeKind::File {
            acc.files.insert(path.to_string());
        } else if is_test {
            acc.tests.push(node);
        } else if is_symbol {
            assigned_symbols += 1;
            acc.symbols.push(node);
            // framework role on the symbol
            if let Some(family) = framework_family(node) {
                acc.framework_roles.insert(family);
            }
        }
    }

    let known_slugs: BTreeSet<String> = modules.keys().cloned().collect();

    // ---- 2. associate docs by normalised segment match ------------------
    for node in nodes {
        if node.kind != NodeKind::DocSection {
            continue;
        }
        let Some(path) = node.path.as_deref() else {
            continue;
        };
        if let Some(slug) = match_doc_to_module(path, &known_slugs) {
            if let Some(acc) = modules.get_mut(&slug) {
                acc.docs.push(node);
            }
        }
    }

    // ---- 3. roll up semantic signals + dependencies from edges ----------
    let mut dependency_weights: BTreeMap<(String, String), usize> = BTreeMap::new();
    for edge in edges {
        let Some(from_mod) = module_of_node.get(&edge.from_id) else {
            continue;
        };
        match edge.kind {
            EdgeKind::NavigatesTo => {
                if let Some(target) = nodes_by_id.get(&edge.to_id) {
                    if let Some(acc) = modules.get_mut(from_mod) {
                        acc.routes.insert(display_label(target));
                    }
                }
            }
            EdgeKind::ReadsProvider => {
                if let Some(target) = nodes_by_id.get(&edge.to_id) {
                    if let Some(acc) = modules.get_mut(from_mod) {
                        acc.providers.insert(display_label(target));
                    }
                }
            }
            EdgeKind::PersistsTo => {
                if let Some(target) = nodes_by_id.get(&edge.to_id) {
                    if let Some(acc) = modules.get_mut(from_mod) {
                        acc.storage.insert(display_label(target));
                    }
                }
            }
            EdgeKind::SubscribesStream => {
                if let Some(acc) = modules.get_mut(from_mod) {
                    acc.stream_subscriptions += 1;
                }
            }
            EdgeKind::Calls | EdgeKind::References | EdgeKind::Imports => {
                let from_mod = from_mod.clone();
                // in-degree of the target module (used for entry-point ranking)
                in_degree_bump(&mut modules, &edge.to_id, &module_of_node);
                // cross-module dependency
                if let Some(to_mod) = module_of_node.get(&edge.to_id) {
                    if *to_mod != from_mod {
                        *dependency_weights
                            .entry((from_mod, to_mod.clone()))
                            .or_default() += 1;
                    }
                }
            }
            _ => {}
        }
    }

    // ---- 4. materialise module reports ----------------------------------
    let mut reports: Vec<ModuleEvidence> = Vec::new();
    for acc in modules.values() {
        // scaffolding buckets are real (counted in totals) but never a
        // business module — skip them at the reporting stage.
        if NON_BUSINESS_BUCKETS.contains(&acc.slug.as_str()) {
            continue;
        }
        let entry_points = acc.select_entry_points(options.max_entry_points);
        // Docs/tests are graphed per *section* / per *case*; collapse each to
        // one evidence per file so a heading-heavy doc or case-heavy test
        // file does not flood the module.
        let mut docs = dedup_refs_by_path(&acc.docs);
        let unique_doc_files = docs.len();
        docs.truncate(options.max_signal_samples);
        let mut tests = dedup_refs_by_path(&acc.tests);
        tests.truncate(options.max_signal_samples);

        let signal_score = acc.signal_score(&entry_points, unique_doc_files);
        let evidence = build_evidence_list(&entry_points, &docs, &tests);

        reports.push(ModuleEvidence {
            id: acc.slug.clone(),
            name: acc.name.clone(),
            path_prefix: acc.path_prefix.clone(),
            file_count: acc.files.len(),
            symbol_count: acc.symbols.len(),
            test_count: acc.tests.len(),
            signal_score,
            cohesion: acc.cohesion,
            entry_points,
            routes: capped_sorted(&acc.routes, options.max_signal_samples),
            providers: capped_sorted(&acc.providers, options.max_signal_samples),
            storage: capped_sorted(&acc.storage, options.max_signal_samples),
            stream_subscriptions: acc.stream_subscriptions,
            framework_roles: acc.framework_roles.iter().cloned().collect(),
            docs,
            tests,
            depends_on: Vec::new(), // filled below once we know which modules survive
            evidence,
        });
    }

    // Drop trivial modules (no symbols at all) and order by signal.
    reports.retain(|m| m.symbol_count > 0 || !m.docs.is_empty());
    // `total_modules` counts the discovered *business* modules (after the
    // scaffolding denylist), independent of the `max_modules` cap.
    let total_modules = reports.len();
    reports.sort_by(|a, b| {
        b.signal_score
            .cmp(&a.signal_score)
            .then(b.symbol_count.cmp(&a.symbol_count))
            .then(a.name.cmp(&b.name))
    });
    reports.truncate(options.max_modules);

    let surviving: BTreeSet<String> = reports.iter().map(|m| m.id.clone()).collect();

    // depends_on per module (restricted to surviving modules)
    let mut deps_by_module: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut module_dependencies: Vec<ModuleDependency> = Vec::new();
    for ((from, to), weight) in &dependency_weights {
        if !surviving.contains(from) || !surviving.contains(to) {
            continue;
        }
        deps_by_module
            .entry(from.clone())
            .or_default()
            .insert(to.clone());
        module_dependencies.push(ModuleDependency {
            from: from.clone(),
            to: to.clone(),
            weight: *weight,
        });
    }
    module_dependencies.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then(a.from.cmp(&b.from))
            .then(a.to.cmp(&b.to))
    });
    for m in reports.iter_mut() {
        if let Some(deps) = deps_by_module.get(&m.id) {
            m.depends_on = deps.iter().cloned().collect();
        }
    }

    let modules_reported = reports.len();
    BusinessPack {
        schema_version: BUSINESS_PACK_SCHEMA_VERSION,
        repo_root: options.repo_root.to_string_lossy().into_owned(),
        stats: BusinessPackStats {
            total_modules,
            modules_reported,
            total_symbols,
            assigned_symbols,
            unassigned_symbols: total_symbols.saturating_sub(assigned_symbols),
            total_docs,
            total_tests,
        },
        modules: reports,
        module_dependencies,
        prompt: prompt_text(),
    }
}

/// Bump the in-degree counter for `target`'s module. Kept as a free fn so
/// the borrow of `modules` is scoped tightly inside the edge loop.
fn in_degree_bump(
    modules: &mut BTreeMap<String, ModuleAcc>,
    target: &ArtifactId,
    module_of_node: &HashMap<&ArtifactId, String>,
) {
    if let Some(slug) = module_of_node.get(target) {
        if let Some(acc) = modules.get_mut(slug) {
            *acc.in_degree.entry(target.to_string()).or_default() += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Per-module accumulator
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ModuleAcc<'a> {
    slug: String,
    name: String,
    path_prefix: String,
    cohesion: f64,
    files: BTreeSet<String>,
    symbols: Vec<&'a Node>,
    tests: Vec<&'a Node>,
    docs: Vec<&'a Node>,
    routes: BTreeSet<String>,
    providers: BTreeSet<String>,
    storage: BTreeSet<String>,
    framework_roles: BTreeSet<String>,
    stream_subscriptions: usize,
    in_degree: HashMap<String, usize>,
}

impl ModuleAcc<'_> {
    fn select_entry_points(&self, limit: usize) -> Vec<EvidenceSymbol> {
        let mut scored: Vec<(i64, &Node)> = self
            .symbols
            .iter()
            .map(|n| (self.entry_point_score(n), *n))
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.path.cmp(&b.1.path))
                .then(a.1.name.cmp(&b.1.name))
                .then(a.1.id.as_str().cmp(b.1.id.as_str()))
        });
        scored
            .into_iter()
            .take(limit)
            .map(|(_, n)| EvidenceSymbol {
                id: n.id.to_string(),
                kind: n.kind.as_str().to_string(),
                name: display_label(n),
                path: n.path.clone().unwrap_or_default(),
                line_range: line_range(n),
                roles: framework_family(n).into_iter().collect(),
            })
            .collect()
    }

    fn entry_point_score(&self, node: &Node) -> i64 {
        // Symbols defined in test scaffolding (mocks/fakes/helpers) or in
        // generated codegen files (.freezed.dart / .g.dart / …) are not
        // business entry points; tests are covered by the tests list and
        // codegen is noise.
        if let Some(path) = node.path.as_deref() {
            if is_test_path(path) || is_generated_path(path) {
                return 0;
            }
        }
        let name = node
            .name
            .clone()
            .or_else(|| node.stable_key.clone())
            .unwrap_or_default();
        // Synthetic / unnamed symbols (e.g. Dart's `<default>` constructor)
        // are not business entry points.
        if name.is_empty() || name.contains('<') {
            return 0;
        }
        let name_lower = name.to_ascii_lowercase();
        // Strip a leading container (`Foo.bar` -> `bar`) for the noise check.
        let leaf = name_lower.rsplit('.').next().unwrap_or(&name_lower);
        if NOISE_METHODS.contains(&leaf) {
            return 0;
        }
        let mut score: i64 = 1;
        if framework_family(node).is_some() {
            score += 50;
        }
        if ENTRY_POINT_KEYWORDS
            .iter()
            .any(|kw| name_lower.contains(kw))
        {
            score += 20;
        }
        if language_traits::is_type(node.kind) {
            score += 5;
        }
        let indeg = self.in_degree.get(node.id.as_str()).copied().unwrap_or(0);
        score += i64::try_from(indeg.min(15)).unwrap_or(15);
        score
    }

    fn signal_score(&self, entry_points: &[EvidenceSymbol], doc_file_count: usize) -> u32 {
        let mut score: u32 = 0;
        score += u32::try_from(self.framework_roles.len().min(10)).unwrap_or(10) * 8;
        score += u32::try_from(self.routes.len().min(20)).unwrap_or(20) * 3;
        score += u32::try_from(self.providers.len().min(20)).unwrap_or(20) * 3;
        score += u32::try_from(self.storage.len().min(20)).unwrap_or(20) * 3;
        score += u32::try_from(self.tests.len().min(30)).unwrap_or(30) * 2;
        // score on unique doc *files*, not raw sections, so a heading-heavy
        // single doc cannot dominate the ranking.
        score += u32::try_from(doc_file_count.min(20)).unwrap_or(20) * 4;
        score += u32::try_from(entry_points.len().min(20)).unwrap_or(20);
        score += u32::try_from(self.symbols.len().min(200)).unwrap_or(200) / 10;
        score
    }
}

// ---------------------------------------------------------------------------
// Graph-driven module partition (code graph is the source of truth)
// ---------------------------------------------------------------------------

/// The result of clustering files into business modules from the call /
/// import graph. Maps each file path to a contiguous module index, with a
/// parallel-indexed slug / name / prefix / cohesion for each module.
struct ModulePartition {
    by_file: HashMap<String, usize>,
    slug: Vec<String>,
    name: Vec<String>,
    prefix: Vec<String>,
    cohesion: Vec<f64>,
}

impl ModulePartition {
    fn community_of(&self, path: &str) -> Option<usize> {
        self.by_file.get(path).copied()
    }
}

/// Cluster every code/test file into a business module.
///
/// The placement rule deliberately mixes two signals at *different*
/// confidence levels, which is what makes it robust to both tidy and
/// chaotic repos:
///
/// * **Explicit feature folders win.** A file under an unambiguous feature
///   marker (`.../features/<x>/...`, `.../modules/<x>/...`) is placed in
///   module `<x>`. This is a deliberate, business-level boundary the repo
///   author drew, and the graph invariably agrees (high cohesion). It also
///   *dissolves* the cross-feature "infrastructure blobs" community
///   detection would otherwise form (a feature's repository couples to
///   every other feature's repository), keeping modules feature-shaped.
/// * **Everything else follows the code graph.** Files with no explicit
///   feature marker — a flat `lib/`, a layer split (`lib/models`,
///   `lib/services`), an outright mess — are placed by Louvain community
///   over their call/import coupling. Loose path conventions
///   (`lib/<x>`, first-directory) are *never* trusted for placement,
///   because that is exactly where "the repo is managed chaotically"
///   bites; the graph is the source of truth there.
///
/// So a feature is recovered whether the author filed it under
/// `features/auth/` or smeared it across `models/`, `services/`, `ui/`.
fn partition_modules(nodes: &[Node], edges: &[EdgeAssertion]) -> ModulePartition {
    // ---- file universe: every file that holds code / tests -------------
    let mut file_idx: HashMap<String, usize> = HashMap::new();
    let mut files: Vec<String> = Vec::new();
    let mut node_file: HashMap<&ArtifactId, usize> = HashMap::new();
    for node in nodes {
        let relevant = node.kind == NodeKind::File
            || language_traits::is_callable(node.kind)
            || language_traits::is_type(node.kind)
            || node.kind.is_test();
        if !relevant {
            continue;
        }
        let Some(path) = node.path.as_deref() else {
            continue;
        };
        let idx = *file_idx.entry(path.to_string()).or_insert_with(|| {
            files.push(path.to_string());
            files.len() - 1
        });
        node_file.insert(&node.id, idx);
    }
    let n = files.len();
    if n == 0 {
        return ModulePartition {
            by_file: HashMap::new(),
            slug: Vec::new(),
            name: Vec::new(),
            prefix: Vec::new(),
            cohesion: Vec::new(),
        };
    }

    // ---- lift symbol coupling onto a weighted file graph ---------------
    // Calls / References (behavioural coupling) weigh more than Imports
    // (structural). Self-edges (same file) are skipped — we want *cross*-
    // file cohesion to define a module boundary.
    let mut sym_indegree: HashMap<&ArtifactId, usize> = HashMap::new();
    let mut weights: HashMap<(usize, usize), f64> = HashMap::new();
    for edge in edges {
        let w = match edge.kind {
            EdgeKind::Calls | EdgeKind::References => 2.0,
            EdgeKind::Imports => 1.0,
            _ => continue,
        };
        *sym_indegree.entry(&edge.to_id).or_insert(0) += 1;
        let (Some(&a), Some(&b)) = (node_file.get(&edge.from_id), node_file.get(&edge.to_id))
        else {
            continue;
        };
        if a == b {
            continue;
        }
        let key = if a < b { (a, b) } else { (b, a) };
        *weights.entry(key).or_insert(0.0) += w;
    }
    let edge_list: Vec<(usize, usize, f64)> =
        weights.iter().map(|(&(a, b), &w)| (a, b, w)).collect();
    let mut connected = vec![false; n];
    for &(a, b, _) in &edge_list {
        connected[a] = true;
        connected[b] = true;
    }

    // ---- community detection -------------------------------------------
    let comm = detect_communities(n, &edge_list);

    // ---- business symbols per file (for central-symbol naming) ---------
    let mut file_symbols: HashMap<usize, Vec<&Node>> = HashMap::new();
    for node in nodes {
        if !(language_traits::is_callable(node.kind) || language_traits::is_type(node.kind)) {
            continue;
        }
        if let Some(path) = node.path.as_deref() {
            if let Some(&fi) = file_idx.get(path) {
                file_symbols.entry(fi).or_default().push(node);
            }
        }
    }

    // ---- placement key per file ----------------------------------------
    // `feat::<slug>` for files under an explicit feature marker (trusted
    // business boundary), else `comm::<id>` for graph-clustered files, else
    // `path::<slug>` for isolated files with no graph signal. This single
    // key space is what later groups files into modules.
    let group_key: Vec<String> = (0..n)
        .map(|i| {
            if let Some(tok) = feature_marker_token(&files[i]) {
                format!("feat::{}", slugify(&tok))
            } else if connected[i] {
                format!("comm::{}", comm[i])
            } else {
                let b = feature_key(&files[i])
                    .map(|k| k.slug)
                    .unwrap_or_else(|| "misc".to_string());
                format!("path::{b}")
            }
        })
        .collect();

    // ---- group files by placement key, deterministic by key order ------
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, key) in group_key.iter().enumerate() {
        groups.entry(key.as_str()).or_default().push(i);
    }

    // ---- name each group, MERGING groups that derive the same slug -----
    // Distinct graph communities that resolve to the same identity *are*
    // the same module: e.g. a `lib/core` with no feature marker splits into
    // several loosely-coupled communities that all name themselves "core";
    // collapsing them avoids `core / core_2 / …` noise. Two genuinely
    // different features never collide because they get distinct names
    // (a dominant token or a distinct central symbol).
    let mut slug: Vec<String> = Vec::new();
    let mut name: Vec<String> = Vec::new();
    let mut prefix: Vec<String> = Vec::new();
    let mut slug_index: HashMap<String, usize> = HashMap::new();
    // placement key -> final module index
    let mut key_module: HashMap<&str, usize> = HashMap::new();

    for (&key, file_idxs) in &groups {
        let (disp, pfx) = name_module(key, file_idxs, &files, &file_symbols, &sym_indegree);
        let mut base = slugify(&disp);
        if base.is_empty() {
            base = format!("module_{}", slug.len() + 1);
        }
        let mi = match slug_index.get(&base) {
            Some(&existing) => {
                // merge: keep the shorter (more general) path anchor
                if !pfx.is_empty()
                    && (prefix[existing].is_empty() || pfx.len() < prefix[existing].len())
                {
                    prefix[existing] = pfx;
                }
                existing
            }
            None => {
                let idx = slug.len();
                slug_index.insert(base.clone(), idx);
                slug.push(base);
                name.push(prettify(&disp));
                prefix.push(pfx);
                idx
            }
        };
        key_module.insert(key, mi);
    }

    // ---- final file -> module index ------------------------------------
    let mut by_file: HashMap<String, usize> = HashMap::new();
    let mut module_of_file: Vec<usize> = vec![usize::MAX; n];
    for (i, path) in files.iter().enumerate() {
        if let Some(&mi) = key_module.get(group_key[i].as_str()) {
            by_file.insert(path.clone(), mi);
            module_of_file[i] = mi;
        }
    }

    // ---- cohesion per *final module* (internal / total coupling) -------
    let mut internal = vec![0.0f64; slug.len()];
    let mut external = vec![0.0f64; slug.len()];
    for &(a, b, w) in &edge_list {
        let (ma, mb) = (module_of_file[a], module_of_file[b]);
        if ma == usize::MAX || mb == usize::MAX {
            continue;
        }
        if ma == mb {
            internal[ma] += w;
        } else {
            external[ma] += w;
            external[mb] += w;
        }
    }
    let cohesion: Vec<f64> = (0..slug.len())
        .map(|i| {
            let total = internal[i] + external[i];
            if total > 0.0 {
                internal[i] / total
            } else {
                0.0
            }
        })
        .collect();

    ModulePartition {
        by_file,
        slug,
        name,
        prefix,
        cohesion,
    }
}

/// Return the feature token for a path **only** when it sits under an
/// explicit, unambiguous feature marker (`.../features/<x>/...`). Loose
/// conventions (`lib/<x>`, first directory) deliberately return `None` —
/// they are not trusted as business boundaries.
fn feature_marker_token(path: &str) -> Option<String> {
    let norm = path.replace('\\', "/");
    let raw: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if raw.is_empty() {
        return None;
    }
    let dirs: &[&str] = if raw.last().map(|s| s.contains('.')).unwrap_or(false) {
        &raw[..raw.len() - 1]
    } else {
        &raw[..]
    };
    for (i, seg) in dirs.iter().enumerate() {
        if FEATURE_MARKERS.contains(&seg.to_ascii_lowercase().as_str()) && i + 1 < dirs.len() {
            return Some(dirs[i + 1].to_string());
        }
    }
    None
}

/// Generic identifiers that carry no business meaning — never use one as a
/// module name even if it is the most-referenced symbol in a cluster.
const GENERIC_NAMES: &[&str] = &[
    "default",
    "load",
    "build",
    "get",
    "set",
    "init",
    "main",
    "run",
    "call",
    "create",
    "update",
    "delete",
    "fromjson",
    "tojson",
    "copywith",
    "tostring",
    "of",
    "instance",
    "value",
    "data",
    "state",
    "model",
    "result",
    "response",
    "request",
    "client",
    "service",
    "base",
    "common",
    "utils",
    "util",
    "helper",
    "helpers",
    "constants",
    "config",
];

/// Name a module group. `feat::` groups are named directly after their
/// feature marker; otherwise we try a dominant feature-folder token, then
/// the most-referenced *business* symbol (preferring types), and finally
/// the longest common directory.
fn name_module(
    key: &str,
    file_idxs: &[usize],
    files: &[String],
    file_symbols: &HashMap<usize, Vec<&Node>>,
    sym_indegree: &HashMap<&ArtifactId, usize>,
) -> (String, String) {
    // (0) explicit feature marker — authoritative. Anchor the path to a
    //     non-test source file so a feature spanning `lib/features/<x>` and
    //     `test/features/<x>` reports `lib/features/<x>`, not an empty
    //     prefix (their common directory diverges at the top level).
    if key.starts_with("feat::") {
        if let Some(tok) = file_idxs
            .iter()
            .find_map(|&fi| feature_marker_token(&files[fi]))
        {
            let pfx = file_idxs
                .iter()
                .filter(|&&fi| !is_test_path(&files[fi]))
                .find_map(|&fi| feature_key(&files[fi]).map(|k| k.prefix))
                .or_else(|| {
                    file_idxs
                        .iter()
                        .find_map(|&fi| feature_key(&files[fi]).map(|k| k.prefix))
                })
                .unwrap_or_default();
            return (tok, pfx);
        }
    }

    // (a) dominant feature-folder token
    let mut token_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut token_prefix: HashMap<String, String> = HashMap::new();
    for &fi in file_idxs {
        if let Some(k) = feature_key(&files[fi]) {
            *token_count.entry(k.display.clone()).or_insert(0) += 1;
            token_prefix.entry(k.display).or_insert(k.prefix);
        }
    }
    let dominant = token_count
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(d, c)| (d.clone(), *c));
    if let Some((disp, cnt)) = dominant {
        if cnt * 2 >= file_idxs.len() || token_count.len() == 1 {
            let pfx = token_prefix.get(&disp).cloned().unwrap_or_default();
            return (disp, pfx);
        }
    }

    // (b) most-referenced business symbol (types beat callables; generic
    //     names are skipped so we never surface "load"/"default").
    let mut best: Option<(usize, bool, String)> = None;
    for &fi in file_idxs {
        if let Some(syms) = file_symbols.get(&fi) {
            for s in syms {
                let Some(nm) = s.name.as_deref() else {
                    continue;
                };
                if nm.is_empty() || GENERIC_NAMES.contains(&nm.to_ascii_lowercase().as_str()) {
                    continue;
                }
                let deg = sym_indegree.get(&s.id).copied().unwrap_or(0);
                let is_ty = language_traits::is_type(s.kind);
                let better = match &best {
                    Some((bd, bt, bn)) => {
                        deg > *bd
                            || (deg == *bd && is_ty && !*bt)
                            || (deg == *bd && is_ty == *bt && nm < bn.as_str())
                    }
                    None => true,
                };
                if better {
                    best = Some((deg, is_ty, nm.to_string()));
                }
            }
        }
    }
    if let Some((deg, _, nm)) = best {
        if deg > 0 {
            return (strip_role_suffix(&nm), common_dir_prefix(file_idxs, files));
        }
    }

    // (c) fallback: last segment of the common directory prefix
    let pfx = common_dir_prefix(file_idxs, files);
    let disp = pfx
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("module")
        .to_string();
    (disp, pfx)
}

/// Strip a single trailing architectural-role word so a community
/// centred on `CheckoutController` is named "Checkout", not "Checkout
/// Controller". Only one suffix is removed and never the whole name.
fn strip_role_suffix(name: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "Controller",
        "Repository",
        "UseCase",
        "Usecase",
        "Service",
        "Provider",
        "Notifier",
        "Manager",
        "Handler",
        "Bloc",
        "Cubit",
        "ViewModel",
        "Screen",
        "Widget",
        "Page",
        "View",
        "Model",
        "State",
        "Store",
        "Client",
        "Impl",
    ];
    for suf in SUFFIXES {
        if name.len() > suf.len() && name.ends_with(suf) {
            return name[..name.len() - suf.len()].to_string();
        }
    }
    name.to_string()
}

/// Longest common directory prefix (filename dropped) across files.
fn common_dir_prefix(file_idxs: &[usize], files: &[String]) -> String {
    let mut split: Vec<Vec<String>> = Vec::new();
    for &fi in file_idxs {
        let norm = files[fi].replace('\\', "/");
        let mut parts: Vec<String> = norm
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if parts.last().map(|s| s.contains('.')).unwrap_or(false) {
            parts.pop();
        }
        if !parts.is_empty() {
            split.push(parts);
        }
    }
    if split.is_empty() {
        return String::new();
    }
    let mut prefix = split[0].clone();
    for s in &split[1..] {
        let mut i = 0;
        while i < prefix.len() && i < s.len() && prefix[i] == s[i] {
            i += 1;
        }
        prefix.truncate(i);
        if prefix.is_empty() {
            break;
        }
    }
    prefix.join("/")
}

// ---------------------------------------------------------------------------
// Feature-key segmentation (pure, testable)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct FeatureKey {
    slug: String,
    display: String,
    prefix: String,
}

/// Derive the business module a code/test path belongs to. Returns `None`
/// for paths with no usable directory segment.
fn feature_key(path: &str) -> Option<FeatureKey> {
    let norm = path.replace('\\', "/");
    let raw: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if raw.is_empty() {
        return None;
    }
    // Directory segments only: drop a trailing filename (has an extension).
    let dirs: Vec<&str> = if raw.last().map(|s| s.contains('.')).unwrap_or(false) {
        raw[..raw.len() - 1].to_vec()
    } else {
        raw.clone()
    };
    if dirs.is_empty() {
        return None;
    }

    // 1) explicit feature markers win.
    for (i, seg) in dirs.iter().enumerate() {
        if FEATURE_MARKERS.contains(&seg.to_ascii_lowercase().as_str()) && i + 1 < dirs.len() {
            return Some(make_key(dirs[i + 1], &dirs[..=i + 1]));
        }
    }
    // 2) segment after a source root.
    for (i, seg) in dirs.iter().enumerate() {
        if SOURCE_ROOTS.contains(&seg.to_ascii_lowercase().as_str()) && i + 1 < dirs.len() {
            return Some(make_key(dirs[i + 1], &dirs[..=i + 1]));
        }
    }
    // 3) fallback: first directory segment.
    Some(make_key(dirs[0], &dirs[..1]))
}

fn make_key(name: &str, prefix_segments: &[&str]) -> FeatureKey {
    FeatureKey {
        slug: slugify(name),
        display: name.to_string(),
        prefix: prefix_segments.join("/"),
    }
}

/// Match a doc path to one of the known module slugs by normalised
/// segment equality (so `docs/feature-docs/customer-edit/…` matches the
/// `customer_edit` module). Returns the longest matching slug.
fn match_doc_to_module(path: &str, known: &BTreeSet<String>) -> Option<String> {
    let norm = path.replace('\\', "/");
    let segments: Vec<String> = norm
        .split('/')
        .filter(|s| !s.is_empty())
        .map(normalise_token)
        .collect();
    let mut best: Option<&String> = None;
    for slug in known {
        let target = normalise_token(slug);
        if target.is_empty() {
            continue;
        }
        if segments.iter().any(|seg| seg == &target) {
            match best {
                Some(b) if b.len() >= slug.len() => {}
                _ => best = Some(slug),
            }
        }
    }
    best.cloned()
}

/// Lower-case and strip separators so `customer-edit` == `customer_edit`.
fn normalise_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Turn an arbitrary directory name into a valid business-candidate slug
/// (`^[a-z0-9][a-z0-9_-]*$`).
fn slugify(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        return "module".to_string();
    }
    // Must start with alnum.
    let first = trimmed.chars().next().unwrap();
    if first.is_ascii_alphanumeric() {
        trimmed
    } else {
        format!("m_{trimmed}")
    }
}

/// Prettify a folder name for display: `customer_edit` -> `Customer Edit`.
fn prettify(name: &str) -> String {
    let words: Vec<String> = name
        .split(['_', '-', ' '])
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        name.to_string()
    } else {
        words.join(" ")
    }
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn framework_family(node: &Node) -> Option<String> {
    let meta = node.metadata_json.as_deref()?;
    let role: crate::python_frameworks::FrameworkRole = serde_json::from_str(meta).ok()?;
    Some(role.family().to_string())
}

fn display_label(node: &Node) -> String {
    node.name
        .clone()
        .or_else(|| node.stable_key.clone())
        .unwrap_or_else(|| node.id.to_string())
}

fn line_range(node: &Node) -> Option<(u32, u32)> {
    match (node.start_line, node.end_line) {
        (Some(s), Some(e)) => Some((s, e)),
        _ => None,
    }
}

/// Collapse fine-grained nodes (doc *sections*, test *cases*) to one
/// [`EvidenceRef`] per source file, in stable path order. For a business
/// pack we want the *files* that describe/verify a module, not every
/// heading or assertion. The representative id is the first node of the
/// file (a real node id, so it still grounds the evidence list), and the
/// name is the file name.
fn dedup_refs_by_path(nodes: &[&Node]) -> Vec<EvidenceRef> {
    let mut sorted: Vec<&&Node> = nodes.iter().collect();
    sorted.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<EvidenceRef> = Vec::new();
    for node in sorted {
        let path = node.path.clone().unwrap_or_default();
        if path.is_empty() || !seen.insert(path.clone()) {
            continue;
        }
        let name = path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| display_label(node));
        out.push(EvidenceRef {
            id: node.id.to_string(),
            path,
            name,
        });
    }
    out
}

/// Heuristic: does this path live in test/spec scaffolding? Used to keep
/// test doubles (mocks/fakes) out of the *business* entry-point ranking.
fn is_test_path(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    let in_test_dir = p.split('/').any(|seg| {
        matches!(
            seg,
            "test"
                | "tests"
                | "testing"
                | "integration_test"
                | "test_driver"
                | "__tests__"
                | "spec"
                | "specs"
        )
    });
    in_test_dir
        || p.ends_with("_test.dart")
        || p.contains("_test.")
        || p.contains(".test.")
        || p.contains(".spec.")
        || p.contains("_spec.")
}

/// Heuristic: is this a generated / codegen file? Such symbols (freezed
/// copyWith impls, json_serializable `.g.dart`, protobuf, mockito mocks)
/// are machine-written plumbing, never a business entry point.
fn is_generated_path(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    const GENERATED_SUFFIXES: &[&str] = &[
        ".freezed.dart",
        ".g.dart",
        ".gr.dart",
        ".config.dart",
        ".mocks.dart",
        ".pb.dart",
        ".pbenum.dart",
        ".pbjson.dart",
        ".pbserver.dart",
        ".gen.dart",
    ];
    GENERATED_SUFFIXES.iter().any(|suf| p.ends_with(suf))
        || p.ends_with(".generated.ts")
        || p.ends_with(".g.ts")
}

fn capped_sorted(set: &BTreeSet<String>, limit: usize) -> Vec<String> {
    set.iter().take(limit).cloned().collect()
}

/// Curate the node-id evidence list an AI should ground a candidate in:
/// the entry-point symbols, then docs, then a couple of tests.
fn build_evidence_list(
    entry_points: &[EvidenceSymbol],
    docs: &[EvidenceRef],
    tests: &[EvidenceRef],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for ep in entry_points {
        out.push(ep.id.clone());
    }
    for d in docs.iter().take(3) {
        out.push(d.id.clone());
    }
    for t in tests.iter().take(3) {
        out.push(t.id.clone());
    }
    out.dedup();
    out
}

fn prompt_text() -> String {
    r#"你是 SpecSlice 的业务逻辑提炼器。下面的 `modules` 是按业务模块（feature）聚合的**代码事实证据**（入口符号、路由、Provider、存储、测试、文档、模块依赖）。

请基于这些证据，为每个值得记录的业务模块写出**中文业务逻辑候选**，输出 `business_logic.yaml`，schema 如下：

```yaml
schema_version: 1
candidates:
  - id: <模块 id，与 modules[].id 一致，^[a-z0-9][a-z0-9_-]*$>
    name: "<一句话中文业务标题>"
    description: |
      <1-3 句中文业务逻辑描述：这个模块在产品里负责什么、关键流程、对外契约边界>
    evidence:        # 只能引用证据包里出现过的 id（modules[].evidence / entry_points[].id / docs[].id / tests[].id）
      - <node id>
    confidence: 0.0..1.0   # 证据越充分（有入口+测试+文档+语义边）越高
    open_questions:        # 代码无法证明的问题（外部配置、服务端行为、设备能力等）
      - "<问题>"
    risks:                 # 看起来脆弱/缺测试/边界不清的风险
      - "<风险>"
    recommendation: "<给审阅人的一句话建议，如 建议接受 / 建议补测试后再确认>"
    status: proposed
```

硬约束：
1. 只能引用证据包里**真实出现过的 id / 路径**，严禁臆造任何 path / name / id。
2. 信息不足时，写入 `open_questions` 或调低 `confidence`，不要编造高置信描述。
3. 描述聚焦"业务做什么、为什么"，不是"代码怎么写"；用产品/领域语言。
4. 所有候选 `status: proposed`；是否确认由人工 `specslice candidate review` 决定，绝不冒充人工确认。
5. 把生成结果写入 `.specslice/candidates/business_logic.yaml`。"#
        .to_string()
}

// ---------------------------------------------------------------------------
// config helpers (mirrors connect.rs to keep the module self-contained)
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

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{EdgeCertainty, EdgeSource, EdgeStatus};
    use tempfile::TempDir;

    fn empty_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn node(id: &str, kind: NodeKind, path: &str, name: &str) -> Node {
        Node {
            id: ArtifactId::new(id.to_string()),
            kind,
            path: Some(path.to_string()),
            name: Some(name.to_string()),
            start_line: Some(1),
            end_line: Some(9),
            content_hash: None,
            stable_key: None,
            source_file: Some(path.to_string()),
            source_hash: None,
            indexer: Some("test".into()),
            index_generation: None,
            metadata_json: None,
        }
    }

    fn edge(from: &str, to: &str, kind: EdgeKind) -> EdgeAssertion {
        EdgeAssertion {
            id: ArtifactId::new(format!("{}::{from}->{to}", kind.as_str())),
            from_id: ArtifactId::new(from.to_string()),
            to_id: ArtifactId::new(to.to_string()),
            kind,
            source: EdgeSource::LanguageAdapter,
            certainty: EdgeCertainty::Fact,
            status: EdgeStatus::Confirmed,
            confidence: 1.0,
            evidence_json: None,
            source_file: None,
            source_hash: None,
            indexer: Some("test".into()),
            index_generation: None,
            metadata_json: None,
        }
    }

    #[test]
    fn feature_key_uses_marker_segment() {
        let k = feature_key("lib/features/auth/data/auth_repository.dart").unwrap();
        assert_eq!(k.slug, "auth");
        assert_eq!(k.prefix, "lib/features/auth");
    }

    #[test]
    fn feature_key_falls_back_to_source_root_child() {
        let k = feature_key("lib/core/settings/pro_provider.dart").unwrap();
        assert_eq!(k.slug, "core");
        assert_eq!(k.prefix, "lib/core");

        let k2 = feature_key("src/billing/checkout.ts").unwrap();
        assert_eq!(k2.slug, "billing");
    }

    #[test]
    fn feature_key_first_dir_when_no_known_root() {
        let k = feature_key("server/payments/handler.go").unwrap();
        assert_eq!(k.slug, "server");
    }

    #[test]
    fn slugify_sanitises_to_candidate_id() {
        assert_eq!(slugify("Customer Edit"), "customer_edit");
        assert_eq!(slugify("ai-tryon"), "ai_tryon");
        assert_eq!(slugify("123abc"), "123abc");
        assert_eq!(slugify("!!!"), "module");
    }

    #[test]
    fn match_doc_to_module_handles_hyphen_underscore_drift() {
        let mut known = BTreeSet::new();
        known.insert("customer_edit".to_string());
        known.insert("auth".to_string());
        assert_eq!(
            match_doc_to_module("docs/feature-docs/customer-edit/index.md", &known).as_deref(),
            Some("customer_edit")
        );
        assert_eq!(
            match_doc_to_module("docs/feature-docs/auth/index.md", &known).as_deref(),
            Some("auth")
        );
        assert_eq!(
            match_doc_to_module("docs/architecture/overview.md", &known),
            None
        );
    }

    #[test]
    fn build_pack_groups_modules_and_rolls_up_signals() {
        let (mut store, _dir) = empty_store();
        // ---- auth module: bloc + repository + service (densely coupled),
        //      plus a test that exercises the bloc and a doc. The modules
        //      are detected from the call graph, so each feature needs real
        //      intra-feature coupling (mirroring how a real feature reads).
        let ab = "dart_class::lib/features/auth/presentation/auth_bloc.dart#AuthBloc";
        let ar = "dart_class::lib/features/auth/data/auth_repository.dart#AuthRepository";
        let as_ = "dart_class::lib/features/auth/domain/auth_service.dart#AuthService";
        let auth_test = "test_case::test/features/auth/auth_bloc_test.dart#login works";
        store
            .upsert_node(&node(
                ab,
                NodeKind::DartClass,
                "lib/features/auth/presentation/auth_bloc.dart",
                "AuthBloc",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                ar,
                NodeKind::DartClass,
                "lib/features/auth/data/auth_repository.dart",
                "AuthRepository",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                as_,
                NodeKind::DartClass,
                "lib/features/auth/domain/auth_service.dart",
                "AuthService",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                auth_test,
                NodeKind::TestCase,
                "test/features/auth/auth_bloc_test.dart",
                "login works",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                "doc_section::docs/feature-docs/auth/index.md#Auth",
                NodeKind::DocSection,
                "docs/feature-docs/auth/index.md",
                "Auth",
            ))
            .unwrap();
        // ---- products module: screen + repository (coupled), navigates to
        //      a route and reads a provider.
        let ps = "dart_class::lib/features/products/products_screen.dart#ProductsScreen";
        let pr =
            "dart_class::lib/features/products/data/products_repository.dart#ProductsRepository";
        store
            .upsert_node(&node(
                ps,
                NodeKind::DartClass,
                "lib/features/products/products_screen.dart",
                "ProductsScreen",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                pr,
                NodeKind::DartClass,
                "lib/features/products/data/products_repository.dart",
                "ProductsRepository",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                "route::/design",
                NodeKind::Route,
                "route::/design",
                "/design",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                "dart_provider::lib/core/cart_provider.dart#cartProvider",
                NodeKind::DartProvider,
                "lib/core/cart_provider.dart",
                "cartProvider",
            ))
            .unwrap();

        // intra-auth coupling (clique) + the test exercising the bloc
        for (f, t) in [(ab, ar), (ab, as_), (as_, ar), (auth_test, ab)] {
            store.upsert_edge(&edge(f, t, EdgeKind::Calls)).unwrap();
        }
        // intra-products coupling
        store.upsert_edge(&edge(ps, pr, EdgeKind::Calls)).unwrap();
        // semantic signals on the products screen
        store
            .upsert_edge(&edge(ps, "route::/design", EdgeKind::NavigatesTo))
            .unwrap();
        store
            .upsert_edge(&edge(
                ps,
                "dart_provider::lib/core/cart_provider.dart#cartProvider",
                EdgeKind::ReadsProvider,
            ))
            .unwrap();
        // single cross-module dependency: products -> auth
        store.upsert_edge(&edge(ps, ar, EdgeKind::Calls)).unwrap();

        let pack =
            propose_business_pack_with_store(&store, &BusinessPackOptions::default()).unwrap();

        let auth = pack
            .modules
            .iter()
            .find(|m| m.id == "auth")
            .expect("auth module");
        assert_eq!(auth.name, "Auth");
        assert_eq!(auth.test_count, 1);
        assert_eq!(auth.docs.len(), 1, "doc associated by normalised segment");
        assert!(auth.entry_points.iter().any(|e| e.name == "AuthBloc"));

        let products = pack
            .modules
            .iter()
            .find(|m| m.id == "products")
            .expect("products module");
        assert_eq!(products.routes, vec!["/design".to_string()]);
        assert_eq!(products.providers, vec!["cartProvider".to_string()]);
        assert_eq!(
            products.depends_on,
            vec!["auth".to_string()],
            "cross-module call recorded as dependency"
        );

        // module dependency graph carries the products->auth edge
        assert!(pack
            .module_dependencies
            .iter()
            .any(|d| d.from == "products" && d.to == "auth"));

        // cohesion is reported and bounded; auth (a clique) is highly
        // self-contained, so its cohesion beats the leaky products module.
        let auth_coh = pack
            .modules
            .iter()
            .find(|m| m.id == "auth")
            .unwrap()
            .cohesion;
        assert!(
            auth_coh > 0.5,
            "auth is a clique -> high cohesion, got {auth_coh}"
        );
        for m in &pack.modules {
            assert!((0.0..=1.0).contains(&m.cohesion));
        }

        // evidence list references only real node ids
        for m in &pack.modules {
            for ev in &m.evidence {
                assert!(
                    store
                        .find_node(&ArtifactId::new(ev.clone()))
                        .unwrap()
                        .is_some(),
                    "evidence id {ev} must resolve to a real node"
                );
            }
        }

        // prompt is Chinese + mentions the target file
        assert!(pack.prompt.contains("business_logic.yaml"));
        assert!(pack.prompt.contains("业务逻辑"));
    }

    #[test]
    fn layer_organised_repo_still_aggregates_by_business_feature() {
        // The repo is organised by *layer* (`lib/models`, `lib/services`,
        // `lib/widgets`), not by feature — exactly the "messy"/conventional
        // case the user warned about. A naive path split would report
        // "Models / Services / Widgets" (architecture, not business). The
        // call graph, however, couples each feature across the layers, so
        // graph-driven detection must recover the *business* modules
        // ("User", "Order") and name them after their central symbol.
        let (mut store, _dir) = empty_store();
        let nodes = [
            (
                "dart_class::lib/models/user.dart#User",
                "lib/models/user.dart",
                "User",
            ),
            (
                "dart_class::lib/services/user_service.dart#UserService",
                "lib/services/user_service.dart",
                "UserService",
            ),
            (
                "dart_class::lib/widgets/user_page.dart#UserPage",
                "lib/widgets/user_page.dart",
                "UserPage",
            ),
            (
                "dart_class::lib/models/order.dart#Order",
                "lib/models/order.dart",
                "Order",
            ),
            (
                "dart_class::lib/services/order_service.dart#OrderService",
                "lib/services/order_service.dart",
                "OrderService",
            ),
            (
                "dart_class::lib/widgets/order_page.dart#OrderPage",
                "lib/widgets/order_page.dart",
                "OrderPage",
            ),
        ];
        for (id, path, name) in nodes {
            store
                .upsert_node(&node(id, NodeKind::DartClass, path, name))
                .unwrap();
        }
        let user = "dart_class::lib/models/user.dart#User";
        let user_svc = "dart_class::lib/services/user_service.dart#UserService";
        let user_page = "dart_class::lib/widgets/user_page.dart#UserPage";
        let order = "dart_class::lib/models/order.dart#Order";
        let order_svc = "dart_class::lib/services/order_service.dart#OrderService";
        let order_page = "dart_class::lib/widgets/order_page.dart#OrderPage";
        // each feature is a triangle across the three layers
        for (f, t) in [
            (user_page, user_svc),
            (user_svc, user),
            (user_page, user),
            (order_page, order_svc),
            (order_svc, order),
            (order_page, order),
        ] {
            store.upsert_edge(&edge(f, t, EdgeKind::Calls)).unwrap();
        }
        // single weak cross-feature link: order references user
        store
            .upsert_edge(&edge(order_svc, user, EdgeKind::Calls))
            .unwrap();

        let pack =
            propose_business_pack_with_store(&store, &BusinessPackOptions::default()).unwrap();

        // Exactly two business modules, by *feature*, not by layer.
        let ids: BTreeSet<&str> = pack.modules.iter().map(|m| m.id.as_str()).collect();
        assert!(
            !ids.contains("models") && !ids.contains("services") && !ids.contains("widgets"),
            "must not segment by architectural layer, got {ids:?}"
        );
        assert!(
            ids.contains("user"),
            "expected a graph-derived `user` module, got {ids:?}"
        );
        assert!(
            ids.contains("order"),
            "expected a graph-derived `order` module, got {ids:?}"
        );

        // the user module pulled in all three layer files (proof it
        // followed the graph, not the directory): its 3 symbols live under
        // models/, services/ and widgets/ respectively.
        let user_mod = pack.modules.iter().find(|m| m.id == "user").unwrap();
        assert_eq!(
            user_mod.symbol_count, 3,
            "user feature spans models+services+widgets"
        );
        let dirs: BTreeSet<String> = user_mod
            .entry_points
            .iter()
            .filter_map(|e| e.path.rsplit_once('/').map(|(d, _)| d.to_string()))
            .collect();
        assert!(
            dirs.contains("lib/models")
                && dirs.contains("lib/services")
                && dirs.contains("lib/widgets"),
            "user module entry points should span all three layers, got {dirs:?}"
        );
    }

    #[test]
    fn docs_dedup_by_file_and_scaffolding_buckets_excluded() {
        let (mut store, _dir) = empty_store();
        // a real business module
        store
            .upsert_node(&node(
                "dart_class::lib/features/billing/billing_bloc.dart#BillingBloc",
                NodeKind::DartClass,
                "lib/features/billing/billing_bloc.dart",
                "BillingBloc",
            ))
            .unwrap();
        // one doc FILE split into three sections — must collapse to 1 doc
        for heading in ["Overview", "Pricing", "Refunds"] {
            store
                .upsert_node(&node(
                    &format!("doc_section::docs/feature-docs/billing/guide.md#{heading}"),
                    NodeKind::DocSection,
                    "docs/feature-docs/billing/guide.md",
                    heading,
                ))
                .unwrap();
        }
        // scaffolding buckets that must NOT become business modules
        store
            .upsert_node(&node(
                "dart_class::test/support/test_harness.dart#TestHarness",
                NodeKind::DartClass,
                "test/support/test_harness.dart",
                "TestHarness",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                "dart_function::tool/codegen/run.dart#main",
                NodeKind::DartFunction,
                "tool/codegen/run.dart",
                "runCodegen",
            ))
            .unwrap();

        let pack =
            propose_business_pack_with_store(&store, &BusinessPackOptions::default()).unwrap();

        let billing = pack
            .modules
            .iter()
            .find(|m| m.id == "billing")
            .expect("billing module");
        assert_eq!(
            billing.docs.len(),
            1,
            "three sections of one doc file collapse to a single doc evidence"
        );
        assert_eq!(billing.docs[0].path, "docs/feature-docs/billing/guide.md");

        assert!(
            pack.modules.iter().all(|m| m.id != "test"),
            "`test` scaffolding bucket must not be a business module"
        );
        assert!(
            pack.modules.iter().all(|m| m.id != "tool"),
            "`tool` scaffolding bucket must not be a business module"
        );
    }

    #[test]
    fn entry_points_prefer_production_over_test_symbols() {
        let (mut store, _dir) = empty_store();
        // production bloc
        store
            .upsert_node(&node(
                "dart_class::lib/features/checkout/presentation/checkout_bloc.dart#CheckoutBloc",
                NodeKind::DartClass,
                "lib/features/checkout/presentation/checkout_bloc.dart",
                "CheckoutBloc",
            ))
            .unwrap();
        // test-only mock that *looks* like an entry point (matches `repository`)
        store
            .upsert_node(&node(
                "dart_class::test/features/checkout/checkout_bloc_test.dart#_MockCheckoutRepository",
                NodeKind::DartClass,
                "test/features/checkout/checkout_bloc_test.dart",
                "_MockCheckoutRepository",
            ))
            .unwrap();

        let pack =
            propose_business_pack_with_store(&store, &BusinessPackOptions::default()).unwrap();
        let checkout = pack
            .modules
            .iter()
            .find(|m| m.id == "checkout")
            .expect("checkout module");
        assert!(
            checkout
                .entry_points
                .iter()
                .any(|e| e.name == "CheckoutBloc"),
            "production bloc is an entry point"
        );
        assert!(
            checkout
                .entry_points
                .iter()
                .all(|e| e.name != "_MockCheckoutRepository"),
            "test-file mock must not be a business entry point"
        );
    }

    #[test]
    fn generated_codegen_symbols_are_not_entry_points() {
        let (mut store, _dir) = empty_store();
        // hand-written model
        store
            .upsert_node(&node(
                "dart_class::lib/features/cart/data/models/my_cart_item_dto.dart#MyCartPageDto",
                NodeKind::DartClass,
                "lib/features/cart/data/models/my_cart_item_dto.dart",
                "MyCartPageDto",
            ))
            .unwrap();
        // freezed-generated copyWith noise (same module, .freezed.dart file)
        store
            .upsert_node(&node(
                "dart_class::lib/features/cart/data/models/my_cart_item_dto.freezed.dart#_$MyCartPageDtoCopyWithImpl",
                NodeKind::DartClass,
                "lib/features/cart/data/models/my_cart_item_dto.freezed.dart",
                "_$MyCartPageDtoCopyWithImpl",
            ))
            .unwrap();

        let pack =
            propose_business_pack_with_store(&store, &BusinessPackOptions::default()).unwrap();
        let cart = pack.modules.iter().find(|m| m.id == "cart").expect("cart");
        assert!(
            cart.entry_points.iter().any(|e| e.name == "MyCartPageDto"),
            "hand-written model is an entry point"
        );
        assert!(
            cart.entry_points
                .iter()
                .all(|e| !e.name.contains("CopyWith")),
            "freezed-generated copyWith classes must not be entry points"
        );
    }

    #[test]
    fn tests_dedup_by_file_keeps_true_count() {
        let (mut store, _dir) = empty_store();
        store
            .upsert_node(&node(
                "dart_class::lib/features/wallet/wallet_bloc.dart#WalletBloc",
                NodeKind::DartClass,
                "lib/features/wallet/wallet_bloc.dart",
                "WalletBloc",
            ))
            .unwrap();
        // one test file, three test cases
        for case in ["loads balance", "tops up", "handles error"] {
            store
                .upsert_node(&node(
                    &format!("test_case::test/features/wallet/wallet_bloc_test.dart#{case}"),
                    NodeKind::TestCase,
                    "test/features/wallet/wallet_bloc_test.dart",
                    case,
                ))
                .unwrap();
        }

        let pack =
            propose_business_pack_with_store(&store, &BusinessPackOptions::default()).unwrap();
        let wallet = pack
            .modules
            .iter()
            .find(|m| m.id == "wallet")
            .expect("wallet module");
        assert_eq!(wallet.test_count, 3, "true test-node count preserved");
        assert_eq!(
            wallet.tests.len(),
            1,
            "displayed test list collapses to one entry per file"
        );
        assert_eq!(
            wallet.tests[0].path,
            "test/features/wallet/wallet_bloc_test.dart"
        );
    }

    #[test]
    fn noise_methods_are_not_entry_points() {
        let acc = ModuleAcc {
            slug: "x".into(),
            name: "X".into(),
            ..Default::default()
        };
        let build = node(
            "dart_method::lib/features/x/widget.dart#X.build",
            NodeKind::DartMethod,
            "lib/features/x/widget.dart",
            "X.build",
        );
        assert_eq!(acc.entry_point_score(&build), 0);
        let bloc = node(
            "dart_class::lib/features/x/x_bloc.dart#XBloc",
            NodeKind::DartClass,
            "lib/features/x/x_bloc.dart",
            "XBloc",
        );
        assert!(acc.entry_point_score(&bloc) > 0);
    }
}
