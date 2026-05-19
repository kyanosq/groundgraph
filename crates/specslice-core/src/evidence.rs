//! Evidence rows attached to nodes and edges.

use serde::{Deserialize, Serialize};

use crate::artifact_id::ArtifactId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    DocSection,
    DartDocComment,
    DartTestCall,
    DartGroupCall,
    Import,
    GitDiff,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::DocSection => "doc_section",
            EvidenceKind::DartDocComment => "dart_doc_comment",
            EvidenceKind::DartTestCall => "dart_test_call",
            EvidenceKind::DartGroupCall => "dart_group_call",
            EvidenceKind::Import => "import",
            EvidenceKind::GitDiff => "git_diff",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: ArtifactId,
    pub artifact_id: ArtifactId,
    pub kind: EvidenceKind,
    pub path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub snippet: Option<String>,
    pub hash: Option<String>,
    pub metadata_json: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_kind_strings() {
        assert_eq!(EvidenceKind::DocSection.as_str(), "doc_section");
        assert_eq!(EvidenceKind::DartDocComment.as_str(), "dart_doc_comment");
        assert_eq!(EvidenceKind::DartTestCall.as_str(), "dart_test_call");
        assert_eq!(EvidenceKind::DartGroupCall.as_str(), "dart_group_call");
        assert_eq!(EvidenceKind::Import.as_str(), "import");
        assert_eq!(EvidenceKind::GitDiff.as_str(), "git_diff");
    }

    #[test]
    fn evidence_round_trips_through_json() {
        let ev = Evidence {
            id: ArtifactId::new("ev::1"),
            artifact_id: ArtifactId::new("a"),
            kind: EvidenceKind::DocSection,
            path: Some("docs/a.md".into()),
            start_line: Some(1),
            end_line: Some(10),
            snippet: Some("# Heading".into()),
            hash: None,
            metadata_json: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: Evidence = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }
}
