//! P5 — `specslice search` golden regression against PixCraft fixture.
//!
//! Three high-value smoke surfaces:
//!   1. Keyword search `purchase` must find paywall / pro / candidate
//!      nodes and produce match_reasons.
//!   2. `--code` snippet input extracts deterministic tokens and finds
//!      the same surface without the operator having to know the
//!      identifier exactly.
//!   3. `--file --line` resolves to the enclosing symbol and the
//!      result includes a `graph_commands` follow-up.
//!
//! Skips when the Dart SDK or the sidecar source isn't available — the
//! search results depend on the index produced by the sidecar.

use std::path::PathBuf;

use specslice_engine::business_candidates::{apply_review, ReviewStatus, ReviewVerdict};
use specslice_engine::dart_indexer::{index_dart, DartIndexOptions, RESOLVER_DART_ANALYZER};
use specslice_engine::init::{init_repository, InitOptions};
use specslice_engine::search::{
    compute_search_html_payload, run_search_with_store, SearchOptions, SearchQuery,
    SCORE_EDGE_EVIDENCE, SCORE_EXACT_ID, SCORE_NAME_TOKEN, SCORE_PATH_SEGMENT,
};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures/pixcraft_iap")
}

fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn copy_fixture_into(dst: &std::path::Path) {
    let src = fixture_path();
    for entry in walkdir::WalkDir::new(&src) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&src).unwrap();
        let target = dst.join(rel);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(entry.path(), &target).unwrap();
    }
}

fn dart_available() -> bool {
    std::process::Command::new("dart")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn sidecar_source_present() -> bool {
    workspace_dir()
        .join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart")
        .exists()
}

struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: Option<&str>) -> Self {
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        Self {
            key: key.into(),
            prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(p) => std::env::set_var(&self.key, p),
            None => std::env::remove_var(&self.key),
        }
    }
}

fn setup_indexed_repo() -> Option<(tempfile::TempDir, EnvGuard, EnvGuard)> {
    if !sidecar_source_present() || !dart_available() {
        eprintln!("skipping: dart sidecar unavailable");
        return None;
    }
    let on = EnvGuard::set("SPECSLICE_DART_ANALYZER", Some("1"));
    let sidecar_abs =
        workspace_dir().join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart");
    let bin = EnvGuard::set(
        "SPECSLICE_DART_ANALYZER_BIN",
        Some(&format!("dart run {}", sidecar_abs.display())),
    );

    let tmp = tempfile::TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    copy_fixture_into(tmp.path());

    let mut store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    store.migrate().unwrap();
    let result = index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: vec!["lib".into(), "test".into()],
            exclude_globs: vec![],
            disable_analyzer: false,
        },
    )
    .unwrap();
    assert_eq!(
        result.resolver_used, RESOLVER_DART_ANALYZER,
        "P5 search golden requires sidecar resolver"
    );
    Some((tmp, on, bin))
}

#[test]
fn p5_keyword_search_purchase_surfaces_paywall_and_pro_with_reasons() {
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };

    let store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let result = run_search_with_store(
        &store,
        SearchOptions {
            repo_root: tmp.path().into(),
            query: SearchQuery::Keywords("purchase".into()),
            depth: 1,
            kinds: Vec::new(),
            limit: 25,
            include_noise: false,
        },
    )
    .unwrap();

    // Tokenisation: `purchase` deduped to a single token.
    assert_eq!(result.tokens, vec!["purchase"]);

    // Direct matches must include at least one paywall/pro symbol and
    // a business_candidate node mentioning purchases.
    let ids: Vec<&str> = result.matches.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.iter()
            .any(|id| id.contains("PaywallScreen") && id.contains("listenToPurchaseUpdates")),
        "expected PaywallScreen.listenToPurchaseUpdates hit; got {ids:?}"
    );
    assert!(
        ids.iter()
            .any(|id| id.contains("ProNotifier") && id.contains("applyPurchase")),
        "expected ProNotifier.applyPurchase hit; got {ids:?}"
    );
    assert!(
        ids.iter()
            .any(|id| id.contains("business_candidate::complete_purchase_unlocks_pro")),
        "expected the accepted candidate to be searchable too; got {ids:?}"
    );

    // Every hit must come with explanation.
    for m in &result.matches {
        assert!(
            !m.match_reasons.is_empty(),
            "every match must carry match_reasons, got empty for {}",
            m.id
        );
    }

    // The top hit scores must be at least one full name-token bucket
    // — otherwise the search ranking is mis-tuned.
    let top_score = result.matches.first().map(|m| m.score).unwrap_or(0);
    assert!(
        top_score >= SCORE_NAME_TOKEN,
        "top score too weak: {top_score}"
    );

    // graph_commands must focus on the top hit so the agent / human
    // has a paste-ready follow-up.
    assert!(
        !result.graph_commands.is_empty(),
        "graph_commands must include a focus suggestion"
    );
    let top_id = &result.matches[0].id;
    assert!(
        result.graph_commands[0].contains(top_id),
        "graph_commands should focus on top hit {top_id}; got {:?}",
        result.graph_commands
    );

    // Subgraph must include at least one calls / persists_to / reads_provider
    // edge for the PaywallScreen.listenToPurchaseUpdates anchor — that's
    // the whole reason the search is graph-aware.
    let edge_kinds: std::collections::BTreeSet<&str> = result
        .subgraph
        .edges
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    assert!(
        edge_kinds.contains("calls")
            || edge_kinds.contains("persists_to")
            || edge_kinds.contains("reads_provider"),
        "subgraph must expose at least one semantic edge; got {edge_kinds:?}"
    );
}

