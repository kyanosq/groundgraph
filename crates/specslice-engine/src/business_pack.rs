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
//! - It segments code/test symbols into *business modules* using the
//!   repository's own feature-folder convention (`lib/features/<x>`,
//!   `src/<x>`, ...). This is path-aware and deterministic, matching how
//!   humans think about business modules, rather than the call-coupling
//!   clusters of `specslice features`.
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

fn build_pack(nodes: &[Node], edges: &[EdgeAssertion], options: &BusinessPackOptions) -> BusinessPack {
    let nodes_by_id: HashMap<&ArtifactId, &Node> = nodes.iter().map(|n| (&n.id, n)).collect();

    // ---- 1. assign every code symbol / test / file to a module ----------
    // Code + tests are assigned by their own path. Docs are matched to a
    // module separately (normalised segment match) because doc trees often
    // sit under `docs/feature-docs/<x>` rather than the source layout.
    let mut module_of_node: HashMap<&ArtifactId, String> = HashMap::new();
    let mut modules: BTreeMap<String, ModuleAcc> = BTreeMap::new();

    let mut total_symbols = 0usize;
    let mut assigned_symbols = 0usize;
    let mut total_tests = 0usize;
    let mut total_docs = 0usize;

    for node in nodes {
        let is_symbol = language_traits::is_callable(node.kind) || language_traits::is_type(node.kind);
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
        let Some(key) = feature_key(path) else {
            continue;
        };
        let acc = modules.entry(key.slug.clone()).or_insert_with(|| ModuleAcc {
            slug: key.slug.clone(),
            name: prettify(&key.display),
            path_prefix: key.prefix.clone(),
            ..Default::default()
        });
        // Prefer the shortest prefix as the canonical anchor.
        if key.prefix.len() < acc.path_prefix.len() {
            acc.path_prefix = key.prefix.clone();
        }
        module_of_node.insert(&node.id, key.slug.clone());

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
        .map(|s| normalise_token(s))
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
        } else if matches!(ch, '_' | '-') {
            out.push('_');
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
        .split(|c: char| c == '_' || c == '-' || c == ' ')
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
        // auth module: a bloc + repository + a test + a doc
        store
            .upsert_node(&node(
                "dart_class::lib/features/auth/presentation/auth_bloc.dart#AuthBloc",
                NodeKind::DartClass,
                "lib/features/auth/presentation/auth_bloc.dart",
                "AuthBloc",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                "dart_class::lib/features/auth/data/auth_repository.dart#AuthRepository",
                NodeKind::DartClass,
                "lib/features/auth/data/auth_repository.dart",
                "AuthRepository",
            ))
            .unwrap();
        store
            .upsert_node(&node(
                "test_case::test/features/auth/auth_bloc_test.dart#login works",
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
        // products module: a class that navigates to a route + reads a provider
        store
            .upsert_node(&node(
                "dart_class::lib/features/products/products_screen.dart#ProductsScreen",
                NodeKind::DartClass,
                "lib/features/products/products_screen.dart",
                "ProductsScreen",
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

        // edges
        store
            .upsert_edge(&edge(
                "dart_class::lib/features/products/products_screen.dart#ProductsScreen",
                "route::/design",
                EdgeKind::NavigatesTo,
            ))
            .unwrap();
        store
            .upsert_edge(&edge(
                "dart_class::lib/features/products/products_screen.dart#ProductsScreen",
                "dart_provider::lib/core/cart_provider.dart#cartProvider",
                EdgeKind::ReadsProvider,
            ))
            .unwrap();
        // cross-module dependency: products -> auth
        store
            .upsert_edge(&edge(
                "dart_class::lib/features/products/products_screen.dart#ProductsScreen",
                "dart_class::lib/features/auth/data/auth_repository.dart#AuthRepository",
                EdgeKind::Calls,
            ))
            .unwrap();

        let pack = propose_business_pack_with_store(&store, &BusinessPackOptions::default()).unwrap();

        let auth = pack.modules.iter().find(|m| m.id == "auth").expect("auth module");
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

        // evidence list references only real node ids
        for m in &pack.modules {
            for ev in &m.evidence {
                assert!(
                    store.find_node(&ArtifactId::new(ev.clone())).unwrap().is_some(),
                    "evidence id {ev} must resolve to a real node"
                );
            }
        }

        // prompt is Chinese + mentions the target file
        assert!(pack.prompt.contains("business_logic.yaml"));
        assert!(pack.prompt.contains("业务逻辑"));
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
            checkout.entry_points.iter().any(|e| e.name == "CheckoutBloc"),
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
