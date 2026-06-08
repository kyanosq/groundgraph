//! P24 — behavioural fact extraction (gap #1) + node purity (gap #6).
//!
//! The code graph answers *who calls whom / what type / which field*. It
//! does **not** answer the questions that actually decide behaviour during a
//! rewrite: *where are the branches? what are the ordering / equality
//! comparisons? does this function do IO, await, or touch UI?* A refactor
//! assistant that wants "no omissions" has to surface those too.
//!
//! This module reads each code symbol's source span (via
//! [`source_text`](crate::source_text)) and extracts **deterministic,
//! honest signals** — never a semantic proof:
//!
//! - [`BehaviorCounts`] — how many branches / loops / early returns /
//!   throws / comparisons / null-or-optional checks / awaits the body has.
//! - [`SymbolFact::evidence`] — the *actual source lines* that carry a
//!   decision (branch condition, sort comparator, early `return`, `throw`).
//!   This is the part a human/agent reads to recover branch logic without
//!   re-reading the whole file.
//! - [`Purity`] + [`SymbolFact::impurity_signals`] — pure vs IO / async /
//!   UI / time / randomness / global-mutation, with the reason listed.
//!
//! Everything is computed by cheap lexical scanning over comment/string-
//! stripped text, so two runs on the same source always agree.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::language_traits::{is_callable, is_type};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::source_text::{identifier_tokens, read_node_source, strip_noise, NodeSource};

pub const SYMBOL_FACTS_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Data contract
// ---------------------------------------------------------------------------

/// Coarse purity classification. Deliberately three-valued: `Unknown`
/// is honest about symbols whose body we could not read (no path / range
/// / missing file) instead of silently calling them pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purity {
    /// No IO / async / UI / time / randomness / global-mutation signal.
    Pure,
    /// At least one side-effect signal (see `impurity_signals`).
    Impure,
    /// Body unavailable — cannot judge.
    Unknown,
}

impl Purity {
    pub fn as_str(self) -> &'static str {
        match self {
            Purity::Pure => "pure",
            Purity::Impure => "impure",
            Purity::Unknown => "unknown",
        }
    }
}

/// Per-symbol behavioural counts. Each is a count of word/operator
/// occurrences in the comment/string-stripped body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BehaviorCounts {
    /// `if` / `elif` / `else if` / `switch` / `case` / `when` / `guard` /
    /// `match` — number of decision points.
    pub branches: u32,
    /// `for` / `while` / `loop` / `repeat` / `forEach`.
    pub loops: u32,
    /// `return` statements (proxy for the number of distinct outcomes).
    pub early_returns: u32,
    /// `throw` / `raise` / `panic` / `fatalError` / `unwrap` / `expect`.
    pub throws: u32,
    /// `catch` / `except` / `rescue` / `recover`.
    pub catches: u32,
    /// `==` / `!=` / `<=` / `>=` — ordering / equality rules (sort
    /// comparators, LWW `>` checks, …).
    pub comparisons: u32,
    /// `null` / `nil` / `None` / `undefined` words plus `??` / `?.`.
    pub null_checks: u32,
    /// `await` occurrences.
    pub awaits: u32,
}

/// One source line that carries a behavioural decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactLine {
    pub line: u32,
    pub text: String,
    /// Which categories triggered: `branch` / `loop` / `return` / `throw` /
    /// `compare` / `null` / `await`. Sorted, deduplicated.
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolFact {
    pub id: String,
    pub kind: String,
    pub name: Option<String>,
    pub path: Option<String>,
    pub line_range: Option<(u32, u32)>,
    pub language: Option<String>,
    pub counts: BehaviorCounts,
    pub purity: Purity,
    /// Side-effect reasons making this symbol impure: `async` / `io` /
    /// `ui` / `time` / `randomness` / `global_mutation`. Sorted, unique.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impurity_signals: Vec<String>,
    /// Human-readable Chinese summary lines derived from the counts /
    /// signals (cheap to render in `text` mode).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary: Vec<String>,
    /// The actual decision lines (capped). Empty when the body had no
    /// branch/return/throw/comparison.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<FactLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SymbolFactsStats {
    pub analyzed: usize,
    pub with_source: usize,
    pub pure: usize,
    pub impure: usize,
    pub unknown: usize,
    pub returned: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolFactsReport {
    pub schema_version: u32,
    pub stats: SymbolFactsStats,
    pub facts: Vec<SymbolFact>,
}

#[derive(Debug, Clone)]
pub struct SymbolFactsOptions {
    pub repo_root: PathBuf,
    /// Also analyse type containers (class / struct / enum). Default
    /// `false`: only callables carry behaviour worth porting.
    pub include_types: bool,
    /// Keep only symbols with this purity (used by the `purity` view).
    pub purity_filter: Option<Purity>,
    /// Cap the number of returned facts (0 = unlimited). Stats still
    /// reflect the full analysed set.
    pub max_symbols: usize,
    /// Cap evidence lines per symbol.
    pub max_evidence_per_symbol: usize,
}

impl Default for SymbolFactsOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            include_types: false,
            purity_filter: None,
            max_symbols: 0,
            max_evidence_per_symbol: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn analyze_symbol_facts(options: SymbolFactsOptions) -> Result<SymbolFactsReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    analyze_symbol_facts_with_store(&store, &options)
}

