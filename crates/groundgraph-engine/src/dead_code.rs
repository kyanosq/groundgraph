//! P7 — dead-code detection.
//!
//! `groundgraph dead-code` returns *possibly_dead* candidates with an
//! explicit confidence score and a list of human-readable reasons.
//! The goal is **never** to recommend deletion automatically — it is
//! to surface symbols that no in-repo caller or framework root can
//! reach so the operator (or an AI agent) can decide.
//!
//! ## Detection pipeline
//!
//! 1. **Entry point set.** Built from
//!    - `dead_code.entrypoints` config (top-level functions in those
//!      files, e.g. `lib/main.dart#main`),
//!    - every `Route`, `DartProvider`, `TestCase` and `TestGroup`,
//!    - every method whose name matches a known Flutter lifecycle
//!      callback (`build`, `initState`, `dispose`, ...),
//!    - every symbol under `dead_code.public_api_roots`.
//! 2. **Forward reachability.** BFS along outbound usage edges
//!    (`calls`, `references`, `reads_provider`, `persists_to`,
//!    `navigates_to`, `subscribes_stream`, `declares_verification`,
//!    `contains`). Anything not reached is a candidate.
//! 3. **Confidence binning.** For each unreached code symbol:
//!    - **High** — no inbound usage edges, private (`_`-prefixed) name,
//!      not in `public_api_roots`, not a lifecycle name, file not in
//!      `ignore` glob → very likely safe to delete after manual review.
//!    - **Medium** — same but the symbol is public, has a lifecycle
//!      name, or sits under `public_api_roots`. May be consumed by
//!      reflection, code-gen, framework, or external callers.
//!    - **Low** — has inbound usage edges, but every source is itself
//!      a candidate (a "dead island"). Surfaced last; cheapest to
//!      ignore.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use groundgraph_core::{EdgeKind, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::config::{DeadCodeConfig, EngineConfig};

pub const DEAD_CODE_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Data contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeadCodeConfidence {
    Low,
    Medium,
    High,
}

impl DeadCodeConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            DeadCodeConfidence::High => "high",
            DeadCodeConfidence::Medium => "medium",
            DeadCodeConfidence::Low => "low",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeadCodeOptions {
    pub repo_root: std::path::PathBuf,
    pub min_confidence: DeadCodeConfidence,
    /// When `true`, test cases / test groups are themselves eligible
    /// to appear as dead-code candidates (orphan tests). They remain
    /// entry points for reachability either way.
    pub include_tests: bool,
}

impl Default for DeadCodeOptions {
    fn default() -> Self {
        Self {
            repo_root: ".".into(),
            min_confidence: DeadCodeConfidence::Medium,
            include_tests: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadCodeReport {
    pub schema_version: u32,
    pub min_confidence: String,
    pub stats: DeadCodeStats,
    pub candidates: Vec<DeadCodeCandidate>,
    /// Engine-side warnings collected while building the report
    /// (e.g. failed edge-quality probes). Mirrors the pattern used by
    /// [`impact::ImpactReport`](crate::impact::ImpactReport) and
    /// [`logic_confidence::LogicConfidenceReport`](crate::logic_confidence::LogicConfidenceReport):
    /// engine never writes to stderr; consumers render warnings
    /// explicitly. Skipped in JSON when empty to keep old consumers
    /// fully backward compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeadCodeStats {
    pub total_code_symbols: usize,
    pub entrypoints: usize,
    pub reachable: usize,
    pub possibly_dead: usize,
    pub ignored_by_pattern: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadCodeCandidate {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
    pub line_range: Option<(u32, u32)>,
    pub confidence: DeadCodeConfidence,
    pub reasons: Vec<String>,
    /// Inbound usage edges that survived filtering (empty for
    /// `High`/`Medium`; non-empty for `Low` "dead island" cases).
    pub inbound_sources: Vec<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn analyze_dead_code(opts: DeadCodeOptions) -> Result<DeadCodeReport> {
    let config = load_workspace_config(&opts.repo_root)?;
    let db_path = resolve_storage_path(&opts.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    let dead_cfg = config.dead_code.clone();
    analyze_dead_code_with_store(&store, opts, &dead_cfg)
}

fn load_workspace_config(repo_root: &Path) -> Result<EngineConfig> {
    crate::config::load_config(repo_root)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    crate::config::resolve_storage_path(repo_root, config)
}

pub fn analyze_dead_code_with_store(
    store: &Store,
    opts: DeadCodeOptions,
    config: &DeadCodeConfig,
) -> Result<DeadCodeReport> {
    let ignore_set = build_globset(&config.ignore).context("compiling dead_code.ignore globs")?;
    let public_set = build_globset(&config.public_api_roots)
        .context("compiling dead_code.public_api_roots globs")?;

    let nodes = store.list_all_nodes().context("listing nodes")?;
    let edges = store.list_all_edges().context("listing edges")?;

    let node_index: HashMap<String, &groundgraph_core::Node> =
        nodes.iter().map(|n| (n.id.to_string(), n)).collect();

    // Precompute outbound usage edges per node.
    let mut outbound: HashMap<&str, Vec<&groundgraph_core::EdgeAssertion>> = HashMap::new();
    let mut inbound: HashMap<&str, Vec<&groundgraph_core::EdgeAssertion>> = HashMap::new();
    for edge in &edges {
        if !is_usage_edge(edge.kind) {
            continue;
        }
        outbound
            .entry(edge.from_id.as_str())
            .or_default()
            .push(edge);
        inbound.entry(edge.to_id.as_str()).or_default().push(edge);
    }

    // Entry points.
    let mut entry_ids: BTreeSet<String> = BTreeSet::new();
    seed_config_entrypoints(&config.entrypoints, &nodes, &mut entry_ids);
    entry_ids.extend(implicit_entry_ids(&nodes, &edges));
    for n in &nodes {
        // public_api_roots: anything under those paths.
        if let Some(p) = n.path.as_deref() {
            if matches_set(&public_set, p) && is_code_kind(n.kind) {
                entry_ids.insert(n.id.to_string());
            }
        }
    }

    // Build the `Contains` parent map up-front so up-propagation can be
    // interleaved with the forward search. A module / file container is alive
    // once it transitively owns a reachable symbol — you never delete a `mod`
    // (or file) that still holds live code — and, crucially, a freshly
    // reachable file may itself *anchor* module-level references (e.g. a
    // top-level `FACTORS = [_amihud(20)]` registration). Following those
    // requires the file node to be expanded by the same worklist, so the two
    // phases share one queue rather than running back-to-back.
    let mut contains_parents: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &edges {
        if edge.kind == EdgeKind::Contains {
            contains_parents
                .entry(edge.to_id.as_str())
                .or_default()
                .push(edge.from_id.as_str());
        }
    }

    // Reachability fixpoint (phase 1): follow usage edges forward — `Contains`
    // included, so a reached type/file expands *down* to the members the
    // framework or codegen drives without an in-repo call edge (getters reached
    // through a property chain `_shiftDao.batchInsert()`, freezed factories,
    // enum-like const values, lifecycle callbacks). Propagate reachability *up*
    // onto module/file parents (and expand them so module-level references they
    // anchor get followed), and let a reachable *constructor* keep its owning
    // class alive (constructing a type uses it). The set only grows, so this
    // can never flip a reachable symbol back into a candidate.
    let mut reachable: BTreeSet<String> = BTreeSet::new();
    let mut queue: std::collections::VecDeque<String> = entry_ids.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        // Forward: follow usage edges (Contains included) out of this node.
        if let Some(out) = outbound.get(id.as_str()) {
            for e in out {
                let to = e.to_id.to_string();
                if !reachable.contains(&to) {
                    queue.push_back(to);
                }
            }
        }
        // Up: keep module/file containers alive, and let a reachable ctor keep
        // its owning class alive (the construction edge lands on the ctor, e.g.
        // a private `_AppLocalizationsDelegate` named solely by
        // `static const delegate = _AppLocalizationsDelegate()`).
        let current_is_ctor = node_index.get(id.as_str()).is_some_and(|n| {
            n.kind == NodeKind::DartConstructor || is_dart_constructor_shaped_method(n)
        });
        if let Some(parents) = contains_parents.get(id.as_str()) {
            for parent in parents {
                let parent_is_container = node_index
                    .get(*parent)
                    .is_some_and(|n| groundgraph_core::language_traits::is_module_or_file(n.kind));
                if (parent_is_container || current_is_ctor) && !reachable.contains(*parent) {
                    queue.push_back((*parent).to_string());
                }
            }
        }
    }

    // Phase 2 — rescue live *containers*. A static-only class is never
    // constructed and a class named only through a static call
    // (`LunarService.generateDayInfoRange()`) gets no usage edge on the class
    // node — the edge lands on the method — so phase 1 leaves the type unreached
    // even though it plainly owns reachable code. Walk *up* `Contains` from
    // every reachable symbol and mark each type/file/module ancestor alive.
    // This only flows up and only rescues containers, so a class with one live
    // member is saved while its genuinely-unused siblings (an unreferenced
    // `AppButton.icon` ctor) keep their verdict: the candidate set can only
    // shrink, never gain a false positive.
    {
        let mut up_queue: std::collections::VecDeque<String> = reachable.iter().cloned().collect();
        let mut walked: std::collections::HashSet<String> = std::collections::HashSet::new();
        while let Some(id) = up_queue.pop_front() {
            if !walked.insert(id.clone()) {
                continue;
            }
            if let Some(parents) = contains_parents.get(id.as_str()) {
                for parent in parents {
                    let parent_is_container = node_index.get(*parent).is_some_and(|n| {
                        groundgraph_core::language_traits::is_type(n.kind)
                            || groundgraph_core::language_traits::is_module_or_file(n.kind)
                    });
                    if parent_is_container && reachable.insert((*parent).to_string()) {
                        up_queue.push_back((*parent).to_string());
                    }
                }
            }
        }
    }

    // Classify unreached code symbols.
    let mut candidates: Vec<DeadCodeCandidate> = Vec::new();
    let mut ignored_count: usize = 0;
    let mut total_code: usize = 0;
    for n in &nodes {
        if !is_code_kind(n.kind) {
            continue;
        }
        let id_str = n.id.to_string();
        if matches!(n.kind, NodeKind::TestCase | NodeKind::TestGroup) {
            if !opts.include_tests {
                // Tests are always roots for production reachability, but are
                // not reported as dead-code candidates unless explicitly asked.
                continue;
            }
            total_code += 1;
            let outbound_for: &[&groundgraph_core::EdgeAssertion] = outbound
                .get(id_str.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if let Some(candidate) = orphan_test_candidate(n, outbound_for) {
                if candidate.confidence >= opts.min_confidence {
                    candidates.push(candidate);
                }
            }
            continue;
        }
        if n.path.as_deref().is_some_and(is_test_path) {
            // Test-file helper functions are execution scaffolding, not
            // production dead-code candidates. `--include-tests` reports the
            // semantic TestCase/TestGroup nodes above instead.
            continue;
        }
        total_code += 1;
        if reachable.contains(&id_str) {
            continue;
        }
        // Ignore patterns.
        if let Some(p) = n.path.as_deref() {
            if matches_set(&ignore_set, p) {
                ignored_count += 1;
                continue;
            }
        }
        let inbound_for: &[&groundgraph_core::EdgeAssertion] = inbound
            .get(id_str.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (confidence, reasons) = classify(n, inbound_for, &public_set, &reachable);
        if confidence < opts.min_confidence {
            continue;
        }
        let label = n
            .name
            .clone()
            .or_else(|| n.stable_key.clone())
            .unwrap_or_else(|| id_str.clone());
        let line_range = match (n.start_line, n.end_line) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => None,
        };
        // Only surface *usage* inbound sources here so the listing
        // matches the reasons. Structural `contains` parents are
        // implied by `path` and would otherwise mislead operators
        // ("look! someone references it!" — no, it's just the file
        // that owns it).
        let inbound_sources: Vec<String> = inbound_for
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EdgeKind::Calls
                        | EdgeKind::References
                        | EdgeKind::ReadsProvider
                        | EdgeKind::PersistsTo
                        | EdgeKind::NavigatesTo
                        | EdgeKind::SubscribesStream
                        | EdgeKind::DeclaresVerification
                )
            })
            .map(|e| e.from_id.to_string())
            .collect();
        candidates.push(DeadCodeCandidate {
            id: id_str,
            kind: n.kind.as_str().into(),
            label,
            path: n.path.clone(),
            line_range,
            confidence,
            reasons,
            inbound_sources,
        });
    }

    // Sort: confidence desc, then path asc, then label asc.
    candidates.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.id.cmp(&b.id))
    });