#[test]
fn p5_code_snippet_input_finds_targets_via_deterministic_token_extraction() {
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };

    let store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let snippet = r#"
        // Imagine the operator pasted this fragment from a code review:
        proNotifier.applyPurchase(productId);
        Hive.box("pro_entitlement").put("entitled", true);
    "#;
    let result = run_search_with_store(
        &store,
        SearchOptions {
            repo_root: tmp.path().into(),
            query: SearchQuery::Code(snippet.into()),
            depth: 1,
            kinds: Vec::new(),
            limit: 25,
            include_noise: false,
        },
    )
    .unwrap();

    // Deterministic tokens — must contain identifier + sub-tokens AND
    // the string-literal contents (storage bucket key).
    assert!(
        result.tokens.iter().any(|t| t == "applypurchase"),
        "code tokeniser must keep `applyPurchase`; got {:?}",
        result.tokens
    );
    assert!(
        result.tokens.iter().any(|t| t == "pro_entitlement"),
        "code tokeniser must extract string-literal bucket name; got {:?}",
        result.tokens
    );

    let ids: Vec<&str> = result.matches.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.iter()
            .any(|id| id.contains("ProNotifier") && id.contains("applyPurchase")),
        "code-snippet input must locate ProNotifier.applyPurchase; got {ids:?}"
    );
    // Storage bucket synthetic node should be findable via string-literal.
    assert!(
        ids.contains(&"storage::hive::pro_entitlement"),
        "code-snippet input must find the synthetic storage bucket; got {ids:?}"
    );
}

#[test]
fn p5_file_line_input_resolves_to_enclosing_symbol() {
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };

    let store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    // Find the actual line range of PaywallScreen.listenToPurchaseUpdates
    // in the index so the test stays robust against fixture line drift.
    let ranges = store
        .list_symbol_ranges_for_file("lib/features/paywall/paywall_screen.dart")
        .unwrap();
    let target = ranges
        .iter()
        .find(|r| r.qualified_name.contains("listenToPurchaseUpdates"))
        .expect("fixture must define PaywallScreen.listenToPurchaseUpdates");
    let probe_line = target.start_line + 1;

    let result = run_search_with_store(
        &store,
        SearchOptions {
            repo_root: tmp.path().into(),
            query: SearchQuery::Position {
                path: "lib/features/paywall/paywall_screen.dart".into(),
                line: probe_line,
            },
            depth: 1,
            kinds: Vec::new(),
            limit: 25,
            include_noise: false,
        },
    )
    .unwrap();

    let top = &result.matches[0];
    // Base score is SCORE_EXACT_ID; v0.3.0-A adds SCORE_EDGE_EVIDENCE
    // when the resolved symbol has ≥1 high-tier outbound edge, which
    // is the case for `listenToPurchaseUpdates` (it calls into the
    // billing client via analyzer-resolved edges). Accept either the
    // bare base score (fixture without high-tier outbound) or the
    // boosted score.
    assert!(
        top.score == SCORE_EXACT_ID || top.score == SCORE_EXACT_ID + SCORE_EDGE_EVIDENCE,
        "position-resolved hit must score at SCORE_EXACT_ID (100) or \
         SCORE_EXACT_ID+SCORE_EDGE_EVIDENCE (130); got {}",
        top.score,
    );
    assert!(
        top.id.contains("listenToPurchaseUpdates"),
        "position search must resolve to enclosing symbol; got {}",
        top.id
    );
    assert!(
        top.match_reasons.iter().any(|r| r.starts_with("symbol at")),
        "must explain how the symbol was resolved; got {:?}",
        top.match_reasons
    );
    assert!(
        !result.graph_commands.is_empty(),
        "must seed a graph_commands focus on the enclosing symbol"
    );
}

#[test]
fn p5_path_segment_match_scores_above_weak_substring() {
    // Sanity for the scoring contract: a path-segment hit must
    // out-rank a weak substring hit so operators searching for a
    // module name go to the module first.
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };
    let store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let result = run_search_with_store(
        &store,
        SearchOptions {
            repo_root: tmp.path().into(),
            query: SearchQuery::Keywords("paywall".into()),
            depth: 0,
            kinds: Vec::new(),
            limit: 50,
            include_noise: false,
        },
    )
    .unwrap();
    // Highest-scoring match must have a path-segment reason.
    let top = result.matches.first().expect("paywall must have hits");
    assert!(
        top.match_reasons
            .iter()
            .any(|r| r.contains("path contains segment `paywall`")),
        "top paywall hit must come from a path-segment match; got {:?}",
        top.match_reasons
    );
    assert!(top.score >= SCORE_PATH_SEGMENT);
}

