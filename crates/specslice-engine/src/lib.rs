//! SpecSlice engine.
//!
//! MVP-0 surface area:
//! - [`EngineConfig`] — workspace-level config persisted to `.specslice.yaml`.
//! - [`init_repository`] — generate the config file and graph database.

pub mod checks;
pub mod config;
pub mod context_pack;
pub mod dart_indexer;
pub mod docs_indexer;
pub mod export;
pub mod git_diff;
pub mod impact;
pub mod index;
pub mod init;
pub mod links_indexer;
pub mod slice;

pub use checks::{
    compute_checks, run_checks, CheckFinding, CheckOptions, CheckReport, CheckSeverity,
};
pub use config::EngineConfig;
pub use context_pack::{
    build_context, CodeSnippet, ContextOptions, ContextPack, DocSnippet, EdgeSummary,
};
pub use docs_indexer::{DocsIndexOptions, DocsIndexResult, DOCS_INDEXER_NAME};
pub use export::{export, ExportFormat, ExportOptions, ExportOutcome};
pub use impact::{run_impact, ImpactOptions, ImpactReport};
pub use index::{index_repository, IndexOptions, IndexResult};
pub use init::{init_repository, InitOptions, InitOutcome};
pub use links_indexer::{index_links, LinksIndexOptions, LinksIndexResult, LINKS_INDEXER_NAME};
pub use slice::{slice_requirement, FeatureSlice, SliceItem, SliceOptions};
