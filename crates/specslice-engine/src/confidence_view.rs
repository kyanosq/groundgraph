//! Scope-aware views over each node's edge-confidence distribution.
//!
//! v0.3.0-A glue between the existing [`edge_confidence`](crate::edge_confidence)
//! tier rule and downstream consumers (dead-code reason strings, search
//! ranking, future candidate evidence scoring). The point of this module
//! is that **the question "which edge kinds count as usage evidence?"
//! has one answer, encoded as [`EdgeQualityScope`]**, instead of being
//! re-derived ad-hoc in every consumer.
//!
//! Spec: `docs/superpowers/specs/2026-05-22-v030-a-confidence-plumbing-design.md`.

use anyhow::{Context, Result};
use specslice_core::{
    edge::{EdgeAssertion, EdgeKind, EdgeStatus},
    ArtifactId,
};
use specslice_store::Store;
use std::collections::BTreeMap;

use crate::edge_confidence::{confidence_for_edge, EdgeConfidence};

/// Which question we're asking the graph. Different consumers care about
/// different edge kinds (see scope rules below).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeQualityScope {
    /// "Is this symbol actually used by the rest of the codebase?"
    ///
    /// Counts: semantic usage edges and explicit verification declarations.
    /// Excludes: structural edges (`Contains`), soft-structural edges
    /// (`Imports`), declarative edges (`Documents`, `DeclaresImplementation`),
    /// and AI-derived edges (`DerivesFrom`).
    ///
    /// Drives the dead-code "only low-tier evidence" reason.
    Usage,
    /// "How strong is this symbol's outgoing evidence for the purpose
    /// of ranking it in a search result?"
    ///
    /// v0.3.0-A keeps this identical to [`Usage`](Self::Usage). The
    /// enum exists so B/C/D can broaden the kind set (e.g. add
    /// `Imports` as a soft signal) without changing the public API.
    SearchRanking,
}

impl EdgeQualityScope {
    /// Whether this edge kind counts toward the summary under the
    /// current scope. **The single source of truth** — every consumer
    /// must route through this function.
    pub fn allows(self, kind: EdgeKind) -> bool {
        match self {
            EdgeQualityScope::Usage | EdgeQualityScope::SearchRanking => matches!(
                kind,
                EdgeKind::Calls
                    | EdgeKind::References
                    | EdgeKind::ReadsProvider
                    | EdgeKind::PersistsTo
                    | EdgeKind::NavigatesTo
                    | EdgeKind::SubscribesStream
                    | EdgeKind::DeclaresVerification
            ),
        }
    }
}

/// Counts of edges per [`EdgeConfidence`] tier inside a given scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EdgeQualitySummary {
    pub high: u32,
    pub medium: u32,
    pub low: u32,
}