#[test]
fn p6_html_payload_for_purchase_keeps_canvas_readable_and_carries_business_signals() {
    // P6.5 验收：搜 "purchase" 后，HTML payload 必须满足：
    //   1. 至少给出 PaywallScreen.listenToPurchaseUpdates 的焦点卡片
    //   2. 焦点画布 ≤25 节点（与设计约束一致），可读性能在 30 秒内被人理解
    //   3. 焦点卡里能看到 calls / persists_to / declares_verification 等业务边
    //   4. 已 accepted 的 complete_purchase_unlocks_pro 候选作为卡片直接渲染
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };

    // 真实场景：人工已经在 candidate review 里把
    // complete_purchase_unlocks_pro 标为 accepted，HTML 阅读器要能
    // 把这条事实展示为「已确认业务候选」。
    apply_review(
        tmp.path(),
        "complete_purchase_unlocks_pro",
        ReviewVerdict {
            status: ReviewStatus::Accepted,
            reviewer: Some("p6-golden".into()),
            note: Some("HTML reader golden — accepted at test time".into()),
            answered_questions: vec![],
            reviewed_at: None,
        },
    )
    .unwrap();

    let store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let result = run_search_with_store(
        &store,
        SearchOptions {
            repo_root: tmp.path().into(),
            query: SearchQuery::Keywords("purchase".into()),
            depth: 1,
            kinds: Vec::new(),
            limit: 25,
            include_noise: false,
        },
    )
    .unwrap();
    let payload = compute_search_html_payload(&result, tmp.path(), 25);
    assert_eq!(
        payload.schema_version, 2,
        "schema must bump when full_subgraph / edge_kinds appear"
    );
    // P0b — the reader needs the full union subgraph + edge-kind
    // catalogue so it can render filter chips and expand neighbours
    // without re-running search.
    assert!(
        !payload.full_subgraph.nodes.is_empty(),
        "schema 2 payload must carry the full union subgraph"
    );
    assert!(
        payload.full_subgraph.nodes.len() >= payload.focus_cards[0].focused.nodes.len(),
        "full_subgraph must be a superset of any focus card's canvas"
    );
    assert!(
        !payload.edge_kinds.is_empty(),
        "edge_kinds catalogue powers the filter chip toolbar"
    );
    // Catalogue must be sorted by priority high→low, deterministic.
    let priorities: Vec<u8> = payload.edge_kinds.iter().map(|m| m.priority).collect();
    for w in priorities.windows(2) {
        assert!(
            w[0] >= w[1],
            "edge_kinds must be sorted by priority desc, got {priorities:?}"
        );
    }
    assert!(
        !payload.focus_cards.is_empty(),
        "payload must contain focus cards"
    );

    // (1) 找到 listenToPurchaseUpdates 的焦点卡片。
    let paywall_card = payload
        .focus_cards
        .iter()
        .find(|c| c.match_id.contains("PaywallScreen.listenToPurchaseUpdates"))
        .expect("listenToPurchaseUpdates focus card must exist");
    // (2) 画布严守 ≤25 节点。
    assert!(
        paywall_card.focused.nodes.len() <= 25,
        "P6 budget: focused canvas must be ≤25 nodes, got {}",
        paywall_card.focused.nodes.len()
    );
    // (3) 必须能直接读到关键业务边：calls + (persists_to or navigates_to + reads_provider 至少一种)
    let edge_kinds: std::collections::BTreeSet<&str> = paywall_card
        .edge_groups
        .keys()
        .map(|s| s.as_str())
        .collect();
    assert!(
        edge_kinds.contains("calls"),
        "PaywallScreen focus card must surface at least one calls edge; got {edge_kinds:?}"
    );
    assert!(
        edge_kinds.contains("persists_to")
            || edge_kinds.contains("reads_provider")
            || edge_kinds.contains("navigates_to")
            || edge_kinds.contains("subscribes_stream"),
        "PaywallScreen focus card must surface a business-semantic edge; got {edge_kinds:?}"
    );
    // (3') 候选证据边 derives_from 不能因为 noise filter 被吞掉。
    assert!(
        payload
            .focus_cards
            .iter()
            .any(|c| c.edge_groups.contains_key("derives_from")),
        "至少一个焦点卡片应当展示 candidate derives_from 证据边"
    );

    // (4) 已 accepted 的业务候选卡片要被独立 render。
    let candidate_card = payload
        .focus_cards
        .iter()
        .find(|c| c.match_id == "business_candidate::complete_purchase_unlocks_pro")
        .expect("accepted candidate must appear as a focus card");
    let details = candidate_card
        .candidate
        .as_ref()
        .expect("candidate card must carry description payload");
    assert_eq!(details.status, "accepted");
    assert!(
        candidate_card.badge.contains("已确认"),
        "accepted candidate badge must be Chinese-readable: {}",
        candidate_card.badge
    );
    // 候选卡片画布也应当 ≤25 节点。
    assert!(
        candidate_card.focused.nodes.len() <= 25,
        "candidate focus card canvas must respect the budget, got {}",
        candidate_card.focused.nodes.len()
    );
}
