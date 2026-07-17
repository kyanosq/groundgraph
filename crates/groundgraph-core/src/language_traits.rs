//! Cross-language node-kind traits.
//!
//! Every other crate previously held its own ad-hoc `match` arms to
//! decide things like "is this a code symbol?", "is it callable?",
//! "what column should it sit in on the graph?". That coupling silently
//! drifted: when Python landed, `questions.rs` got a fresh
//! `is_code_symbol` that forgot `PythonModule` and the entire Swift
//! enum / protocol family, and `dead_code` / `search` ended up answering
//! slightly different questions for the same kind.
//!
//! This module is the single source of truth. New languages add their
//! `NodeKind` variants to [`crate::node::NodeKind`] and an arm here; every
//! consumer keeps working.
//!
//! Design rules:
//! - All predicates are total `fn(NodeKind) -> _`. Every `NodeKind`
//!   variant must be handled — the compiler will refuse to build if a
//!   new kind is added without updating every arm.
//! - Predicates are independent: callers compose them, we don't bake
//!   in business logic ("is this similarity-eligible?" lives here, but
//!   *deciding whether to actually run similarity on it* lives in the
//!   similarity engine, which calls us).
//! - `GraphColumn` lives in `groundgraph-engine` to keep this crate
//!   dependency-light; the column predicate returns a small enum
//!   ([`SymbolFamily`]) that the engine maps to its own type.

use crate::node::NodeKind;

/// Language an artifact belongs to. `Doc` covers Markdown / requirement
/// / ADR; `Markup` is reserved for future framework-anchored node kinds
/// (route, storage, provider) that don't have a host language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Dart,
    Swift,
    Go,
    Python,
    Typescript,
    Java,
    Rust,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
    Kotlin,
    /// Markdown / Requirement / ADR / DocSection / AcceptanceCriterion.
    Doc,
    /// Synthetic graph anchors not tied to a host language (Route, Storage,
    /// DartProvider — these are produced by *framework* recognition rather
    /// than a parser).
    Synthetic,
    /// `BusinessCandidate` and `File` (file is multi-language; callers
    /// inspect the path).
    Generic,
}

/// How a language lexes a single quote `'`. Hand-rolled scanners (literal
/// catalogue, noise stripping) need this to tell a Dart string `'a.b'` from a
/// Rust char `'a'` / lifetime `'a`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleQuote {
    /// `'...'` is a string, identical to `"..."` (Dart / Python / TS / JS).
    String,
    /// `'a'` is a char/rune literal; a bare `'a` (no closing quote) is a
    /// label / lifetime to be skipped (Rust / C / C++ / Go / Java / Swift).
    Char,
}

/// Lexical surface facts a scanner needs when it walks **raw source without a
/// full grammar** (e.g. `constants::scan_literals`, `source_text::strip_noise`).
///
/// Before this existed, every such scanner re-hardcoded the same per-language
/// `match`/`matches!(lang, Python)` arms inline, and they silently drifted out
/// of sync (the literal scanner treated `'…'` as a string only for
/// Dart/Python/TS while the noise stripper treated it as a string for *every*
/// language). Centralising the facts here — next to the other per-language
/// truths — means a new language is described in ONE place and every scanner
/// stays consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexSyntax {
    /// Marker that starts a line comment (`//` for the C family, `#` Python).
    pub line_comment: &'static str,
    /// `(open, close)` block-comment delimiters, when the language has them.
    pub block_comment: Option<(&'static str, &'static str)>,
    /// How `'` is lexed (string vs char/lifetime).
    pub single_quote: SingleQuote,
}

impl LexSyntax {
    /// True when the line comment is `#` (Python). Convenience for scanners
    /// whose hand-rolled loop only distinguishes `//` from `#`.
    pub fn uses_hash_comments(self) -> bool {
        self.line_comment == "#"
    }

    /// True when `'…'` should be lexed as a string literal.
    pub fn single_quote_is_string(self) -> bool {
        matches!(self.single_quote, SingleQuote::String)
    }
}

const LEX_C_FAMILY: LexSyntax = LexSyntax {
    line_comment: "//",
    block_comment: Some(("/*", "*/")),
    single_quote: SingleQuote::Char,
};