    let possibly_dead = candidates.len();
    // Count reachable nodes over the *same* universe as `total_code` so the
    // header reconciles (`可达 ≤ 总符号`). Test cases / groups are production
    // roots but not code symbols (unless `--include-tests`), and test-file
    // helpers are scaffolding — neither counts here, exactly as in the
    // classification loop above.
    let reachable_count = reachable
        .iter()
        .filter_map(|id| node_index.get(id.as_str()))
        .filter(|n| {
            if !is_code_kind(n.kind) {
                return false;
            }
            if matches!(n.kind, NodeKind::TestCase | NodeKind::TestGroup) {
                return opts.include_tests;
            }
            !n.path.as_deref().is_some_and(is_test_path)
        })
        .count();
    let entrypoints = entry_ids.len();

    // Honest self-assessment: the analysis is only meaningful when it has both
    // entry points to start the walk from and precision-tier edges to walk
    // along. Without them every symbol looks unreachable — say so loudly rather
    // than emit thousands of false positives silently.
    let mut warnings: Vec<String> = Vec::new();
    if total_code > 0 && entry_ids.is_empty() {
        warnings.push(
            "未匹配到任何入口点：dead_code.entrypoints 未命中本仓库的任何文件，\
             也没有发现语言内置入口（main / 测试 / 框架路由）。没有入口点时，\
             所有符号都会被判为“可能死代码”，本报告不可用。\
             请在 .groundgraph.yaml 的 dead_code.entrypoints 配置真实入口文件。"
                .to_string(),
        );
    }
    // Precision = an actual code call/usage graph. `DeclaresVerification`
    // (requirement → test) and `Contains` are deliberately excluded: a handful
    // of requirement edges must not mask the fact that the code itself has no
    // resolved calls to walk.
    let has_precision_edges = edges.iter().any(|e| {
        matches!(
            e.kind,
            EdgeKind::Calls
                | EdgeKind::References
                | EdgeKind::ReadsProvider
                | EdgeKind::PersistsTo
                | EdgeKind::NavigatesTo
                | EdgeKind::SubscribesStream
        )
    });
    if total_code > 0 && !has_precision_edges {
        warnings.push(
            "代码图中没有 calls/references 等精确层边（当前语言未启用 Tier-3 富化：\
             LSP / analyzer / SCIP）。可达性仅能依赖结构边（contains/imports），\
             会显著高估死代码。请将结果当作“候选”而非结论。"
                .to_string(),
        );
    }

    Ok(DeadCodeReport {
        schema_version: DEAD_CODE_SCHEMA_VERSION,
        min_confidence: opts.min_confidence.as_str().into(),
        stats: DeadCodeStats {
            total_code_symbols: total_code,
            entrypoints,
            reachable: reachable_count,
            possibly_dead,
            ignored_by_pattern: ignored_count,
        },
        candidates,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Symbols a framework, runtime, or harness invokes without leaving an
/// in-repo edge: routes, providers, tests, lifecycle callbacks, per-language
/// entry names, Cargo implicit targets, Python framework roles, and Swift
/// types instantiated by UIKit/AppKit/SwiftUI (plus their members).
///
/// This is the single source of "implicitly alive" knowledge — dead-code
/// seeds reachability from it and questions suppresses orphan prompts with
/// it, so the two analyses can never disagree about what a framework drives.
pub fn implicit_entry_ids(
    nodes: &[groundgraph_core::Node],
    edges: &[groundgraph_core::EdgeAssertion],
) -> BTreeSet<String> {
    let mut entry_ids: BTreeSet<String> = BTreeSet::new();
    for n in nodes {
        // Auxiliary code (examples/, demos/, tools/, benchmarks/) is a set of
        // standalone programs invoked by humans, not by in-repo callers.
        // Treating them as entries (a) stops them being flagged dead and
        // (b) keeps the production APIs they exercise reachable.
        if is_code_kind(n.kind)
            && n.path
                .as_deref()
                .is_some_and(crate::path_class::is_auxiliary_path)
        {
            entry_ids.insert(n.id.to_string());
            continue;
        }
        match n.kind {
            NodeKind::Route | NodeKind::DartProvider | NodeKind::TestCase | NodeKind::TestGroup => {
                entry_ids.insert(n.id.to_string());
            }
            NodeKind::DartMethod => {
                if is_lifecycle_method_name(n.name.as_deref()) {
                    entry_ids.insert(n.id.to_string());
                }
            }
            NodeKind::DartFunction => {
                if n.path.as_deref().is_some_and(is_test_path)
                    && is_lifecycle_method_name(n.name.as_deref())
                {
                    entry_ids.insert(n.id.to_string());
                }
            }
            // Go: `main` and `init` are the language's hard entry points.
            // Treat `Test*` / `Benchmark*` / `Example*` exported functions as
            // entry points too because the `go test` runner resolves them by
            // name via reflection.
            NodeKind::GoFunction | NodeKind::GoMethod => {
                if is_go_entry_name(n.name.as_deref()) {
                    entry_ids.insert(n.id.to_string());
                }
            }
            // Swift: top-level `main()` (Swift Argument Parser / `@main`)
            // and XCTest's `test*` instance methods are reflection-driven
            // entries. Tag them so reachability does not flag them as dead.
            NodeKind::SwiftFunction | NodeKind::SwiftMethod => {
                if is_swift_entry_name(n.name.as_deref()) {
                    entry_ids.insert(n.id.to_string());
                }
            }
            // Python: pytest discovers `def test_*` / `class Test*` by
            // name, and conventional entrypoints (`main`, `__main__`,
            // `app`, `cli`) are invoked through reflection / frameworks.
            // Tag them so reachability never flags them as dead.
            NodeKind::PythonFunction | NodeKind::PythonMethod => {
                if is_python_entry_name(n.name.as_deref()) {
                    entry_ids.insert(n.id.to_string());
                }
            }
            // Rust: `fn main` is the binary entry point. `#[test]`
            // functions are tagged as `TestCase` by the Rust spec (so they
            // are seeded above), and the in-process heuristic resolver emits
            // `Calls` / `References` edges, so reachability now flows through
            // the real call graph instead of stopping at `main`. Cargo also
            // invokes `benches/`, `examples/` and `build.rs` targets directly
            // (criterion macros / harness reflection leave no in-repo call
            // edge), so everything in them counts as an entry.
            NodeKind::RustFunction | NodeKind::RustMethod
                if n.name.as_deref() == Some("main")
                    || n.path.as_deref().is_some_and(is_rust_cargo_target_path) =>
            {
                entry_ids.insert(n.id.to_string());
            }
            // C / C++: the runtime invokes `main` (and a few libc-contracted
            // hooks) with no in-repo caller.
            NodeKind::CFunction | NodeKind::CppFunction
                if matches!(n.name.as_deref(), Some("main" | "wmain" | "WinMain")) =>
            {
                entry_ids.insert(n.id.to_string());
            }
            // Java: the JVM invokes `main`; JUnit discovers `test*` methods
            // via reflection; `java.lang.Object` contract overrides
            // (`toString` in string concat, `equals`/`hashCode` in
            // collections, …) are runtime-invoked with no in-repo edge.
            NodeKind::JavaMethod if is_java_entry_name(n.name.as_deref()) => {
                entry_ids.insert(n.id.to_string());
            }
            _ => {}
        }
        // P17: any symbol whose `metadata_json` carries a framework
        // role classified as an "externally triggered" entry point
        // (FastAPI route, Celery task, Click/Typer command, …) is
        // not really dead — the framework calls it. We parse the
        // JSON inline; failures fall back to the default rules so a
        // malformed payload never flips a real symbol into reachable.
        if is_code_kind(n.kind) {
            if let Some(json) = n.metadata_json.as_deref() {
                if is_python_framework_entrypoint_metadata(json) {
                    entry_ids.insert(n.id.to_string());
                }
            }
        }
    }

    // Swift: types that derive (transitively) from a framework base are
    // instantiated by UIKit / AppKit / SwiftUI through storyboards, XIBs,
    // cell registration, segues and the `@UIApplicationMain` responder chain
    // — none of which leave an in-repo edge. Seed those types *and* their
    // members (which the framework invokes via lifecycle callbacks,
    // target-action and data sources) so reachability does not mistake the
    // whole UI layer for dead code. Driven by the `swift_inherits` metadata
    // the structural scanner records on every class/struct declaration.
    let swift_framework_types = swift_framework_instantiated_types(nodes);
    if !swift_framework_types.is_empty() {
        let mut contains_children: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in edges {
            if edge.kind == EdgeKind::Contains {
                contains_children
                    .entry(edge.from_id.as_str())
                    .or_default()
                    .push(edge.to_id.as_str());
            }
        }
        for n in nodes {
            if !matches!(n.kind, NodeKind::SwiftClass | NodeKind::SwiftStruct) {
                continue;
            }
            if !n
                .name
                .as_deref()
                .is_some_and(|nm| swift_framework_types.contains(nm))
            {
                continue;
            }
            // Seed the type and every symbol it transitively `Contains`.
            let mut stack = vec![n.id.as_str()];
            while let Some(id) = stack.pop() {
                if entry_ids.insert(id.to_string()) {
                    if let Some(children) = contains_children.get(id) {
                        stack.extend(children.iter().copied());
                    }
                }
            }
        }
    }
    entry_ids
}

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p).with_context(|| format!("invalid glob `{p}`"))?);
    }
    Ok(builder.build()?)
}

fn matches_set(set: &GlobSet, path: &str) -> bool {
    if set.is_empty() {
        return false;
    }
    set.is_match(path)
}

fn is_usage_edge(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls
            | EdgeKind::References
            | EdgeKind::ReadsProvider
            | EdgeKind::PersistsTo
            | EdgeKind::NavigatesTo
            | EdgeKind::SubscribesStream
            | EdgeKind::DeclaresVerification
            | EdgeKind::Contains
    )
}

