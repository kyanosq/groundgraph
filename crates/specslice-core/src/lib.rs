//! SpecSlice core types.
//!
//! Defines the shared vocabulary used by every other crate: artifact IDs,
//! node kinds, edge assertions, evidence, language adapter batches.

pub mod artifact_id;
pub mod edge;
pub mod evidence;
pub mod language_batch;
pub mod node;

pub use artifact_id::ArtifactId;
pub use edge::{EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus};
pub use evidence::{Evidence, EvidenceKind};
pub use language_batch::{
    AdapterDiagnostic, FileArtifact, ImportEdge, LanguageIndexBatch, SymbolArtifact, SymbolRange,
    TestArtifact,
};
pub use node::{Node, NodeKind};