/// Lexical facts for a language. `Doc`/`Synthetic`/`Generic` are never scanned
/// for literals, so they return the C-family default harmlessly.
pub fn lex_syntax(lang: Language) -> LexSyntax {
    match lang {
        // `'…'` is a string, `//` + `/* */` comments.
        Language::Dart | Language::Typescript => LexSyntax {
            single_quote: SingleQuote::String,
            ..LEX_C_FAMILY
        },
        // `#` line comments, no block comment, `'…'`/`"…"` both strings.
        Language::Python => LexSyntax {
            line_comment: "#",
            block_comment: None,
            single_quote: SingleQuote::String,
        },
        // `'a'` is a char/rune (or a Rust lifetime when unbalanced).
        Language::Swift
        | Language::Go
        | Language::Java
        | Language::Rust
        | Language::C
        | Language::Cpp
        | Language::CSharp
        | Language::Kotlin => LEX_C_FAMILY,
        // `#` line comments, no block comments, single-quoted strings.
        Language::Ruby => LexSyntax {
            line_comment: "#",
            block_comment: None,
            single_quote: SingleQuote::String,
        },
        // `//` + `/* */` like C, but `'…'` is a string.
        Language::Php => LexSyntax {
            single_quote: SingleQuote::String,
            ..LEX_C_FAMILY
        },
        // Not host languages — never scanned; harmless default.
        Language::Doc | Language::Synthetic | Language::Generic => LEX_C_FAMILY,
    }
}

/// Coarse "what is this thing structurally?" family. Consumers map this
/// to UI columns / sort order / serialization names without re-doing the
/// per-kind `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolFamily {
    /// File-level node (`File`, `*Module`, `*Package`).
    Module,
    /// User-defined type (class / struct / enum / interface / protocol).
    Type,
    /// User-defined callable (function / method / constructor / initializer).
    Callable,
    /// Test artifact (TestCase / TestGroup).
    Test,
    /// Documentation artifact (requirement / ADR / doc-section / acceptance).
    Doc,
    /// Framework-anchored synthetic target (route / storage / provider).
    Framework,
    /// AI-authored business candidate.
    Candidate,
    /// Persistence-layer artifact (DB table / schema), carried in the graph
    /// so a rewrite can be checked for data-contract parity.
    Schema,
}

/// Language each `NodeKind` belongs to.
pub fn language_of(kind: NodeKind) -> Language {
    match kind {
        NodeKind::DartClass
        | NodeKind::DartMethod
        | NodeKind::DartFunction
        | NodeKind::DartConstructor
        | NodeKind::DartProvider => Language::Dart,
        NodeKind::SwiftClass
        | NodeKind::SwiftStruct
        | NodeKind::SwiftEnum
        | NodeKind::SwiftProtocol
        | NodeKind::SwiftMethod
        | NodeKind::SwiftFunction
        | NodeKind::SwiftInitializer => Language::Swift,
        NodeKind::GoStruct | NodeKind::GoInterface | NodeKind::GoMethod | NodeKind::GoFunction => {
            Language::Go
        }
        NodeKind::PythonModule
        | NodeKind::PythonClass
        | NodeKind::PythonFunction
        | NodeKind::PythonMethod => Language::Python,
        NodeKind::TypescriptModule
        | NodeKind::TypescriptClass
        | NodeKind::TypescriptInterface
        | NodeKind::TypescriptEnum
        | NodeKind::TypescriptFunction
        | NodeKind::TypescriptMethod => Language::Typescript,
        NodeKind::JavaPackage
        | NodeKind::JavaClass
        | NodeKind::JavaInterface
        | NodeKind::JavaEnum
        | NodeKind::JavaMethod
        | NodeKind::JavaConstructor => Language::Java,
        NodeKind::RustModule
        | NodeKind::RustStruct
        | NodeKind::RustEnum
        | NodeKind::RustTrait
        | NodeKind::RustFunction
        | NodeKind::RustMethod
        | NodeKind::RustMacro => Language::Rust,
        NodeKind::CFunction | NodeKind::CStruct | NodeKind::CEnum => Language::C,
        NodeKind::CppNamespace
        | NodeKind::CppClass
        | NodeKind::CppStruct
        | NodeKind::CppEnum
        | NodeKind::CppFunction
        | NodeKind::CppMethod => Language::Cpp,
        NodeKind::CSharpClass
        | NodeKind::CSharpInterface
        | NodeKind::CSharpStruct
        | NodeKind::CSharpEnum
        | NodeKind::CSharpMethod
        | NodeKind::CSharpFunction => Language::CSharp,
        NodeKind::RubyModule
        | NodeKind::RubyClass
        | NodeKind::RubyMethod
        | NodeKind::RubyFunction => Language::Ruby,
        NodeKind::PhpClass
        | NodeKind::PhpInterface
        | NodeKind::PhpTrait
        | NodeKind::PhpEnum
        | NodeKind::PhpMethod
        | NodeKind::PhpFunction => Language::Php,
        NodeKind::KotlinClass
        | NodeKind::KotlinInterface
        | NodeKind::KotlinEnum
        | NodeKind::KotlinObject
        | NodeKind::KotlinMethod
        | NodeKind::KotlinFunction => Language::Kotlin,
        NodeKind::Requirement
        | NodeKind::AcceptanceCriterion
        | NodeKind::Adr
        | NodeKind::DocSection => Language::Doc,
        NodeKind::Route | NodeKind::Storage | NodeKind::HttpRoute => Language::Synthetic,
        // Tests are produced by language-specific parsers but their kind
        // is shared (`TestCase` / `TestGroup`). Callers that need the
        // actual host language must look at the file path; here we mark
        // them generic to make `is_test` symmetric across languages.
        NodeKind::TestCase | NodeKind::TestGroup => Language::Generic,
        NodeKind::File
        | NodeKind::BusinessCandidate
        | NodeKind::DbTable
        | NodeKind::SqlMapperStmt => Language::Generic,
    }
}

