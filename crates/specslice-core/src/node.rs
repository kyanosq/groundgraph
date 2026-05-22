//! Artifact nodes stored in the `nodes` table.

use serde::{Deserialize, Serialize};

use crate::artifact_id::ArtifactId;

/// All node kinds known to MVP-0..MVP-5. New kinds must append to keep
/// stable string serialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Requirement,
    AcceptanceCriterion,
    Adr,
    DocSection,
    DartClass,
    DartMethod,
    DartFunction,
    DartConstructor,
    TestCase,
    TestGroup,
    // ---- P8 framework-aware synthetic targets -----------------------------
    /// Top-level Riverpod provider variable (e.g. `final proProvider =
    /// StateNotifierProvider(...)`). The Dart analyzer sidecar adds these
    /// during Pass 1 so `reads_provider` edges have something to point at.
    DartProvider,
    /// Synthetic node for a navigation destination identified only by
    /// its route string — `route::/paywall`, `route::/editor`. Created
    /// on demand by the sidecar when it sees `context.push("/foo")`.
    Route,
    /// Synthetic node for a persistence target identified by storage
    /// backend + bucket (`storage::hive::pro_entitlement`,
    /// `storage::shared_prefs::onboarding_done`). Created on demand by the
    /// sidecar.
    Storage,
    // ---- P9 AI-authored business candidates -------------------------------
    /// A business-logic candidate produced by the AI layer (P9) — a
    /// human-readable description of a flow, gated by `status` and
    /// `confidence`. Always lives in `GraphLayer::Candidate` until a
    /// human confirms it.
    BusinessCandidate,
    // ---- P11 multi-language sidecars (Swift / Go via LSP) -----------------
    // Each new language reuses the Dart-style language-prefixed
    // convention so existing graph view / search code paths keep their
    // explicit `match kind` arms. Names map to Swift declarations
    // surfaced by `sourcekit-lsp` via `textDocument/documentSymbol`.
    SwiftClass,
    SwiftStruct,
    SwiftEnum,
    SwiftProtocol,
    SwiftMethod,
    SwiftFunction,
    SwiftInitializer,
    // Go declarations surfaced by `gopls` via the same LSP request.
    // `gopls` reports Go structs as `SymbolKind::Struct` and Go
    // interfaces as `SymbolKind::Interface`; we keep the distinction
    // explicit because they have very different ownership semantics in
    // a code graph.
    GoStruct,
    GoInterface,
    GoMethod,
    GoFunction,
    // ---- P16 Python (LSP first, AST 补强) ----------------------------------
    // Python is reached via `pyright-langserver`/`basedpyright-langserver`/
    // `pylsp` when one is available; otherwise a minimal AST scanner
    // produces the same structural kinds. Modules are surfaced as their
    // own node so module-level imports / pytest hooks have an anchor.
    PythonModule,
    PythonClass,
    PythonFunction,
    PythonMethod,
    // ---- P20 TypeScript (LSP first, AST 补强) ------------------------------
    // Driven by `typescript-language-server` (`tsserver` under the hood).
    // The AST scanner handles imports and `describe/it` (jest / vitest)
    // when no LSP is configured. `TypescriptEnum` is kept distinct from
    // `TypescriptClass` because `tsserver` reports it separately and the
    // graph view colours them differently.
    TypescriptModule,
    TypescriptClass,
    TypescriptInterface,
    TypescriptEnum,
    TypescriptFunction,
    TypescriptMethod,
    // ---- P20 Java (LSP first, AST 补强) -----------------------------------
    // Driven by `jdtls` (Eclipse JDT Language Server). The AST scanner
    // handles `package` declarations and JUnit 4/5 test methods so
    // ungenerated configurations still produce a usable graph.
    JavaPackage,
    JavaClass,
    JavaInterface,
    JavaEnum,
    JavaMethod,
    JavaConstructor,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Requirement => "requirement",
            NodeKind::AcceptanceCriterion => "acceptance_criterion",
            NodeKind::Adr => "adr",
            NodeKind::DocSection => "doc_section",
            NodeKind::DartClass => "dart_class",
            NodeKind::DartMethod => "dart_method",
            NodeKind::DartFunction => "dart_function",
            NodeKind::DartConstructor => "dart_constructor",
            NodeKind::TestCase => "test_case",
            NodeKind::TestGroup => "test_group",
            NodeKind::DartProvider => "dart_provider",
            NodeKind::Route => "route",
            NodeKind::Storage => "storage",
            NodeKind::BusinessCandidate => "business_candidate",
            NodeKind::SwiftClass => "swift_class",
            NodeKind::SwiftStruct => "swift_struct",
            NodeKind::SwiftEnum => "swift_enum",
            NodeKind::SwiftProtocol => "swift_protocol",
            NodeKind::SwiftMethod => "swift_method",
            NodeKind::SwiftFunction => "swift_function",
            NodeKind::SwiftInitializer => "swift_initializer",
            NodeKind::GoStruct => "go_struct",
            NodeKind::GoInterface => "go_interface",
            NodeKind::GoMethod => "go_method",
            NodeKind::GoFunction => "go_function",
            NodeKind::PythonModule => "python_module",
            NodeKind::PythonClass => "python_class",
            NodeKind::PythonFunction => "python_function",
            NodeKind::PythonMethod => "python_method",
            NodeKind::TypescriptModule => "typescript_module",
            NodeKind::TypescriptClass => "typescript_class",
            NodeKind::TypescriptInterface => "typescript_interface",
            NodeKind::TypescriptEnum => "typescript_enum",
            NodeKind::TypescriptFunction => "typescript_function",
            NodeKind::TypescriptMethod => "typescript_method",
            NodeKind::JavaPackage => "java_package",
            NodeKind::JavaClass => "java_class",
            NodeKind::JavaInterface => "java_interface",
            NodeKind::JavaEnum => "java_enum",
            NodeKind::JavaMethod => "java_method",
            NodeKind::JavaConstructor => "java_constructor",
        }
    }
}