pub fn analyze_symbol_facts_with_store(
    store: &Store,
    options: &SymbolFactsOptions,
) -> Result<SymbolFactsReport> {
    let mut facts: Vec<SymbolFact> = Vec::new();
    let mut stats = SymbolFactsStats::default();

    for node in store.list_all_nodes()? {
        let eligible = is_callable(node.kind) || (options.include_types && is_type(node.kind));
        if !eligible {
            continue;
        }
        stats.analyzed += 1;

        let fact = match read_node_source(&options.repo_root, &node) {
            Some(src) => {
                stats.with_source += 1;
                build_fact(&node, &src, options.max_evidence_per_symbol)
            }
            None => SymbolFact {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                name: node.name.clone(),
                path: node.path.clone(),
                line_range: line_range(&node),
                language: language_token(node.kind),
                counts: BehaviorCounts::default(),
                purity: Purity::Unknown,
                impurity_signals: Vec::new(),
                summary: vec!["源码不可用，无法抽取行为事实".to_string()],
                evidence: Vec::new(),
            },
        };

        match fact.purity {
            Purity::Pure => stats.pure += 1,
            Purity::Impure => stats.impure += 1,
            Purity::Unknown => stats.unknown += 1,
        }

        if let Some(filter) = options.purity_filter {
            if fact.purity != filter {
                continue;
            }
        }
        facts.push(fact);
    }

    facts.sort_by(|a, b| a.id.cmp(&b.id));
    stats.returned = facts.len();
    if options.max_symbols > 0 && facts.len() > options.max_symbols {
        facts.truncate(options.max_symbols);
        stats.truncated = true;
        stats.returned = facts.len();
    }

    Ok(SymbolFactsReport {
        schema_version: SYMBOL_FACTS_SCHEMA_VERSION,
        stats,
        facts,
    })
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

fn line_range(node: &specslice_core::Node) -> Option<(u32, u32)> {
    match (node.start_line, node.end_line) {
        (Some(s), Some(e)) => Some((s, e)),
        _ => None,
    }
}

fn language_token(kind: specslice_core::NodeKind) -> Option<String> {
    kind.language().map(|s| s.to_string())
}

const BRANCH_WORDS: &[&str] = &[
    "if", "elif", "elsif", "switch", "case", "when", "guard", "match",
];
const LOOP_WORDS: &[&str] = &["for", "while", "loop", "repeat", "foreach", "forEach"];
const RETURN_WORDS: &[&str] = &["return"];
const THROW_WORDS: &[&str] = &[
    "throw",
    "raise",
    "panic",
    "fatalError",
    "abort",
    "unreachable",
    "unwrap",
    "expect",
];
const CATCH_WORDS: &[&str] = &["catch", "except", "rescue", "recover"];
const NULL_WORDS: &[&str] = &["null", "nil", "None", "undefined"];

/// Build a [`SymbolFact`] from a recovered source span.
pub fn build_fact(
    node: &specslice_core::Node,
    src: &NodeSource,
    max_evidence: usize,
) -> SymbolFact {
    let stripped = strip_noise(&src.raw, src.language);
    let tokens = identifier_tokens(&stripped);
    let mut counts = BehaviorCounts::default();

    for &tok in &tokens {
        if BRANCH_WORDS.contains(&tok) {
            counts.branches += 1;
        }
        if LOOP_WORDS.contains(&tok) {
            counts.loops += 1;
        }
        if RETURN_WORDS.contains(&tok) {
            counts.early_returns += 1;
        }
        if THROW_WORDS.contains(&tok) {
            counts.throws += 1;
        }
        if CATCH_WORDS.contains(&tok) {
            counts.catches += 1;
        }
        if NULL_WORDS.contains(&tok) {
            counts.null_checks += 1;
        }
        if tok == "await" {
            counts.awaits += 1;
        }
    }
    // Equality/relational operators. Bare `<` / `>` are only counted when
    // space-padded (` < `, ` > `) so generics (`Vec<T>`), shifts (`<<`),
    // and arrows (`->`, `=>`) don't masquerade as comparisons — those are
    // never written with surrounding spaces, real comparisons usually are.
    counts.comparisons = count_substr(&stripped, "==")
        + count_substr(&stripped, "!=")
        + count_substr(&stripped, "<=")
        + count_substr(&stripped, ">=")
        + count_substr(&stripped, " < ")
        + count_substr(&stripped, " > ");
    counts.null_checks += count_substr(&stripped, "??") + count_substr(&stripped, "?.");

    let impurity = impurity_signals(&stripped, &tokens, counts.awaits);
    let purity = if src.raw.trim().is_empty() {
        Purity::Unknown
    } else if impurity.is_empty() {
        Purity::Pure
    } else {
        Purity::Impure
    };

    let evidence = collect_evidence(src, &stripped, max_evidence);
    let summary = build_summary(&counts, &impurity, purity);

    SymbolFact {
        id: node.id.to_string(),
        kind: node.kind.as_str().to_string(),
        name: node.name.clone(),
        path: node.path.clone(),
        line_range: line_range(node),
        language: language_token(node.kind),
        counts,
        purity,
        impurity_signals: impurity,
        summary,
        evidence,
    }
}

fn count_substr(haystack: &str, needle: &str) -> u32 {
    u32::try_from(haystack.matches(needle).count()).unwrap_or(u32::MAX)
}

/// Curated side-effect markers. `async` is decided from tokens + await
/// count; the rest are substring probes on stripped text.
fn impurity_signals(stripped: &str, tokens: &[&str], awaits: u32) -> Vec<String> {
    let mut set: BTreeSet<&'static str> = BTreeSet::new();
    let tokset: BTreeSet<&str> = tokens.iter().copied().collect();

    // async / concurrency
    if awaits > 0
        || tokset.contains("async")
        || tokset.contains("Future")
        || tokset.contains("Promise")
        || tokset.contains("Deferred")
    {
        set.insert("async");
    }
    // io / persistence / network
    const IO_MARKERS: &[&str] = &[
        "print(",
        "println",
        "eprintln",
        "fmt.Print",
        "console.log",
        "fopen",
        "fwrite",
        "fread",
        "File(",
        "open(",
        ".read(",
        ".write(",
        "readFile",
        "writeFile",
        "std::fs",
        "fs::",
        "fs.",
        "ioutil",
        "os.Open",
        "http",
        "fetch(",
        "axios",
        "URLSession",
        "Socket",
        "reqwest",
        "dio",
        "sqlite",
        "execute(",
        ".query(",
        "INSERT ",
        "SELECT ",
        "UPDATE ",
        "DELETE ",
        "prepare(",
        "SharedPreferences",
        "UserDefaults",
        "localStorage",
        "getenv",
        "std::env",
    ];
    if IO_MARKERS.iter().any(|m| stripped.contains(m)) {
        set.insert("io");
    }
    // ui
    const UI_MARKERS: &[&str] = &[
        "setState",
        "WidgetCenter",
        "UIView",
        "NSView",
        "Scaffold",
        "some View",
        ".draw(",
        "render(",
    ];
    if UI_MARKERS.iter().any(|m| stripped.contains(m)) {
        set.insert("ui");
    }
    // time
    const TIME_MARKERS: &[&str] = &[
        "DateTime.now",
        ".now(",
        "SystemTime",
        "Instant::now",
        "currentTimeMillis",
        "time.Now",
        "Date.now",
        "Clock",
    ];
    if TIME_MARKERS.iter().any(|m| stripped.contains(m)) {
        set.insert("time");
    }
    // randomness
    if tokset.contains("random")
        || tokset.contains("Random")
        || tokset.contains("rand")
        || tokset.contains("shuffle")
        || tokset.contains("uuid")
        || tokset.contains("UUID")
        || stripped.contains("Math.random")
    {
        set.insert("randomness");
    }
    // global mutation
    if stripped.contains("static mut") || stripped.contains("setenv") || tokset.contains("global") {
        set.insert("global_mutation");
    }

    set.into_iter().map(|s| s.to_string()).collect()
}

fn collect_evidence(src: &NodeSource, stripped: &str, max: usize) -> Vec<FactLine> {
    if max == 0 {
        return Vec::new();
    }
    let raw_lines: Vec<&str> = src.raw.lines().collect();
    let scan_lines: Vec<&str> = stripped.lines().collect();
    let mut out = Vec::new();
    for (i, scan) in scan_lines.iter().enumerate() {
        let mut tags: BTreeSet<&'static str> = BTreeSet::new();
        let toks = identifier_tokens(scan);
        for &t in &toks {
            if BRANCH_WORDS.contains(&t) {
                tags.insert("branch");
            }
            if LOOP_WORDS.contains(&t) {
                tags.insert("loop");
            }
            if RETURN_WORDS.contains(&t) {
                tags.insert("return");
            }
            if THROW_WORDS.contains(&t) {
                tags.insert("throw");
            }
            if t == "await" {
                tags.insert("await");
            }
            if NULL_WORDS.contains(&t) {
                tags.insert("null");
            }
        }
        if scan.contains("==")
            || scan.contains("!=")
            || scan.contains("<=")
            || scan.contains(">=")
            || scan.contains(" < ")
            || scan.contains(" > ")
        {
            tags.insert("compare");
        }
        if scan.contains("??") || scan.contains("?.") {
            tags.insert("null");
        }
        if tags.is_empty() {
            continue;
        }
        let raw = raw_lines.get(i).copied().unwrap_or("").trim();
        if raw.is_empty() {
            continue;
        }
        out.push(FactLine {
            line: src.start_line + u32::try_from(i).unwrap_or(0),
            text: raw.to_string(),
            tags: tags.into_iter().map(|s| s.to_string()).collect(),
        });
        if out.len() >= max {
            break;
        }
    }
    out
}

fn build_summary(counts: &BehaviorCounts, impurity: &[String], purity: Purity) -> Vec<String> {
    let mut out = Vec::new();
    if counts.branches > 0 {
        out.push(format!(
            "{} 个分支判定（if/switch/guard/match）",
            counts.branches
        ));
    }
    if counts.loops > 0 {
        out.push(format!("{} 个循环", counts.loops));
    }
    if counts.early_returns > 0 {
        out.push(format!("{} 处 return（结果分支）", counts.early_returns));
    }
    if counts.comparisons > 0 {
        out.push(format!(
            "{} 处比较运算（排序 / 相等 / LWW 规则候选）",
            counts.comparisons
        ));
    }
    if counts.null_checks > 0 {
        out.push(format!("{} 处空值 / 可选处理", counts.null_checks));
    }
    if counts.throws > 0 {
        out.push(format!("{} 处可能抛出 / 强解包", counts.throws));
    }
    if counts.catches > 0 {
        out.push(format!("{} 处异常捕获", counts.catches));
    }
    match purity {
        Purity::Pure => out.push("纯逻辑：未检测到 IO/异步/UI/时间/随机/全局副作用".to_string()),
        Purity::Impure => out.push(format!("有副作用：{}", impurity.join(", "))),
        Purity::Unknown => {}
    }
    out
}

// ---------------------------------------------------------------------------
// Workspace helpers (mirrors slice.rs / context_pack.rs)
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
    use specslice_core::{ArtifactId, Node, NodeKind};

    fn write(tmp: &Path, rel: &str, body: &str) {
        let abs = tmp.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, body).unwrap();
    }

    fn node(rel: &str, name: &str, start: u32, end: u32) -> Node {
        let mut n = Node::new(
            ArtifactId::new(format!("rust::{rel}#{name}")),
            NodeKind::RustFunction,
        );
        n.path = Some(rel.to_string());
        n.name = Some(name.to_string());
        n.start_line = Some(start);
        n.end_line = Some(end);
        n
    }

    #[test]
    fn pure_function_has_branches_and_no_impurity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let body = "fn classify(n: i32) -> i32 {\n    if n == 0 {\n        return 0;\n    } else if n < 0 {\n        return -1;\n    }\n    return 1;\n}";
        write(tmp.path(), "src/a.rs", body);
        let n = node("src/a.rs", "classify", 1, 8);
        let src = read_node_source(tmp.path(), &n).unwrap();
        let fact = build_fact(&n, &src, 60);
        assert_eq!(fact.purity, Purity::Pure);
        assert!(fact.impurity_signals.is_empty());
        assert!(
            fact.counts.branches >= 2,
            "branches={}",
            fact.counts.branches
        );
        assert_eq!(fact.counts.early_returns, 3);
        assert!(fact.counts.comparisons >= 2);
        // evidence captures the actual branch lines
        assert!(fact.evidence.iter().any(|e| e.text.contains("if n == 0")));
        assert!(fact
            .evidence
            .iter()
            .any(|e| e.tags.contains(&"branch".to_string())));
    }

    #[test]
    fn io_function_is_impure_with_io_signal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let body = "fn log_it(x: i32) {\n    println!(\"value\");\n}";
        write(tmp.path(), "src/b.rs", body);
        let n = node("src/b.rs", "log_it", 1, 3);
        let src = read_node_source(tmp.path(), &n).unwrap();
        let fact = build_fact(&n, &src, 60);
        assert_eq!(fact.purity, Purity::Impure);
        assert!(fact.impurity_signals.contains(&"io".to_string()));
    }

    #[test]
    fn comments_and_strings_do_not_inflate_counts() {
        let tmp = tempfile::TempDir::new().unwrap();
        // The only *real* keyword is the function itself; the `if`/`for`
        // hide in a comment and a string.
        let body =
            "fn noop() {\n    // if for while return\n    let s = \"if for return throw\";\n}";
        write(tmp.path(), "src/c.rs", body);
        let n = node("src/c.rs", "noop", 1, 4);
        let src = read_node_source(tmp.path(), &n).unwrap();
        let fact = build_fact(&n, &src, 60);
        assert_eq!(fact.counts.branches, 0);
        assert_eq!(fact.counts.loops, 0);
        assert_eq!(fact.counts.early_returns, 0);
        assert_eq!(fact.counts.throws, 0);
    }

    #[test]
    fn async_io_function_flags_async_and_io() {
        let tmp = tempfile::TempDir::new().unwrap();
        let body = "async fn fetch_user() -> String {\n    let r = http_get(\"/u\").await;\n    return r;\n}";
        write(tmp.path(), "src/d.rs", body);
        let n = node("src/d.rs", "fetch_user", 1, 4);
        let src = read_node_source(tmp.path(), &n).unwrap();
        let fact = build_fact(&n, &src, 60);
        assert_eq!(fact.purity, Purity::Impure);
        assert!(fact.impurity_signals.contains(&"async".to_string()));
        assert!(fact.impurity_signals.contains(&"io".to_string()));
        assert_eq!(fact.counts.awaits, 1);
    }

    #[test]
    fn report_filters_by_purity_and_counts_stats() {
        let tmp = tempfile::TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "fn pure_add(a: i32, b: i32) -> i32 {\n    if a == b { return a; }\n    return a + b;\n}\nfn impure() {\n    println!(\"x\");\n}",
        );
        let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let mut a = node("src/lib.rs", "pure_add", 1, 4);
        a.id = ArtifactId::new("rust::src/lib.rs#pure_add");
        store.upsert_node(&a).unwrap();
        let mut b = node("src/lib.rs", "impure", 5, 7);
        b.id = ArtifactId::new("rust::src/lib.rs#impure");
        store.upsert_node(&b).unwrap();

        let opts = SymbolFactsOptions {
            repo_root: tmp.path().to_path_buf(),
            purity_filter: Some(Purity::Pure),
            ..Default::default()
        };
        let report = analyze_symbol_facts_with_store(&store, &opts).unwrap();
        assert_eq!(report.stats.analyzed, 2);
        assert_eq!(report.stats.pure, 1);
        assert_eq!(report.stats.impure, 1);
        // filter kept only the pure one
        assert_eq!(report.facts.len(), 1);
        assert_eq!(report.facts[0].name.as_deref(), Some("pure_add"));
    }
}
