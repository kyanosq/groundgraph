//! P24 — test suggestions from facts (gap #5).
//!
//! A rewrite needs tests, and the facts GroundGraph already extracts say
//! exactly *what* to test: a function with 3 branches needs 3 branch cases;
//! a comparison needs boundary values; a `throw` needs an error-path case; a
//! magic constant `3` is a boundary worth probing; an impure function needs
//! a fake/mock seam.
//!
//! This module composes [`symbol_facts`](crate::symbol_facts) (behaviour +
//! purity) with [`constants`](crate::constants) (numeric boundaries) and
//! emits deterministic, per-symbol [`Suggestion`]s. It never writes tests —
//! it produces a prioritised checklist (pure, branchy functions first,
//! because they are the cheapest high-value tests in a port).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use groundgraph_core::language_traits::{is_callable, is_type};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::config::{resolve_storage_path, EngineConfig};
use crate::constants::{scan_literals, LiteralKind};
use crate::error::EngineResult;
use crate::source_text::read_node_source;
use crate::symbol_facts::{build_fact, Purity, SymbolFact};

pub const TEST_SUGGESTIONS_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Data contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    /// Cover each decision branch.
    Branch,
    /// Probe values around a comparison / magic constant.
    Boundary,
    /// Null / optional / missing input.
    Null,
    /// Error / throw / force-unwrap path.
    Error,
    /// Collection size: empty / one / many.
    Loop,
    /// Pure → direct input/output asserts (incl. property tests).
    Purity,
    /// Impure → inject a fake/mock or extract pure logic first.
    Dependency,
    /// Baseline happy-path smoke test.
    Smoke,
}

impl SuggestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SuggestionKind::Branch => "branch",
            SuggestionKind::Boundary => "boundary",
            SuggestionKind::Null => "null",
            SuggestionKind::Error => "error",
            SuggestionKind::Loop => "loop",
            SuggestionKind::Purity => "purity",
            SuggestionKind::Dependency => "dependency",
            SuggestionKind::Smoke => "smoke",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    pub message: String,
    /// Concrete hints: branch condition lines, boundary values, etc.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolSuggestions {
    pub id: String,
    pub name: Option<String>,
    pub kind: String,
    pub path: Option<String>,
    pub line_range: Option<(u32, u32)>,
    pub purity: String,
    /// Heuristic testing-value score (branches/comparisons weigh most).
    pub priority: u32,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TestSuggestionsStats {
    pub analyzed: usize,
    pub with_suggestions: usize,
    pub total_suggestions: usize,
    pub returned: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestSuggestionsReport {
    pub schema_version: u32,
    pub stats: TestSuggestionsStats,
    pub items: Vec<SymbolSuggestions>,
}

#[derive(Debug, Clone)]
pub struct TestSuggestionsOptions {
    pub repo_root: PathBuf,
    pub include_types: bool,
    /// Only pure symbols (cheapest, most deterministic tests).
    pub only_pure: bool,
    /// Drop symbols below this priority score.
    pub min_priority: u32,
    pub max_symbols: usize,
}

impl Default for TestSuggestionsOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            include_types: false,
            only_pure: false,
            min_priority: 1,
            max_symbols: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn analyze_test_suggestions(
    options: TestSuggestionsOptions,
) -> EngineResult<TestSuggestionsReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config)?;
    let store = Store::open(&db_path)?;
    analyze_test_suggestions_with_store(&store, &options)
}

