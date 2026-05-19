//! Artifact nodes stored in the `nodes` table.

use serde::{Deserialize, Serialize};

use crate::artifact_id::ArtifactId;

/// All node kinds known to MVP-0..MVP-5. New kinds must append to keep
/// stable string serialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Requirement,
    AcceptanceCriterion,
    Adr,
    DocSection,
    DartClass,
    DartMethod,
    DartFunction,
    DartConstructor,
    TestCase,
    TestGroup,
    // ---- P8 framework-aware synthetic targets -----------------------------
    /// Top-level Riverpod provider variable (e.g. `final proProvider =
    /// StateNotifierProvider(...)`). The Dart analyzer sidecar adds these
    /// during Pass 1 so `reads_provider` edges have something to point at.
    DartProvider,
    /// Synthetic node for a navigation destination identified only by
    /// its route string — `route::/paywall`, `route::/editor`. Created
    /// on demand by the sidecar when it sees `context.push("/foo")`.
    Route,
    /// Synthetic node for a persistence target identified by storage
    /// backend + bucket (`storage::hive::pro_entitlement`,
    /// `storage::shared_prefs::onboarding_done`). Created on demand by the
    /// sidecar.
    Storage,
    // ---- P9 AI-authored business candidates -------------------------------
    /// A business-logic candidate produced by the AI layer (P9) — a
    /// human-readable description of a flow, gated by `status` and
    /// `confidence`. Always lives in `GraphLayer::Candidate` until a
    /// human confirms it.
    BusinessCandidate,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Requirement => "requirement",
            NodeKind::AcceptanceCriterion => "acceptance_criterion",
            NodeKind::Adr => "adr",
            NodeKind::DocSection => "doc_section",
            NodeKind::DartClass => "dart_class",
            NodeKind::DartMethod => "dart_method",
            NodeKind::DartFunction => "dart_function",
            NodeKind::DartConstructor => "dart_constructor",
            NodeKind::TestCase => "test_case",
            NodeKind::TestGroup => "test_group",
            NodeKind::DartProvider => "dart_provider",
            NodeKind::Route => "route",
            NodeKind::Storage => "storage",
            NodeKind::BusinessCandidate => "business_candidate",
        }
    }
}

/// In-memory representation of a row in the `nodes` SQLite table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: ArtifactId,
    pub kind: NodeKind,
    pub path: Option<String>,
    pub name: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub content_hash: Option<String>,
    pub stable_key: Option<String>,
    pub source_file: Option<String>,
    pub source_hash: Option<String>,
    pub indexer: Option<String>,
    pub index_generation: Option<i64>,
    pub metadata_json: Option<String>,
}

impl Node {
    pub fn new(id: ArtifactId, kind: NodeKind) -> Self {
        Self {
            id,
            kind,
            path: None,
            name: None,
            start_line: None,
            end_line: None,
            content_hash: None,
            stable_key: None,
            source_file: None,
            source_hash: None,
            indexer: None,
            index_generation: None,
            metadata_json: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_str_round_trip() {
        for kind in [
            NodeKind::File,
            NodeKind::Requirement,
            NodeKind::AcceptanceCriterion,
            NodeKind::Adr,
            NodeKind::DocSection,
            NodeKind::DartClass,
            NodeKind::DartMethod,
            NodeKind::DartFunction,
            NodeKind::DartConstructor,
            NodeKind::TestCase,
            NodeKind::TestGroup,
            NodeKind::DartProvider,
            NodeKind::Route,
            NodeKind::Storage,
            NodeKind::BusinessCandidate,
        ] {
            assert!(!kind.as_str().is_empty());
        }
    }

    #[test]
    fn node_new_sets_defaults_and_serialises() {
        let node = Node::new(ArtifactId::new("a"), NodeKind::Requirement);
        let json = serde_json::to_string(&node).expect("serialise");
        assert!(json.contains("\"kind\":\"requirement\""));
        let back: Node = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, node);
    }
}
