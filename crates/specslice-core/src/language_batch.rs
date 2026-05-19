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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceEdge {
    pub from_symbol_id: ArtifactId,
    pub to_symbol_id: ArtifactId,
    pub kind: EdgeKind,
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