pub fn analyze_test_suggestions_with_store(
    store: &Store,
    options: &TestSuggestionsOptions,
) -> EngineResult<TestSuggestionsReport> {
    let mut items: Vec<SymbolSuggestions> = Vec::new();
    let mut stats = TestSuggestionsStats::default();

    for node in store.list_all_nodes()? {
        let eligible = is_callable(node.kind) || (options.include_types && is_type(node.kind));
        if !eligible {
            continue;
        }
        let Some(src) = read_node_source(&options.repo_root, &node) else {
            continue;
        };
        stats.analyzed += 1;

        let fact = build_fact(&node, &src, 30);
        if options.only_pure && fact.purity != Purity::Pure {
            continue;
        }
        let boundaries = numeric_boundaries(&src.raw, src.language);
        let suggestions = derive_suggestions(&fact, &boundaries);
        let priority = priority_of(&fact);
        if priority < options.min_priority {
            continue;
        }
        stats.with_suggestions += 1;
        stats.total_suggestions += suggestions.len();
        items.push(SymbolSuggestions {
            id: fact.id.clone(),
            name: fact.name.clone(),
            kind: fact.kind.clone(),
            path: fact.path.clone(),
            line_range: fact.line_range,
            purity: fact.purity.as_str().to_string(),
            priority,
            suggestions,
        });
    }

    // Highest testing value first; stable tie-break by id.
    items.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
    stats.returned = items.len();
    if options.max_symbols > 0 && items.len() > options.max_symbols {
        items.truncate(options.max_symbols);
        stats.truncated = true;
        stats.returned = items.len();
    }

    Ok(TestSuggestionsReport {
        schema_version: TEST_SUGGESTIONS_SCHEMA_VERSION,
        stats,
        items,
    })
}

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

fn priority_of(f: &SymbolFact) -> u32 {
    let c = &f.counts;
    c.branches
        .saturating_mul(3)
        .saturating_add(c.comparisons.saturating_mul(2))
        .saturating_add(c.throws.saturating_mul(2))
        .saturating_add(c.null_checks)
        .saturating_add(c.loops)
}

/// Distinct non-trivial numeric literal values in the body, sorted.
fn numeric_boundaries(raw: &str, lang: groundgraph_core::language_traits::Language) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for lit in scan_literals(raw, lang) {
        if !matches!(lit.kind, LiteralKind::Int | LiteralKind::Float) {
            continue;
        }
        let digits: String = lit.value.chars().filter(|c| c.is_ascii_digit()).collect();
        if matches!(digits.as_str(), "" | "0" | "1") {
            continue;
        }
        set.insert(lit.value);
    }
    set.into_iter().collect()
}

fn derive_suggestions(f: &SymbolFact, boundaries: &[String]) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let name = f.name.clone().unwrap_or_else(|| "<anonymous>".to_string());
    let c = &f.counts;

    // Smoke — always.
    out.push(Suggestion {
        kind: SuggestionKind::Smoke,
        message: format!("为 {name} 写一个 happy-path 基础用例，断言典型输入的返回值"),
        hints: Vec::new(),
    });

    if c.branches > 0 {
        let hints: Vec<String> = f
            .evidence
            .iter()
            .filter(|e| e.tags.iter().any(|t| t == "branch" || t == "compare"))
            .take(6)
            .map(|e| format!("L{}: {}", e.line, e.text))
            .collect();
        out.push(Suggestion {
            kind: SuggestionKind::Branch,
            message: format!("覆盖 {} 个分支判定：为每条判定写正/反两类用例", c.branches),
            hints,
        });
    }

    if !boundaries.is_empty() {
        let shown: Vec<String> = boundaries.iter().take(8).cloned().collect();
        out.push(Suggestion {
            kind: SuggestionKind::Boundary,
            message: "在关键常量边界取值（值本身、值±1、越界）".to_string(),
            hints: shown,
        });
    } else if c.comparisons > 0 {
        out.push(Suggestion {
            kind: SuggestionKind::Boundary,
            message: format!(
                "存在 {} 处比较：在比较点两侧取值（小于 / 等于 / 大于）",
                c.comparisons
            ),
            hints: Vec::new(),
        });
    }

    if c.null_checks > 0 {
        out.push(Suggestion {
            kind: SuggestionKind::Null,
            message: format!(
                "存在 {} 处空值 / 可选处理：分别传入 null/None 与非空输入",
                c.null_checks
            ),
            hints: Vec::new(),
        });
    }

    if c.throws > 0 {
        out.push(Suggestion {
            kind: SuggestionKind::Error,
            message: format!(
                "存在 {} 处抛出 / 强解包：构造触发错误的输入并断言报错",
                c.throws
            ),
            hints: Vec::new(),
        });
    }

    if c.loops > 0 {
        out.push(Suggestion {
            kind: SuggestionKind::Loop,
            message: "测试集合规模：空集合 / 单元素 / 多元素".to_string(),
            hints: Vec::new(),
        });
    }

    match f.purity {
        Purity::Pure => out.push(Suggestion {
            kind: SuggestionKind::Purity,
            message: "纯函数：可做确定性输入/输出断言，并考虑 property-based 测试".to_string(),
            hints: Vec::new(),
        }),
        Purity::Impure => out.push(Suggestion {
            kind: SuggestionKind::Dependency,
            message: format!(
                "有副作用（{}）：抽出纯逻辑或注入 fake/mock 依赖后再断言",
                f.impurity_signals.join(", ")
            ),
            hints: Vec::new(),
        }),
        Purity::Unknown => {}
    }

    out
}