/// In-memory representation of a row in the `nodes` SQLite table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: ArtifactId,
    pub kind: NodeKind,
    pub path: Option<String>,
    pub name: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub content_hash: Option<String>,
    pub stable_key: Option<String>,
    pub source_file: Option<String>,
    pub source_hash: Option<String>,
    pub indexer: Option<String>,
    pub index_generation: Option<i64>,
    pub metadata_json: Option<String>,
}

impl Node {
    pub fn new(id: ArtifactId, kind: NodeKind) -> Self {
        Self {
            id,
            kind,
            path: None,
            name: None,
            start_line: None,
            end_line: None,
            content_hash: None,
            stable_key: None,
            source_file: None,
            source_hash: None,
            indexer: None,
            index_generation: None,
            metadata_json: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_str_round_trip() {
        for kind in [
            NodeKind::File,
            NodeKind::Requirement,
            NodeKind::AcceptanceCriterion,
            NodeKind::Adr,
            NodeKind::DocSection,
            NodeKind::DartClass,
            NodeKind::DartMethod,
            NodeKind::DartFunction,
            NodeKind::DartConstructor,
            NodeKind::TestCase,
            NodeKind::TestGroup,
            NodeKind::DartProvider,
            NodeKind::Route,
            NodeKind::Storage,
            NodeKind::BusinessCandidate,
            NodeKind::SwiftClass,
            NodeKind::SwiftStruct,
            NodeKind::SwiftEnum,
            NodeKind::SwiftProtocol,
            NodeKind::SwiftMethod,
            NodeKind::SwiftFunction,
            NodeKind::SwiftInitializer,
            NodeKind::GoStruct,
            NodeKind::GoInterface,
            NodeKind::GoMethod,
            NodeKind::GoFunction,
            NodeKind::PythonModule,
            NodeKind::PythonClass,
            NodeKind::PythonFunction,
            NodeKind::PythonMethod,
            NodeKind::TypescriptModule,
            NodeKind::TypescriptClass,
            NodeKind::TypescriptInterface,
            NodeKind::TypescriptEnum,
            NodeKind::TypescriptFunction,
            NodeKind::TypescriptMethod,
            NodeKind::JavaPackage,
            NodeKind::JavaClass,
            NodeKind::JavaInterface,
            NodeKind::JavaEnum,
            NodeKind::JavaMethod,
            NodeKind::JavaConstructor,
        ] {
            assert!(!kind.as_str().is_empty());
        }
    }

    #[test]
    fn typescript_and_java_kinds_serialise_with_language_prefix() {
        let cases = [
            (NodeKind::TypescriptModule, "typescript_module"),
            (NodeKind::TypescriptClass, "typescript_class"),
            (NodeKind::TypescriptInterface, "typescript_interface"),
            (NodeKind::TypescriptEnum, "typescript_enum"),
            (NodeKind::TypescriptFunction, "typescript_function"),
            (NodeKind::TypescriptMethod, "typescript_method"),
            (NodeKind::JavaPackage, "java_package"),
            (NodeKind::JavaClass, "java_class"),
            (NodeKind::JavaInterface, "java_interface"),
            (NodeKind::JavaEnum, "java_enum"),
            (NodeKind::JavaMethod, "java_method"),
            (NodeKind::JavaConstructor, "java_constructor"),
        ];
        for (kind, expected) in cases {
            assert_eq!(kind.as_str(), expected);
            let json = serde_json::to_string(&kind).expect("serialise");
            assert_eq!(json, format!("\"{}\"", expected));
            let back: NodeKind = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn python_kinds_serialise_with_language_prefix() {
        let cases = [
            (NodeKind::PythonModule, "python_module"),
            (NodeKind::PythonClass, "python_class"),
            (NodeKind::PythonFunction, "python_function"),
            (NodeKind::PythonMethod, "python_method"),
        ];
        for (kind, expected) in cases {
            assert_eq!(kind.as_str(), expected);
            let json = serde_json::to_string(&kind).expect("serialise");
            assert_eq!(json, format!("\"{}\"", expected));
            let back: NodeKind = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn swift_and_go_kinds_serialise_with_language_prefix() {
        let cases = [
            (NodeKind::SwiftClass, "swift_class"),
            (NodeKind::SwiftStruct, "swift_struct"),
            (NodeKind::SwiftEnum, "swift_enum"),
            (NodeKind::SwiftProtocol, "swift_protocol"),
            (NodeKind::SwiftMethod, "swift_method"),
            (NodeKind::SwiftFunction, "swift_function"),
            (NodeKind::SwiftInitializer, "swift_initializer"),
            (NodeKind::GoStruct, "go_struct"),
            (NodeKind::GoInterface, "go_interface"),
            (NodeKind::GoMethod, "go_method"),
            (NodeKind::GoFunction, "go_function"),
        ];
        for (kind, expected) in cases {
            assert_eq!(kind.as_str(), expected);
            let json = serde_json::to_string(&kind).expect("serialise");
            assert_eq!(json, format!("\"{}\"", expected));
            let back: NodeKind = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn node_new_sets_defaults_and_serialises() {
        let node = Node::new(ArtifactId::new("a"), NodeKind::Requirement);
        let json = serde_json::to_string(&node).expect("serialise");
        assert!(json.contains("\"kind\":\"requirement\""));
        let back: Node = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, node);
    }
}
