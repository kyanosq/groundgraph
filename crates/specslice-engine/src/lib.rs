//! SpecSlice engine.
//!
//! MVP-0 surface area:
//! - [`EngineConfig`] — workspace-level config persisted to `.specslice.yaml`.
//! - [`init_repository`] — generate the config file and graph database.

pub mod business_candidates;
pub mod business_doc;
pub mod business_pack;
pub mod c_treesitter;
pub mod checks;
pub mod confidence_view;
pub mod config;
pub mod connect;
pub mod constants;
pub mod context_pack;
pub mod cpp_treesitter;
pub mod dart_indexer;
pub mod dart_sidecar;
pub mod dart_treesitter;
pub mod data_contract;
pub mod dead_code;
pub mod docs_indexer;
pub mod edge_confidence;
pub mod export;
pub mod feature_cluster;
pub mod feature_map;
pub mod feature_pack;
pub mod fts_text;
pub mod fulltext_indexer;
pub mod git_diff;
pub mod go_indexer;
pub mod go_treesitter;
pub mod graph;
pub mod graph_diff;
pub mod graph_equiv;
pub mod impact;
pub mod index;
pub mod csharp_treesitter;
pub mod init;
pub mod java_indexer;
pub mod java_treesitter;
pub mod kotlin_treesitter;
pub mod links_indexer;
pub mod logic_confidence;
pub mod lsp_client;
pub mod lsp_indexer;
pub mod lsp_probe;
pub mod network;
pub mod path_class;
pub mod port_coverage;
pub mod python_frameworks;
pub mod python_indexer;
pub mod python_treesitter;
pub mod php_treesitter;
pub mod questions;
pub mod requirements_md_indexer;
pub mod route_coverage;
pub mod ruby_treesitter;
pub mod rust_indexer;
pub mod rust_treesitter;
pub mod schema_indexer;
pub mod scip_overlay;
pub mod scip_runner;
pub mod search;
pub mod similarity;
pub mod slice;
pub mod source_text;
pub mod stats;
pub mod swift_indexer;
pub mod swift_treesitter;
pub mod symbol_facts;
pub mod test_selection;
pub mod test_suggestions;
pub mod trace;
pub mod treesitter;
pub mod typescript_indexer;
pub mod typescript_treesitter;

pub use business_candidates::{
    apply_review, candidate_artifact_id, list_for_review, load_business_candidates,
    BusinessCandidate, BusinessCandidatesDocument, CandidateListSnapshot, CandidateReview,
    LoadOutcome as BusinessCandidatesLoadOutcome, ReviewApplyOutcome, ReviewStatus, ReviewVerdict,
    BUSINESS_CANDIDATES_REL_PATH, BUSINESS_CANDIDATES_SCHEMA_VERSION,
};

