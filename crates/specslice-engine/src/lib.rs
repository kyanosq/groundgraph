//! SpecSlice engine.
//!
//! MVP-0 surface area:
//! - [`EngineConfig`] — workspace-level config persisted to `.specslice.yaml`.
//! - [`init_repository`] — generate the config file and graph database.

pub mod business_candidates;
pub mod checks;
pub mod config;
pub mod connect;
pub mod context_pack;
pub mod dart_indexer;
pub mod dart_sidecar;
pub mod docs_indexer;
pub mod export;
pub mod git_diff;
pub mod graph;
pub mod impact;
pub mod index;
pub mod init;
pub mod links_indexer;
pub mod logic_confidence;
pub mod slice;

pub use business_candidates::{
    apply_review, candidate_artifact_id, list_for_review, load_business_candidates,
    BusinessCandidate, BusinessCandidatesDocument, CandidateListSnapshot, CandidateReview,
    LoadOutcome as BusinessCandidatesLoadOutcome, ReviewApplyOutcome, ReviewStatus, ReviewVerdict,
    BUSINESS_CANDIDATES_REL_PATH, BUSINESS_CANDIDATES_SCHEMA_VERSION,
};

pub use checks::{
    compute_checks, run_checks, CheckFinding, CheckOptions, CheckReport, CheckSeverity,
};
pub use config::EngineConfig;
pub use connect::{
    apply_candidates, propose_evidence, AcceptedCandidate, ApplyOptions, ApplyOutcome,
    CandidatesDocument, ClarifyingQuestion, EvidenceDocSection, EvidencePack, EvidenceRequirement,
    EvidenceSymbol, EvidenceTest, LinkCandidate, RejectedCandidate,
};
pub use context_pack::{
    build_context, CodeSnippet, ContextOptions, ContextPack, DocSnippet, EdgeSummary,
};
pub use docs_indexer::{DocsIndexOptions, DocsIndexResult, DOCS_INDEXER_NAME};
pub use export::{export, ExportFormat, ExportOptions, ExportOutcome};
pub use graph::{
    build_graph_view, GraphColumn, GraphEdge, GraphFinding, GraphLayer, GraphNode, GraphOptions,
    GraphStats, GraphStatus, GraphView, GraphViewModel, GRAPH_SCHEMA_VERSION,
};
pub use impact::{
    compute_impact, compute_impact_with_policy, merge_confirmed_candidates, run_impact,
    ImpactOptions, ImpactReport,
};
pub use index::{index_repository, IndexOptions, IndexResult};
pub use init::{init_repository, InitOptions, InitOutcome};
pub use links_indexer::{index_links, LinksIndexOptions, LinksIndexResult, LINKS_INDEXER_NAME};
pub use logic_confidence::{
    compute_logic_confidence, run_logic_confidence, LogicConfidenceItem, LogicConfidenceKind,
    LogicConfidenceOptions, LogicConfidenceReport, LogicConfidenceSource, LogicConfidenceSummary,
};
pub use slice::{slice_requirement, FeatureSlice, SliceItem, SliceOptions};
