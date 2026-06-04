//! P23.5 — Swift language spec for the generic tree-sitter driver.
//!
//! Owns `.swift` and is the **sole structural backend** for Swift:
//! classes / structs / enums / actors / protocols, their methods,
//! initializers and deinitializers, free functions, `XCTest` /
//! swift-testing cases, and `import` declarations all flow from here.
//! Output is tagged `indexer = swift_treesitter`.
//!
//! `sourcekit-lsp` is demoted to an optional Tier-3 enrichment that only
//! overlays `Calls` / `References` by the same symbol id (see
//! [`crate::swift_indexer`]).
//!
//! The alex-pinkus grammar is irregular; the discriminators below were
//! confirmed empirically against the compiled grammar:
//! - `class`, `struct`, `enum`, `actor`, **and** `extension` all parse to
//!   `class_declaration`. They are told apart by the leading keyword token
//!   (a direct child). `enum` additionally carries an `enum_class_body`.
//! - An `extension`'s name is a `user_type` (vs a `type_identifier` for the
//!   others); it is handled like a Rust `impl` block via [`swift_extension_type`]
//!   so its members nest under the extended type without emitting a
//!   duplicate type node.
//! - `init` / `deinit` declarations have no `name` field; their names are
//!   the literals `init` / `deinit` ([`swift_name_of`]).
//! - `@Test` / `@Suite` (swift-testing) live under `modifiers → attribute`;
//!   `XCTest` cases are `test*` methods with no parameters.

use crate::treesitter::{
    body_from_field, name_from_field, no_call_test, no_src_roots, no_text, node_text, normalise_ws,
    LangSpec, RefKind, SymKind, TestKind, MAX_NESTING_DEPTH,
};
use specslice_core::NodeKind;

pub(crate) fn swift_language() -> tree_sitter::Language {
    tree_sitter_swift::LANGUAGE.into()
}

/// The leading declaration keyword (`class` / `struct` / `enum` / `actor`
/// / `extension`) of a `class_declaration`, skipping any `modifiers`.
fn swift_decl_keyword<'a>(node: tree_sitter::Node<'a>) -> Option<&'a str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let kw @ ("class" | "struct" | "enum" | "actor" | "extension") = child.kind() {
            return Some(kw);
        }
    }
    None
}

/// Reduce a (possibly dotted / generic) type reference to its bare name:
/// `Swift.Array<Int>` → `Array`, `Greeter` → `Greeter`.
fn swift_bare(text: &str) -> Option<String> {
    let before_generics = text.split('<').next().unwrap_or(text);
    let bare = before_generics
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(before_generics)
        .trim();
    (!bare.is_empty()).then(|| bare.to_string())
}

fn swift_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        "protocol_declaration" => Some(SymKind::Type(NodeKind::SwiftProtocol)),
        "class_declaration" => match swift_decl_keyword(node) {
            Some("struct") => Some(SymKind::Type(NodeKind::SwiftStruct)),
            Some("enum") => Some(SymKind::Type(NodeKind::SwiftEnum)),
            // `actor` is a reference type like `class`; no dedicated kind.
            Some("class") | Some("actor") => Some(SymKind::Type(NodeKind::SwiftClass)),
            // `extension` is handled as an impl-like block, not a container.
            _ => None,
        },
        _ => None,
    }
}

/// An `extension Foo { … }` is structurally like a Rust `impl`: it owns no
/// new type, but its members belong to the extended type. Returns that
/// type's bare name so the driver nests the members under it.
fn swift_extension_type(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() != "class_declaration" || swift_decl_keyword(node) != Some("extension") {
        return None;
    }
    let name = node.child_by_field_name("name")?;
    swift_bare(node_text(name, src)?)
}

fn swift_is_callable(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "protocol_function_declaration"
            | "init_declaration"
            | "deinit_declaration"
    )
}

/// Initializers keep their own [`NodeKind`]; `deinit` and ordinary
/// functions stay whatever the driver chose (method vs free function).
fn swift_callable_kind(node: tree_sitter::Node<'_>, _src: &[u8], default: NodeKind) -> NodeKind {
    if node.kind() == "init_declaration" {
        NodeKind::SwiftInitializer
    } else {
        default
    }
}

/// `init` / `deinit` have no `name` field; everything else uses the first
/// `name` field reduced to its bare identifier.
fn swift_name_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "init_declaration" => Some("init".to_string()),
        "deinit_declaration" => Some("deinit".to_string()),
        _ => name_from_field(node, src).and_then(|n| swift_bare(&n)),
    }
}

