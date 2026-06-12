//! Edge assertions stored in the `edge_assertions` table.

use serde::{Deserialize, Serialize};

use crate::artifact_id::ArtifactId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    Imports,
    Documents,
    DeclaresImplementation,
    DeclaresVerification,
    /// Body-level reference from a method/function/constructor to a class or
    /// other symbol. Emitted by the lightweight Dart adapter when an
    /// identifier in a method body matches a known symbol's name.
    References,
    /// Same shape as [`EdgeKind::References`] but specifically for callable
    /// targets (identifier followed by `(`). Lets the UI distinguish "uses
    /// constant" from "invokes method" without inspecting evidence.
    Calls,
    // ---- P8 framework-aware semantic edges --------------------------------
    /// `ref.read(X)`, `ref.watch(X)`, or `ref.listen(X)` on a Riverpod
    /// `Ref` / `WidgetRef` resolves to a provider top-level variable.
    /// Emitted from analyzer-resolved AST only.
    ReadsProvider,
    /// `context.push("/route")`, `context.go(...)`, `Navigator.pushNamed(...)`
    /// and friends. Target is a synthetic `route::<path>` node.
    NavigatesTo,
    /// `Hive.box(name).put/get/delete(...)` and `SharedPreferences`-style
    /// calls. Target is a synthetic `storage::<backend>::<bucket>` node.
    PersistsTo,
    /// `stream.listen(callback)` on anything whose static type implements
    /// `Stream<T>`. Target is the producer of the stream (the symbol or
    /// member that returned it), when resolvable.
    SubscribesStream,
    /// P9 evidence edge: an AI-authored business candidate cites this
    /// concrete code fact as its supporting evidence. Always travels from
    /// a `business_candidate::*` node to a Fact-layer edge target.
    DerivesFrom,
}

impl EdgeKind {
    /// Every variant, in declaration order. Single source of truth for
    /// `from_str` — decoders never re-implement the text→kind map.
    pub const ALL: &'static [EdgeKind] = &[
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

    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Contains => "contains",
            EdgeKind::Imports => "imports",
            EdgeKind::Documents => "documents",
            EdgeKind::DeclaresImplementation => "declares_implementation",
            EdgeKind::DeclaresVerification => "declares_verification",
            EdgeKind::References => "references",
            EdgeKind::Calls => "calls",
            EdgeKind::ReadsProvider => "reads_provider",
            EdgeKind::NavigatesTo => "navigates_to",
            EdgeKind::PersistsTo => "persists_to",
            EdgeKind::SubscribesStream => "subscribes_stream",
            EdgeKind::DerivesFrom => "derives_from",
        }
    }

    /// Inverse of [`EdgeKind::as_str`]; `None` for unknown strings.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<EdgeKind> {
        EdgeKind::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeSource {
    Filesystem,
    LanguageAdapter,
    Markdown,
    ExternalManifest,
    GitDiff,
}

impl EdgeSource {
    pub const ALL: &'static [EdgeSource] = &[
        EdgeSource::Filesystem,
        EdgeSource::LanguageAdapter,
        EdgeSource::Markdown,
        EdgeSource::ExternalManifest,
        EdgeSource::GitDiff,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EdgeSource::Filesystem => "filesystem",
            EdgeSource::LanguageAdapter => "language_adapter",
            EdgeSource::Markdown => "markdown",
            EdgeSource::ExternalManifest => "external_manifest",
            EdgeSource::GitDiff => "git_diff",
        }
    }

    /// Inverse of [`EdgeSource::as_str`]; `None` for unknown strings.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<EdgeSource> {
        EdgeSource::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeCertainty {
    Fact,
    Declared,
}

impl EdgeCertainty {
    pub const ALL: &'static [EdgeCertainty] = &[EdgeCertainty::Fact, EdgeCertainty::Declared];

    pub fn as_str(self) -> &'static str {
        match self {
            EdgeCertainty::Fact => "fact",
            EdgeCertainty::Declared => "declared",
        }
    }

    /// Inverse of [`EdgeCertainty::as_str`]; `None` for unknown strings.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<EdgeCertainty> {
        EdgeCertainty::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeStatus {
    Confirmed,
    Deprecated,
}

impl EdgeStatus {
    pub const ALL: &'static [EdgeStatus] = &[EdgeStatus::Confirmed, EdgeStatus::Deprecated];

    pub fn as_str(self) -> &'static str {
        match self {
            EdgeStatus::Confirmed => "confirmed",
            EdgeStatus::Deprecated => "deprecated",
        }
    }

    /// Inverse of [`EdgeStatus::as_str`]; `None` for unknown strings.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<EdgeStatus> {
        EdgeStatus::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

/// Row in the `edge_assertions` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeAssertion {
    pub id: ArtifactId,
    pub from_id: ArtifactId,
    pub to_id: ArtifactId,
    pub kind: EdgeKind,
    pub source: EdgeSource,
    pub certainty: EdgeCertainty,
    pub status: EdgeStatus,
    pub confidence: f32,
    pub evidence_json: Option<String>,
    pub source_file: Option<String>,
    pub source_hash: Option<String>,
    pub indexer: Option<String>,
    pub index_generation: Option<i64>,
    pub metadata_json: Option<String>,
}

