//! P19 — `specslice questions`: surface the unresolved facts an
//! AI agent (or a human reviewer) needs to confirm before
//! claiming the graph is "complete".
//!
//! The MVP reports four categories. Each category is intentionally
//! conservative — we don't list every node with a missing field;
//! we only list things a human or AI can plausibly answer in one
//! short pass.
//!
//! 1. **Orphan symbols** — code symbols with no incoming `Calls` /
//!    `References` / `Imports` edges and no framework role. These
//!    are likely either dead code or framework-invoked code we
//!    haven't classified yet.
//! 2. **Pending business candidates** — AI-authored
//!    `BusinessCandidate` nodes that the human reviewer has not
//!    accepted / rejected.
//! 3. **Tests with no symbol reference** — TestCase / TestGroup
//!    nodes whose body references no symbol the indexer knows
//!    about. These tests probably exercise functionality that
//!    falls through the indexer's blind spots.
//! 4. **Files imported by tests but not indexed** — tests' module
//!    Imports edges that point at a node SpecSlice never created.
//!    Usually means cross-language imports (Python → C extension,
//!    Dart → generated code) the AST pass dropped.
//!
//! Output is the "AI clarification packet" — give it to a chat
//! agent verbatim and it knows what to ask the user next.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::language_traits::is_code_symbol;
use specslice_core::{ArtifactId, EdgeKind, Node, NodeKind};
use specslice_store::Store;

use crate::business_candidates::{
    candidate_artifact_id, load_business_candidates, BUSINESS_CANDIDATES_REL_PATH,
};
use crate::python_frameworks::FrameworkRole;

pub const QUESTIONS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct QuestionsOptions {
    pub repo_root: PathBuf,
    /// Maximum number of questions PER category to emit. Stops
    /// the report exploding on large repos.
    pub max_per_category: usize,
}

