//! Language adapter output contracts.
//!
//! Adapters never write SQLite directly: they produce a `LanguageIndexBatch`
//! that the engine merges into the store.

use serde::{Deserialize, Serialize};

use crate::artifact_id::ArtifactId;
use crate::edge::EdgeKind;
use crate::node::NodeKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileArtifact {
    pub id: ArtifactId,
    pub path: String,
    pub language: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolArtifact {
    pub id: ArtifactId,
    pub kind: NodeKind,
    pub path: String,
    pub name: String,
    pub qualified_name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub parent_symbol_id: Option<ArtifactId>,
    /// Optional structured metadata produced by a language adapter
    /// (framework facts, confidence, similarity fingerprint, …).
    /// The string is JSON so different adapters can publish their own
    /// schemas without forcing a single struct. `None` keeps the
    /// minimal artifact size for adapters that have nothing to add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestArtifact {
    pub id: ArtifactId,
    pub kind: NodeKind,
    pub path: String,
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub parent_symbol_id: Option<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportEdge {
    pub from_file: ArtifactId,
    pub to_path: String,
}

/// Lightweight body-level reference produced by a language adapter.
///
/// `kind` must be [`EdgeKind::References`] (class / constant) or
/// [`EdgeKind::Calls`] (callable target). The engine maps the edge through
/// `EdgeAssertion::fact` with `EdgeSource::LanguageAdapter`.
///
/// P6.3 — every reference now carries evidence: `source_file`, the line the
/// match was found on, and a trimmed `snippet` of the source line. `resolver`
/// names the analyser that produced the edge (today: `dart_lightweight`;
/// the analyzer sidecar in P7 will emit `dart_analyzer`). Confidence is
/// kept on [`crate::EdgeAssertion`] so the engine can blend heuristic
/// matches with analyzer / AI candidates without changing this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceEdge {
    pub from_symbol_id: ArtifactId,
    pub to_symbol_id: ArtifactId,
    pub kind: EdgeKind,
    #[serde(default)]
    pub source_file: String,
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub resolver: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRange {
    pub file_path: String,
    pub symbol_id: ArtifactId,
    pub start_line: u32,
    pub end_line: u32,
    pub symbol_kind: NodeKind,
    pub qualified_name: String,
    pub parent_symbol_id: Option<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterDiagnostic {
    pub path: String,
    pub message: String,
}

/// P8 synthetic target (route/storage/etc.) that exists only because some
/// piece of code referenced it by string. The adapter promises that the
/// `id` is unique and that `kind` is one of:
/// `route` / `storage` / `dart_provider`.
///
/// `label` is what we show in the graph (e.g. `/paywall` or
/// `hive:pro_entitlement`). It is *not* a file path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticNode {
    pub id: ArtifactId,
    pub kind: NodeKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LanguageIndexBatch {
    pub language: String,
    pub files: Vec<FileArtifact>,
    pub symbols: Vec<SymbolArtifact>,
    pub tests: Vec<TestArtifact>,
    pub imports: Vec<ImportEdge>,
    pub symbol_ranges: Vec<SymbolRange>,
    pub diagnostics: Vec<AdapterDiagnostic>,
    /// Body-level lightweight references (calls / class refs). Optional in
    /// JSON for forward compatibility with older adapters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ReferenceEdge>,
    /// P8 synthetic targets — routes, storage buckets, etc. Optional in
    /// JSON for forward compatibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synthetic_nodes: Vec<SyntheticNode>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batch_round_trips() {
        let batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&batch).unwrap();
        let back: LanguageIndexBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back, batch);
    }
}
