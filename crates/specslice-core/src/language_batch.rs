//! Language adapter output contracts.
//!
//! Adapters never write SQLite directly: they produce a `LanguageIndexBatch`
//! that the engine merges into the store.

use serde::{Deserialize, Serialize};

use crate::artifact_id::ArtifactId;
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

/// A `@implements` / `@verifies` / `@related` annotation found in a doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceTag {
    Implements,
    Verifies,
    Related,
}

impl TraceTag {
    pub fn as_str(self) -> &'static str {
        match self {
            TraceTag::Implements => "implements",
            TraceTag::Verifies => "verifies",
            TraceTag::Related => "related",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredTrace {
    pub from_symbol_id: ArtifactId,
    pub tag: TraceTag,
    pub target: String,
    pub start_line: u32,
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
    pub trace_links: Vec<DeclaredTrace>,
    pub symbol_ranges: Vec<SymbolRange>,
    pub diagnostics: Vec<AdapterDiagnostic>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_tag_strings() {
        assert_eq!(TraceTag::Implements.as_str(), "implements");
        assert_eq!(TraceTag::Verifies.as_str(), "verifies");
        assert_eq!(TraceTag::Related.as_str(), "related");
    }

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
