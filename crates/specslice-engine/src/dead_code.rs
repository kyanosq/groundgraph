//! P7 — dead-code detection.
//!
//! `specslice dead-code` returns *possibly_dead* candidates with an
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
use serde::{Deserialize, Serialize};
use specslice_core::{EdgeKind, NodeKind};
use specslice_store::Store;

use crate::config::{DeadCodeConfig, EngineConfig, DEFAULT_CONFIG_FILE_NAME};

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
    let path = repo_root.join(DEFAULT_CONFIG_FILE_NAME);
    if !path.exists() {
        anyhow::bail!(
            "no SpecSlice workspace at {}: run `specslice init` first",
            repo_root.display()
        );
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    serde_yaml::from_str::<EngineConfig>(&raw)
        .with_context(|| format!("parsing config {}", path.display()))
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = config.storage.path.clone();
    if raw.is_empty() {
        return repo_root.join(".specslice/graph.db");
    }
    let candidate = PathBuf::from(&raw);
    if candidate.is_absolute() {
        candidate
    } else {
        repo_root.join(candidate)
    }
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

    let node_index: HashMap<String, &specslice_core::Node> =
        nodes.iter().map(|n| (n.id.to_string(), n)).collect();

    // Precompute outbound usage edges per node.
    let mut outbound: HashMap<&str, Vec<&specslice_core::EdgeAssertion>> = HashMap::new();
    let mut inbound: HashMap<&str, Vec<&specslice_core::EdgeAssertion>> = HashMap::new();
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
    for n in &nodes {
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
        // public_api_roots: anything under those paths.
        if let Some(p) = n.path.as_deref() {
            if matches_set(&public_set, p) && is_code_kind(n.kind) {
                entry_ids.insert(n.id.to_string());
            }
        }
    }

    // BFS forward.
    let mut reachable: BTreeSet<String> = BTreeSet::new();
    let mut queue: std::collections::VecDeque<String> = entry_ids.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        if let Some(out) = outbound.get(id.as_str()) {
            for e in out {
                let to = e.to_id.to_string();
                if !reachable.contains(&to) {
                    queue.push_back(to);
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
            let outbound_for: &[&specslice_core::EdgeAssertion] = outbound
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
        let inbound_for: &[&specslice_core::EdgeAssertion] = inbound
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
    let reachable_count = reachable
        .iter()
        .filter(|id| node_index.contains_key(id.as_str()))
        .count();
    let entrypoints = entry_ids.len();
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
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
    specslice_core::language_traits::is_code_symbol(kind)
        || specslice_core::language_traits::is_test(kind)
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
    matches!(
        last,
        "viewDidLoad"
            | "viewWillAppear"
            | "viewDidAppear"
            | "viewWillDisappear"
            | "viewDidDisappear"
            | "applicationDidFinishLaunching"
            | "applicationDidBecomeActive"
            | "applicationWillResignActive"
            | "scene"
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
    nodes: &[specslice_core::Node],
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
    node: &specslice_core::Node,
    inbound: &[&specslice_core::EdgeAssertion],
    public_set: &GlobSet,
    reachable: &BTreeSet<String>,
) -> (DeadCodeConfidence, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    let mitigating_factors: Vec<String> = collect_mitigating_factors(node, public_set);
    let inbound_usage: Vec<&&specslice_core::EdgeAssertion> = inbound
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
    let live_inbound: Vec<&&&specslice_core::EdgeAssertion> = inbound_usage
        .iter()
        .filter(|e| reachable.contains(e.from_id.as_str()))
        .collect();
    let dead_island_inbound: Vec<&&&specslice_core::EdgeAssertion> = inbound_usage
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

fn reason_unreached(node: &specslice_core::Node) -> String {
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

fn collect_mitigating_factors(node: &specslice_core::Node, public_set: &GlobSet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(p) = node.path.as_deref() {
        if matches_set(public_set, p) {
            out.push(format!(
                "位于 public_api_roots（{p}），可能被仓库外消费者使用"
            ));
        }
    }
    if node.kind == NodeKind::DartConstructor {
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
    node: &specslice_core::Node,
    outbound: &[&specslice_core::EdgeAssertion],
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
    use specslice_core::{
        ArtifactId, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus, Node, NodeKind,
    };
    use specslice_store::Store;
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
}
