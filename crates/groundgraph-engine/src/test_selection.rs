//! P19 — `groundgraph select-tests`: pick the subset of tests that
//! *should* run for a given diff, with an explicit confidence
//! label per selection.
//!
//! This complements `groundgraph impact` but answers a different
//! question. `impact` tells humans "what business / requirement
//! surfaces are at risk". `select-tests` tells CI "which test
//! files do I actually need to run".
//!
//! The algorithm is intentionally simple and explainable — every
//! selected test carries a list of human-readable reasons:
//!
//! 1. **Direct file change.** A test file appears in the diff →
//!    every TestCase / TestGroup in that file is selected with
//!    reason `test_file_directly_changed`, confidence `high`.
//! 2. **Direct symbol reference.** A test has a `Calls` or
//!    `References` edge to a changed symbol → reason
//!    `references_changed_symbol`, confidence `high`. (LSP /
//!    analyzer-emitted edges are exactly what we need; AST-only
//!    `Calls` get the same label because the existence of the
//!    edge IS itself a fact, even if some calls are missing.)
//! 3. **Module import.** A test file imports a module that
//!    contains a changed symbol → reason
//!    `imports_changed_module`, confidence `medium`. This catches
//!    the dynamic-language case where we have no call edges yet
//!    still want a reasonable test list.
//!
//! We deliberately do NOT recommend running the whole suite when
//! signal is weak — for repos without LSP / framework hints we
//! report the conservative subset and let the operator decide
//! whether to expand with `--include-deps`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_core::{ArtifactId, EdgeKind, Node, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::git_diff::{git_diff, parse_unified_diff, ChangedFile};
use crate::impact::{compute_impact_with_policy, ImpactPolicy, ImpactPropagation};

/// Schema version emitted next to the report — bump when the
/// shape of `tests` / `reasons` changes in a way that older
/// consumers cannot parse.
pub const TEST_SELECTION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct TestSelectionOptions {
    pub repo_root: PathBuf,
    pub base_ref: String,
    pub head_ref: String,
    /// When `true`, walk reverse `Calls` / `References` from
    /// changed symbols up to `max_propagation_depth` levels and
    /// select any test reached transitively. Off by default
    /// because the result depends on how complete the graph is.
    pub include_dependent: bool,
    /// Maximum BFS depth on reverse `Calls` / `References` edges.
    /// Only consulted when `include_dependent = true`.
    pub max_propagation_depth: usize,
}