pub use business_doc::{
    build_business_doc, BusinessDoc, BusinessDocEntry, BusinessDocOptions, BusinessDocStats,
    DocEvidence, BUSINESS_DOC_SCHEMA_VERSION,
};
pub use business_pack::{
    propose_business_pack, propose_business_pack_with_store, BusinessPack, BusinessPackOptions,
    BusinessPackStats, EvidenceRef as BusinessEvidenceRef,
    EvidenceSymbol as BusinessEvidenceSymbol, ModuleDependency, ModuleEvidence,
    BUSINESS_PACK_SCHEMA_VERSION,
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
pub use constants::{
    analyze_constants, analyze_constants_with_store, ConstantEntry, ConstantsOptions,
    ConstantsReport, ConstantsStats, LiteralKind, LiteralSite, CONSTANTS_SCHEMA_VERSION,
};
pub use context_pack::{
    build_context, CodeSnippet, ContextOptions, ContextPack, DocSnippet, EdgeSummary,
};
pub use data_contract::{
    analyze_data_contract, analyze_data_contract_with_store, DataContractOptions,
    DataContractReport, DataContractStats, JsonKey, KeySite, TableColumn, TableSchema,
    DATA_CONTRACT_SCHEMA_VERSION,
};
pub use dead_code::{
    analyze_dead_code, analyze_dead_code_with_store, DeadCodeCandidate, DeadCodeConfidence,
    DeadCodeOptions, DeadCodeReport, DeadCodeStats, DEAD_CODE_SCHEMA_VERSION,
};
pub use docs_indexer::{DocsIndexOptions, DocsIndexResult, DOCS_INDEXER_NAME};
pub use edge_confidence::{confidence_for_edge, derive_confidence, EdgeConfidence};
pub use export::{export, ExportFormat, ExportOptions, ExportOutcome};
pub use feature_map::{
    analyze_feature_map, analyze_feature_map_with_store, FeatureCluster, FeatureClusterMember,
    FeatureMap, FeatureMapOptions, FeatureMapStats, FEATURE_MAP_SCHEMA_VERSION,
};
pub use feature_pack::{
    build_feature_pack, build_feature_pack_with_store, FeaturePack, FeaturePackOptions,
    FeaturePackSelector, FeaturePackStats, PackEdge, FEATURE_PACK_SCHEMA_VERSION,
};
pub use graph::{
    build_graph_view, GraphColumn, GraphEdge, GraphFinding, GraphLayer, GraphNode, GraphOptions,
    GraphStats, GraphStatus, GraphView, GraphViewModel, GRAPH_SCHEMA_VERSION,
};
pub use graph_diff::{
    diff_graphs, diff_graphs_with_stores, DiffEdge, DiffEdgeStatusChange, DiffNode,
    DiffNodeKindChange, GraphDiff, GraphDiffOptions, GraphDiffStats, GRAPH_DIFF_SCHEMA_VERSION,
};
pub use impact::{
    compute_impact, compute_impact_with_policy, merge_confirmed_candidates, run_impact,
    ImpactOptions, ImpactReport,
};
pub use index::{index_repository, IndexOptions, IndexResult, TreeSitterLangResult};
pub use init::{init_repository, InitOptions, InitOutcome};
pub use links_indexer::{index_links, LinksIndexOptions, LinksIndexResult, LINKS_INDEXER_NAME};
pub use logic_confidence::{
    compute_logic_confidence, run_logic_confidence, LogicConfidenceItem, LogicConfidenceKind,
    LogicConfidenceOptions, LogicConfidenceReport, LogicConfidenceSource, LogicConfidenceSummary,
};
pub use port_coverage::{
    analyze_port_coverage, analyze_port_coverage_with_stores, FileCoverage, MissingSymbol,
    PortCoverageOptions, PortCoverageReport, PortCoverageStats, PortedSymbol,
    PORT_COVERAGE_SCHEMA_VERSION,
};
pub use questions::{
    analyze_questions, analyze_questions_with_store, Question, QuestionsOptions, QuestionsReport,
    QuestionsStats, QUESTIONS_SCHEMA_VERSION,
};
pub use search::{
    default_search_kinds, run_search, run_search_with_store, tokenise_code, tokenise_keywords,
    SearchEdge, SearchMatch, SearchNode, SearchOptions, SearchQuery, SearchResult, SearchSubgraph,
    DEFAULT_DEPTH as SEARCH_DEFAULT_DEPTH, DEFAULT_LIMIT as SEARCH_DEFAULT_LIMIT,
    EXPANSION_EDGE_KINDS as SEARCH_EXPANSION_EDGE_KINDS,
};
pub use similarity::{
    analyze_similarity, analyze_similarity_with_store, SimilarityCluster, SimilarityMember,
    SimilarityMode, SimilarityOptions, SimilarityReport, SimilarityStats,
    DEFAULT_MAX_PAIRWISE_SYMBOLS as SIMILARITY_DEFAULT_MAX_PAIRWISE_SYMBOLS,
    DEFAULT_MIN_SIMILARITY as SIMILARITY_DEFAULT_MIN_SIMILARITY,
    DEFAULT_SHINGLE_K as SIMILARITY_DEFAULT_SHINGLE_K, SIMILARITY_SCHEMA_VERSION,
};
pub use slice::{slice_requirement, FeatureSlice, SliceItem, SliceOptions};
pub use symbol_facts::{
    analyze_symbol_facts, analyze_symbol_facts_with_store, BehaviorCounts, FactLine, Purity,
    SymbolFact, SymbolFactsOptions, SymbolFactsReport, SymbolFactsStats,
    SYMBOL_FACTS_SCHEMA_VERSION,
};
pub use test_selection::{
    select_tests, select_tests_with_store, SelectedTest, TestSelection, TestSelectionOptions,
    TestSelectionStats, TEST_SELECTION_SCHEMA_VERSION,
};
pub use test_suggestions::{
    analyze_test_suggestions, analyze_test_suggestions_with_store, Suggestion, SuggestionKind,
    SymbolSuggestions, TestSuggestionsOptions, TestSuggestionsReport, TestSuggestionsStats,
    TEST_SUGGESTIONS_SCHEMA_VERSION,
};

pub use go_indexer::{index_go, GoIndexOptions, GoIndexResult, GO_LANGUAGE_ID};
pub use java_indexer::{index_java, JavaIndexOptions, JavaIndexResult, JAVA_LANGUAGE_ID};
pub use python_indexer::{index_python, PythonIndexOptions, PythonIndexResult, PYTHON_LANGUAGE_ID};
pub use rust_indexer::{
    index_rust, RustIndexOptions, RustIndexResult, RUST_INDEXER_NAME, RUST_LANGUAGE_ID,
};
pub use swift_indexer::{
    index_swift, swift_lsp_available, SwiftIndexOptions, SwiftIndexResult, SWIFT_INDEXER_NAME,
    SWIFT_LSP_COMMAND_ENV,
};
pub use typescript_indexer::{
    index_typescript, TypescriptIndexOptions, TypescriptIndexResult, TYPESCRIPT_LANGUAGE_ID,
};
