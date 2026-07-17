//! GroundGraph core types.
//!
//! Defines the shared vocabulary used by every other crate: artifact IDs,
//! node kinds, edge assertions, evidence, language adapter batches.

pub mod artifact_id;
pub mod confidence;
pub mod edge;
pub mod evidence;
pub mod language_batch;
pub mod language_traits;
pub mod node;
pub mod paths;

pub use artifact_id::ArtifactId;
pub use confidence::{Confidence, InvalidConfidence};
pub use edge::{
    sanitize_confidence, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus,
};
pub use evidence::{Evidence, EvidenceKind};
pub use language_batch::{
    AdapterDiagnostic, FileArtifact, ImportEdge, LanguageIndexBatch, ReferenceEdge, SymbolArtifact,
    SymbolRange, TestArtifact,
};
pub use language_traits::{
    default_dead_code_reason, family_of, is_callable, is_code_symbol, is_module_or_file, is_test,
    is_type, language_of, search_aliases, similarity_supported, Language, SymbolFamily,
};
pub use node::{LineRangeError, Node, NodeKind};
pub use paths::{confine_under_root, PathEscapeError};
