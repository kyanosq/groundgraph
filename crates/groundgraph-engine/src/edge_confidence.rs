//! P19 base — derive an AI-readable confidence tier for every edge.
//!
//! GroundGraph already stores rich provenance on each edge:
//! `(EdgeKind, EdgeSource, EdgeCertainty, indexer)`. Consumers
//! (humans, AI agents, MCP clients) shouldn't have to parse that
//! tuple every time. This module collapses it into a three-tier
//! label — `high` / `medium` / `low` — that downstream tools can
//! filter on directly.
//!
//! ### Tier definitions
//!
//! - **high** — facts that a deterministic parser can verify:
//!   - `Contains` / `Imports` / `Documents` edges produced by
//!     language adapters that resolve via the file system or the
//!     full source AST (`*_lsp`, `dart_analyzer`, `*_ast` for
//!     imports — Python imports are structural even when
//!     bottlenecked by dynamic call sites).
//!   - `Calls` / `References` / `ReadsProvider` / `NavigatesTo` /
//!     `PersistsTo` / `SubscribesStream` produced by an LSP server
//!     (`*_lsp` indexer name), the Dart analyzer sidecar
//!     (`dart_analyzer`), or offline SCIP ingestion (`scip` /
//!     `scip:<lang>`), since those resolve symbol bindings
//!     using compiler-grade type information.
//!   - Markdown `Documents`, `DeclaresImplementation` /
//!     `DeclaresVerification` from explicit manifests — user
//!     intent stated in source-of-truth files.
//!
//! - **medium** — facts that a static AST can establish but that
//!   cannot survive monkey-patching, dependency injection, or
//!   reflection:
//!   - `Calls` / `References` produced by `*_ast` indexers
//!     (Python AST today; future TS/JS/Swift AST passes).
//!   - Anything else with `EdgeCertainty::Fact` we don't have a
//!     stronger rule for.
//!
//! - **low** — facts that need human review:
//!   - `DerivesFrom` edges produced by the AI business-candidate
//!     pipeline.
//!   - GitDiff-sourced edges (provisional, may be stale).
//!   - Anything with `EdgeStatus::Deprecated`.
//!
//! The mapping is intentionally conservative: when in doubt we
//! return `Medium` rather than `High`. New edge kinds added later
//! default to `Medium` until someone makes a deliberate decision.

use serde::{Deserialize, Serialize};

use groundgraph_core::edge::{EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus};

/// Three-tier label exported next to the numeric `confidence`
/// field. Stable string form is `"high" | "medium" | "low"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeConfidence {
    High,
    Medium,
    Low,
}

impl EdgeConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeConfidence::High => "high",
            EdgeConfidence::Medium => "medium",
            EdgeConfidence::Low => "low",
        }
    }

    /// Numeric rank usable for sorting (`high` first).
    pub fn rank(self) -> u8 {
        match self {
            EdgeConfidence::High => 2,
            EdgeConfidence::Medium => 1,
            EdgeConfidence::Low => 0,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "high" => Some(EdgeConfidence::High),
            "medium" => Some(EdgeConfidence::Medium),
            "low" => Some(EdgeConfidence::Low),
            _ => None,
        }
    }
}

/// Convenience wrapper around [`derive_confidence`] that reads
/// the four interesting fields directly off an [`EdgeAssertion`].
pub fn confidence_for_edge(edge: &EdgeAssertion) -> EdgeConfidence {
    derive_confidence(
        edge.kind,
        edge.source,
        edge.certainty,
        edge.status,
        edge.indexer.as_deref(),
    )
}