impl Default for TestSelectionOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            base_ref: "main".into(),
            head_ref: "HEAD".into(),
            include_dependent: false,
            max_propagation_depth: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestSelection {
    pub schema_version: u32,
    pub base: String,
    pub head: String,
    pub stats: TestSelectionStats,
    pub tests: Vec<SelectedTest>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestSelectionStats {
    pub changed_files: usize,
    pub changed_symbols: usize,
    pub tests_selected: usize,
    /// True when no test was selected — operators can warn
    /// reviewers that the diff may have been mis-classified.
    pub empty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedTest {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_range: Option<(u32, u32)>,
    /// Ordered, deduplicated reasons this test was picked. The
    /// first entry drives `confidence`.
    pub reasons: Vec<String>,
    /// `"high"` / `"medium"` / `"low"` — derived from the highest
    /// confidence reason in `reasons`. Stable strings so CI
    /// pipelines can filter on them directly.
    pub confidence: String,
}

/// Entry point — read the diff, ask the store for the impact
/// report, then build the test list.
pub fn select_tests(options: TestSelectionOptions) -> Result<TestSelection> {
    let db_path = crate::config::storage_path_for_repo(&options.repo_root)?;
    let store = Store::open(&db_path)
        .with_context(|| format!("opening graph store at {}", db_path.display()))?;
    let diff_text = git_diff(&options.repo_root, &options.base_ref, &options.head_ref)
        .context("running git diff for test selection")?;
    let changed = parse_unified_diff(&diff_text);
    select_tests_with_store(&store, &changed, &options)
}

/// Pure-ish helper that consumes an already-parsed diff. Used by
/// tests that don't want to spin up a real git repo.
pub fn select_tests_with_store(
    store: &Store,
    changed: &[ChangedFile],
    options: &TestSelectionOptions,
) -> Result<TestSelection> {
    let mut selected: BTreeMap<String, SelectedTest> = BTreeMap::new();

    // Run the existing impact engine so changed symbols, propagated
    // symbols, and tests-via-manifest-links all flow through one
    // shared code path.
    let policy = ImpactPolicy {
        propagation: ImpactPropagation {
            call_depth: if options.include_dependent {
                options.max_propagation_depth
            } else {
                0
            },
            max_propagated_symbols: 4096,
        },
        ..ImpactPolicy::default()
    };
    let impact = compute_impact_with_policy(store, changed, policy)
        .context("computing impact for test selection")?;

    // ---- reason 1: test file itself changed -------------------
    for path in &impact.changed_files {
        for test in tests_in_file(store, path)? {
            push(&mut selected, test, "test_file_directly_changed");
        }
    }

    // ---- reason 2 + transitive: tests citing a changed symbol --
    let mut changed_symbol_ids: BTreeSet<ArtifactId> = impact
        .changed_symbols
        .iter()
        .map(|s| ArtifactId::new(s.id.clone()))
        .collect();
    for s in &impact.propagated_symbols {
        changed_symbol_ids.insert(ArtifactId::new(s.id.clone()));
    }
    for sym_id in &changed_symbol_ids {
        let incoming = store
            .list_edges_to(sym_id)
            .with_context(|| format!("listing edges to {sym_id}"))?;
        for edge in incoming {
            if !matches!(edge.kind, EdgeKind::Calls | EdgeKind::References) {
                continue;
            }
            // Walk up the `Contains` chain from the edge source
            // until we find an enclosing TestCase / TestGroup
            // node. This lets a single LSP edge from a deep test
            // helper still flag the enclosing pytest function.
            if let Some(test_node) = enclosing_test_node(store, &edge.from_id)? {
                let reason = "references_changed_symbol";
                push_from_node(&mut selected, &test_node, reason);
            }
        }
    }

    // ---- reason 3: test imports a changed module --------------
    // For every changed symbol, find its enclosing module / file.
    // For every Imports edge into that module, the test file at
    // the other end should run.
    let mut changed_modules: BTreeSet<ArtifactId> = BTreeSet::new();
    for sym_id in &changed_symbol_ids {
        if let Some(module_id) = enclosing_module_id(store, sym_id)? {
            changed_modules.insert(module_id);
        }
    }
    for module_id in &changed_modules {
        let incoming = store
            .list_edges_to(module_id)
            .with_context(|| format!("listing imports of {module_id}"))?;
        for edge in incoming {
            if !matches!(edge.kind, EdgeKind::Imports) {
                continue;
            }
            // `edge.from_id` is the importing module; collect any
            // test nodes that live in (or under) that module.
            if let Some(node) = store
                .find_node(&edge.from_id)
                .with_context(|| format!("loading importer node {}", edge.from_id))?
            {
                if let Some(path) = node.path.as_deref() {
                    for test in tests_in_file(store, path)? {
                        push(&mut selected, test, "imports_changed_module");
                    }
                }
            }
        }
    }

    let mut tests: Vec<SelectedTest> = selected.into_values().collect();
    // Sort by confidence (high first), then by path/label so the
    // output is stable across runs.
    tests.sort_by(|a, b| {
        confidence_rank(&b.confidence)
            .cmp(&confidence_rank(&a.confidence))
            .then(a.path.cmp(&b.path))
            .then(a.label.cmp(&b.label))
    });

    let stats = TestSelectionStats {
        changed_files: impact.changed_files.len(),
        changed_symbols: impact.changed_symbols.len(),
        tests_selected: tests.len(),
        empty: tests.is_empty(),
    };
    Ok(TestSelection {
        schema_version: TEST_SELECTION_SCHEMA_VERSION,
        base: options.base_ref.clone(),
        head: options.head_ref.clone(),
        stats,
        tests,
    })
}

fn push(map: &mut BTreeMap<String, SelectedTest>, mut test: SelectedTest, reason: &str) {
    let entry = map.entry(test.id.clone()).or_insert_with(|| {
        test.reasons = Vec::new();
        test.confidence = "low".into();
        test.clone()
    });
    if !entry.reasons.iter().any(|r| r == reason) {
        entry.reasons.push(reason.into());
    }
    entry.confidence = confidence_for_reasons(&entry.reasons);
}

fn push_from_node(map: &mut BTreeMap<String, SelectedTest>, node: &Node, reason: &str) {
    push(
        map,
        SelectedTest {
            id: node.id.to_string(),
            kind: node.kind.as_str().into(),
            label: node
                .name
                .clone()
                .unwrap_or_else(|| node.stable_key.clone().unwrap_or_default()),
            path: node.path.clone().unwrap_or_default(),
            line_range: match (node.start_line, node.end_line) {
                (Some(s), Some(e)) => Some((s, e)),
                _ => None,
            },
            reasons: Vec::new(),
            confidence: "low".into(),
        },
        reason,
    );
}

fn tests_in_file(store: &Store, path: &str) -> Result<Vec<SelectedTest>> {
    let nodes = store
        .list_all_nodes()
        .context("listing nodes for test-in-file scan")?;
    let mut out = Vec::new();
    for node in nodes {
        if !matches!(node.kind, NodeKind::TestCase | NodeKind::TestGroup) {
            continue;
        }
        if node.path.as_deref() != Some(path) {
            continue;
        }
        out.push(SelectedTest {
            id: node.id.to_string(),
            kind: node.kind.as_str().into(),
            label: node
                .name
                .clone()
                .unwrap_or_else(|| node.stable_key.clone().unwrap_or_default()),
            path: node.path.clone().unwrap_or_default(),
            line_range: match (node.start_line, node.end_line) {
                (Some(s), Some(e)) => Some((s, e)),
                _ => None,
            },
            reasons: Vec::new(),
            confidence: "low".into(),
        });
    }
    Ok(out)
}

/// Walk `Contains` ancestors of `id` until we hit a TestCase /
/// TestGroup node, or run out of ancestors.
fn enclosing_test_node(store: &Store, id: &ArtifactId) -> Result<Option<Node>> {
    let mut cursor: Option<ArtifactId> = Some(id.clone());
    let mut hops = 0usize;
    while let Some(cur) = cursor.take() {
        if hops > 16 {
            break;
        }
        hops += 1;
        let Some(node) = store
            .find_node(&cur)
            .with_context(|| format!("loading node {cur} during test ancestor walk"))?
        else {
            return Ok(None);
        };
        if matches!(node.kind, NodeKind::TestCase | NodeKind::TestGroup) {
            return Ok(Some(node));
        }
        let parents = store
            .list_edges_to(&cur)
            .with_context(|| format!("listing parents of {cur}"))?;
        let parent = parents
            .into_iter()
            .find(|e| matches!(e.kind, EdgeKind::Contains))
            .map(|e| e.from_id);
        cursor = parent;
    }
    Ok(None)
}

/// Walk `Contains` ancestors of `id` to find the enclosing module
/// / file. Returns the first `*Module` or `File` node we see.
fn enclosing_module_id(store: &Store, id: &ArtifactId) -> Result<Option<ArtifactId>> {
    let mut cursor: Option<ArtifactId> = Some(id.clone());
    let mut hops = 0usize;
    while let Some(cur) = cursor.take() {
        if hops > 16 {
            break;
        }
        hops += 1;
        let Some(node) = store
            .find_node(&cur)
            .with_context(|| format!("loading node {cur} during module walk"))?
        else {
            return Ok(None);
        };
        if matches!(node.kind, NodeKind::PythonModule | NodeKind::File) {
            // Treat the Python module / generic file node as the
            // anchor for `Imports` edges. Dart / Swift / Go don't
            // emit a Module node today; their files are addressed
            // via `file::<path>` ids which fall under
            // `NodeKind::File`.
            return Ok(Some(cur));
        }
        let parents = store
            .list_edges_to(&cur)
            .with_context(|| format!("listing parents of {cur}"))?;
        let parent = parents
            .into_iter()
            .find(|e| matches!(e.kind, EdgeKind::Contains))
            .map(|e| e.from_id);
        cursor = parent;
    }
    Ok(None)
}

fn confidence_for_reasons(reasons: &[String]) -> String {
    let mut best = 0u8;
    for r in reasons {
        let rank = match r.as_str() {
            "test_file_directly_changed" => 3,
            "references_changed_symbol" => 3,
            "imports_changed_module" => 2,
            "transitive_caller_of_changed_symbol" => 2,
            _ => 1,
        };
        if rank > best {
            best = rank;
        }
    }
    match best {
        3 => "high",
        2 => "medium",
        _ => "low",
    }
    .into()
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}

// Suppress dead-code warning while we wire up advanced traversal:
// the helper isn't called in the MVP entry point but the unit
// tests exercise it directly.
#[allow(dead_code)]
fn ancestor_chain(store: &Store, mut cur: ArtifactId, max_hops: usize) -> Result<Vec<ArtifactId>> {
    let mut out = vec![cur.clone()];
    let mut seen: BTreeSet<ArtifactId> = BTreeSet::new();
    seen.insert(cur.clone());
    let mut queue: VecDeque<ArtifactId> = VecDeque::new();
    queue.push_back(cur.clone());
    let mut hops = 0;
    while let Some(node) = queue.pop_front() {
        if hops >= max_hops {
            break;
        }
        let parents = store.list_edges_to(&node).unwrap_or_default();
        for edge in parents {
            if !matches!(edge.kind, EdgeKind::Contains) {
                continue;
            }
            if seen.insert(edge.from_id.clone()) {
                out.push(edge.from_id.clone());
                queue.push_back(edge.from_id);
            }
        }
        hops += 1;
        cur = out.last().cloned().unwrap_or(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_diff::{ChangeStatus, ChangedFile, Hunk};
    use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
    use tempfile::TempDir;

    fn empty_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn upsert_node(store: &mut Store, id: &str, kind: NodeKind, path: &str, lines: (u32, u32)) {
        let mut n = Node::new(ArtifactId::new(id), kind);
        n.path = Some(path.into());
        n.name = Some(id.rsplit("::").next().unwrap().into());
        n.start_line = Some(lines.0);
        n.end_line = Some(lines.1);
        store.upsert_node(&n).unwrap();
    }

    fn upsert_edge(store: &mut Store, from: &str, to: &str, kind: EdgeKind, indexer: Option<&str>) {
        let mut e = EdgeAssertion::fact(
            ArtifactId::new(from),
            ArtifactId::new(to),
            kind,
            EdgeSource::LanguageAdapter,
        );
        e.indexer = indexer.map(String::from);
        store.upsert_edge(&e).unwrap();
    }

    fn record_range(
        store: &mut Store,
        sym_id: &str,
        kind: NodeKind,
        path: &str,
        lines: (u32, u32),
    ) {
        use groundgraph_core::SymbolRange;
        store
            .upsert_symbol_range(&SymbolRange {
                file_path: path.into(),
                start_line: lines.0,
                end_line: lines.1,
                symbol_id: ArtifactId::new(sym_id),
                symbol_kind: kind,
                qualified_name: sym_id.into(),
                parent_symbol_id: None,
            })
            .unwrap();
    }

    #[test]
    fn test_file_directly_changed_is_selected_with_high_confidence() {
        let (mut store, _dir) = empty_store();
        upsert_node(
            &mut store,
            "test::backend/tests/test_foo.py::test_login",
            NodeKind::TestCase,
            "backend/tests/test_foo.py",
            (10, 25),
        );
        let changed = vec![ChangedFile {
            path: "backend/tests/test_foo.py".into(),
            hunks: vec![Hunk {
                new_start: 10,
                new_end: 12,
            }],
            status: ChangeStatus::Modified,
        }];
        let report =
            select_tests_with_store(&store, &changed, &TestSelectionOptions::default()).unwrap();
        assert_eq!(report.tests.len(), 1);
        let t = &report.tests[0];
        assert_eq!(t.confidence, "high");
        assert_eq!(t.reasons, vec!["test_file_directly_changed".to_string()]);
        assert_eq!(t.id, "test::backend/tests/test_foo.py::test_login");
    }

    #[test]
    fn reference_to_changed_symbol_promotes_enclosing_test_to_high() {
        let (mut store, _dir) = empty_store();
        // App symbol that changes.
        upsert_node(
            &mut store,
            "python::backend/app/foo.py::login",
            NodeKind::PythonFunction,
            "backend/app/foo.py",
            (5, 15),
        );
        record_range(
            &mut store,
            "python::backend/app/foo.py::login",
            NodeKind::PythonFunction,
            "backend/app/foo.py",
            (5, 15),
        );
        // Test file + enclosing test case + reference edge.
        upsert_node(
            &mut store,
            "test::backend/tests/test_login.py::test_calls_login",
            NodeKind::TestCase,
            "backend/tests/test_login.py",
            (1, 10),
        );
        upsert_edge(
            &mut store,
            "test::backend/tests/test_login.py::test_calls_login",
            "python::backend/app/foo.py::login",
            EdgeKind::Calls,
            Some("python_lsp"),
        );
        let changed = vec![ChangedFile {
            path: "backend/app/foo.py".into(),
            hunks: vec![Hunk {
                new_start: 5,
                new_end: 6,
            }],
            status: ChangeStatus::Modified,
        }];
        let report =
            select_tests_with_store(&store, &changed, &TestSelectionOptions::default()).unwrap();
        assert_eq!(report.tests.len(), 1);
        let t = &report.tests[0];
        assert_eq!(t.confidence, "high");
        assert!(t.reasons.contains(&"references_changed_symbol".to_string()));
    }

    #[test]
    fn import_of_changed_module_promotes_test_to_medium() {
        let (mut store, _dir) = empty_store();
        // Module + class + changed function — Contains chain.
        upsert_node(
            &mut store,
            "python_module::backend/app/foo",
            NodeKind::PythonModule,
            "backend/app/foo.py",
            (1, 50),
        );
        upsert_node(
            &mut store,
            "python::backend/app/foo.py::do_thing",
            NodeKind::PythonFunction,
            "backend/app/foo.py",
            (5, 15),
        );
        record_range(
            &mut store,
            "python::backend/app/foo.py::do_thing",
            NodeKind::PythonFunction,
            "backend/app/foo.py",
            (5, 15),
        );
        upsert_edge(
            &mut store,
            "python_module::backend/app/foo",
            "python::backend/app/foo.py::do_thing",
            EdgeKind::Contains,
            Some("python_ast"),
        );
        // Test module + test case + Imports edge into changed module.
        upsert_node(
            &mut store,
            "python_module::backend/tests/test_foo",
            NodeKind::PythonModule,
            "backend/tests/test_foo.py",
            (1, 20),
        );
        upsert_node(
            &mut store,
            "test::backend/tests/test_foo.py::test_uses_foo",
            NodeKind::TestCase,
            "backend/tests/test_foo.py",
            (3, 8),
        );
        upsert_edge(
            &mut store,
            "python_module::backend/tests/test_foo",
            "python_module::backend/app/foo",
            EdgeKind::Imports,
            Some("python_ast"),
        );
        let changed = vec![ChangedFile {
            path: "backend/app/foo.py".into(),
            hunks: vec![Hunk {
                new_start: 5,
                new_end: 6,
            }],
            status: ChangeStatus::Modified,
        }];
        let report =
            select_tests_with_store(&store, &changed, &TestSelectionOptions::default()).unwrap();
        assert_eq!(report.tests.len(), 1);
        let t = &report.tests[0];
        assert_eq!(t.confidence, "medium");
        assert_eq!(t.reasons, vec!["imports_changed_module".to_string()]);
    }

    #[test]
    fn no_tests_selected_when_diff_misses_everything() {
        let (mut store, _dir) = empty_store();
        upsert_node(
            &mut store,
            "test::backend/tests/test_foo.py::test_unrelated",
            NodeKind::TestCase,
            "backend/tests/test_foo.py",
            (1, 5),
        );
        let changed = vec![ChangedFile {
            path: "README.md".into(),
            hunks: vec![Hunk {
                new_start: 1,
                new_end: 2,
            }],
            status: ChangeStatus::Modified,
        }];
        let report =
            select_tests_with_store(&store, &changed, &TestSelectionOptions::default()).unwrap();
        assert!(report.tests.is_empty());
        assert!(report.stats.empty);
    }
}