fn is_code_kind(kind: NodeKind) -> bool {
    // Dead-code analysis counts every code symbol (any family-callable,
    // family-type, or known module-level node) plus tests as a thing
    // worth following reachability into. We route through the
    // `language_traits` predicates so adding a new language gives the
    // dead-code analyzer free coverage instead of silent drift.
    groundgraph_core::language_traits::is_code_symbol(kind)
        || groundgraph_core::language_traits::is_test(kind)
}

/// Flutter / Dart names that look like framework callbacks. Anything
/// matching here is treated as a *root* (cannot be high-confidence
/// dead) because the framework / reflection invokes it.
fn is_lifecycle_method_name(name: Option<&str>) -> bool {
    let Some(raw) = name else {
        return false;
    };
    // Names can be qualified like `MyWidget.build` — match on the
    // trailing identifier.
    let last = raw.rsplit(['.', '#']).next().unwrap_or(raw);
    matches!(
        last,
        "build"
            | "initState"
            | "dispose"
            | "didChangeDependencies"
            | "didUpdateWidget"
            | "didChangeAppLifecycleState"
            | "createState"
            | "createElement"
            | "main"
            | "noSuchMethod"
            | "toString"
            | "hashCode"
            | "=="
            | "didChangeMetrics"
            | "didChangePlatformBrightness"
            | "didChangeLocales"
            | "didHaveMemoryPressure"
    )
}

/// Cargo-invoked target files: nothing in-repo calls into them, cargo does.
fn is_rust_cargo_target_path(path: &str) -> bool {
    let p = path.replace('\\', "/");
    p.ends_with("/build.rs")
        || p == "build.rs"
        || p.split('/')
            .any(|seg| seg == "benches" || seg == "examples")
}

fn is_go_entry_name(name: Option<&str>) -> bool {
    let Some(raw) = name else {
        return false;
    };
    let last = raw.rsplit(['.', '#']).next().unwrap_or(raw);
    if last == "main" || last == "init" {
        return true;
    }
    // `go test` invokes any `TestXxx(*testing.T)` / `BenchmarkXxx` /
    // `ExampleXxx` via reflection — they have no callers in the graph
    // but must not be reported as dead. Same convention for the
    // `TestMain` hook.
    if let Some(rest) = last
        .strip_prefix("Test")
        .or_else(|| last.strip_prefix("Benchmark"))
        .or_else(|| last.strip_prefix("Example"))
    {
        return rest
            .chars()
            .next()
            .is_none_or(|c| c.is_ascii_uppercase() || !c.is_alphabetic());
    }
    false
}

/// UIKit / AppKit / WatchKit / SwiftUI base types that the OS instantiates
/// on the app's behalf — from storyboards, XIBs, `register(_:…)` cell
/// registration, segues, the responder chain and the `@UIApplicationMain` /
/// `@main` attribute. None of these leave an in-repo call edge, so a repo
/// type that (transitively) derives from one is reachable *through the
/// framework*, not dead. Kept deliberately to instantiable bases; `NSObject`
/// is excluded because nearly everything descends from it.
const SWIFT_FRAMEWORK_INSTANTIATED_BASES: &[&str] = &[
    // App / scene entry points (responder chain + @UIApplicationMain).
    "UIApplicationDelegate",
    "UIResponder",
    "UISceneDelegate",
    "UIWindowSceneDelegate",
    "NSApplicationDelegate",
    "WKExtensionDelegate",
    // View controllers.
    "UIViewController",
    "UITableViewController",
    "UICollectionViewController",
    "UINavigationController",
    "UITabBarController",
    "UISplitViewController",
    "UIPageViewController",
    "NSViewController",
    "WKInterfaceController",
    // Views, controls and reusable cells / supplementary views.
    "UIView",
    "UIControl",
    "UIScrollView",
    "UIStackView",
    "UICollectionView",
    "UITableView",
    "UIImageView",
    "UILabel",
    "UIButton",
    "UITextField",
    "UITextView",
    "UISwitch",
    "UISlider",
    "UISegmentedControl",
    "UIPickerView",
    "UIDatePicker",
    "UIProgressView",
    "UIActivityIndicatorView",
    "UIVisualEffectView",
    "UIRefreshControl",
    "UIWindow",
    "NSView",
    "UITableViewCell",
    "UICollectionViewCell",
    "UICollectionReusableView",
    "UITableViewHeaderFooterView",
    // SwiftUI value-type entry points (instantiated by the SwiftUI runtime).
    "View",
    "App",
    "Scene",
    "ViewModifier",
    "PreviewProvider",
    // Test harnesses: XCTest / Quick instantiate the case class by reflection
    // and invoke `setUp` / `tearDown` / `test*` with no in-repo caller.
    "XCTestCase",
    "XCTest",
    "QuickSpec",
];

/// Parse the `swift_inherits` list the structural scanner records on a Swift
/// type node (`{"swift_inherits":["UIViewController","Foo"]}`). Returns the
/// bare supertype names. Non-Swift / unrelated metadata yields `None`.
fn parse_swift_inherits(metadata_json: &str) -> Option<Vec<String>> {
    if !metadata_json.contains("swift_inherits") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(metadata_json).ok()?;
    let arr = value.get("swift_inherits")?.as_array()?;
    let names: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    (!names.is_empty()).then_some(names)
}

/// Compute the set of in-repo Swift type *names* that are framework
/// instantiated — i.e. whose transitive supertype closure (following in-repo
/// supers by name) reaches a [`SWIFT_FRAMEWORK_INSTANTIATED_BASES`] entry.
fn swift_framework_instantiated_types(nodes: &[groundgraph_core::Node]) -> BTreeSet<String> {
    // name -> direct supertype names, merged across (partial-class) decls.
    let mut supers: HashMap<String, BTreeSet<String>> = HashMap::new();
    for n in nodes {
        if !matches!(n.kind, NodeKind::SwiftClass | NodeKind::SwiftStruct) {
            continue;
        }
        let (Some(name), Some(meta)) = (n.name.as_deref(), n.metadata_json.as_deref()) else {
            continue;
        };
        if let Some(list) = parse_swift_inherits(meta) {
            let entry = supers.entry(name.to_string()).or_default();
            entry.extend(list);
        }
    }
    let bases: BTreeSet<&str> = SWIFT_FRAMEWORK_INSTANTIATED_BASES.iter().copied().collect();

    // Memoised DFS over the in-repo supertype graph, with a recursion stack
    // guarding against cyclic / self-referential inheritance metadata.
    fn reaches(
        name: &str,
        supers: &HashMap<String, BTreeSet<String>>,
        bases: &BTreeSet<&str>,
        memo: &mut HashMap<String, bool>,
        stack: &mut BTreeSet<String>,
    ) -> bool {
        if bases.contains(name) {
            return true;
        }
        if let Some(v) = memo.get(name) {
            return *v;
        }
        if !stack.insert(name.to_string()) {
            return false;
        }
        let mut hit = false;
        if let Some(direct) = supers.get(name) {
            for s in direct {
                if reaches(s, supers, bases, memo, stack) {
                    hit = true;
                    break;
                }
            }
        }
        stack.remove(name);
        memo.insert(name.to_string(), hit);
        hit
    }

    let mut memo: HashMap<String, bool> = HashMap::new();
    let mut out: BTreeSet<String> = BTreeSet::new();
    let names: Vec<String> = supers.keys().cloned().collect();
    for name in names {
        let mut stack = BTreeSet::new();
        if reaches(&name, &supers, &bases, &mut memo, &mut stack) {
            out.insert(name);
        }
    }
    out
}

fn is_swift_entry_name(name: Option<&str>) -> bool {
    let Some(raw) = name else {
        return false;
    };
    let last = raw.rsplit(['.', '#']).next().unwrap_or(raw);
    if last == "main" {
        return true;
    }
    // XCTest discovers `test*` instance methods by reflection, and
    // SwiftUI / UIKit lifecycle callbacks are likewise framework-invoked.
    // Many UIKit callbacks share the *base* selector name the scanner records
    // (`application(_:didFinishLaunchingWithOptions:)` → `application`,
    // `scene(_:willConnectTo:)` → `scene`), so the bare heads are listed too.
    matches!(
        last,
        // UIViewController lifecycle / layout overrides.
        "viewDidLoad"
            | "viewWillAppear"
            | "viewDidAppear"
            | "viewWillDisappear"
            | "viewDidDisappear"
            | "viewWillLayoutSubviews"
            | "viewDidLayoutSubviews"
            | "viewSafeAreaInsetsDidChange"
            | "updateViewConstraints"
            | "didReceiveMemoryWarning"
            | "loadView"
            | "awakeFromNib"
            | "prepareForReuse"
            // UIApplicationDelegate (bare + SwiftUI-style heads).
            | "application"
            | "applicationDidFinishLaunching"
            | "applicationDidBecomeActive"
            | "applicationWillResignActive"
            | "applicationDidEnterBackground"
            | "applicationWillEnterForeground"
            | "applicationWillTerminate"
            | "applicationDidReceiveMemoryWarning"
            | "applicationProtectedDataDidBecomeAvailable"
            | "applicationProtectedDataWillBecomeUnavailable"
            // UISceneDelegate.
            | "scene"
            | "sceneDidDisconnect"
            | "sceneDidBecomeActive"
            | "sceneWillResignActive"
            | "sceneWillEnterForeground"
            | "sceneDidEnterBackground"
            // SwiftUI entry / body.
            | "body"
    ) || last.starts_with("test")
}

/// Names that Python frameworks / runtimes invoke via reflection or
/// CLI dispatch — treating them as dead would generate noise. We are
/// intentionally conservative because Python is highly dynamic: only
/// the very common entrypoints get the "never dead" tag.
/// Inspect a node's `metadata_json` payload (typically written by
/// the Python adapter via [`crate::python_frameworks::FrameworkRole`])
/// and decide whether the symbol is a framework-triggered entry
/// point that the engine must NOT flag as dead. We only deserialize
/// when the payload looks like a framework role to avoid pulling in
/// serde_json for unrelated metadata schemas.
fn is_python_framework_entrypoint_metadata(json: &str) -> bool {
    if !json.contains("\"framework\"") {
        return false;
    }
    match serde_json::from_str::<crate::python_frameworks::FrameworkRole>(json) {
        Ok(role) => role.is_framework_entrypoint(),
        Err(_) => false,
    }
}

fn is_java_entry_name(name: Option<&str>) -> bool {
    let Some(raw) = name else {
        return false;
    };
    let last = raw.rsplit(['.', '#']).next().unwrap_or(raw);
    // JVM entry point + java.lang.Object contract methods the runtime (or
    // collections / string concatenation) invokes without an in-repo edge.
    if matches!(
        last,
        "main" | "toString" | "equals" | "hashCode" | "clone" | "finalize"
    ) {
        return true;
    }
    // JUnit 3 convention (`testFoo`); JUnit 4/5 `@Test` methods are usually
    // ALSO named `test*` in the wild. Annotation-driven discovery without
    // the prefix is covered by the TestCase nodes the indexer emits.
    last.starts_with("test") && last.len() > 4
}