/// Structural family. See [`SymbolFamily`].
pub fn family_of(kind: NodeKind) -> SymbolFamily {
    match kind {
        // Modules / files / packages.
        NodeKind::File
        | NodeKind::PythonModule
        | NodeKind::TypescriptModule
        | NodeKind::JavaPackage
        | NodeKind::RustModule
        | NodeKind::CppNamespace => SymbolFamily::Module,
        // Types.
        NodeKind::DartClass
        | NodeKind::SwiftClass
        | NodeKind::SwiftStruct
        | NodeKind::SwiftEnum
        | NodeKind::SwiftProtocol
        | NodeKind::GoStruct
        | NodeKind::GoInterface
        | NodeKind::PythonClass
        | NodeKind::TypescriptClass
        | NodeKind::TypescriptInterface
        | NodeKind::TypescriptEnum
        | NodeKind::JavaClass
        | NodeKind::JavaInterface
        | NodeKind::JavaEnum
        | NodeKind::RustStruct
        | NodeKind::RustEnum
        | NodeKind::RustTrait
        | NodeKind::RustMacro
        | NodeKind::CStruct
        | NodeKind::CEnum
        | NodeKind::CppClass
        | NodeKind::CppStruct
        | NodeKind::CppEnum
        | NodeKind::CSharpClass
        | NodeKind::CSharpInterface
        | NodeKind::CSharpStruct
        | NodeKind::CSharpEnum
        | NodeKind::RubyModule
        | NodeKind::RubyClass
        | NodeKind::PhpClass
        | NodeKind::PhpInterface
        | NodeKind::PhpTrait
        | NodeKind::PhpEnum
        | NodeKind::KotlinClass
        | NodeKind::KotlinInterface
        | NodeKind::KotlinEnum
        | NodeKind::KotlinObject => SymbolFamily::Type,
        // Callables.
        NodeKind::DartMethod
        | NodeKind::DartFunction
        | NodeKind::DartConstructor
        | NodeKind::SwiftMethod
        | NodeKind::SwiftFunction
        | NodeKind::SwiftInitializer
        | NodeKind::GoMethod
        | NodeKind::GoFunction
        | NodeKind::PythonFunction
        | NodeKind::PythonMethod
        | NodeKind::TypescriptFunction
        | NodeKind::TypescriptMethod
        | NodeKind::JavaMethod
        | NodeKind::JavaConstructor
        | NodeKind::RustFunction
        | NodeKind::RustMethod
        | NodeKind::CFunction
        | NodeKind::CppFunction
        | NodeKind::CppMethod
        | NodeKind::CSharpMethod
        | NodeKind::CSharpFunction
        | NodeKind::RubyMethod
        | NodeKind::RubyFunction
        | NodeKind::PhpMethod
        | NodeKind::PhpFunction
        | NodeKind::KotlinMethod
        | NodeKind::KotlinFunction => SymbolFamily::Callable,
        // Tests.
        NodeKind::TestCase | NodeKind::TestGroup => SymbolFamily::Test,
        // Docs.
        NodeKind::Requirement
        | NodeKind::AcceptanceCriterion
        | NodeKind::Adr
        | NodeKind::DocSection => SymbolFamily::Doc,
        // Framework-anchored.
        NodeKind::DartProvider | NodeKind::Route | NodeKind::Storage | NodeKind::HttpRoute => {
            SymbolFamily::Framework
        }
        // Candidates.
        NodeKind::BusinessCandidate => SymbolFamily::Candidate,
        // Schema (persistence layer): tables + mapper SQL statements.
        NodeKind::DbTable | NodeKind::SqlMapperStmt => SymbolFamily::Schema,
    }
}

