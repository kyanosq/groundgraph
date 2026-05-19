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
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Contains => "contains",
            EdgeKind::Imports => "imports",
            EdgeKind::Documents => "documents",
            EdgeKind::DeclaresImplementation => "declares_implementation",
            EdgeKind::DeclaresVerification => "declares_verification",
            EdgeKind::References => "references",
            EdgeKind::Calls => "calls",
        }
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
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeSource::Filesystem => "filesystem",
            EdgeSource::LanguageAdapter => "language_adapter",
            EdgeSource::Markdown => "markdown",
            EdgeSource::ExternalManifest => "external_manifest",
            EdgeSource::GitDiff => "git_diff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeCertainty {
    Fact,
    Declared,
}

impl EdgeCertainty {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeCertainty::Fact => "fact",
            EdgeCertainty::Declared => "declared",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeStatus {
    Confirmed,
    Deprecated,
}

impl EdgeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeStatus::Confirmed => "confirmed",
            EdgeStatus::Deprecated => "deprecated",
        }
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