impl Default for QuestionsOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            max_per_category: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionsReport {
    pub schema_version: u32,
    pub stats: QuestionsStats,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionsStats {
    pub total_questions: usize,
    pub by_category: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// Stable category id (`orphan_symbol`, `pending_candidate`,
    /// `test_without_references`, `dangling_import`).
    pub category: String,
    /// Severity hint for downstream tooling (`info` / `warn`).
    pub severity: String,
    /// AI-ready natural-language prompt. Each prompt is written
    /// in second person and ends in a `?` — paste it directly
    /// into the chat.
    pub prompt: String,
    /// Optional pointer to the related artifact id. Lets agents
    /// fetch the surrounding context via `specslice graph
    /// --focus`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    /// Optional repo-relative path the agent / human should
    /// open next.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

pub fn analyze_questions(options: QuestionsOptions) -> Result<QuestionsReport> {
    let db_path = options.repo_root.join(".specslice").join("graph.db");
    let store = Store::open(&db_path)
        .with_context(|| format!("opening graph store at {}", db_path.display()))?;
    analyze_questions_with_store(&store, &options)
}

pub fn analyze_questions_with_store(
    store: &Store,
    options: &QuestionsOptions,
) -> Result<QuestionsReport> {
    let nodes = store.list_all_nodes().context("listing nodes")?;
    let edges = store.list_all_edges().context("listing edges")?;

    // EdgeKind does not implement Ord today (it's tagged as a Copy
    // enum in core), so use HashSet for the bag-of-kinds index.
    let mut incoming_kinds: BTreeMap<ArtifactId, HashSet<EdgeKind>> = BTreeMap::new();
    let mut outgoing_kinds: BTreeMap<ArtifactId, HashSet<EdgeKind>> = BTreeMap::new();
    let mut import_targets_present: BTreeSet<ArtifactId> = BTreeSet::new();
    for edge in &edges {
        incoming_kinds
            .entry(edge.to_id.clone())
            .or_default()
            .insert(edge.kind);
        outgoing_kinds
            .entry(edge.from_id.clone())
            .or_default()
            .insert(edge.kind);
    }
    let known_node_ids: BTreeSet<ArtifactId> = nodes.iter().map(|n| n.id.clone()).collect();
    for edge in &edges {
        if matches!(edge.kind, EdgeKind::Imports) && known_node_ids.contains(&edge.to_id) {
            import_targets_present.insert(edge.to_id.clone());
        }
    }

    let mut questions: Vec<Question> = Vec::new();

    // --- 1. orphan symbols (no incoming Calls/References/Imports,
    //         no framework role) -------------------------------
    {
        let mut count = 0usize;
        for node in &nodes {
            if !is_code_symbol(node.kind) {
                continue;
            }
            if has_framework_role(node) {
                continue;
            }
            let inc = incoming_kinds.get(&node.id);
            let has_real_users = inc
                .map(|kinds| {
                    kinds.iter().any(|k| {
                        matches!(
                            k,
                            EdgeKind::Calls
                                | EdgeKind::References
                                | EdgeKind::Imports
                                | EdgeKind::ReadsProvider
                                | EdgeKind::NavigatesTo
                                | EdgeKind::PersistsTo
                                | EdgeKind::SubscribesStream
                        )
                    })
                })
                .unwrap_or(false);
            if has_real_users {
                continue;
            }
            // Skip top-level entry points: `main`, names ending
            // with `_main`, anything obviously framework-invoked
            // by convention.
            if matches!(node.name.as_deref(), Some("main") | Some("__main__")) {
                continue;
            }
            questions.push(Question {
                category: "orphan_symbol".into(),
                severity: "info".into(),
                prompt: format!(
                    "符号 `{}` ({}) 没有任何 Calls/References/Imports 入边，也没有识别到框架装饰器。它是被外部框架或反射调用的吗？还是已经过时？",
                    node.name.clone().unwrap_or_else(|| node.id.to_string()),
                    node.kind.as_str(),
                ),
                artifact_id: Some(node.id.to_string()),
                path: node.path.clone(),
            });
            count += 1;
            if count >= options.max_per_category {
                break;
            }
        }
    }

    // --- 2. pending business candidates ---------------------------
    //
    // Real-world candidates live in
    // `.specslice/candidates/business_logic.yaml` (graph.rs merges them
    // into the in-memory graph view; they are not persisted as
    // `BusinessCandidate` nodes). We therefore load the YAML directly.
    // For backward compatibility we also keep the store-node path —
    // older tests / pipelines that did persist candidate nodes still
    // surface them.
    {
        use crate::business_candidates::ReviewStatus;

        let mut count = 0usize;
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        // (a) YAML source of truth. We ignore parse warnings — they
        // bubble up through `specslice candidate list` instead.
        let yaml_outcome = load_business_candidates(&options.repo_root).ok();
        let yaml_path = options
            .repo_root
            .join(BUSINESS_CANDIDATES_REL_PATH)
            .to_string_lossy()
            .to_string();
        if let Some(outcome) = yaml_outcome {
            for c in &outcome.document.candidates {
                let status = c.review_status();
                let is_pending = matches!(
                    status,
                    None | Some(ReviewStatus::Pending) | Some(ReviewStatus::NeedsChanges)
                );
                if !is_pending {
                    continue;
                }
                let id = candidate_artifact_id(&c.id);
                if !seen_ids.insert(id.clone()) {
                    continue;
                }
                questions.push(Question {
                    category: "pending_candidate".into(),
                    severity: "warn".into(),
                    prompt: format!(
                        "业务候选 `{}` 还没有被确认进入 confirmed graph。请阅读它的证据后选择 accept / reject / 修订描述。",
                        c.name,
                    ),
                    artifact_id: Some(id),
                    path: Some(yaml_path.clone()),
                });
                count += 1;
                if count >= options.max_per_category {
                    break;
                }
            }
        }

        // (b) Legacy store-node path. Some pipelines persist
        // BusinessCandidate nodes directly; honour them so the
        // pre-P19 behaviour stays green.
        if count < options.max_per_category {
            for node in &nodes {
                if !matches!(node.kind, NodeKind::BusinessCandidate) {
                    continue;
                }
                let id_str = node.id.to_string();
                if seen_ids.contains(&id_str) {
                    continue;
                }
                let inc = incoming_kinds.get(&node.id);
                let already_confirmed = inc
                    .map(|kinds| {
                        kinds
                            .iter()
                            .any(|k| matches!(k, EdgeKind::DeclaresImplementation))
                    })
                    .unwrap_or(false);
                if already_confirmed {
                    continue;
                }
                seen_ids.insert(id_str.clone());
                questions.push(Question {
                    category: "pending_candidate".into(),
                    severity: "warn".into(),
                    prompt: format!(
                        "业务候选 `{}` 还没有被确认进入 confirmed graph。请阅读它的证据后选择 accept / reject / 修订描述。",
                        node.name.clone().unwrap_or_else(|| id_str.clone()),
                    ),
                    artifact_id: Some(id_str),
                    path: node.path.clone(),
                });
                count += 1;
                if count >= options.max_per_category {
                    break;
                }
            }
        }
    }

    // --- 3. tests with no references / calls --------------------
    {
        let mut count = 0usize;
        for node in &nodes {
            if !matches!(node.kind, NodeKind::TestCase | NodeKind::TestGroup) {
                continue;
            }
            let out = outgoing_kinds.get(&node.id);
            let has_links = out
                .map(|kinds| {
                    kinds
                        .iter()
                        .any(|k| matches!(k, EdgeKind::Calls | EdgeKind::References))
                })
                .unwrap_or(false);
            if has_links {
                continue;
            }
            questions.push(Question {
                category: "test_without_references".into(),
                severity: "info".into(),
                prompt: format!(
                    "测试 `{}` 没有任何到代码符号的 Calls/References 边。它是断言纯文本 / fixture / 外部 IO 吗？需要补哪一类语言适配器才能识别？",
                    node.name.clone().unwrap_or_else(|| node.id.to_string()),
                ),
                artifact_id: Some(node.id.to_string()),
                path: node.path.clone(),
            });
            count += 1;
            if count >= options.max_per_category {
                break;
            }
        }
    }

    // --- 4. dangling imports (test imports a module we never
    //          indexed as a node) ---------------------------------
    {
        let mut count = 0usize;
        // We approximate test files by location (`tests/` /
        // `test/`) so the question stays scoped to test-side
        // misses; non-test dangling imports are usually noisy
        // cross-language assets.
        for edge in &edges {
            if !matches!(edge.kind, EdgeKind::Imports) {
                continue;
            }
            if known_node_ids.contains(&edge.to_id) {
                continue;
            }
            // Pull the source-side node (the importer) so we can
            // filter to tests.
            let Some(src_path) = edge.source_file.clone().or_else(|| {
                nodes
                    .iter()
                    .find(|n| n.id == edge.from_id)
                    .and_then(|n| n.path.clone())
            }) else {
                continue;
            };
            let is_test = src_path.starts_with("test")
                || src_path.contains("/tests/")
                || src_path.contains("/test/");
            if !is_test {
                continue;
            }
            questions.push(Question {
                category: "dangling_import".into(),
                severity: "info".into(),
                prompt: format!(
                    "测试文件 `{src_path}` 导入了 `{}`，但代码图里没有这个模块。它是外部依赖 / 跨语言 / 还是被排除的目录？",
                    edge.to_id,
                ),
                artifact_id: Some(edge.to_id.to_string()),
                path: Some(src_path),
            });
            count += 1;
            if count >= options.max_per_category {
                break;
            }
        }
    }

    // Compose stats.
    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    for q in &questions {
        *by_category.entry(q.category.clone()).or_default() += 1;
    }
    let stats = QuestionsStats {
        total_questions: questions.len(),
        by_category,
    };

    // Sort: warnings first, then by category, then by path.
    questions.sort_by(|a, b| {
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then(a.category.cmp(&b.category))
            .then(a.path.cmp(&b.path))
    });

    Ok(QuestionsReport {
        schema_version: QUESTIONS_SCHEMA_VERSION,
        stats,
        questions,
    })
}

fn has_framework_role(node: &Node) -> bool {
    let Some(meta) = node.metadata_json.as_deref() else {
        return false;
    };
    serde_json::from_str::<FrameworkRole>(meta)
        .map(|r| r.is_framework_entrypoint())
        .unwrap_or(false)
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "error" => 3,
        "warn" => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{EdgeAssertion, EdgeSource};
    use tempfile::TempDir;

    fn empty_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn upsert(store: &mut Store, id: &str, kind: NodeKind, path: Option<&str>, name: Option<&str>) {
        let mut n = Node::new(ArtifactId::new(id), kind);
        if let Some(p) = path {
            n.path = Some(p.into());
        }
        if let Some(name) = name {
            n.name = Some(name.into());
        }
        store.upsert_node(&n).unwrap();
    }

    fn upsert_edge(store: &mut Store, from: &str, to: &str, kind: EdgeKind) {
        let e = EdgeAssertion::fact(
            ArtifactId::new(from),
            ArtifactId::new(to),
            kind,
            EdgeSource::LanguageAdapter,
        );
        store.upsert_edge(&e).unwrap();
    }

    #[test]
    fn orphan_symbol_is_surfaced_with_artifact_id() {
        let (mut store, _d) = empty_store();
        upsert(
            &mut store,
            "python::foo.py::lonely",
            NodeKind::PythonFunction,
            Some("foo.py"),
            Some("lonely"),
        );
        let r = analyze_questions_with_store(&store, &QuestionsOptions::default()).unwrap();
        let orphans: Vec<_> = r
            .questions
            .iter()
            .filter(|q| q.category == "orphan_symbol")
            .collect();
        assert_eq!(orphans.len(), 1);
        assert_eq!(
            orphans[0].artifact_id.as_deref(),
            Some("python::foo.py::lonely")
        );
    }

    #[test]
    fn referenced_symbol_is_not_surfaced_as_orphan() {
        let (mut store, _d) = empty_store();
        upsert(
            &mut store,
            "python::foo.py::caller",
            NodeKind::PythonFunction,
            Some("foo.py"),
            Some("caller"),
        );
        upsert(
            &mut store,
            "python::foo.py::callee",
            NodeKind::PythonFunction,
            Some("foo.py"),
            Some("callee"),
        );
        upsert_edge(
            &mut store,
            "python::foo.py::caller",
            "python::foo.py::callee",
            EdgeKind::Calls,
        );
        let r = analyze_questions_with_store(&store, &QuestionsOptions::default()).unwrap();
        let orphans: Vec<_> = r
            .questions
            .iter()
            .filter(|q| q.category == "orphan_symbol")
            .collect();
        // `caller` is still an orphan (nothing calls it), but
        // `callee` is not.
        assert!(orphans
            .iter()
            .any(|q| q.artifact_id.as_deref() == Some("python::foo.py::caller")));
        assert!(!orphans
            .iter()
            .any(|q| q.artifact_id.as_deref() == Some("python::foo.py::callee")));
    }

    #[test]
    fn pending_business_candidate_is_surfaced_as_warn() {
        // Legacy path: a `BusinessCandidate` node that some upstream
        // step persisted into the store (e.g. the graph-view pipeline
        // writing its in-memory representation back). When no YAML is
        // present we still detect via the store; this keeps the
        // pre-P19 behaviour green.
        let (mut store, _d) = empty_store();
        upsert(
            &mut store,
            "business_candidate::auth_login",
            NodeKind::BusinessCandidate,
            None,
            Some("auth_login"),
        );
        let r = analyze_questions_with_store(&store, &QuestionsOptions::default()).unwrap();
        let pending: Vec<_> = r
            .questions
            .iter()
            .filter(|q| q.category == "pending_candidate")
            .collect();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].severity, "warn");
    }