fn is_python_entry_name(name: Option<&str>) -> bool {
    let Some(raw) = name else {
        return false;
    };
    let last = raw.rsplit(['.', '#']).next().unwrap_or(raw);
    if matches!(
        last,
        "main" | "__main__" | "app" | "cli" | "create_app" | "run"
    ) {
        return true;
    }
    // `pytest` collects `def test_*` and `class Test*.test_*`. They are
    // also surfaced as `TestCase` nodes, but the underlying function
    // node should not be dead either.
    if last.starts_with("test_") {
        return true;
    }
    // Dunder lifecycle hooks (`__init__`, `__call__`, `__enter__`,
    // `__exit__`, etc.) are framework-invoked.
    if last.starts_with("__") && last.ends_with("__") {
        return true;
    }
    false
}

fn is_private_dart_name(name: Option<&str>) -> bool {
    let Some(raw) = name else {
        return false;
    };
    let last = raw.rsplit(['.', '#']).next().unwrap_or(raw);
    last.starts_with('_')
}

fn is_test_path(path: &str) -> bool {
    path.starts_with("test/") || path.contains("/test/")
}

fn seed_config_entrypoints(
    entrypoints: &[String],
    nodes: &[groundgraph_core::Node],
    sink: &mut BTreeSet<String>,
) {
    // Each entrypoint path is a *file*. Promote any top-level
    // function/method declared in that file (typically `main()`) and
    // the file node itself.
    let entry_files: BTreeSet<&str> = entrypoints.iter().map(String::as_str).collect();
    for n in nodes {
        let Some(path) = n.path.as_deref() else {
            continue;
        };
        if entry_files.contains(path) {
            sink.insert(n.id.to_string());
        }
    }
}

fn classify(
    node: &groundgraph_core::Node,
    inbound: &[&groundgraph_core::EdgeAssertion],
    public_set: &GlobSet,
    reachable: &BTreeSet<String>,
) -> (DeadCodeConfidence, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    let mitigating_factors: Vec<String> = collect_mitigating_factors(node, public_set);
    let inbound_usage: Vec<&&groundgraph_core::EdgeAssertion> = inbound
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EdgeKind::Calls
                    | EdgeKind::References
                    | EdgeKind::ReadsProvider
                    | EdgeKind::PersistsTo
                    | EdgeKind::NavigatesTo
                    | EdgeKind::SubscribesStream
                    | EdgeKind::DeclaresVerification
            )
        })
        .collect();
    // Some edges (e.g. Contains from a file to a class) are not
    // "usage"; exclude them from the inbound count too.
    let live_inbound: Vec<&&&groundgraph_core::EdgeAssertion> = inbound_usage
        .iter()
        .filter(|e| reachable.contains(e.from_id.as_str()))
        .collect();
    let dead_island_inbound: Vec<&&&groundgraph_core::EdgeAssertion> = inbound_usage
        .iter()
        .filter(|e| !reachable.contains(e.from_id.as_str()))
        .collect();

    reasons.push(reason_unreached(node));

    if inbound_usage.is_empty() {
        reasons.push("无任何 calls / references / declares_verification 入边".into());
    } else if live_inbound.is_empty() && !dead_island_inbound.is_empty() {
        reasons.push(format!(
            "仅被 {} 个同样不可达的符号引用（dead island）",
            dead_island_inbound.len()
        ));
    } else {
        // Inbound from live nodes but still unreached — only happens
        // when forward edges are missing (e.g., reflective access).
        reasons.push("入边存在但未被入口点覆盖".into());
    }

    // v0.3.0-A: if every surviving inbound usage edge sits in the
    // low-tier bucket (per `edge_confidence::confidence_for_edge`),
    // tell the operator the evidence is weak — typical when the
    // candidate is reached only via `*_ast` AST fallback or via
    // git-diff provisional edges. Reach-set decisions stay unchanged;
    // this is reason-string-only.
    if !inbound_usage.is_empty() {
        let summary = crate::confidence_view::summarize_edges(
            inbound_usage.iter().map(|e| **e),
            crate::confidence_view::EdgeQualityScope::Usage,
        );
        if summary.is_only_low() {
            reasons.push(format!(
                "仅有 {} 条 low-tier 入边（来自低置信 indexer / AST fallback / lightweight resolver），证据较弱",
                summary.low,
            ));
        }
    }

    for m in &mitigating_factors {
        reasons.push(m.clone());
    }

    let confidence = if !inbound_usage.is_empty() && live_inbound.is_empty() {
        // Dead island.
        DeadCodeConfidence::Low
    } else if mitigating_factors.is_empty() && inbound_usage.is_empty() {
        DeadCodeConfidence::High
    } else if inbound_usage.is_empty() {
        DeadCodeConfidence::Medium
    } else {
        DeadCodeConfidence::Low
    };
    (confidence, reasons)
}

fn reason_unreached(node: &groundgraph_core::Node) -> String {
    match node.kind {
        NodeKind::DartMethod | NodeKind::DartFunction | NodeKind::DartConstructor => {
            "未被 main / 路由 / Provider / 测试 / lifecycle 任一入口点可达".into()
        }
        NodeKind::DartClass => "类未被任何入口点引用".into(),
        NodeKind::TestCase | NodeKind::TestGroup => "测试未被 test runner / 父级 group 关联".into(),
        NodeKind::SwiftMethod | NodeKind::SwiftFunction | NodeKind::SwiftInitializer => {
            "未被 Swift 入口（@main / 公开 API / 测试）任一入口点可达".into()
        }
        NodeKind::SwiftClass
        | NodeKind::SwiftStruct
        | NodeKind::SwiftEnum
        | NodeKind::SwiftProtocol => "类型未被任何 Swift 入口点引用".into(),
        NodeKind::GoMethod | NodeKind::GoFunction => {
            "未被 Go 入口（main / init / 公开 API / 测试）任一入口点可达".into()
        }
        NodeKind::GoStruct | NodeKind::GoInterface => "类型未被任何 Go 入口点引用".into(),
        NodeKind::PythonMethod | NodeKind::PythonFunction => {
            "未被 Python 入口（main / app / pytest / dunder / 公开 API）任一入口点可达".into()
        }
        NodeKind::PythonClass | NodeKind::PythonModule => {
            "类型 / 模块未被任何 Python 入口点引用".into()
        }
        _ => "未被任何入口点可达".into(),
    }
}

/// A `DartMethod` whose qualified name is `Class.Class` (its trailing two
/// segments are identical) is really a *constructor*: the vendored Dart
/// grammar lowers a constructor that carries a body (or a `factory`) into a
/// method-shaped node and mis-extracts the class name as the member name. In
/// valid Dart a method can never share its class's simple name (that slot is
/// the constructor), so this shape is an unambiguous constructor twin. The
/// analyzer's precise `dart_ctor::` node carries the real construction edges,
/// leaving this twin without inbound — so it must never be a *high*-confidence
/// deletion candidate, mirroring the explicit `DartConstructor` demotion.
fn is_dart_constructor_shaped_method(node: &groundgraph_core::Node) -> bool {
    if node.kind != NodeKind::DartMethod {
        return false;
    }
    let Some(qualified) = node.stable_key.as_deref() else {
        return false;
    };
    let mut segments = qualified.rsplit('.');
    match (segments.next(), segments.next()) {
        (Some(leaf), Some(parent)) => !leaf.is_empty() && leaf == parent,
        _ => false,
    }
}

fn collect_mitigating_factors(node: &groundgraph_core::Node, public_set: &GlobSet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(p) = node.path.as_deref() {
        if matches_set(public_set, p) {
            out.push(format!(
                "位于 public_api_roots（{p}），可能被仓库外消费者使用"
            ));
        }
    }
    if node.kind == NodeKind::DartConstructor || is_dart_constructor_shaped_method(node) {
        out.push(
            "构造器调用可能由类实例化、const 构造或框架创建触发，默认不作为 high 置信删除候选"
                .into(),
        );
    }
    if !is_private_dart_name(node.name.as_deref()) {
        out.push("公共可见符号（无 `_` 前缀），可能被反射 / 代码生成 / 框架调用".into());
    }
    if is_lifecycle_method_name(node.name.as_deref()) {
        out.push("名称匹配 Flutter / Dart 生命周期 / 框架回调".into());
    }
    out
}

