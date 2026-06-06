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
    // ---- P21 Rust (tree-sitter, in-process) -------------------------------
    // The first "breadth backend" language driven by an in-process
    // tree-sitter grammar instead of an external LSP. Deterministic,
    // dependency-free, and fast enough to index SpecSlice's own sources
    // (the LSP-first adapters could not self-host). `RustModule` covers
    // both the crate root file and inline `mod` blocks; methods inside
    // `impl` / `trait` blocks are surfaced as `RustMethod`, free
    // functions as `RustFunction`.
    RustModule,
    RustStruct,
    RustEnum,
    RustTrait,
    RustFunction,
    RustMethod,
    // ---- P22 C / C++ (tree-sitter, in-process) ----------------------------
    // The breadth backend's second wave. C has no classes/namespaces, so it
    // contributes structs, enums and free functions only. C++ adds
    // namespaces, classes and methods (callables nested in a class body).
    // Both are notoriously hard for LSP-first indexing (compile databases,
    // include paths); the in-process tree-sitter driver indexes them with
    // zero project configuration, which is exactly the breadth win.
    CFunction,
    CStruct,
    CEnum,
    CppNamespace,
    CppClass,
    CppStruct,
    CppEnum,
    CppFunction,
    CppMethod,
    // ---- P25 data layer: DB schema as a first-class graph node ------------
    /// A database table (logical name) extracted from `CREATE TABLE` (.sql)
    /// or an ORM annotation (MyBatis-Plus `@TableName` / JPA `@Table`).
    /// Columns are carried in `metadata_json`. Putting tables in the graph
    /// lets a rewrite be checked for table/column parity, not just call
    /// parity — the persistence contract becomes evidence.
    DbTable,
    /// A MyBatis mapper statement (`<select|insert|update|delete id="...">`)
    /// extracted from mapper XML. `name` is the statement id (== the Java
    /// mapper-interface method); `metadata_json` carries the raw SQL + kind +
    /// namespace. This makes the SQL searchable graph evidence so a Java→X port
    /// can read the query semantics from the graph instead of grepping XML.
    SqlMapperStmt,
}