/// Core derivation rule. Pure function over the four provenance
/// fields plus the `indexer` name; everything else lives in tests
/// so future changes remain auditable.
pub fn derive_confidence(
    kind: EdgeKind,
    source: EdgeSource,
    certainty: EdgeCertainty,
    status: EdgeStatus,
    indexer: Option<&str>,
) -> EdgeConfidence {
    // Deprecated edges always degrade. No matter how strong the
    // original signal was, the graph is telling us it's stale.
    if matches!(status, EdgeStatus::Deprecated) {
        return EdgeConfidence::Low;
    }

    use EdgeKind::*;
    use EdgeSource::*;

    // AI-generated business evidence is always a low-tier candidate.
    if matches!(kind, DerivesFrom) {
        return EdgeConfidence::Low;
    }

    // GitDiff edges are provisional — they might evaporate the
    // next time the index runs.
    if matches!(source, GitDiff) {
        return EdgeConfidence::Low;
    }

    // Structural / file-layout edges are always high-confidence
    // regardless of which adapter emitted them: filesystem walks
    // and language ASTs both name the parent-of relationship
    // exactly, never heuristically.
    if matches!(kind, Contains) {
        return EdgeConfidence::High;
    }
    if matches!((kind, source), (Documents, Markdown)) {
        return EdgeConfidence::High;
    }

    // User-declared edges (manifest links) are intent statements —
    // always high so long as the manifest still references them.
    if matches!(
        (kind, source),
        (
            DeclaresImplementation | DeclaresVerification,
            ExternalManifest
        )
    ) {
        return EdgeConfidence::High;
    }

    // Imports are structural even in dynamic languages: the AST
    // pass can resolve `from foo import bar` to a real file path.
    // LSP / analyzer are obviously high; the AST-only path is also
    // high because import resolution is purely lexical.
    if matches!(kind, Imports) {
        return EdgeConfidence::High;
    }

    // Semantic resolved edges (calls / references / framework
    // semantic edges) — depend on the indexer that produced them.
    if matches!(
        kind,
        Calls | References | ReadsProvider | NavigatesTo | PersistsTo | SubscribesStream
    ) {
        if matches!(certainty, EdgeCertainty::Declared) {
            // Declared manifest entry for a semantic edge — rare,
            // but treat as high (user intent).
            return EdgeConfidence::High;
        }
        return match indexer {
            Some(i) if i.ends_with("_lsp") => EdgeConfidence::High,
            Some("dart_analyzer") => EdgeConfidence::High,
            // SCIP ingestion (`scip`, `scip:rust`, …): the edge was resolved
            // offline by a language's real compiler frontend (rust-analyzer /
            // scip-go / scip-typescript / scip-java / scip-python), so it is
            // compiler-grade — at least as trustworthy as a live LSP.
            Some(i) if i == "scip" || i.starts_with("scip:") => EdgeConfidence::High,
            Some(i) if i.ends_with("_ast") => EdgeConfidence::Medium,
            _ => EdgeConfidence::Medium,
        };
    }

    // Fallback: when we don't recognise the combination, default
    // to medium rather than over-promising.
    EdgeConfidence::Medium
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derive(kind: EdgeKind, source: EdgeSource, indexer: Option<&'static str>) -> EdgeConfidence {
        derive_confidence(
            kind,
            source,
            EdgeCertainty::Fact,
            EdgeStatus::Confirmed,
            indexer,
        )
    }

    #[test]
    fn lsp_resolved_calls_are_high_confidence() {
        assert_eq!(
            derive(
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
                Some("python_lsp")
            ),
            EdgeConfidence::High
        );
        assert_eq!(
            derive(
                EdgeKind::References,
                EdgeSource::LanguageAdapter,
                Some("go_lsp")
            ),
            EdgeConfidence::High
        );
        assert_eq!(
            derive(
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
                Some("dart_analyzer")
            ),
            EdgeConfidence::High
        );
    }

    #[test]
    fn scip_resolved_calls_are_high_confidence() {
        // SCIP indexers run a language's real compiler frontend offline, so a
        // `Calls`/`References` edge they produce is as trustworthy as an LSP's
        // — strictly stronger than the in-process heuristic resolver.
        assert_eq!(
            derive(EdgeKind::Calls, EdgeSource::LanguageAdapter, Some("scip")),
            EdgeConfidence::High
        );
        assert_eq!(
            derive(
                EdgeKind::References,
                EdgeSource::LanguageAdapter,
                Some("scip")
            ),
            EdgeConfidence::High
        );
        // Per-language SCIP tags (`scip:rust`, `scip:go`, …) are also high.
        assert_eq!(
            derive(
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
                Some("scip:rust")
            ),
            EdgeConfidence::High
        );
    }

    #[test]
    fn ast_resolved_calls_are_medium_confidence() {
        assert_eq!(
            derive(
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
                Some("python_ast")
            ),
            EdgeConfidence::Medium
        );
        assert_eq!(
            derive(
                EdgeKind::References,
                EdgeSource::LanguageAdapter,
                Some("python_ast")
            ),
            EdgeConfidence::Medium
        );
    }

    #[test]
    fn imports_are_always_high_regardless_of_indexer() {
        for indexer in [Some("python_ast"), Some("python_lsp"), None] {
            assert_eq!(
                derive(EdgeKind::Imports, EdgeSource::LanguageAdapter, indexer),
                EdgeConfidence::High,
                "imports must be high for indexer = {indexer:?}",
            );
        }
    }

    #[test]
    fn ai_business_candidate_evidence_is_always_low() {
        for src in [
            EdgeSource::Filesystem,
            EdgeSource::LanguageAdapter,
            EdgeSource::Markdown,
            EdgeSource::ExternalManifest,
        ] {
            assert_eq!(
                derive(EdgeKind::DerivesFrom, src, None),
                EdgeConfidence::Low,
            );
        }
    }

    #[test]
    fn deprecated_edges_collapse_to_low_even_if_signal_was_strong() {
        let c = derive_confidence(
            EdgeKind::Calls,
            EdgeSource::LanguageAdapter,
            EdgeCertainty::Fact,
            EdgeStatus::Deprecated,
            Some("python_lsp"),
        );
        assert_eq!(c, EdgeConfidence::Low);
    }

    #[test]
    fn git_diff_source_is_provisional_low() {
        assert_eq!(
            derive(EdgeKind::References, EdgeSource::GitDiff, Some("git")),
            EdgeConfidence::Low
        );
    }

    #[test]
    fn manifest_declarations_are_high() {
        assert_eq!(
            derive(
                EdgeKind::DeclaresImplementation,
                EdgeSource::ExternalManifest,
                None
            ),
            EdgeConfidence::High
        );
        assert_eq!(
            derive(
                EdgeKind::DeclaresVerification,
                EdgeSource::ExternalManifest,
                None
            ),
            EdgeConfidence::High
        );
    }

    #[test]
    fn markdown_documents_edge_is_high() {
        assert_eq!(
            derive(EdgeKind::Documents, EdgeSource::Markdown, Some("links")),
            EdgeConfidence::High
        );
    }

    #[test]
    fn contains_edge_is_high_regardless_of_source() {
        for src in [
            EdgeSource::Filesystem,
            EdgeSource::LanguageAdapter,
            EdgeSource::Markdown,
        ] {
            assert_eq!(
                derive(EdgeKind::Contains, src, None),
                EdgeConfidence::High,
                "contains must be high regardless of source = {src:?}",
            );
        }
    }

    #[test]
    fn rank_orders_high_above_medium_above_low() {
        assert!(EdgeConfidence::High.rank() > EdgeConfidence::Medium.rank());
        assert!(EdgeConfidence::Medium.rank() > EdgeConfidence::Low.rank());
    }

    #[test]
    fn parse_round_trips_strings() {
        for c in [
            EdgeConfidence::High,
            EdgeConfidence::Medium,
            EdgeConfidence::Low,
        ] {
            assert_eq!(EdgeConfidence::parse(c.as_str()), Some(c));
        }
        assert_eq!(EdgeConfidence::parse("nope"), None);
    }

    #[test]
    fn confidence_for_edge_wraps_derive_correctly() {
        let mut edge = EdgeAssertion::fact(
            groundgraph_core::ArtifactId::new("a"),
            groundgraph_core::ArtifactId::new("b"),
            EdgeKind::Calls,
            EdgeSource::LanguageAdapter,
        );
        edge.indexer = Some("python_lsp".into());
        assert_eq!(confidence_for_edge(&edge), EdgeConfidence::High);
        edge.indexer = Some("python_ast".into());
        assert_eq!(confidence_for_edge(&edge), EdgeConfidence::Medium);
        edge.status = EdgeStatus::Deprecated;
        assert_eq!(confidence_for_edge(&edge), EdgeConfidence::Low);
    }
}