fn orphan_test_candidate(
    node: &groundgraph_core::Node,
    outbound: &[&groundgraph_core::EdgeAssertion],
) -> Option<DeadCodeCandidate> {
    if outbound
        .iter()
        .any(|e| e.kind == EdgeKind::DeclaresVerification)
    {
        return None;
    }
    let id = node.id.to_string();
    let label = node
        .name
        .clone()
        .or_else(|| node.stable_key.clone())
        .unwrap_or_else(|| id.clone());
    let line_range = match (node.start_line, node.end_line) {
        (Some(s), Some(e)) => Some((s, e)),
        _ => None,
    };
    Some(DeadCodeCandidate {
        id,
        kind: node.kind.as_str().into(),
        label,
        path: node.path.clone(),
        line_range,
        confidence: DeadCodeConfidence::Low,
        reasons: vec![
            "测试没有解析到验证目标（无 declares_verification 边）".into(),
            "测试仍可能被 test runner 执行；这是孤儿测试提示，不是删除建议".into(),
        ],
        inbound_sources: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Display helper
// ---------------------------------------------------------------------------

/// Group candidates by confidence for human-readable output.
pub fn group_by_confidence(
    candidates: &[DeadCodeCandidate],
) -> BTreeMap<DeadCodeConfidence, Vec<&DeadCodeCandidate>> {
    let mut out: BTreeMap<DeadCodeConfidence, Vec<&DeadCodeCandidate>> = BTreeMap::new();
    for c in candidates {
        out.entry(c.confidence).or_default().push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::{
        ArtifactId, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus, Node, NodeKind,
    };
    use groundgraph_store::Store;
    use tempfile::TempDir;

    fn empty_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn insert_method(store: &mut Store, file: &str, qualified: &str) -> String {
        let id = format!("dart_method::{file}#{qualified}");
        store
            .upsert_node(&Node {
                id: ArtifactId::new(id.clone()),
                kind: NodeKind::DartMethod,
                path: Some(file.into()),
                name: Some(qualified.into()),
                start_line: Some(1),
                end_line: Some(5),
                content_hash: None,
                stable_key: None,
                source_file: Some(file.into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        id
    }

    fn insert_function(store: &mut Store, file: &str, name: &str) -> String {
        let id = format!("dart_function::{file}#{name}");
        store
            .upsert_node(&Node {
                id: ArtifactId::new(id.clone()),
                kind: NodeKind::DartFunction,
                path: Some(file.into()),
                name: Some(name.into()),
                start_line: Some(1),
                end_line: Some(3),
                content_hash: None,
                stable_key: None,
                source_file: Some(file.into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        id
    }

    fn insert_calls(store: &mut Store, from: &str, to: &str) {
        store
            .upsert_edge(&EdgeAssertion {
                id: ArtifactId::new(format!("calls::{from}->{to}")),
                from_id: ArtifactId::new(from.to_string()),
                to_id: ArtifactId::new(to.to_string()),
                kind: EdgeKind::Calls,
                source: EdgeSource::LanguageAdapter,
                certainty: EdgeCertainty::Fact,
                status: EdgeStatus::Confirmed,
                confidence: 1.0,
                evidence_json: None,
                source_file: None,
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
    }

    fn config_with(entrypoints: Vec<&str>) -> DeadCodeConfig {
        DeadCodeConfig {
            entrypoints: entrypoints.into_iter().map(String::from).collect(),
            ignore: vec!["**/*.g.dart".into()],
            public_api_roots: vec![],
        }
    }

    fn insert_node_kind(store: &mut Store, id: &str, kind: NodeKind, file: &str, name: &str) {
        store
            .upsert_node(&Node {
                id: ArtifactId::new(id.to_string()),
                kind,
                path: Some(file.into()),
                name: Some(name.into()),
                start_line: Some(1),
                end_line: Some(9),
                content_hash: None,
                stable_key: None,
                source_file: Some(file.into()),
                source_hash: None,
                indexer: Some("rust_treesitter".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
    }

    fn insert_node_meta(
        store: &mut Store,
        id: &str,
        kind: NodeKind,
        file: &str,
        name: &str,
        metadata_json: Option<&str>,
    ) {
        store
            .upsert_node(&Node {
                id: ArtifactId::new(id.to_string()),
                kind,
                path: Some(file.into()),
                name: Some(name.into()),
                start_line: Some(1),
                end_line: Some(9),
                content_hash: None,
                stable_key: None,
                source_file: Some(file.into()),
                source_hash: None,
                indexer: Some("swift_treesitter".into()),
                index_generation: None,
                metadata_json: metadata_json.map(String::from),
            })
            .unwrap();
    }

    fn insert_contains(store: &mut Store, from: &str, to: &str) {
        store
            .upsert_edge(&EdgeAssertion {
                id: ArtifactId::new(format!("contains::{from}->{to}")),
                from_id: ArtifactId::new(from.to_string()),
                to_id: ArtifactId::new(to.to_string()),
                kind: EdgeKind::Contains,
                source: EdgeSource::LanguageAdapter,
                certainty: EdgeCertainty::Fact,
                status: EdgeStatus::Confirmed,
                confidence: 1.0,
                evidence_json: None,
                source_file: None,
                source_hash: None,
                indexer: Some("rust_treesitter".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
    }

    #[test]
    fn c_and_cpp_main_are_implicit_entry_points() {
        // C/C++: the runtime invokes `main`; nothing in the repo calls it.
        let (mut store, _dir) = empty_store();
        insert_node_kind(
            &mut store,
            "c::src/server.c::main",
            NodeKind::CFunction,
            "src/server.c",
            "main",
        );
        insert_node_kind(
            &mut store,
            "c::src/dict.c::dictCreate",
            NodeKind::CFunction,
            "src/dict.c",
            "dictCreate",
        );
        insert_calls(
            &mut store,
            "c::src/server.c::main",
            "c::src/dict.c::dictCreate",
        );
        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions::default(),
            &DeadCodeConfig::default(),
        )
        .unwrap();
        assert!(
            !report
                .candidates
                .iter()
                .any(|c| c.id.contains("main") || c.id.contains("dictCreate")),
            "main and its callees are alive: {:?}",
            report.candidates.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn auxiliary_code_is_an_implicit_entry_not_a_dead_candidate() {
        // gson dogfood: `extras/examples/**` classes were reported dead.
        // Examples / demos / tools are standalone programs whose "caller" is
        // a human reading docs — they are entries (which also keeps the core
        // APIs they exercise reachable), never dead-code candidates.
        let (mut store, _dir) = empty_store();
        insert_node_kind(
            &mut store,
            "java::examples/rawcollections/Example.java::Example.run",
            NodeKind::JavaMethod,
            "examples/rawcollections/Example.java",
            "run",
        );
        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions::default(),
            &DeadCodeConfig::default(),
        )
        .unwrap();
        assert!(
            report.candidates.is_empty(),
            "examples are entries, not dead code: {:?}",
            report.candidates.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn java_main_junit_and_object_contracts_are_implicit_entry_points() {
        // Regression (gson): `public static void main` (JVM-invoked), JUnit
        // `testXxx` methods (reflection-discovered) and `java.lang.Object`
        // contract overrides (`toString` — invoked by the runtime / string
        // concat, never via an in-repo edge) were all reported dead.
        let (mut store, _dir) = empty_store();
        insert_node_kind(
            &mut store,
            "java::src/Example.java::Example.main",
            NodeKind::JavaMethod,
            "src/Example.java",
            "main",
        );
        insert_node_kind(
            &mut store,
            "java::src/Money.java::Money.toString",
            NodeKind::JavaMethod,
            "src/Money.java",
            "toString",
        );
        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions::default(),
            &DeadCodeConfig::default(),
        )
        .unwrap();
        assert!(
            !report
                .candidates
                .iter()
                .any(|c| c.id.contains("main") || c.id.contains("toString")),
            "JVM/runtime-invoked methods must not be dead: {:?}",
            report.candidates.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn swift_framework_subclasses_and_their_members_are_reachable() {
        // Regression (yunlan): UIKit instantiates `UIViewController` / `UIView`
        // / cell subclasses from storyboards, XIBs and cell registration with
        // no in-repo caller. Such types — and their members, which the
        // framework invokes via the responder chain / target-action / data
        // sources — must be seeded as reachable from their inheritance clause,
        // never reported as dead. A pure value/logic type with no framework
        // base and no caller stays dead, so the report keeps its signal.
        let (mut store, _dir) = empty_store();
        // FooCell -> BaseCell -> UITableViewCell (transitive, in-repo chain).
        insert_node_meta(
            &mut store,
            "swift::A.swift::BaseCell",
            NodeKind::SwiftClass,
            "A.swift",
            "BaseCell",
            Some(r#"{"swift_inherits":["UITableViewCell"]}"#),
        );
        insert_node_meta(
            &mut store,
            "swift::A.swift::FooCell",
            NodeKind::SwiftClass,
            "A.swift",
            "FooCell",
            Some(r#"{"swift_inherits":["BaseCell"]}"#),
        );
        insert_node_kind(
            &mut store,
            "swift::A.swift::FooCell.configure",
            NodeKind::SwiftMethod,
            "A.swift",
            "configure",
        );
        insert_contains(
            &mut store,
            "swift::A.swift::FooCell",
            "swift::A.swift::FooCell.configure",
        );
        // App entry: AppDelegate conforms to UIApplicationDelegate.
        insert_node_meta(
            &mut store,
            "swift::App.swift::AppDelegate",
            NodeKind::SwiftClass,
            "App.swift",
            "AppDelegate",
            Some(r#"{"swift_inherits":["UIResponder","UIApplicationDelegate"]}"#),
        );
        // A pure-logic type with no framework base and no caller: still dead.
        insert_node_meta(
            &mut store,
            "swift::A.swift::DeadHelper",
            NodeKind::SwiftClass,
            "A.swift",
            "DeadHelper",
            None,
        );

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec![]),
        )
        .unwrap();
        let dead: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !dead.contains(&"swift::A.swift::FooCell"),
            "a framework-instantiated subclass must be reachable: {dead:?}"
        );
        assert!(
            !dead.contains(&"swift::A.swift::BaseCell"),
            "a transitive framework base must be reachable: {dead:?}"
        );
        assert!(
            !dead.contains(&"swift::A.swift::FooCell.configure"),
            "members of a framework subclass are framework-invoked: {dead:?}"
        );
        assert!(
            !dead.contains(&"swift::App.swift::AppDelegate"),
            "the @UIApplicationMain delegate is the app entry, never dead: {dead:?}"
        );
        assert!(
            dead.contains(&"swift::A.swift::DeadHelper"),
            "a pure-logic type with no framework base / caller is still dead: {dead:?}"
        );
    }

    #[test]
    fn module_containing_a_reachable_symbol_is_not_dead() {
        let (mut store, _dir) = empty_store();
        // entry → calls feature::do_work, which lives inside module `feature`.
        let main_id = insert_function(&mut store, "lib/main.dart", "main");
        let module_id = "rust::crates/x/src/feature.rs::feature";
        insert_node_kind(
            &mut store,
            module_id,
            NodeKind::RustModule,
            "crates/x/src/feature.rs",
            "feature",
        );
        let work_id = "rust::crates/x/src/feature.rs::feature::do_work";
        insert_node_kind(
            &mut store,
            work_id,
            NodeKind::RustFunction,
            "crates/x/src/feature.rs",
            "do_work",
        );
        insert_contains(&mut store, module_id, work_id);
        insert_calls(&mut store, &main_id, work_id);

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            // Entry points are *files*: seed the file that declares `main`.
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let _ = &main_id;

        let dead: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !dead.contains(&module_id),
            "a module holding live code must not be dead, got {dead:?}"
        );
        assert!(
            !dead.contains(&work_id),
            "the reachable function must not be dead, got {dead:?}"
        );
    }

    #[test]
    fn rust_cargo_targets_are_entry_points_not_dead_code() {
        // `benches/`, `examples/` and `build.rs` are cargo-invoked targets:
        // nothing in-repo calls them, yet cargo runs them. Dogfooding the
        // sokoban repo flagged `benches/generator_bench.rs::bench_generation`
        // as dead — a guaranteed false positive for any crate with benches.
        let (mut store, _dir) = empty_store();
        insert_node_kind(
            &mut store,
            "rust::generator/benches/gen_bench.rs::bench_generation",
            NodeKind::RustFunction,
            "generator/benches/gen_bench.rs",
            "bench_generation",
        );
        insert_node_kind(
            &mut store,
            "rust::examples/demo.rs::run_demo",
            NodeKind::RustFunction,
            "examples/demo.rs",
            "run_demo",
        );
        insert_node_kind(
            &mut store,
            "rust::crates/x/build.rs::generate_tables",
            NodeKind::RustFunction,
            "crates/x/build.rs",
            "generate_tables",
        );
        insert_node_kind(
            &mut store,
            "rust::crates/x/src/lib.rs::truly_dead",
            NodeKind::RustFunction,
            "crates/x/src/lib.rs",
            "truly_dead",
        );

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec![]),
        )
        .unwrap();

        let dead: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        for cargo_target in [
            "rust::generator/benches/gen_bench.rs::bench_generation",
            "rust::examples/demo.rs::run_demo",
            "rust::crates/x/build.rs::generate_tables",
        ] {
            assert!(
                !dead.contains(&cargo_target),
                "cargo target {cargo_target} must not be dead, got {dead:?}"
            );
        }
        assert!(
            dead.contains(&"rust::crates/x/src/lib.rs::truly_dead"),
            "the unreferenced library function must still be reported, got {dead:?}"
        );
    }

    #[test]
    fn module_level_reference_from_reachable_file_keeps_target_alive() {
        // Mirrors the Python `FACTORS = [_amihud(20)]` registration pattern:
        // a private helper is only ever named by a *module-level* statement,
        // which the heuristic resolver anchors on the file node. The helper
        // must stay alive once that file is proven reachable (it owns another
        // symbol that an entrypoint calls), instead of being a false positive.
        let (mut store, _dir) = empty_store();

        // app.py (entrypoint file) declares `main`, which calls registry.register.
        let main_id = "python::lib/app.py::main";
        insert_node_kind(
            &mut store,
            main_id,
            NodeKind::PythonFunction,
            "lib/app.py",
            "main",
        );

        // registry.py is NOT an entrypoint. It is only reachable transitively
        // because `register` is called from `main`.
        let registry_file = "file::lib/registry.py";
        insert_node_kind(
            &mut store,
            registry_file,
            NodeKind::File,
            "lib/registry.py",
            "registry.py",
        );
        let register_id = "python::lib/registry.py::register";
        insert_node_kind(
            &mut store,
            register_id,
            NodeKind::PythonFunction,
            "lib/registry.py",
            "register",
        );
        let amihud_id = "python::lib/registry.py::_amihud";
        insert_node_kind(
            &mut store,
            amihud_id,
            NodeKind::PythonFunction,
            "lib/registry.py",
            "_amihud",
        );

        // File owns both top-level functions.
        insert_contains(&mut store, registry_file, register_id);
        insert_contains(&mut store, registry_file, amihud_id);
        // main → register makes the file (transitively) reachable.
        insert_calls(&mut store, main_id, register_id);
        // Module-level `_amihud(20)` registration: anchored on the file node.
        insert_calls(&mut store, registry_file, amihud_id);

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/app.py"]),
        )
        .unwrap();

        let dead: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !dead.contains(&amihud_id),
            "a helper referenced only by a module-level statement on a reachable \
             file must not be dead, got {dead:?}"
        );
    }

    #[test]
    fn reachable_count_counts_code_symbols_not_test_nodes() {
        let (mut store, _dir) = empty_store();
        let main_id = insert_function(&mut store, "lib/main.dart", "main");
        let helper = insert_function(&mut store, "lib/util.dart", "helper");
        insert_calls(&mut store, &main_id, &helper);
        // A reachable TestCase node (auto-seeded as a production root) must not
        // inflate the reachable *code-symbol* count past the total — otherwise
        // the header reads `可达 > 总符号`, which is nonsensical.
        insert_node_kind(
            &mut store,
            "test::test/util_test.dart::checks_helper",
            NodeKind::TestCase,
            "test/util_test.dart",
            "checks_helper",
        );

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();

        assert_eq!(
            report.stats.total_code_symbols, 2,
            "main + helper are the only counted code symbols"
        );
        assert!(
            report.stats.reachable <= report.stats.total_code_symbols,
            "reachable code symbols ({}) must never exceed total ({})",
            report.stats.reachable,
            report.stats.total_code_symbols,
        );
        assert_eq!(
            report.stats.reachable, 2,
            "main + helper are reachable; the seeded test-case node is not a code symbol"
        );
    }

    #[test]
    fn high_confidence_dead_when_private_unreferenced_unreached() {
        let (mut store, _dir) = empty_store();
        // main calls reachable_helper; orphan is private + has zero inbound edges.
        let main_id = insert_function(&mut store, "lib/main.dart", "main");
        let helper = insert_method(&mut store, "lib/util.dart", "Helper.reachable");
        let orphan = insert_function(&mut store, "lib/util.dart", "_unused_internal");
        insert_calls(&mut store, &main_id, &helper);

        let opts = DeadCodeOptions {
            repo_root: ".".into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        };
        let report =
            analyze_dead_code_with_store(&store, opts, &config_with(vec!["lib/main.dart"]))
                .unwrap();
        let by_id: BTreeMap<&str, &DeadCodeCandidate> = report
            .candidates
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect();
        let c = by_id
            .get(orphan.as_str())
            .expect("private unused function must surface as a candidate");
        assert_eq!(c.confidence, DeadCodeConfidence::High);
        // Helper is reachable, so it must NOT appear.
        assert!(
            !by_id.contains_key(helper.as_str()),
            "reachable helper must not appear as dead"
        );
    }

    #[test]
    fn medium_confidence_when_public_or_lifecycle_unreached() {
        let (mut store, _dir) = empty_store();
        let main_id = insert_function(&mut store, "lib/main.dart", "main");
        // Public name, no inbound usage edges, unreachable.
        let public_unused = insert_method(&mut store, "lib/api/foo.dart", "PublicApi.unused");
        // Lifecycle build() with no inbound (some Flutter route mounts widgets
        // implicitly via runApp, which we don't model).
        let widget_build = insert_method(&mut store, "lib/widgets/x.dart", "MyWidget.build");
        // Touch main so it's not flagged.
        let _ = main_id;
        let opts = DeadCodeOptions {
            repo_root: ".".into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        };
        let report =
            analyze_dead_code_with_store(&store, opts, &config_with(vec!["lib/main.dart"]))
                .unwrap();
        let by_id: BTreeMap<&str, &DeadCodeCandidate> = report
            .candidates
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect();
        // build() is an entry-point lifecycle root → NOT reported.
        assert!(
            !by_id.contains_key(widget_build.as_str()),
            "lifecycle build() must be treated as root, not flagged"
        );
        // PublicApi.unused is public and unreachable → Medium.
        let c = by_id
            .get(public_unused.as_str())
            .expect("public unused must surface");
        assert_eq!(c.confidence, DeadCodeConfidence::Medium);
        assert!(c.reasons.iter().any(|r| r.contains("公共可见符号")));
    }

    #[test]
    fn constructors_are_demoted_from_high_confidence_even_when_synthetic_name_is_private() {
        let (mut store, _dir) = empty_store();
        let _ = insert_function(&mut store, "lib/main.dart", "main");
        let ctor_id = "dart_constructor::lib/widget.dart#HomeScreen._default";
        store
            .upsert_node(&Node {
                id: ArtifactId::new(ctor_id.to_string()),
                kind: NodeKind::DartConstructor,
                path: Some("lib/widget.dart".into()),
                name: Some("_default".into()),
                start_line: Some(1),
                end_line: Some(1),
                content_hash: None,
                stable_key: None,
                source_file: Some("lib/widget.dart".into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let candidate = report
            .candidates
            .iter()
            .find(|c| c.id == ctor_id)
            .expect("unreached constructor should still be surfaced");
        assert_eq!(candidate.confidence, DeadCodeConfidence::Medium);
        assert!(
            candidate.reasons.iter().any(|r| r.contains("构造器调用")),
            "constructor demotion reason must be explicit: {:?}",
            candidate.reasons
        );
    }

    #[test]
    fn constructor_shaped_dart_method_phantom_is_demoted_from_high() {
        // The vendored Dart grammar lowers a constructor *with a body* (or a
        // `factory`) into a method-shaped node whose qualified name is
        // `Class.Class`. The analyzer's precise `dart_ctor::` node owns the
        // real construction edges, so this twin has no inbound — but it is a
        // constructor, never a deletable method, and must not be high.
        let (mut store, _dir) = empty_store();
        let _ = insert_function(&mut store, "lib/main.dart", "main");
        let phantom_id = "dart_method::lib/color.dart#_LabColor._LabColor";
        store
            .upsert_node(&Node {
                id: ArtifactId::new(phantom_id.to_string()),
                kind: NodeKind::DartMethod,
                path: Some("lib/color.dart".into()),
                name: Some("_LabColor".into()),
                start_line: Some(1),
                end_line: Some(3),
                content_hash: None,
                stable_key: Some("_LabColor._LabColor".into()),
                source_file: Some("lib/color.dart".into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let candidate = report
            .candidates
            .iter()
            .find(|c| c.id == phantom_id)
            .expect("unreached constructor twin should still be surfaced");
        assert_eq!(
            candidate.confidence,
            DeadCodeConfidence::Medium,
            "constructor-shaped method must be demoted: {:?}",
            candidate.reasons
        );
        assert!(
            candidate.reasons.iter().any(|r| r.contains("构造器调用")),
            "demotion reason must be explicit: {:?}",
            candidate.reasons
        );
    }

    #[test]
    fn reachable_constructor_keeps_its_owning_class_alive() {
        // l10n delegate shape: a private class is named *only* by a construction
        // `delegate = _AppLocalizationsDelegate()`. The construction edge lands
        // on the ctor node, so without ctor→class up-propagation the class node
        // has no inbound and looks like high-confidence dead code.
        let (mut store, _dir) = empty_store();
        let main_id = insert_function(&mut store, "lib/main.dart", "main");

        // The private delegate class + its const ctor.
        let class_id = "dart_class::lib/l10n.dart#_Delegate";
        store
            .upsert_node(&Node {
                id: ArtifactId::new(class_id.to_string()),
                kind: NodeKind::DartClass,
                path: Some("lib/l10n.dart".into()),
                name: Some("_Delegate".into()),
                start_line: Some(1),
                end_line: Some(3),
                content_hash: None,
                stable_key: Some("_Delegate".into()),
                source_file: Some("lib/l10n.dart".into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        let ctor_id = "dart_ctor::lib/l10n.dart#_Delegate.<default>";
        store
            .upsert_node(&Node {
                id: ArtifactId::new(ctor_id.to_string()),
                kind: NodeKind::DartConstructor,
                path: Some("lib/l10n.dart".into()),
                name: Some("<default>".into()),
                start_line: Some(2),
                end_line: Some(2),
                content_hash: None,
                stable_key: Some("_Delegate.<default>".into()),
                source_file: Some("lib/l10n.dart".into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        // Class contains its ctor; an entrypoint constructs the class.
        insert_contains(&mut store, class_id, ctor_id);
        insert_calls(&mut store, &main_id, ctor_id);

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let dead: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !dead.contains(&class_id),
            "a class kept alive only through its constructed ctor must not be \
             dead, got {dead:?}"
        );
    }

    #[test]
    fn class_named_only_through_a_static_method_call_is_not_dead() {
        // `LunarService.generateDayInfoRange()` — a class used purely through a
        // static method call. The call edge lands on the *method* node, never
        // on the class, so without member→type up-propagation the class looks
        // like dead code even though it is plainly in use. The unused sibling
        // method stays a candidate (up-propagation never flows back down past
        // what the existing container expansion already covers).
        let (mut store, _dir) = empty_store();
        let main_id = insert_function(&mut store, "lib/main.dart", "main");

        let class_id = "dart_class::lib/lunar.dart#LunarService";
        insert_node_kind(
            &mut store,
            class_id,
            NodeKind::DartClass,
            "lib/lunar.dart",
            "LunarService",
        );
        let method_id = "dart_method::lib/lunar.dart#LunarService.generateDayInfoRange";
        insert_node_kind(
            &mut store,
            method_id,
            NodeKind::DartMethod,
            "lib/lunar.dart",
            "generateDayInfoRange",
        );
        insert_contains(&mut store, class_id, method_id);
        // The entrypoint calls the static method; the edge lands on the method.
        insert_calls(&mut store, &main_id, method_id);

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let dead: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !dead.contains(&class_id),
            "a class used only through a static method call must not be dead, got {dead:?}"
        );
        assert!(
            !dead.contains(&method_id),
            "the called method must not be dead, got {dead:?}"
        );
    }

    #[test]
    fn unused_member_of_a_live_class_is_still_dead() {
        // Reaching a class through one member must NOT cascade *down* `Contains`
        // and resurrect its other, genuinely unused members — e.g. an
        // unreferenced `AppButton.icon` named ctor inside a widely-used
        // `AppButton`. This guards against the over-masking failure mode where
        // any live file/type swallows all its dead siblings.
        let (mut store, _dir) = empty_store();
        let main_id = insert_function(&mut store, "lib/main.dart", "main");

        let class_id = "dart_class::lib/btn.dart#AppButton";
        insert_node_kind(
            &mut store,
            class_id,
            NodeKind::DartClass,
            "lib/btn.dart",
            "AppButton",
        );
        let used = "dart_method::lib/btn.dart#AppButton.build";
        insert_node_kind(
            &mut store,
            used,
            NodeKind::DartMethod,
            "lib/btn.dart",
            "build",
        );
        let unused = "dart_ctor::lib/btn.dart#AppButton.icon";
        insert_node_kind(
            &mut store,
            unused,
            NodeKind::DartConstructor,
            "lib/btn.dart",
            "icon",
        );
        insert_contains(&mut store, class_id, used);
        insert_contains(&mut store, class_id, unused);
        // Only `build` is reached; the `icon` named ctor is never constructed.
        insert_calls(&mut store, &main_id, used);

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let dead: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !dead.contains(&class_id),
            "a class with a live member must not be dead, got {dead:?}"
        );
        assert!(
            !dead.contains(&used),
            "the used member must not be dead, got {dead:?}"
        );
        assert!(
            dead.contains(&unused),
            "an unused named ctor of a live class must still be reported, got {dead:?}"
        );
    }

    #[test]
    fn ordinary_dart_method_is_not_treated_as_a_constructor() {
        // Guard against over-demotion: a normal method whose name differs from
        // its class must keep its high-confidence verdict.
        let (mut store, _dir) = empty_store();
        let _ = insert_function(&mut store, "lib/main.dart", "main");
        // Private member name so the public-symbol demotion does not interfere;
        // this isolates the constructor-shape guard.
        let method_id = "dart_method::lib/color.dart#_LabColor._distance";
        store
            .upsert_node(&Node {
                id: ArtifactId::new(method_id.to_string()),
                kind: NodeKind::DartMethod,
                path: Some("lib/color.dart".into()),
                name: Some("_distance".into()),
                start_line: Some(1),
                end_line: Some(3),
                content_hash: None,
                stable_key: Some("_LabColor._distance".into()),
                source_file: Some("lib/color.dart".into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let candidate = report
            .candidates
            .iter()
            .find(|c| c.id == method_id)
            .expect("unreached private method should be surfaced");
        assert_eq!(candidate.confidence, DeadCodeConfidence::High);
    }

    #[test]
    fn low_confidence_dead_island_when_inbound_is_also_dead() {
        let (mut store, _dir) = empty_store();
        let main_id = insert_function(&mut store, "lib/main.dart", "main");
        // Two private methods that only call each other → unreachable dead island.
        let a = insert_method(&mut store, "lib/util.dart", "_orphan_a");
        let b = insert_method(&mut store, "lib/util.dart", "_orphan_b");
        insert_calls(&mut store, &a, &b);
        insert_calls(&mut store, &b, &a);
        let _ = main_id;
        let opts = DeadCodeOptions {
            repo_root: ".".into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        };
        let report =
            analyze_dead_code_with_store(&store, opts, &config_with(vec!["lib/main.dart"]))
                .unwrap();
        let by_id: BTreeMap<&str, &DeadCodeCandidate> = report
            .candidates
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect();
        let a_card = by_id.get(a.as_str()).expect("island member A");
        let b_card = by_id.get(b.as_str()).expect("island member B");
        assert_eq!(a_card.confidence, DeadCodeConfidence::Low);
        assert_eq!(b_card.confidence, DeadCodeConfidence::Low);
        assert!(a_card.reasons.iter().any(|r| r.contains("dead island")));
    }

    #[test]
    fn ignore_glob_drops_candidates() {
        let (mut store, _dir) = empty_store();
        let main_id = insert_function(&mut store, "lib/main.dart", "main");
        let generated = insert_function(&mut store, "lib/foo.g.dart", "_FooGenerated$serializer");
        let _ = main_id;
        let opts = DeadCodeOptions {
            repo_root: ".".into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        };
        let report =
            analyze_dead_code_with_store(&store, opts, &config_with(vec!["lib/main.dart"]))
                .unwrap();
        assert!(
            report.candidates.iter().all(|c| c.id != generated),
            "*.g.dart should be excluded by default ignore glob"
        );
        assert_eq!(report.stats.ignored_by_pattern, 1);
    }

    #[test]
    fn test_reaches_target_so_target_is_not_dead() {
        let (mut store, _dir) = empty_store();
        // main is detached; the only reachable path to the target is
        // via a test_case.
        let target = insert_method(&mut store, "lib/foo.dart", "Foo.runOnce");
        let test_id = "test_case::test/foo_test.dart#exercises Foo.runOnce";
        store
            .upsert_node(&Node {
                id: ArtifactId::new(test_id.to_string()),
                kind: NodeKind::TestCase,
                path: Some("test/foo_test.dart".into()),
                name: Some("exercises Foo.runOnce".into()),
                start_line: Some(1),
                end_line: Some(3),
                content_hash: None,
                stable_key: None,
                source_file: Some("test/foo_test.dart".into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        store
            .upsert_edge(&EdgeAssertion {
                id: ArtifactId::new(format!("declares_verification::{test_id}->{target}")),
                from_id: ArtifactId::new(test_id.to_string()),
                to_id: ArtifactId::new(target.clone()),
                kind: EdgeKind::DeclaresVerification,
                source: EdgeSource::LanguageAdapter,
                certainty: EdgeCertainty::Fact,
                status: EdgeStatus::Confirmed,
                confidence: 1.0,
                evidence_json: None,
                source_file: None,
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        let opts = DeadCodeOptions {
            repo_root: ".".into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        };
        let report = analyze_dead_code_with_store(&store, opts, &config_with(vec![])).unwrap();
        assert!(
            !report.candidates.iter().any(|c| c.id == target),
            "target reached only via a test must remain alive (tests are roots)"
        );
    }

    #[test]
    fn min_confidence_filter_drops_lower_buckets() {
        let (mut store, _dir) = empty_store();
        // a private orphan (High) and a public orphan (Medium).
        let _ = insert_function(&mut store, "lib/main.dart", "main");
        let _private_orphan = insert_function(&mut store, "lib/util.dart", "_private_dead");
        let _public_orphan = insert_method(&mut store, "lib/api/foo.dart", "PublicApi.dead");

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::High,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        assert!(
            report
                .candidates
                .iter()
                .all(|c| c.confidence == DeadCodeConfidence::High),
            "--min-confidence high must drop medium/low"
        );
        assert!(!report.candidates.is_empty());
    }

    #[test]
    fn include_tests_reports_orphan_test_cases() {
        let (mut store, _dir) = empty_store();
        // A bare test_case with no parent group, no incoming edges.
        store
            .upsert_node(&Node {
                id: ArtifactId::new("test_case::test/foo_test.dart#orphan".to_string()),
                kind: NodeKind::TestCase,
                path: Some("test/foo_test.dart".into()),
                name: Some("orphan".into()),
                start_line: Some(1),
                end_line: Some(3),
                content_hash: None,
                stable_key: None,
                source_file: Some("test/foo_test.dart".into()),
                source_hash: None,
                indexer: Some("dart_analyzer".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        // Without --include-tests: not reported.
        let default_report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec![]),
        )
        .unwrap();
        assert!(default_report.candidates.is_empty());
        // With --include-tests: orphan tests are reported even though
        // tests remain reachability roots for production code.
        let with_tests = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: true,
            },
            &config_with(vec![]),
        )
        .unwrap();
        let test_candidate = with_tests
            .candidates
            .iter()
            .find(|c| c.id == "test_case::test/foo_test.dart#orphan")
            .expect("--include-tests must surface orphan test cases");
        assert_eq!(test_candidate.kind, "test_case");
        assert_eq!(test_candidate.confidence, DeadCodeConfidence::Low);
        assert!(
            test_candidate
                .reasons
                .iter()
                .any(|r| r.contains("没有解析到验证目标")),
            "orphan test reason must explain the missing verification edge: {:?}",
            test_candidate.reasons
        );
    }

    #[test]
    fn test_file_helper_functions_are_not_reported_as_dead_code_candidates() {
        let (mut store, _dir) = empty_store();
        let _ = insert_function(&mut store, "lib/main.dart", "main");
        let test_main = insert_function(&mut store, "test/foo_test.dart", "main");
        let test_expect = insert_function(&mut store, "test/foo_test.dart", "expect");

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: true,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();

        let ids: BTreeSet<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !ids.contains(test_main.as_str()) && !ids.contains(test_expect.as_str()),
            "test-file helper functions must not pollute dead-code output: {ids:?}"
        );
    }

    #[test]
    fn test_file_main_keeps_called_production_symbol_reachable() {
        let (mut store, _dir) = empty_store();
        let _ = insert_function(&mut store, "lib/main.dart", "main");
        let test_main = insert_function(&mut store, "test/foo_test.dart", "main");
        let target = insert_method(&mut store, "lib/foo.dart", "Foo.runOnce");
        insert_calls(&mut store, &test_main, &target);

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();

        assert!(
            !report.candidates.iter().any(|c| c.id == target),
            "production symbol exercised by test main must remain reachable"
        );
    }

    fn insert_swift_method(store: &mut Store, file: &str, qualified: &str) -> String {
        let id = format!("swift_method::{file}#{qualified}");
        store
            .upsert_node(&Node {
                id: ArtifactId::new(id.clone()),
                kind: NodeKind::SwiftMethod,
                path: Some(file.into()),
                name: Some(qualified.into()),
                start_line: Some(1),
                end_line: Some(5),
                content_hash: None,
                stable_key: None,
                source_file: Some(file.into()),
                source_hash: None,
                indexer: Some("swift_lsp".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        id
    }

    fn insert_swift_function(store: &mut Store, file: &str, name: &str) -> String {
        let id = format!("swift_function::{file}#{name}");
        store
            .upsert_node(&Node {
                id: ArtifactId::new(id.clone()),
                kind: NodeKind::SwiftFunction,
                path: Some(file.into()),
                name: Some(name.into()),
                start_line: Some(1),
                end_line: Some(3),
                content_hash: None,
                stable_key: None,
                source_file: Some(file.into()),
                source_hash: None,
                indexer: Some("swift_lsp".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        id
    }

    fn insert_lsp_calls(store: &mut Store, from: &str, to: &str, indexer: &str) {
        store
            .upsert_edge(&EdgeAssertion {
                id: ArtifactId::new(format!("calls::{from}->{to}")),
                from_id: ArtifactId::new(from.to_string()),
                to_id: ArtifactId::new(to.to_string()),
                kind: EdgeKind::Calls,
                source: EdgeSource::LanguageAdapter,
                certainty: EdgeCertainty::Fact,
                status: EdgeStatus::Confirmed,
                confidence: 1.0,
                evidence_json: None,
                source_file: None,
                source_hash: None,
                indexer: Some(indexer.into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
    }

    /// P14 regression — `EdgeKind::Calls` produced by the Swift LSP
    /// indexer must participate in the dead-code BFS exactly the same
    /// way Dart `Calls` does:
    ///
    /// * a Swift `test*` method is an automatic entry root, so a
    ///   private helper it calls must not be flagged;
    /// * a sibling private helper with no inbound call edge is High
    ///   confidence dead.
    ///
    /// This locks behaviour against future refactors that could
    /// accidentally allowlist edges by indexer label.
    #[test]
    fn swift_lsp_calls_participate_in_dead_code_reachability() {
        let (mut store, _dir) = empty_store();
        let test_root = insert_swift_method(
            &mut store,
            "Tests/AppTests/AppTests.swift",
            "AppTests.testGreeter",
        );
        let reached =
            insert_swift_function(&mut store, "Sources/App/Greeter.swift", "_privateHello");
        let orphan = insert_swift_function(&mut store, "Sources/App/Greeter.swift", "_neverCalled");
        insert_lsp_calls(&mut store, &test_root, &reached, "swift_lsp");

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec![]),
        )
        .unwrap();

        let ids: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !ids.contains(&reached.as_str()),
            "Swift LSP `Calls` edge from a test* root must keep the callee reachable, got {ids:?}"
        );
        assert!(
            ids.contains(&orphan.as_str()),
            "private Swift helper with no inbound edge must still surface as dead, got {ids:?}"
        );
    }

    fn insert_python_function_with_metadata(
        store: &mut Store,
        file: &str,
        name: &str,
        metadata_json: Option<&str>,
    ) -> String {
        let id = format!("python::{file}::{name}");
        store
            .upsert_node(&Node {
                id: ArtifactId::new(id.clone()),
                kind: NodeKind::PythonFunction,
                path: Some(file.into()),
                name: Some(name.into()),
                start_line: Some(1),
                end_line: Some(5),
                content_hash: None,
                stable_key: None,
                source_file: Some(file.into()),
                source_hash: None,
                indexer: Some("python_ast".into()),
                index_generation: None,
                metadata_json: metadata_json.map(str::to_string),
            })
            .unwrap();
        id
    }

    /// P17 regression — a Python function whose `metadata_json`
    /// carries a `FrameworkRole::FastapiRoute` payload is invoked
    /// by the framework, not by in-repo callers. Dead-code must
    /// treat it as an entry point so 88%+ of Python web backends do
    /// not surface every route handler as "possibly dead" when LSP
    /// is unavailable.
    #[test]
    fn python_framework_decorated_symbols_are_treated_as_entrypoints() {
        let (mut store, _dir) = empty_store();
        // FastAPI route — externally triggered, must NOT be dead.
        let route_meta = serde_json::json!({
            "framework": "fastapi_route",
            "verb": "get",
            "path": "/items",
            "decorator": "router.get(\"/items\")"
        })
        .to_string();
        let route = insert_python_function_with_metadata(
            &mut store,
            "app/web.py",
            "list_items",
            Some(&route_meta),
        );
        // Pydantic dataclass — NOT a framework entry, baseline
        // remains the existing reachability rules.
        let data_meta = serde_json::json!({
            "framework": "data_class",
            "runtime": "stdlib"
        })
        .to_string();
        let pojo = insert_python_function_with_metadata(
            &mut store,
            "app/types.py",
            "ItemDTO",
            Some(&data_meta),
        );
        // Plain helper with no metadata — must still be dead.
        let helper =
            insert_python_function_with_metadata(&mut store, "app/utils.py", "_helper", None);

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec![]),
        )
        .unwrap();
        let ids: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !ids.contains(&route.as_str()),
            "FastAPI route handler must be treated as entry point, got dead set {ids:?}"
        );
        assert!(
            ids.contains(&pojo.as_str()),
            "dataclass-only symbols are NOT framework entrypoints; expected `{pojo}` dead, got {ids:?}"
        );
        assert!(
            ids.contains(&helper.as_str()),
            "metadata-less helper must remain dead, got {ids:?}"
        );
    }

    // -----------------------------------------------------------------------
    // v0.3.0-A — `evidence_quality` plumbing into dead-code reason strings
    // -----------------------------------------------------------------------

    /// Helper: insert a `calls` edge with an explicit indexer name so we
    /// can control whether `confidence_for_edge` returns High/Medium/Low.
    fn insert_calls_with_indexer(store: &mut Store, from: &str, to: &str, indexer: &str) {
        store
            .upsert_edge(&EdgeAssertion {
                id: ArtifactId::new(format!("calls::{from}->{to}::{indexer}")),
                from_id: ArtifactId::new(from.to_string()),
                to_id: ArtifactId::new(to.to_string()),
                kind: EdgeKind::Calls,
                source: EdgeSource::LanguageAdapter,
                certainty: EdgeCertainty::Fact,
                status: EdgeStatus::Confirmed,
                confidence: 1.0,
                evidence_json: None,
                source_file: None,
                source_hash: None,
                indexer: Some(indexer.into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
    }

    /// v0.3.0-A: a dead-island whose only inbound usage edges are
    /// produced by an `*_ast` adapter (medium tier) should not trigger
    /// the new "low-tier evidence" reason — the threshold is *only* the
    /// low bucket. Locks in the rule that "weak but not stale" stays
    /// silent so the new reason is precise.
    ///
    /// The existing `low_confidence_dead_island_when_inbound_is_also_dead`
    /// test uses `dart_analyzer` (high tier) and remains untouched.
    #[test]
    fn low_confidence_dead_island_with_medium_tier_inbound_does_not_get_extra_reason() {
        let (mut store, _dir) = empty_store();
        let _main = insert_function(&mut store, "lib/main.dart", "main");
        let a = insert_method(&mut store, "lib/util.dart", "_orphan_a");
        let b = insert_method(&mut store, "lib/util.dart", "_orphan_b");
        // `python_ast`-style indexer → Medium tier per edge_confidence.
        insert_calls_with_indexer(&mut store, &a, &b, "python_ast");
        insert_calls_with_indexer(&mut store, &b, &a, "python_ast");

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let a_card = report
            .candidates
            .iter()
            .find(|c| c.id == a)
            .expect("dead island member A");
        assert!(
            !a_card.reasons.iter().any(|r| r.contains("low-tier 入边")),
            "medium-tier inbound must NOT trigger the low-tier reason, got: {:?}",
            a_card.reasons,
        );
    }

    /// v0.3.0-A: a dead-island whose only inbound usage edges are
    /// `GitDiff`-sourced (low tier — provisional) gains an extra reason
    /// line explaining the weak evidence. The bucket itself stays at
    /// `Low` (BFS unchanged).
    #[test]
    fn low_confidence_dead_island_with_only_low_tier_inbound_gets_extra_reason() {
        let (mut store, _dir) = empty_store();
        let _main = insert_function(&mut store, "lib/main.dart", "main");
        let a = insert_method(&mut store, "lib/util.dart", "_orphan_a");
        let b = insert_method(&mut store, "lib/util.dart", "_orphan_b");
        // GitDiff source → Low tier per edge_confidence rule.
        let mut e_ab = EdgeAssertion {
            id: ArtifactId::new(format!("calls::{a}->{b}::gitdiff")),
            from_id: ArtifactId::new(a.clone()),
            to_id: ArtifactId::new(b.clone()),
            kind: EdgeKind::Calls,
            source: EdgeSource::GitDiff,
            certainty: EdgeCertainty::Fact,
            status: EdgeStatus::Confirmed,
            confidence: 1.0,
            evidence_json: None,
            source_file: None,
            source_hash: None,
            indexer: Some("git_diff".into()),
            index_generation: None,
            metadata_json: None,
        };
        store.upsert_edge(&e_ab).unwrap();
        e_ab.id = ArtifactId::new(format!("calls::{b}->{a}::gitdiff"));
        std::mem::swap(&mut e_ab.from_id, &mut e_ab.to_id);
        store.upsert_edge(&e_ab).unwrap();

        let report = analyze_dead_code_with_store(
            &store,
            DeadCodeOptions {
                repo_root: ".".into(),
                min_confidence: DeadCodeConfidence::Low,
                include_tests: false,
            },
            &config_with(vec!["lib/main.dart"]),
        )
        .unwrap();
        let a_card = report
            .candidates
            .iter()
            .find(|c| c.id == a)
            .expect("dead island member A");
        assert_eq!(a_card.confidence, DeadCodeConfidence::Low);
        // Old reason still present (regression guard).
        assert!(
            a_card.reasons.iter().any(|r| r.contains("dead island")),
            "old `dead island` reason must remain, got: {:?}",
            a_card.reasons,
        );
        // New reason kicks in.
        assert!(
            a_card.reasons.iter().any(|r| r.contains("low-tier 入边")),
            "low-tier inbound must add the new evidence-strength reason, got: {:?}",
            a_card.reasons,
        );
        assert!(
            a_card.reasons.iter().any(|r| r.contains("证据较弱")),
            "low-tier reason must explain that evidence is weak, got: {:?}",
            a_card.reasons,
        );
    }

    /// `DeadCodeReport.warnings` is a new field. When empty it must be
    /// skipped from the serialized JSON so old consumers see the exact
    /// schema they did before v0.3.0-A.
    #[test]
    fn dead_code_report_warnings_field_skipped_when_empty() {
        let (store, _dir) = empty_store();
        let report =
            analyze_dead_code_with_store(&store, DeadCodeOptions::default(), &config_with(vec![]))
                .unwrap();
        assert!(report.warnings.is_empty());
        let json = serde_json::to_string(&report).unwrap();
        assert!(
            !json.contains("\"warnings\""),
            "empty warnings field must be omitted from JSON for back-compat, got: {json}",
        );
    }

    /// `DeadCodeReport.warnings` round-trips through JSON when present.
    /// Mirror the same skip-if-empty pattern used by `ImpactReport`.
    #[test]
    fn dead_code_report_warnings_field_round_trips_when_present() {
        let (store, _dir) = empty_store();
        let mut report =
            analyze_dead_code_with_store(&store, DeadCodeOptions::default(), &config_with(vec![]))
                .unwrap();
        report.warnings.push("warn: synthetic".into());
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"warnings\""));
        let back: DeadCodeReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.warnings, vec!["warn: synthetic".to_string()]);
    }

    /// Dogfood fix: a structural-only graph (no `Calls`/`References` edges,
    /// e.g. a Rust repo without the precision tier) would silently flag almost
    /// everything as dead. The report must warn that the result is candidate
    /// only.
    #[test]
    fn warns_when_no_precision_edges_present() {
        let (mut store, _dir) = empty_store();
        let _entry = insert_function(&mut store, "lib/main.dart", "main");
        let _other = insert_function(&mut store, "lib/util.dart", "_helper");
        let opts = DeadCodeOptions {
            repo_root: ".".into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        };
        // Entry matches, so this is specifically the *precision* warning.
        let report =
            analyze_dead_code_with_store(&store, opts, &config_with(vec!["lib/main.dart"]))
                .unwrap();
        assert!(
            report.warnings.iter().any(|w| w.contains("精确层")),
            "expected a precision-tier warning, got {:?}",
            report.warnings
        );
    }

    /// Dogfood fix: when no entry point matches the graph, reachability is
    /// empty and every symbol looks dead. The report must say so instead of
    /// emitting thousands of false positives.
    #[test]
    fn warns_when_no_entrypoints_match() {
        let (mut store, _dir) = empty_store();
        let _f = insert_function(&mut store, "lib/util.dart", "_helper");
        let opts = DeadCodeOptions {
            repo_root: ".".into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        };
        let report =
            analyze_dead_code_with_store(&store, opts, &config_with(vec!["does/not/exist.dart"]))
                .unwrap();
        assert!(
            report.warnings.iter().any(|w| w.contains("入口点")),
            "expected a no-entrypoint warning, got {:?}",
            report.warnings
        );
    }
}
