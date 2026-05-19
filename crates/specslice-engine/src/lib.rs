//! SpecSlice engine.
//!
//! MVP-0 surface area:
//! - [`EngineConfig`] — workspace-level config persisted to `.specslice.yaml`.
//! - [`init_repository`] — generate the config file and graph database.

pub mod config;
pub mod dart_indexer;
pub mod docs_indexer;
pub mod export;
pub mod git_diff;
pub mod impact;
pub mod index;
pub mod init;
pub mod slice;

pub use config::EngineConfig;
pub use docs_indexer::{
    DocsIndexOptions, DocsIndexResult, UnresolvedKind, UnresolvedReference, DOCS_INDEXER_NAME,
};
pub use export::{export, ExportFormat, ExportOptions, ExportOutcome};
pub use impact::{run_impact, ImpactOptions, ImpactReport};
pub use index::{index_repository, IndexOptions, IndexResult};
pub use init::{init_repository, InitOptions, InitOutcome};
pub use slice::{slice_requirement, FeatureSlice, SliceItem, SliceOptions};