// ---------------------------------------------------------------------------
// Workspace helpers
// ---------------------------------------------------------------------------

fn load_config(repo_root: &Path) -> crate::error::EngineResult<EngineConfig> {
    crate::config::load_config(repo_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::{ArtifactId, Node, NodeKind};

    fn setup(body: &str, start: u32, end: u32) -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), body).unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let mut n = Node::new(
            ArtifactId::new("rust::src/lib.rs#f"),
            NodeKind::RustFunction,
        );
        n.path = Some("src/lib.rs".to_string());
        n.name = Some("f".to_string());
        n.start_line = Some(start);
        n.end_line = Some(end);
        store.upsert_node(&n).unwrap();
        (store, dir)
    }

    #[test]
    fn pure_branchy_function_gets_branch_boundary_and_purity() {
        let body =
            "fn f(n: i32) -> i32 {\n    if n >= 18 {\n        return 100;\n    }\n    return 0;\n}";
        let (store, dir) = setup(body, 1, 6);
        let report = analyze_test_suggestions_with_store(
            &store,
            &TestSuggestionsOptions {
                repo_root: dir.path().to_path_buf(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert_eq!(item.purity, "pure");
        let kinds: Vec<SuggestionKind> = item.suggestions.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&SuggestionKind::Smoke));
        assert!(kinds.contains(&SuggestionKind::Branch));
        assert!(kinds.contains(&SuggestionKind::Boundary));
        assert!(kinds.contains(&SuggestionKind::Purity));
        // boundary hint surfaces the magic value 18 (and 100), not 0/1.
        let boundary = item
            .suggestions
            .iter()
            .find(|s| s.kind == SuggestionKind::Boundary)
            .unwrap();
        assert!(boundary.hints.contains(&"18".to_string()));
        assert!(boundary.hints.contains(&"100".to_string()));
        // branch hint surfaces the actual condition line.
        let branch = item
            .suggestions
            .iter()
            .find(|s| s.kind == SuggestionKind::Branch)
            .unwrap();
        assert!(branch.hints.iter().any(|h| h.contains("if n >= 18")));
    }

    #[test]
    fn impure_function_gets_dependency_suggestion() {
        let body = "fn f() {\n    println!(\"hi\");\n}";
        let (store, dir) = setup(body, 1, 3);
        let report = analyze_test_suggestions_with_store(
            &store,
            &TestSuggestionsOptions {
                repo_root: dir.path().to_path_buf(),
                min_priority: 0,
                ..Default::default()
            },
        )
        .unwrap();
        let item = &report.items[0];
        assert_eq!(item.purity, "impure");
        let dep = item
            .suggestions
            .iter()
            .find(|s| s.kind == SuggestionKind::Dependency)
            .unwrap();
        assert!(dep.message.contains("io"));
    }

    #[test]
    fn only_pure_filter_and_min_priority() {
        let body = "fn f() {\n    println!(\"x\");\n}";
        let (store, dir) = setup(body, 1, 3);
        // only_pure should drop this impure function entirely.
        let report = analyze_test_suggestions_with_store(
            &store,
            &TestSuggestionsOptions {
                repo_root: dir.path().to_path_buf(),
                only_pure: true,
                min_priority: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.items.len(), 0);
        // analyzed still counted the symbol.
        assert_eq!(report.stats.analyzed, 1);
    }
}