/// Collect the bare attribute heads attached via a declaration's
/// `modifiers` child: `@Test` → `Test`, `@MainActor` → `MainActor`,
/// `@Test("x")` → `Test`.
fn swift_attribute_heads(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut mc = child.walk();
        for m in child.named_children(&mut mc) {
            if m.kind() == "attribute" {
                if let Some(text) = node_text(m, src) {
                    let head = text.trim_start_matches('@');
                    if let Some(bare) = swift_bare(head.split(['(', ' ']).next().unwrap_or(head)) {
                        out.push(bare);
                    }
                }
            }
        }
    }
    out
}

/// True when a callable declares no parameters (XCTest cases take none).
fn swift_has_no_parameters(node: tree_sitter::Node<'_>) -> bool {
    let mut cursor = node.walk();
    let has_param = node
        .named_children(&mut cursor)
        .any(|c| c.kind() == "parameter");
    !has_param
}

/// A method whose name is the XCTest case convention: `test` followed by a
/// non-lowercase rune (`testFoo`, `test_x`, `test1`) — never bare `test`,
/// `tests`, or `testing`.
fn is_xctest_name(name: &str) -> bool {
    name.strip_prefix("test")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| !c.is_ascii_lowercase())
}

/// Reclassify a declaration as a test:
/// - swift-testing: any callable annotated `@Test` is a case; any container
///   annotated `@Suite` is a group.
/// - XCTest: a parameter-less `test*` method (we cannot see the
///   `XCTestCase` superclass from here, so we approximate with XCTest's own
///   discovery rule — prefix + zero args — which is exactly what its ObjC
///   runtime uses).
fn swift_test_of(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    kind: NodeKind,
    name: &str,
    parent_qualified: Option<&str>,
) -> Option<TestKind> {
    let attrs = swift_attribute_heads(node, src);
    if matches!(
        kind,
        NodeKind::SwiftClass | NodeKind::SwiftStruct | NodeKind::SwiftEnum
    ) {
        return attrs
            .iter()
            .any(|a| a == "Suite")
            .then_some(TestKind::Group);
    }
    if matches!(kind, NodeKind::SwiftMethod | NodeKind::SwiftFunction) {
        if attrs.iter().any(|a| a == "Test") {
            return Some(TestKind::Case);
        }
        if kind == NodeKind::SwiftMethod
            && parent_qualified.is_some()
            && is_xctest_name(name)
            && swift_has_no_parameters(node)
        {
            return Some(TestKind::Case);
        }
    }
    None
}

/// Extract the imported module path from an `import_declaration`, stripping
/// any `@testable` attribute, the `import` keyword, and an optional
/// import-kind keyword (`import class UIKit.UIView` → `UIKit.UIView`).
fn swift_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "import_declaration" {
        return Vec::new();
    }
    let Some(text) = node_text(node, src) else {
        return Vec::new();
    };
    let mut t = text.trim();
    // Drop any leading attribute(s) like `@testable`.
    while let Some(stripped) = t.strip_prefix('@') {
        t = stripped
            .split_once(char::is_whitespace)
            .map(|(_, rest)| rest.trim_start())
            .unwrap_or("");
    }
    t = t.strip_prefix("import").unwrap_or(t).trim_start();
    for kw in [
        "typealias",
        "struct",
        "class",
        "enum",
        "protocol",
        "func",
        "let",
        "var",
    ] {
        if let Some(rest) = t.strip_prefix(kw) {
            if rest.starts_with(char::is_whitespace) {
                t = rest.trim_start();
                break;
            }
        }
    }
    let cleaned = normalise_ws(t);
    if cleaned.is_empty() {
        Vec::new()
    } else {
        vec![cleaned]
    }
}

/// Swift `import`s name *modules* (build targets), not files: one module
/// maps to many files with no source-level path. There is therefore no
/// sound file-to-file edge to draw, so every import resolves to `None`
/// (recorded for the count, never a dangling node). A future SCIP / module
/// map could refine this; until then resolution is intentionally deferred.
fn swift_resolve_import(
    _raw: &str,
    _from_file: &str,
    _all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    None
}

/// Heuristic outbound call identifiers from a Swift callable body (see
/// [`crate::treesitter::resolve_heuristic_refs`]). Captures:
/// - `helper()` / `Greeter()` → `Call` to `helper` / the type `Greeter`
///   (Swift construction *is* a call, so the type becomes reachable).
/// - `obj.method()` / `self.method()` → `Call` to the trailing navigation
///   suffix name (links to a same-file method).
///
/// Swift `import`s name *modules*, not files, so cross-file resolution is
/// intentionally out of reach (see [`swift_resolve_import`]); same-file
/// links are still captured and feed reachability within a file / type.
fn swift_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_swift_calls(body, src, &mut out, 0);
    out
}