    /// Real-world path: candidates only ever live in
    /// `.specslice/candidates/business_logic.yaml` (`graph.rs` merges
    /// them into the in-memory graph view but never persists nodes).
    /// `questions` must therefore consult the YAML directly — the
    /// store-node path alone misses every real-repo pending candidate.
    #[test]
    fn pending_business_candidate_is_loaded_from_yaml() {
        let dir = TempDir::new().unwrap();
        let candidates_dir = dir.path().join(".specslice").join("candidates");
        std::fs::create_dir_all(&candidates_dir).unwrap();
        let yaml = r#"
schema_version: 1
candidates:
  - id: pay_flow
    name: PayFlow
    description: stub
    confidence: 0.4
    status: proposed
  - id: signup_flow
    name: SignUp
    description: stub
    status: proposed
    review:
      status: needs_changes
  - id: already_done
    name: AlreadyDone
    description: stub
    status: accepted
    review:
      status: accepted
"#;
        std::fs::write(candidates_dir.join("business_logic.yaml"), yaml).unwrap();

        // Use an empty store: only the YAML should drive the candidate
        // category. Other categories may add unrelated questions —
        // filter on `pending_candidate`.
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let opts = QuestionsOptions {
            repo_root: dir.path().to_path_buf(),
            ..QuestionsOptions::default()
        };
        let r = analyze_questions_with_store(&store, &opts).unwrap();
        let pending: Vec<_> = r
            .questions
            .iter()
            .filter(|q| q.category == "pending_candidate")
            .collect();
        // pay_flow + signup_flow → 2; already_done is accepted → 0
        let ids: std::collections::HashSet<&str> = pending
            .iter()
            .filter_map(|q| q.artifact_id.as_deref())
            .collect();
        assert!(ids.contains("business_candidate::pay_flow"), "{ids:?}");
        assert!(ids.contains("business_candidate::signup_flow"), "{ids:?}");
        assert!(!ids.contains("business_candidate::already_done"), "{ids:?}");
        assert_eq!(pending.len(), 2, "{pending:#?}");
        // path should point at the YAML so an agent can open it.
        assert!(pending[0]
            .path
            .as_deref()
            .map(|p| p.contains("business_logic.yaml"))
            .unwrap_or(false));
    }