impl NodeKind {
    /// Every variant, in declaration order. The single source of truth the
    /// round-trip / categorisation helpers iterate (so adding a kind cannot
    /// silently fall out of `from_str` or the category predicates).
    pub const ALL: &'static [NodeKind] = &[
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
        NodeKind::RustModule,
        NodeKind::RustStruct,
        NodeKind::RustEnum,
        NodeKind::RustTrait,
        NodeKind::RustFunction,
        NodeKind::RustMethod,
        NodeKind::CFunction,
        NodeKind::CStruct,
        NodeKind::CEnum,
        NodeKind::CppNamespace,
        NodeKind::CppClass,
        NodeKind::CppStruct,
        NodeKind::CppEnum,
        NodeKind::CppFunction,
        NodeKind::CppMethod,
        NodeKind::DbTable,
        NodeKind::SqlMapperStmt,
    ];

    /// Parse the stable snake_case string back into a [`NodeKind`]. Inverse
    /// of [`NodeKind::as_str`]; `None` for any unknown string. Centralised
    /// here so CLI/MCP/graph code never re-implement the mapping.
    ///
    /// Deliberately returns `Option` (not the `Result` of [`std::str::FromStr`])
    /// because callers layer their own aliases on top and want a cheap probe.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<NodeKind> {
        NodeKind::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    /// The source language a code kind belongs to (`dart`, `swift`, `go`,
    /// `python`, `typescript`, `java`, `rust`, `c`, `cpp`), or `None` for the
    /// language-agnostic kinds (docs, tests, synthetic targets, candidates).
    pub fn language(self) -> Option<&'static str> {
        let s = self.as_str();
        // `cpp` before `c` so `cpp_*` is not misread as the `c` language.
        [
            "dart",
            "swift",
            "go",
            "python",
            "typescript",
            "java",
            "rust",
            "cpp",
            "c",
        ]
        .into_iter()
        .find(|lang| {
            s.strip_prefix(lang)
                .and_then(|r| r.strip_prefix('_'))
                .is_some()
        })
    }

    /// A user-defined *type* container (class / struct / enum / trait /
    /// protocol / interface / namespace / module). Callables nested in one
    /// are methods; in the graph these sort before their members.
    pub fn is_type_container(self) -> bool {
        use NodeKind::*;
        matches!(
            self,
            DartClass
                | SwiftClass
                | SwiftStruct
                | SwiftEnum
                | SwiftProtocol
                | GoStruct
                | GoInterface
                | PythonClass
                | PythonModule
                | TypescriptClass
                | TypescriptInterface
                | TypescriptEnum
                | TypescriptModule
                | JavaClass
                | JavaInterface
                | JavaEnum
                | JavaPackage
                | RustStruct
                | RustEnum
                | RustTrait
                | RustModule
                | CStruct
                | CEnum
                | CppNamespace
                | CppClass
                | CppStruct
                | CppEnum
        )
    }

    /// A method (callable bound to a type container).
    pub fn is_method(self) -> bool {
        use NodeKind::*;
        matches!(
            self,
            DartMethod
                | SwiftMethod
                | GoMethod
                | PythonMethod
                | TypescriptMethod
                | JavaMethod
                | RustMethod
                | CppMethod
        )
    }

    /// A free function (callable not bound to a type).
    pub fn is_free_function(self) -> bool {
        use NodeKind::*;
        matches!(
            self,
            DartFunction
                | SwiftFunction
                | GoFunction
                | PythonFunction
                | TypescriptFunction
                | RustFunction
                | CFunction
                | CppFunction
        )
    }

    /// A constructor / initializer.
    pub fn is_constructor(self) -> bool {
        matches!(
            self,
            NodeKind::DartConstructor | NodeKind::SwiftInitializer | NodeKind::JavaConstructor
        )
    }

    /// Any callable: method, free function, or constructor.
    pub fn is_callable(self) -> bool {
        self.is_method() || self.is_free_function() || self.is_constructor()
    }

    /// A test node (case or group / suite).
    pub fn is_test(self) -> bool {
        matches!(self, NodeKind::TestCase | NodeKind::TestGroup)
    }

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
            NodeKind::RustModule => "rust_module",
            NodeKind::RustStruct => "rust_struct",
            NodeKind::RustEnum => "rust_enum",
            NodeKind::RustTrait => "rust_trait",
            NodeKind::RustFunction => "rust_function",
            NodeKind::RustMethod => "rust_method",
            NodeKind::CFunction => "c_function",
            NodeKind::CStruct => "c_struct",
            NodeKind::CEnum => "c_enum",
            NodeKind::CppNamespace => "cpp_namespace",
            NodeKind::CppClass => "cpp_class",
            NodeKind::CppStruct => "cpp_struct",
            NodeKind::CppEnum => "cpp_enum",
            NodeKind::CppFunction => "cpp_function",
            NodeKind::CppMethod => "cpp_method",
            NodeKind::DbTable => "db_table",
            NodeKind::SqlMapperStmt => "sql_mapper_stmt",
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
        for kind in NodeKind::ALL {
            assert!(!kind.as_str().is_empty());
            // serde uses the same snake_case mapping as `as_str`.
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
    }

    #[test]
    fn c_and_cpp_kinds_serialise_with_language_prefix() {
        let cases = [
            (NodeKind::CFunction, "c_function"),
            (NodeKind::CStruct, "c_struct"),
            (NodeKind::CEnum, "c_enum"),
            (NodeKind::CppNamespace, "cpp_namespace"),
            (NodeKind::CppClass, "cpp_class"),
            (NodeKind::CppStruct, "cpp_struct"),
            (NodeKind::CppEnum, "cpp_enum"),
            (NodeKind::CppFunction, "cpp_function"),
            (NodeKind::CppMethod, "cpp_method"),
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
    fn rust_kinds_serialise_with_language_prefix() {
        let cases = [
            (NodeKind::RustModule, "rust_module"),
            (NodeKind::RustStruct, "rust_struct"),
            (NodeKind::RustEnum, "rust_enum"),
            (NodeKind::RustTrait, "rust_trait"),
            (NodeKind::RustFunction, "rust_function"),
            (NodeKind::RustMethod, "rust_method"),
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
    fn from_str_is_inverse_of_as_str_for_all_kinds() {
        for kind in NodeKind::ALL {
            assert_eq!(NodeKind::from_str(kind.as_str()), Some(*kind), "{kind:?}");
        }
        assert_eq!(NodeKind::from_str("not_a_kind"), None);
        assert_eq!(NodeKind::from_str(""), None);
    }

    #[test]
    fn language_and_categories_cover_every_language_uniformly() {
        // Every language-prefixed kind reports its language, and the
        // container / callable / test categories are consistent so the graph
        // sort + business rank can be language-agnostic (regression: the
        // hand-written maps used to omit Java / Rust / C / C++ / TypeScript).
        assert_eq!(NodeKind::DartClass.language(), Some("dart"));
        assert_eq!(NodeKind::JavaClass.language(), Some("java"));
        assert_eq!(NodeKind::RustStruct.language(), Some("rust"));
        assert_eq!(NodeKind::CppNamespace.language(), Some("cpp"));
        assert_eq!(NodeKind::Requirement.language(), None);

        // Containers across all languages.
        for k in [
            NodeKind::DartClass,
            NodeKind::SwiftStruct,
            NodeKind::GoInterface,
            NodeKind::PythonClass,
            NodeKind::TypescriptInterface,
            NodeKind::JavaEnum,
            NodeKind::RustTrait,
            NodeKind::CppClass,
            NodeKind::CStruct,
        ] {
            assert!(k.is_type_container(), "{k:?} should be a container");
            assert!(!k.is_callable(), "{k:?} is not callable");
        }

        // Methods / free functions / constructors.
        assert!(NodeKind::JavaMethod.is_method());
        assert!(NodeKind::RustMethod.is_method());
        assert!(NodeKind::CppFunction.is_free_function());
        assert!(NodeKind::TypescriptFunction.is_free_function());
        assert!(NodeKind::JavaConstructor.is_constructor());
        assert!(NodeKind::SwiftInitializer.is_constructor());
        assert!(NodeKind::DartMethod.is_callable());

        // Tests.
        assert!(NodeKind::TestCase.is_test());
        assert!(NodeKind::TestGroup.is_test());
        assert!(!NodeKind::DartClass.is_test());
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
