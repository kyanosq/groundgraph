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
pub mod dead_code;
pub mod docs_indexer;
pub mod export;
pub mod git_diff;
pub mod go_indexer;
pub mod graph;
pub mod impact;
pub mod index;
pub mod init;
pub mod links_indexer;
pub mod logic_confidence;
pub mod lsp_client;
pub mod lsp_indexer;
pub mod python_ast;
pub mod python_frameworks;
pub mod python_indexer;
pub mod search;
pub mod similarity;
pub mod slice;
pub mod swift_indexer;

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
pub use dead_code::{
    analyze_dead_code, analyze_dead_code_with_store, DeadCodeCandidate, DeadCodeConfidence,
    DeadCodeOptions, DeadCodeReport, DeadCodeStats, DEAD_CODE_SCHEMA_VERSION,
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
pub use search::{
    default_search_kinds, run_search, run_search_with_store, tokenise_code, tokenise_keywords,
    SearchEdge, SearchMatch, SearchNode, SearchOptions, SearchQuery, SearchResult, SearchSubgraph,
    DEFAULT_DEPTH as SEARCH_DEFAULT_DEPTH, DEFAULT_LIMIT as SEARCH_DEFAULT_LIMIT,
    EXPANSION_EDGE_KINDS as SEARCH_EXPANSION_EDGE_KINDS,
};
pub use similarity::{
    analyze_similarity, analyze_similarity_with_store, SimilarityCluster, SimilarityMember,
    SimilarityOptions, SimilarityReport, SimilarityStats, SIMILARITY_SCHEMA_VERSION,
};
pub use slice::{slice_requirement, FeatureSlice, SliceItem, SliceOptions};

pub use go_indexer::{
    build_go_batch, go_lsp_available, index_go, GoIndexOptions, GoIndexResult, GO_INDEXER_NAME,
    GO_LSP_COMMAND_ENV,
};
pub use python_indexer::{
    index_python, python_lsp_available, PythonIndexOptions, PythonIndexResult,
    PYTHON_AST_INDEXER_NAME, PYTHON_INDEXER_NAME, PYTHON_LSP_COMMAND_ENV,
};
pub use swift_indexer::{
    build_swift_batch, index_swift, swift_lsp_available, SwiftIndexOptions, SwiftIndexResult,
    SWIFT_INDEXER_NAME, SWIFT_LSP_COMMAND_ENV,
};