/// "Is this a code symbol the AI / human is expected to navigate to,
/// review, or reason about?" Covers callables + types + modules across
/// every supported language. Does **not** include `File` (too generic),
/// `Test*`, docs, framework anchors, or candidates.
pub fn is_code_symbol(kind: NodeKind) -> bool {
    matches!(family_of(kind), SymbolFamily::Type | SymbolFamily::Callable)
        || matches!(
            kind,
            NodeKind::PythonModule
                | NodeKind::TypescriptModule
                | NodeKind::JavaPackage
                | NodeKind::RustModule
                | NodeKind::CppNamespace
        )
}

/// Callable function-like symbol (function / method / constructor).
pub fn is_callable(kind: NodeKind) -> bool {
    matches!(family_of(kind), SymbolFamily::Callable)
}

/// Type-like declaration (class / struct / enum / interface / protocol).
pub fn is_type(kind: NodeKind) -> bool {
    matches!(family_of(kind), SymbolFamily::Type)
}

/// File-level / module-level / package-level node.
pub fn is_module_or_file(kind: NodeKind) -> bool {
    matches!(family_of(kind), SymbolFamily::Module)
}

/// Test artifact.
pub fn is_test(kind: NodeKind) -> bool {
    matches!(family_of(kind), SymbolFamily::Test)
}

/// `Calls`/`References` similarity is only meaningful for **callable
/// bodies** — fingerprinting a type declaration or a module gives a
/// huge cluster of useless near-matches. Callers (P18 similarity tier 1
/// + 2) consult this to decide whether to fingerprint a node.
pub fn similarity_supported(kind: NodeKind) -> bool {
    is_callable(kind)
}

/// Default `--reason` string the dead-code analyzer surfaces for a kind
/// it can't more specifically explain. Keeps wording uniform across
/// languages and prevents the engine from inventing freshly worded
/// reasons every release.
pub fn default_dead_code_reason(kind: NodeKind) -> &'static str {
    match family_of(kind) {
        SymbolFamily::Callable => "callable has no incoming Calls/References",
        SymbolFamily::Type => "type has no incoming usages",
        SymbolFamily::Module => "module/file has no incoming Imports",
        SymbolFamily::Test => "test is never referenced from a runner",
        SymbolFamily::Doc => "doc artifact has no implementations/verifications",
        SymbolFamily::Framework => "framework anchor has no incoming reads/navigations",
        SymbolFamily::Candidate => "candidate has no DeclaresImplementation edges",
        SymbolFamily::Schema => "schema table has no incoming code edges",
    }
}