impl EdgeQualitySummary {
    pub fn total(&self) -> u32 {
        self.high + self.medium + self.low
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// Tier with the highest count. Ties break toward higher tier
    /// (high > medium > low) so callers can describe a summary in a
    /// single word.
    pub fn dominant(&self) -> Option<EdgeConfidence> {
        if self.is_empty() {
            return None;
        }
        let high = self.high;
        let medium = self.medium;
        let low = self.low;
        if high >= medium && high >= low {
            Some(EdgeConfidence::High)
        } else if medium >= low {
            Some(EdgeConfidence::Medium)
        } else {
            Some(EdgeConfidence::Low)
        }
    }

    /// All allowed edges sit in the `low` tier. Used by dead-code to
    /// flag "only AST fallback / lightweight resolver evidence".
    pub fn is_only_low(&self) -> bool {
        self.high == 0 && self.medium == 0 && self.low > 0
    }
}

/// Pure summary over an already-loaded edge list. Used by dead-code
/// which holds the inbound edges in memory; the store-backed variants
/// below are thin wrappers around this.
///
/// Skips deprecated edges entirely so stale evidence doesn't inflate
/// the low tier.
///
/// Generic over the iterator so call sites can pass `&[EdgeAssertion]`
/// (via `.iter()`) or `Vec<&EdgeAssertion>` (via `.iter().copied()`)
/// without cloning.
pub fn summarize_edges<'a, I>(edges: I, scope: EdgeQualityScope) -> EdgeQualitySummary
where
    I: IntoIterator<Item = &'a EdgeAssertion>,
{
    let mut s = EdgeQualitySummary::default();
    for edge in edges {
        if matches!(edge.status, EdgeStatus::Deprecated) {
            continue;
        }
        if !scope.allows(edge.kind) {
            continue;
        }
        match confidence_for_edge(edge) {
            EdgeConfidence::High => s.high += 1,
            EdgeConfidence::Medium => s.medium += 1,
            EdgeConfidence::Low => s.low += 1,
        }
    }
    s
}

pub fn inbound_edge_quality(
    store: &Store,
    node_id: &str,
    scope: EdgeQualityScope,
) -> Result<EdgeQualitySummary> {
    let aid = ArtifactId::new(node_id.to_string());
    let edges = store
        .list_edges_to(&aid)
        .with_context(|| format!("listing inbound edges for `{node_id}`"))?;
    Ok(summarize_edges(edges.iter(), scope))
}

pub fn outbound_edge_quality(
    store: &Store,
    node_id: &str,
    scope: EdgeQualityScope,
) -> Result<EdgeQualitySummary> {
    let aid = ArtifactId::new(node_id.to_string());
    let edges = store
        .list_edges_from(&aid)
        .with_context(|| format!("listing outbound edges for `{node_id}`"))?;
    Ok(summarize_edges(edges.iter(), scope))
}

/// Lightweight neighbour info — id + name + kind of the *other endpoint*
/// of an edge touching the anchor node. Used by search Pass B
/// ([`SCORE_NEIGHBOR`](crate::search::SCORE_NEIGHBOR)) to look up
/// cluster mates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeighborInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// One-hop neighbours (both directions), deduplicated by the other
/// endpoint's id, sorted ascending by id, capped at `cap`.
///
/// Self-loops (edges where `from == to == node_id`) are excluded.
/// Edges to nodes that don't exist in the `nodes` table are also
/// excluded (the search subgraph would otherwise show dangling refs).
///
/// Scope-free on purpose: adjacency is a structural question, and
/// future consumers may want it independently of the
/// [`EdgeQualityScope`] decision.
pub fn neighbors_of(store: &Store, node_id: &str, cap: usize) -> Result<Vec<NeighborInfo>> {
    if cap == 0 {
        return Ok(Vec::new());
    }
    let aid = ArtifactId::new(node_id.to_string());
    let from_edges = store
        .list_edges_from(&aid)
        .with_context(|| format!("listing outbound edges for `{node_id}`"))?;
    let to_edges = store
        .list_edges_to(&aid)
        .with_context(|| format!("listing inbound edges for `{node_id}`"))?;

    // BTreeMap dedups by other-endpoint id and keeps deterministic
    // ascending order in one step.
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    for edge in from_edges.iter().chain(to_edges.iter()) {
        let other = if edge.from_id.as_str() == node_id {
            edge.to_id.as_str()
        } else {
            edge.from_id.as_str()
        };
        if other == node_id {
            continue;
        }
        seen.insert(other.to_string(), ());
    }

    let mut out = Vec::with_capacity(seen.len().min(cap));
    for other_id in seen.keys() {
        if out.len() >= cap {
            break;
        }
        let other_aid = ArtifactId::new(other_id.clone());
        if let Some(node) = store
            .find_node(&other_aid)
            .with_context(|| format!("resolving neighbour `{other_id}`"))?
        {
            out.push(NeighborInfo {
                id: node.id.to_string(),
                name: node
                    .name
                    .clone()
                    .or_else(|| node.stable_key.clone())
                    .unwrap_or_else(|| node.id.to_string()),
                kind: node.kind.as_str().to_string(),
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{
        edge::{EdgeAssertion, EdgeSource, EdgeStatus},
        ArtifactId, Node, NodeKind,
    };
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // EdgeKind matrix — locks the scope contract for every variant.
    // Adding a new EdgeKind variant must require updating this matrix
    // (compiler + test together).
    // -----------------------------------------------------------------------

    const ALL_EDGE_KINDS: &[EdgeKind] = &[
        EdgeKind::Contains,
        EdgeKind::Imports,
        EdgeKind::Documents,
        EdgeKind::DeclaresImplementation,
        EdgeKind::DeclaresVerification,
        EdgeKind::References,
        EdgeKind::Calls,
        EdgeKind::ReadsProvider,
        EdgeKind::NavigatesTo,
        EdgeKind::PersistsTo,
        EdgeKind::SubscribesStream,
        EdgeKind::DerivesFrom,
    ];

    #[test]
    fn all_edge_kinds_have_explicit_scope_decision() {
        let expected_allowed = |kind: EdgeKind| {
            matches!(
                kind,
                EdgeKind::Calls
                    | EdgeKind::References
                    | EdgeKind::ReadsProvider
                    | EdgeKind::PersistsTo
                    | EdgeKind::NavigatesTo
                    | EdgeKind::SubscribesStream
                    | EdgeKind::DeclaresVerification
            )
        };
        for &kind in ALL_EDGE_KINDS {
            assert_eq!(
                EdgeQualityScope::Usage.allows(kind),
                expected_allowed(kind),
                "Usage scope decision for {kind:?} drifted",
            );
            assert_eq!(
                EdgeQualityScope::SearchRanking.allows(kind),
                expected_allowed(kind),
                "SearchRanking scope decision for {kind:?} drifted",
            );
        }
    }

    #[test]
    fn usage_scope_excludes_contains_imports_documents_declares_impl_derives_from() {
        for kind in [
            EdgeKind::Contains,
            EdgeKind::Imports,
            EdgeKind::Documents,
            EdgeKind::DeclaresImplementation,
            EdgeKind::DerivesFrom,
        ] {
            assert!(
                !EdgeQualityScope::Usage.allows(kind),
                "{kind:?} must not be in Usage scope"
            );
        }
    }

    #[test]
    fn usage_scope_allows_calls_references_and_semantic_kinds() {
        for kind in [
            EdgeKind::Calls,
            EdgeKind::References,
            EdgeKind::ReadsProvider,
            EdgeKind::NavigatesTo,
            EdgeKind::PersistsTo,
            EdgeKind::SubscribesStream,
            EdgeKind::DeclaresVerification,
        ] {
            assert!(
                EdgeQualityScope::Usage.allows(kind),
                "{kind:?} must be in Usage scope"
            );
        }
    }

    #[test]
    fn search_ranking_scope_equals_usage_scope_in_v030_a() {
        // Locks the design promise that v0.3.0-A does not introduce
        // a divergent search scope. Future sub-projects that broaden
        // SearchRanking *must* explicitly modify this assertion so the
        // policy change is reviewable.
        for &kind in ALL_EDGE_KINDS {
            assert_eq!(
                EdgeQualityScope::SearchRanking.allows(kind),
                EdgeQualityScope::Usage.allows(kind),
                "SearchRanking scope must equal Usage in v0.3.0-A; \
                 if you intentionally diverged for {kind:?}, update \
                 this test and document the change in the v0.3.0-A spec.",
            );
        }
    }

    // -----------------------------------------------------------------------
    // summarize_edges — pure function over an in-memory edge list.
    // -----------------------------------------------------------------------

    fn calls_edge(from: &str, to: &str, indexer: &str) -> EdgeAssertion {
        let mut e = EdgeAssertion::fact(
            ArtifactId::new(from),
            ArtifactId::new(to),
            EdgeKind::Calls,
            EdgeSource::LanguageAdapter,
        );
        e.indexer = Some(indexer.into());
        e
    }

    #[test]
    fn summarize_edges_counts_by_tier() {
        let edges = vec![
            calls_edge("a", "x", "python_lsp"),     // high
            calls_edge("b", "x", "dart_analyzer"),  // high
            calls_edge("c", "x", "python_ast"),     // medium
            calls_edge("d", "x", "typescript_ast"), // medium
            {
                // low: deprecated edge (collapses to Low per
                // edge_confidence rule) — actually we *exclude*
                // deprecated, so this should not count. See separate
                // test below.
                let mut e = calls_edge("e", "x", "python_ast");
                e.status = EdgeStatus::Confirmed;
                e
            },
        ];
        let s = summarize_edges(&edges, EdgeQualityScope::Usage);
        assert_eq!(s.high, 2);
        assert_eq!(s.medium, 3);
        assert_eq!(s.low, 0);
        assert_eq!(s.total(), 5);
    }

    #[test]
    fn summarize_edges_counts_derives_from_as_low_when_scope_allowed() {
        // Sanity check: DerivesFrom is EdgeConfidence::Low. Once we
        // allow it (e.g. in a future scope) it would land in the low
        // bucket. v0.3.0-A excludes it, so total stays 0 today.
        let mut e = EdgeAssertion::fact(
            ArtifactId::new("c"),
            ArtifactId::new("x"),
            EdgeKind::DerivesFrom,
            EdgeSource::LanguageAdapter,
        );
        e.indexer = Some("connect_ai".into());
        let s = summarize_edges(std::slice::from_ref(&e), EdgeQualityScope::Usage);
        assert!(s.is_empty(), "DerivesFrom must be excluded from Usage");
    }

    #[test]
    fn summarize_edges_excludes_deprecated_status() {
        let mut e = calls_edge("a", "x", "python_lsp");
        e.status = EdgeStatus::Deprecated;
        let s = summarize_edges(std::slice::from_ref(&e), EdgeQualityScope::Usage);
        assert!(s.is_empty(), "deprecated edges must not count");
    }

    #[test]
    fn summarize_edges_excludes_contains_even_when_status_confirmed() {
        let e = EdgeAssertion::fact(
            ArtifactId::new("file"),
            ArtifactId::new("x"),
            EdgeKind::Contains,
            EdgeSource::Filesystem,
        );
        let s = summarize_edges(std::slice::from_ref(&e), EdgeQualityScope::Usage);
        assert!(s.is_empty(), "Contains must never be a Usage signal");
    }

    #[test]
    fn dominant_breaks_ties_in_favor_of_high_then_medium_then_low() {
        let s = EdgeQualitySummary {
            high: 1,
            medium: 1,
            low: 1,
        };
        assert_eq!(s.dominant(), Some(EdgeConfidence::High));
        let s = EdgeQualitySummary {
            high: 0,
            medium: 1,
            low: 1,
        };
        assert_eq!(s.dominant(), Some(EdgeConfidence::Medium));
        let s = EdgeQualitySummary {
            high: 0,
            medium: 0,
            low: 3,
        };
        assert_eq!(s.dominant(), Some(EdgeConfidence::Low));
        let s = EdgeQualitySummary::default();
        assert_eq!(s.dominant(), None);
    }

    #[test]
    fn is_only_low_true_when_only_low_evidence() {
        let s = EdgeQualitySummary {
            high: 0,
            medium: 0,
            low: 2,
        };
        assert!(s.is_only_low());
        let s = EdgeQualitySummary {
            high: 0,
            medium: 1,
            low: 2,
        };
        assert!(!s.is_only_low());
        let s = EdgeQualitySummary::default();
        assert!(!s.is_only_low(), "empty summary is not 'only low'");
    }

    // -----------------------------------------------------------------------
    // Store-backed variants.
    // -----------------------------------------------------------------------

    fn empty_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn upsert_dart_method(store: &mut Store, id: &str) {
        let node = Node {
            id: ArtifactId::new(id),
            kind: NodeKind::DartMethod,
            name: Some(id.into()),
            stable_key: Some(id.into()),
            path: Some("lib/foo.dart".into()),
            start_line: Some(1),
            end_line: Some(2),
            content_hash: None,
            source_file: Some("lib/foo.dart".into()),
            source_hash: None,
            indexer: Some("test".into()),
            index_generation: None,
            metadata_json: None,
        };
        store.upsert_node(&node).unwrap();
    }

    #[test]
    fn inbound_edge_quality_counts_high_medium_low() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        upsert_dart_method(&mut store, "a");
        upsert_dart_method(&mut store, "b");
        upsert_dart_method(&mut store, "c");
        store
            .upsert_edge(&calls_edge("a", "x", "python_lsp"))
            .unwrap();
        store
            .upsert_edge(&calls_edge("b", "x", "python_ast"))
            .unwrap();
        // Use a deprecated lsp edge to materialise a low-tier datapoint;
        // but deprecated edges are excluded entirely. Use GitDiff instead
        // to get a real low-tier survivor.
        let mut low_edge = calls_edge("c", "x", "git");
        low_edge.source = EdgeSource::GitDiff;
        store.upsert_edge(&low_edge).unwrap();

        let s = inbound_edge_quality(&store, "x", EdgeQualityScope::Usage).unwrap();
        assert_eq!(s.high, 1);
        assert_eq!(s.medium, 1);
        assert_eq!(s.low, 1);
    }

    #[test]
    fn outbound_edge_quality_counts_high_medium_low() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        upsert_dart_method(&mut store, "a");
        store
            .upsert_edge(&calls_edge("x", "a", "python_lsp"))
            .unwrap();
        let s = outbound_edge_quality(&store, "x", EdgeQualityScope::SearchRanking).unwrap();
        assert_eq!(s.high, 1);
        assert_eq!(s.medium, 0);
        assert_eq!(s.low, 0);
    }

    #[test]
    fn inbound_edge_quality_excludes_deprecated_edges() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        upsert_dart_method(&mut store, "a");
        let mut e = calls_edge("a", "x", "python_lsp");
        e.status = EdgeStatus::Deprecated;
        store.upsert_edge(&e).unwrap();
        let s = inbound_edge_quality(&store, "x", EdgeQualityScope::Usage).unwrap();
        assert!(s.is_empty(), "deprecated inbound edges must not count");
    }

    #[test]
    fn missing_node_returns_empty() {
        let (store, _dir) = empty_store();
        let s = inbound_edge_quality(&store, "no::such::id", EdgeQualityScope::Usage).unwrap();
        assert!(s.is_empty());
        let s =
            outbound_edge_quality(&store, "no::such::id", EdgeQualityScope::SearchRanking).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn inbound_edge_quality_skips_contains_edges() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        upsert_dart_method(&mut store, "file");
        let e = EdgeAssertion::fact(
            ArtifactId::new("file"),
            ArtifactId::new("x"),
            EdgeKind::Contains,
            EdgeSource::Filesystem,
        );
        store.upsert_edge(&e).unwrap();
        let s = inbound_edge_quality(&store, "x", EdgeQualityScope::Usage).unwrap();
        assert!(
            s.is_empty(),
            "Contains inbound must not register as Usage signal"
        );
    }

    // -----------------------------------------------------------------------
    // neighbors_of
    // -----------------------------------------------------------------------

    #[test]
    fn neighbors_of_dedup_by_other_endpoint() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        upsert_dart_method(&mut store, "a");
        // Two edges to the same endpoint should dedup to one neighbour.
        store
            .upsert_edge(&calls_edge("x", "a", "python_lsp"))
            .unwrap();
        let mut other = EdgeAssertion::fact(
            ArtifactId::new("x"),
            ArtifactId::new("a"),
            EdgeKind::References,
            EdgeSource::LanguageAdapter,
        );
        other.indexer = Some("python_lsp".into());
        store.upsert_edge(&other).unwrap();
        let ns = neighbors_of(&store, "x", 8).unwrap();
        assert_eq!(ns.len(), 1);
        assert_eq!(ns[0].id, "a");
    }

    #[test]
    fn neighbors_of_sorts_alphabetically_and_caps() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        for nbr in ["d", "c", "a", "b", "e"] {
            upsert_dart_method(&mut store, nbr);
            store
                .upsert_edge(&calls_edge("x", nbr, "python_lsp"))
                .unwrap();
        }
        let ns = neighbors_of(&store, "x", 3).unwrap();
        assert_eq!(
            ns.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"],
        );
    }

    #[test]
    fn neighbors_of_excludes_self_loops() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        let mut loop_edge = calls_edge("x", "x", "python_lsp");
        loop_edge.id = ArtifactId::new("edge::self::x");
        store.upsert_edge(&loop_edge).unwrap();
        let ns = neighbors_of(&store, "x", 8).unwrap();
        assert!(ns.is_empty());
    }

    #[test]
    fn neighbors_of_skips_dangling_neighbours_without_node_row() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        // Edge endpoint exists only as an edge — no `nodes` row.
        store
            .upsert_edge(&calls_edge("x", "ghost", "python_lsp"))
            .unwrap();
        let ns = neighbors_of(&store, "x", 8).unwrap();
        assert!(
            ns.is_empty(),
            "dangling neighbours must be dropped (no node row to resolve)"
        );
    }

    #[test]
    fn neighbors_of_with_zero_cap_returns_empty() {
        let (mut store, _dir) = empty_store();
        upsert_dart_method(&mut store, "x");
        upsert_dart_method(&mut store, "a");
        store
            .upsert_edge(&calls_edge("x", "a", "python_lsp"))
            .unwrap();
        let ns = neighbors_of(&store, "x", 0).unwrap();
        assert!(ns.is_empty());
    }
}
