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
//! `NodeKind` variants to [`node::NodeKind`] and an arm here; every
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
//! - `GraphColumn` lives in `specslice-engine` to keep this crate
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
        NodeKind::Requirement
        | NodeKind::AcceptanceCriterion
        | NodeKind::Adr
        | NodeKind::DocSection => Language::Doc,
        NodeKind::Route | NodeKind::Storage => Language::Synthetic,
        // Tests are produced by language-specific parsers but their kind
        // is shared (`TestCase` / `TestGroup`). Callers that need the
        // actual host language must look at the file path; here we mark
        // them generic to make `is_test` symmetric across languages.
        NodeKind::TestCase | NodeKind::TestGroup => Language::Generic,
        NodeKind::File | NodeKind::BusinessCandidate => Language::Generic,
    }
}

/// Structural family. See [`SymbolFamily`].
pub fn family_of(kind: NodeKind) -> SymbolFamily {
    match kind {
        // Modules / files / packages.
        NodeKind::File
        | NodeKind::PythonModule
        | NodeKind::TypescriptModule
        | NodeKind::JavaPackage => SymbolFamily::Module,
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
        | NodeKind::JavaEnum => SymbolFamily::Type,
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
        | NodeKind::JavaConstructor => SymbolFamily::Callable,
        // Tests.
        NodeKind::TestCase | NodeKind::TestGroup => SymbolFamily::Test,
        // Docs.
        NodeKind::Requirement
        | NodeKind::AcceptanceCriterion
        | NodeKind::Adr
        | NodeKind::DocSection => SymbolFamily::Doc,
        // Framework-anchored.
        NodeKind::DartProvider | NodeKind::Route | NodeKind::Storage => SymbolFamily::Framework,
        // Candidates.
        NodeKind::BusinessCandidate => SymbolFamily::Candidate,
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
            NodeKind::PythonModule | NodeKind::TypescriptModule | NodeKind::JavaPackage
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
    }
}

/// Search aliases — extra strings the search engine should match a node
/// kind on, beyond its raw `as_str()` form. Kept tiny to avoid
/// surprising fuzzy matches.
pub fn search_aliases(kind: NodeKind) -> &'static [&'static str] {
    match kind {
        NodeKind::DartProvider => &["provider", "riverpod"],
        NodeKind::Route => &["route", "navigation"],
        NodeKind::Storage => &["storage", "persistence"],
        NodeKind::BusinessCandidate => &["candidate", "business"],
        NodeKind::TestCase | NodeKind::TestGroup => &["test"],
        NodeKind::DartMethod
        | NodeKind::SwiftMethod
        | NodeKind::GoMethod
        | NodeKind::PythonMethod
        | NodeKind::TypescriptMethod
        | NodeKind::JavaMethod => &["method", "fn"],
        NodeKind::DartFunction
        | NodeKind::SwiftFunction
        | NodeKind::GoFunction
        | NodeKind::PythonFunction
        | NodeKind::TypescriptFunction => &["function", "fn"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enumerate every NodeKind so the matrix tests below scream the
    /// moment a new kind is added without updating `language_traits`.
    const ALL_KINDS: &[NodeKind] = &[
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
    ];

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
        // to update ALL_KINDS fails this test loudly.
        assert_eq!(
            ALL_KINDS.len(),
            42,
            "ALL_KINDS missing a NodeKind variant. Add it to the slice and to every predicate arm."
        );
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
}