/// Search aliases — extra strings the search engine should match a node
/// kind on, beyond its raw `as_str()` form. Kept tiny to avoid
/// surprising fuzzy matches.
///
/// Method / free-function aliases are derived from the categorisation
/// predicates (not a hand-listed arm) so every language — present and
/// future — gets them automatically; the old explicit list silently
/// omitted C# / Ruby / PHP / Kotlin.
pub fn search_aliases(kind: NodeKind) -> &'static [&'static str] {
    if kind.is_method() {
        return &["method", "fn"];
    }
    if kind.is_free_function() {
        return &["function", "fn"];
    }
    match kind {
        NodeKind::DartProvider => &["provider", "riverpod"],
        NodeKind::Route => &["route", "navigation"],
        NodeKind::Storage => &["storage", "persistence"],
        NodeKind::BusinessCandidate => &["candidate", "business"],
        NodeKind::TestCase | NodeKind::TestGroup => &["test"],
        NodeKind::RustTrait => &["trait", "interface"],
        NodeKind::RustMacro => &["macro", "macro_rules"],
        NodeKind::CppNamespace => &["namespace", "module"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The matrix tests iterate `NodeKind::ALL` — the single source of
    /// truth maintained next to the enum itself — so a new kind can never
    /// silently bypass these tests the way the old hand-copied slice let
    /// the 25 P25/P26 kinds slip through (regression issues.md #18).
    const ALL_KINDS: &[NodeKind] = NodeKind::ALL;

    #[test]
    fn every_kind_has_a_language_and_family() {
        for kind in ALL_KINDS {
            let _ = language_of(*kind);
            let _ = family_of(*kind);
            let _ = default_dead_code_reason(*kind);
            let _ = search_aliases(*kind);
        }
    }

    #[test]
    fn matrix_total_count_matches_known_kinds() {
        // Hard-code the expected total so a kind addition that forgets
        // to append to `NodeKind::ALL` fails this test loudly.
        assert_eq!(
            ALL_KINDS.len(),
            83,
            "NodeKind::ALL missing a variant. Append it there and update this count."
        );
    }

    #[test]
    fn method_and_function_aliases_cover_every_language() {
        // Searching "method" / "function" must hit the callable kinds of
        // *every* language — the hand-listed arms used to omit C# / Ruby /
        // PHP / Kotlin (issues.md #22).
        for kind in ALL_KINDS {
            if kind.is_method() {
                assert!(
                    search_aliases(*kind).contains(&"method"),
                    "{kind:?} must alias \"method\""
                );
            }
            if kind.is_free_function() {
                assert!(
                    search_aliases(*kind).contains(&"function"),
                    "{kind:?} must alias \"function\""
                );
            }
        }
    }

    #[test]
    fn is_code_symbol_covers_swift_initializer_enum_protocol_go_interface_python_module() {
        // Regression — questions.rs originally forgot these. The test
        // names them explicitly so future drift is obvious.
        for kind in [
            NodeKind::SwiftInitializer,
            NodeKind::SwiftEnum,
            NodeKind::SwiftProtocol,
            NodeKind::GoInterface,
            NodeKind::PythonModule,
        ] {
            assert!(
                is_code_symbol(kind),
                "is_code_symbol({kind:?}) must be true"
            );
        }
    }

    #[test]
    fn families_are_disjoint() {
        // Each kind belongs to exactly one family — caller code relies
        // on this when grouping for UI.
        for kind in ALL_KINDS {
            let f = family_of(*kind);
            let exclusive = [
                is_callable(*kind),
                is_type(*kind),
                is_module_or_file(*kind),
                is_test(*kind),
            ];
            let true_count = exclusive.iter().filter(|x| **x).count();
            // 0 = Doc / Framework / Candidate; 1 = the language families;
            // never > 1.
            assert!(
                true_count <= 1,
                "kind {kind:?} (family {f:?}) matched multiple primary predicates: {exclusive:?}"
            );
        }
    }

    #[test]
    fn similarity_only_targets_callables() {
        for kind in ALL_KINDS {
            assert_eq!(
                similarity_supported(*kind),
                is_callable(*kind),
                "similarity should be wired to is_callable for {kind:?}"
            );
        }
    }

    #[test]
    fn lex_syntax_matches_per_language_surface() {
        // String-quote languages: `'…'` is a string.
        for lang in [Language::Dart, Language::Typescript, Language::Python] {
            assert!(
                lex_syntax(lang).single_quote_is_string(),
                "{lang:?} should treat single quotes as strings"
            );
        }
        // Char/lifetime languages: `'a'` is a char, never a string.
        for lang in [
            Language::Rust,
            Language::C,
            Language::Cpp,
            Language::Go,
            Language::Java,
            Language::Swift,
        ] {
            assert!(
                !lex_syntax(lang).single_quote_is_string(),
                "{lang:?} should treat single quotes as char/lifetime"
            );
        }
        // Only Python uses `#` line comments and has no block comment.
        assert!(lex_syntax(Language::Python).uses_hash_comments());
        assert_eq!(lex_syntax(Language::Python).block_comment, None);
        for lang in [
            Language::Rust,
            Language::Dart,
            Language::Typescript,
            Language::Go,
            Language::Java,
            Language::Swift,
            Language::C,
            Language::Cpp,
        ] {
            let lex = lex_syntax(lang);
            assert!(!lex.uses_hash_comments(), "{lang:?} uses // not #");
            assert_eq!(
                lex.block_comment,
                Some(("/*", "*/")),
                "{lang:?} should have C-style block comments"
            );
        }
    }

    #[test]
    fn dead_code_reason_is_non_empty() {
        for kind in ALL_KINDS {
            let reason = default_dead_code_reason(*kind);
            assert!(!reason.is_empty(), "empty reason for {kind:?}");
        }
    }

    #[test]
    fn typescript_and_java_are_routed() {
        // P20 — the two new languages must light up through every
        // structural predicate. If somebody ships TS/Java kinds but
        // forgets a routing arm, this test breaks.
        for kind in [
            NodeKind::TypescriptFunction,
            NodeKind::TypescriptMethod,
            NodeKind::JavaMethod,
            NodeKind::JavaConstructor,
        ] {
            assert!(is_callable(kind));
            assert!(is_code_symbol(kind));
            assert!(similarity_supported(kind));
        }
        for kind in [
            NodeKind::TypescriptClass,
            NodeKind::TypescriptInterface,
            NodeKind::TypescriptEnum,
            NodeKind::JavaClass,
            NodeKind::JavaInterface,
            NodeKind::JavaEnum,
        ] {
            assert!(is_type(kind));
            assert!(is_code_symbol(kind));
            assert!(!similarity_supported(kind));
        }
        for kind in [NodeKind::TypescriptModule, NodeKind::JavaPackage] {
            assert!(is_module_or_file(kind));
            assert!(is_code_symbol(kind));
        }
    }

    #[test]
    fn rust_kinds_are_routed() {
        // P21 — Rust is the first tree-sitter breadth backend. Every
        // structural predicate must light up so search / dead-code /
        // similarity treat Rust symbols exactly like the LSP languages.
        for kind in [NodeKind::RustFunction, NodeKind::RustMethod] {
            assert_eq!(language_of(kind), Language::Rust);
            assert!(is_callable(kind));
            assert!(is_code_symbol(kind));
            assert!(similarity_supported(kind));
        }
        for kind in [
            NodeKind::RustStruct,
            NodeKind::RustEnum,
            NodeKind::RustTrait,
        ] {
            assert_eq!(language_of(kind), Language::Rust);
            assert!(is_type(kind));
            assert!(is_code_symbol(kind));
            assert!(!similarity_supported(kind));
        }
        assert!(is_module_or_file(NodeKind::RustModule));
        assert!(is_code_symbol(NodeKind::RustModule));
    }

    #[test]
    fn c_and_cpp_are_routed() {
        // P22 — C / C++ via the same tree-sitter breadth backend.
        for kind in [
            NodeKind::CFunction,
            NodeKind::CppFunction,
            NodeKind::CppMethod,
        ] {
            assert!(is_callable(kind));
            assert!(is_code_symbol(kind));
            assert!(similarity_supported(kind));
        }
        for kind in [
            NodeKind::CStruct,
            NodeKind::CEnum,
            NodeKind::CppClass,
            NodeKind::CppStruct,
            NodeKind::CppEnum,
        ] {
            assert!(is_type(kind));
            assert!(is_code_symbol(kind));
            assert!(!similarity_supported(kind));
        }
        assert_eq!(language_of(NodeKind::CFunction), Language::C);
        assert_eq!(language_of(NodeKind::CppMethod), Language::Cpp);
        assert!(is_module_or_file(NodeKind::CppNamespace));
        assert!(is_code_symbol(NodeKind::CppNamespace));
    }
}