fn collect_swift_calls(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    out: &mut Vec<(String, RefKind)>,
    depth: usize,
) {
    if depth > MAX_NESTING_DEPTH {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "call_expression" {
            if let Some(name) = child
                .named_child(0)
                .and_then(|callee| swift_callee_name(callee, src))
            {
                out.push((name, RefKind::Call));
            }
        }
        collect_swift_calls(child, src, out, depth + 1);
    }
}

/// Best-effort callee name for a Swift `call_expression`'s callee node.
fn swift_callee_name(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "simple_identifier" => node_text(node, src).and_then(swift_bare),
        // `obj.member(...)` / `self.member(...)` → the trailing identifier
        // of the navigation suffix.
        "navigation_expression" => {
            let suffix = node.child_by_field_name("suffix").or_else(|| {
                let mut c = node.walk();
                let found = node
                    .named_children(&mut c)
                    .find(|n| n.kind() == "navigation_suffix");
                found
            })?;
            let mut c = suffix.walk();
            let id = suffix
                .named_children(&mut c)
                .find(|n| n.kind() == "simple_identifier");
            id.and_then(|id| node_text(id, src)).and_then(swift_bare)
        }
        "prefix_expression" | "postfix_expression" | "tuple_expression" => node
            .named_child(0)
            .and_then(|inner| swift_callee_name(inner, src)),
        _ => None,
    }
}