    #[test]
    fn test_without_references_is_surfaced() {
        let (mut store, _d) = empty_store();
        upsert(
            &mut store,
            "test::foo.py::test_lonely",
            NodeKind::TestCase,
            Some("tests/foo.py"),
            Some("test_lonely"),
        );
        let r = analyze_questions_with_store(&store, &QuestionsOptions::default()).unwrap();
        let lonely: Vec<_> = r
            .questions
            .iter()
            .filter(|q| q.category == "test_without_references")
            .collect();
        assert_eq!(lonely.len(), 1);
    }

    /// Regression — the original local `is_code_symbol` only matched
    /// `Python{Function,Method,Class} | Dart{Function,Method,Class,
    /// Constructor} | Swift{Function,Method,Class,Struct} |
    /// Go{Function,Method,Struct}`. It dropped:
    /// - `SwiftInitializer`, `SwiftEnum`, `SwiftProtocol`
    /// - `GoInterface`
    /// - `PythonModule`
    /// - every Typescript* / Java* kind shipped in P20.
    ///
    /// This test pins all of them down so future drift screams loudly.
    #[test]
    fn orphan_detection_uses_language_traits_for_every_code_kind() {
        let kinds: &[(&str, NodeKind)] = &[
            ("swift::Foo::init", NodeKind::SwiftInitializer),
            ("swift::Foo", NodeKind::SwiftEnum),
            ("swift::Walker", NodeKind::SwiftProtocol),
            ("go::pkg::Reader", NodeKind::GoInterface),
            ("python::pkg/__init__.py", NodeKind::PythonModule),
            ("ts::src/foo.ts", NodeKind::TypescriptModule),
            ("ts::Foo", NodeKind::TypescriptInterface),
            ("ts::Color", NodeKind::TypescriptEnum),
            ("java::com.example", NodeKind::JavaPackage),
            ("java::com.example.Foo", NodeKind::JavaInterface),
            ("java::com.example.Foo::Foo", NodeKind::JavaConstructor),
        ];
        let (mut store, _d) = empty_store();
        for (id, kind) in kinds {
            upsert(&mut store, id, *kind, Some("dummy"), Some("X"));
        }
        let r = analyze_questions_with_store(&store, &QuestionsOptions::default()).unwrap();
        let ids: std::collections::HashSet<String> = r
            .questions
            .iter()
            .filter(|q| q.category == "orphan_symbol")
            .filter_map(|q| q.artifact_id.clone())
            .collect();
        for (expected, kind) in kinds {
            assert!(
                ids.contains(*expected),
                "{kind:?} ({expected}) should have been surfaced as orphan_symbol; saw {ids:?}",
            );
        }
    }
}