impl EdgeAssertion {
    /// Build a `Declared / Confirmed` edge with confidence 1.0.
    pub fn declared(from: ArtifactId, to: ArtifactId, kind: EdgeKind, source: EdgeSource) -> Self {
        let id = ArtifactId::new(format!("edge::{}::{}::{}", kind.as_str(), from, to));
        Self {
            id,
            from_id: from,
            to_id: to,
            kind,
            source,
            certainty: EdgeCertainty::Declared,
            status: EdgeStatus::Confirmed,
            confidence: 1.0,
            evidence_json: None,
            source_file: None,
            source_hash: None,
            indexer: None,
            index_generation: None,
            metadata_json: None,
        }
    }

    /// Build a `Fact / Confirmed` edge with confidence 1.0 — for structural
    /// edges such as `contains` and `imports`.
    pub fn fact(from: ArtifactId, to: ArtifactId, kind: EdgeKind, source: EdgeSource) -> Self {
        let mut edge = Self::declared(from, to, kind, source);
        edge.certainty = EdgeCertainty::Fact;
        edge
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_strings_are_stable() {
        assert_eq!(EdgeKind::Contains.as_str(), "contains");
        assert_eq!(EdgeKind::Imports.as_str(), "imports");
        assert_eq!(EdgeKind::Documents.as_str(), "documents");
        assert_eq!(
            EdgeKind::DeclaresImplementation.as_str(),
            "declares_implementation"
        );
        assert_eq!(
            EdgeKind::DeclaresVerification.as_str(),
            "declares_verification"
        );
        assert_eq!(EdgeKind::References.as_str(), "references");
        assert_eq!(EdgeKind::Calls.as_str(), "calls");
        assert_eq!(EdgeKind::ReadsProvider.as_str(), "reads_provider");
        assert_eq!(EdgeKind::NavigatesTo.as_str(), "navigates_to");
        assert_eq!(EdgeKind::PersistsTo.as_str(), "persists_to");
        assert_eq!(EdgeKind::SubscribesStream.as_str(), "subscribes_stream");
        assert_eq!(EdgeKind::DerivesFrom.as_str(), "derives_from");

        assert_eq!(EdgeSource::Filesystem.as_str(), "filesystem");
        assert_eq!(EdgeSource::LanguageAdapter.as_str(), "language_adapter");
        assert_eq!(EdgeSource::Markdown.as_str(), "markdown");
        assert_eq!(EdgeSource::ExternalManifest.as_str(), "external_manifest");
        assert_eq!(EdgeSource::GitDiff.as_str(), "git_diff");

        assert_eq!(EdgeCertainty::Fact.as_str(), "fact");
        assert_eq!(EdgeCertainty::Declared.as_str(), "declared");

        assert_eq!(EdgeStatus::Confirmed.as_str(), "confirmed");
        assert_eq!(EdgeStatus::Deprecated.as_str(), "deprecated");
    }

    #[test]
    fn edge_enums_from_str_round_trip_all_variants() {
        for kind in EdgeKind::ALL {
            assert_eq!(EdgeKind::from_str(kind.as_str()), Some(*kind));
        }
        for source in EdgeSource::ALL {
            assert_eq!(EdgeSource::from_str(source.as_str()), Some(*source));
        }
        for certainty in EdgeCertainty::ALL {
            assert_eq!(
                EdgeCertainty::from_str(certainty.as_str()),
                Some(*certainty)
            );
        }
        for status in EdgeStatus::ALL {
            assert_eq!(EdgeStatus::from_str(status.as_str()), Some(*status));
        }
        assert_eq!(EdgeKind::from_str("nope"), None);
        assert_eq!(EdgeSource::from_str(""), None);
    }

    #[test]
    fn declared_edge_id_is_deterministic() {
        let from = ArtifactId::new("dart_class::a.dart#Foo");
        let to = ArtifactId::new("req::REQ-1");
        let edge = EdgeAssertion::declared(
            from.clone(),
            to.clone(),
            EdgeKind::DeclaresImplementation,
            EdgeSource::ExternalManifest,
        );
        assert_eq!(edge.certainty, EdgeCertainty::Declared);
        assert_eq!(edge.status, EdgeStatus::Confirmed);
        assert_eq!(edge.confidence, 1.0);
        let again = EdgeAssertion::declared(
            from,
            to,
            EdgeKind::DeclaresImplementation,
            EdgeSource::ExternalManifest,
        );
        assert_eq!(edge.id, again.id);
    }

    #[test]
    fn fact_edge_changes_certainty_only() {
        let from = ArtifactId::new("a");
        let to = ArtifactId::new("b");
        let edge = EdgeAssertion::fact(
            from.clone(),
            to.clone(),
            EdgeKind::Contains,
            EdgeSource::Filesystem,
        );
        assert_eq!(edge.certainty, EdgeCertainty::Fact);
        assert_eq!(edge.status, EdgeStatus::Confirmed);
        assert_eq!(edge.kind, EdgeKind::Contains);
    }

    #[test]
    fn edge_round_trips_through_json() {
        let edge = EdgeAssertion::declared(
            ArtifactId::new("a"),
            ArtifactId::new("b"),
            EdgeKind::Documents,
            EdgeSource::Markdown,
        );
        let json = serde_json::to_string(&edge).unwrap();
        let back: EdgeAssertion = serde_json::from_str(&json).unwrap();
        assert_eq!(back, edge);
    }
}