pub(crate) static SWIFT_SPEC: LangSpec = LangSpec {
    language_id: "swift",
    grammar: swift_language,
    extensions: &["swift"],
    skip_dirs: &[
        ".git",
        ".build",
        "Pods",
        ".swiftpm",
        "DerivedData",
        "build",
        "node_modules",
    ],
    separator: ".",
    func_kind: NodeKind::SwiftFunction,
    method_kind: NodeKind::SwiftMethod,
    container_of: swift_container_of,
    is_callable_kind: swift_is_callable,
    callable_kind_of: swift_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: swift_extension_type,
    receiver_type_of: no_text,
    import_of: swift_import_of,
    name_of: swift_name_of,
    body_of: body_from_field,
    is_transparent_kind: crate::treesitter::never,
    metadata_of: no_text,
    test_of: swift_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: swift_resolve_import,
    recurse_callables: false,
    call_idents_of: swift_call_idents,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&SWIFT_SPEC, src)
    }
    fn qnames(scan: &Scan, kind: NodeKind) -> Vec<String> {
        scan.symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.qualified_name.clone())
            .collect()
    }
    fn refs(scan: &Scan) -> Vec<(String, String, RefKind)> {
        scan.references
            .iter()
            .map(|r| (r.from_qualified.clone(), r.to_name.clone(), r.kind))
            .collect()
    }

    #[test]
    fn captures_bare_navigation_and_construction_calls() {
        let src = r#"
class Greeter {
    func greet() -> String { return build() }
    func build() -> String { return "hi" }
}

func run() {
    let g = Greeter()
    _ = g.greet()
}
"#;
        let got = refs(&scan(src));
        assert!(
            got.contains(&("Greeter.greet".into(), "build".into(), RefKind::Call)),
            "bare same-type call: {got:?}"
        );
        assert!(
            got.contains(&("run".into(), "Greeter".into(), RefKind::Call)),
            "construction is a call to the type: {got:?}"
        );
        assert!(
            got.contains(&("run".into(), "greet".into(), RefKind::Call)),
            "navigation member call → trailing name: {got:?}"
        );
    }

    #[test]
    fn classes_structs_enums_protocols_keep_distinct_kinds() {
        let src = r#"
class Greeter {
    let name: String
    init(name: String) { self.name = name }
    func greet() -> String { return name }
    deinit {}
}

struct Point { var x: Int; func mag() -> Int { x } }

enum Status {
    case active
    func isLive() -> Bool { true }
}

protocol Walker { func walk() }
"#;
        let s = scan(src);
        assert_eq!(
            qnames(&s, NodeKind::SwiftClass),
            vec!["Greeter".to_string()]
        );
        assert_eq!(qnames(&s, NodeKind::SwiftStruct), vec!["Point".to_string()]);
        assert_eq!(qnames(&s, NodeKind::SwiftEnum), vec!["Status".to_string()]);
        assert_eq!(
            qnames(&s, NodeKind::SwiftProtocol),
            vec!["Walker".to_string()]
        );

        let methods = qnames(&s, NodeKind::SwiftMethod);
        assert!(
            methods.contains(&"Greeter.greet".to_string()),
            "{methods:?}"
        );
        assert!(methods.contains(&"Point.mag".to_string()), "{methods:?}");
        assert!(
            methods.contains(&"Status.isLive".to_string()),
            "enum methods nest through enum_class_body: {methods:?}"
        );
        assert!(
            methods.contains(&"Greeter.deinit".to_string()),
            "deinit is a method-like member: {methods:?}"
        );
        // Protocol requirement methods nest under the protocol.
        assert!(methods.contains(&"Walker.walk".to_string()), "{methods:?}");

        // The initializer keeps its own kind, nested under its type.
        assert_eq!(
            qnames(&s, NodeKind::SwiftInitializer),
            vec!["Greeter.init".to_string()]
        );
    }

    #[test]
    fn extension_members_nest_under_the_extended_type_without_a_duplicate() {
        let s = scan("extension Greeter {\n  func bye() -> String { \"bye\" }\n}\n");
        // No new type node for the extension itself.
        assert!(qnames(&s, NodeKind::SwiftClass).is_empty());
        assert!(qnames(&s, NodeKind::SwiftStruct).is_empty());
        // The method attaches to the extended type as a method.
        assert_eq!(
            qnames(&s, NodeKind::SwiftMethod),
            vec!["Greeter.bye".to_string()]
        );
    }

    #[test]
    fn free_functions_are_functions_not_methods() {
        let s = scan("func top() {}\nfunc add(a: Int, b: Int) -> Int { a + b }\n");
        let funcs = qnames(&s, NodeKind::SwiftFunction);
        assert!(funcs.contains(&"top".to_string()), "{funcs:?}");
        assert!(funcs.contains(&"add".to_string()), "{funcs:?}");
        assert!(qnames(&s, NodeKind::SwiftMethod).is_empty());
    }

    #[test]
    fn xctest_and_swift_testing_cases_are_detected() {
        let src = r#"
import XCTest
import Testing

class FooTests: XCTestCase {
    func testThing() {}
    func testValue(x: Int) {}   // has a param → not an XCTest case
    func helper() {}
}

@Test func standalone() {}

@Suite struct MySuite {
    @Test func inside() {}
}
"#;
        let s = scan(src);
        let cases: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(cases.contains(&"FooTests.testThing"), "{cases:?}");
        assert!(cases.contains(&"standalone"), "swift-testing: {cases:?}");
        assert!(cases.contains(&"MySuite.inside"), "{cases:?}");
        // `testValue(x:)` takes a parameter, so it is not an XCTest case.
        assert!(!cases.contains(&"FooTests.testValue"), "{cases:?}");

        let groups: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Group)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(groups.contains(&"MySuite"), "@Suite is a group: {groups:?}");

        // `helper` and the param-bearing `testValue` remain structural methods.
        let methods = qnames(&s, NodeKind::SwiftMethod);
        assert!(
            methods.contains(&"FooTests.helper".to_string()),
            "{methods:?}"
        );
        assert!(
            methods.contains(&"FooTests.testValue".to_string()),
            "{methods:?}"
        );
    }

    #[test]
    fn imports_strip_keywords_and_attributes() {
        let s = scan(
            "import Foundation\n@testable import MyMod\nimport class UIKit.UIView\nimport func Mod.helper\n",
        );
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(imports.contains(&"Foundation"), "{imports:?}");
        assert!(
            imports.contains(&"MyMod"),
            "@testable stripped: {imports:?}"
        );
        assert!(
            imports.contains(&"UIKit.UIView"),
            "import-kind keyword stripped: {imports:?}"
        );
        assert!(imports.contains(&"Mod.helper"), "{imports:?}");
        // Module imports never resolve to repo files (no dangling edges).
        assert_eq!(
            swift_resolve_import("Foundation", "Sources/App/main.swift", &[], &[]),
            None
        );
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("class class class { @ @ func (((");
        let _ = scan("이름 클래스 { func 메서드() {} }");
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use crate::treesitter::extract;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn never_panics_and_is_deterministic(s in ".*") {
            prop_assert_eq!(extract(&SWIFT_SPEC, &s), extract(&SWIFT_SPEC, &s));
        }

        #[test]
        fn symbols_are_well_formed(s in ".*") {
            for sym in extract(&SWIFT_SPEC, &s).symbols {
                prop_assert!(!sym.name.is_empty());
                prop_assert!(!sym.qualified_name.is_empty());
                prop_assert!(sym.end_line >= sym.start_line);
            }
        }
    }
}
