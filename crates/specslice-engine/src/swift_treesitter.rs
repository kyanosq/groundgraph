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
            // A *computed* property is an accessor; we treat it as a callable
            // and let `swift_name_of` return `None` for stored properties so
            // they are skipped (the driver drops nameless callables).
            | "property_declaration"
    )
}

/// The bare name of a *computed* property (`var x: T { … }` / `var x: T { get … }`),
/// or `None` for a stored property. Computed properties carry a
/// `computed_value` field (a `computed_property` node); stored properties —
/// including `didSet`/`willSet`-observed ones — do not.
fn swift_computed_property_name(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    node.child_by_field_name("computed_value")?;
    let pattern = node.child_by_field_name("name")?;
    let ident = first_simple_identifier(pattern, src)?;
    swift_bare(&ident)
}

/// First `simple_identifier` text within a (small) pattern subtree.
fn first_simple_identifier(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() == "simple_identifier" {
        return node_text(node, src).map(str::to_string);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = first_simple_identifier(child, src) {
            return Some(found);
        }
    }
    None
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
        "property_declaration" => swift_computed_property_name(node, src),
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
    // Computed properties are accessors, never test cases — even if named
    // `testFoo` (which would otherwise match the XCTest prefix heuristic).
    if node.kind() == "property_declaration" {
        return None;
    }
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
        match child.kind() {
            "call_expression" => {
                if let Some(name) = child
                    .named_child(0)
                    .and_then(|callee| swift_callee_name(callee, src))
                {
                    out.push((name, RefKind::Call));
                }
            }
            // A type name used in a value position — a stored-property /
            // local annotation, an `as?` cast, a generic argument, or the
            // element of a `[T]` / `T?` type — is a *reference* to that type.
            // This is the only link to models created by reflection
            // (ObjectMapper / Codable) and to cells registered by metatype,
            // which never appear as a construction call.
            "type_identifier" => {
                if let Some(name) = node_text(child, src).and_then(swift_bare) {
                    out.push((name, RefKind::Reference));
                }
            }
            // A navigation whose *base* is an upper-camel-case identifier is a
            // reference to a type or enum: `FooCell.self` (metatype),
            // `ScreenManager.shared` (singleton), `Status.active` (enum case),
            // `Factory.make()` (static call). The base parses as an expression
            // identifier, not a `type_identifier`, so it needs its own arm.
            // Lower-camel bases (`obj.method`, `self.x`) are instance access
            // and are skipped — the method side is already a `Call`.
            "navigation_expression" => {
                if let Some(name) = child
                    .child_by_field_name("target")
                    .filter(|t| t.kind() == "simple_identifier")
                    .and_then(|t| node_text(t, src))
                    .and_then(swift_bare)
                {
                    if name.chars().next().is_some_and(|c| c.is_uppercase()) {
                        out.push((name, RefKind::Reference));
                    }
                }
            }
            _ => {}
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

/// Serialise a type declaration's inheritance clause — superclass plus
/// adopted protocols — as `{"swift_inherits":["UIViewController","Foo"]}`.
///
/// This is the only static signal of *what instantiates a type*: UIKit /
/// AppKit / SwiftUI create `UIViewController`, `UIView` and cell subclasses
/// from storyboards, XIBs, `register(_:forCellReuseIdentifier:)`, segues and
/// the `@UIApplicationMain` responder chain, none of which leave an in-repo
/// call edge. The dead-code pass reads this list to keep framework-owned
/// types reachable instead of flagging them as orphans (see
/// `dead_code::swift_framework_instantiated_types`).
///
/// `enum` and `extension` declarations are skipped: an enum is never
/// framework-instantiated through its raw-value/base, and an extension's
/// conformances belong to a type that already carries its own clause.
fn swift_metadata(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() != "class_declaration" {
        return None;
    }
    match swift_decl_keyword(node) {
        Some("class") | Some("struct") | Some("actor") => {}
        _ => return None,
    }
    let mut inherits: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "inheritance_specifier" {
            continue;
        }
        let from = child
            .child_by_field_name("inherits_from")
            .or_else(|| child.named_child(0));
        if let Some(name) = from.and_then(|n| node_text(n, src)).and_then(swift_bare) {
            if !inherits.contains(&name) {
                inherits.push(name);
            }
        }
    }
    if inherits.is_empty() {
        return None;
    }
    // Bare identifiers need no JSON escaping; emit by hand so the structural
    // scanner stays free of a serde dependency.
    let body = inherits
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("{{\"swift_inherits\":[{body}]}}"))
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
    metadata_of: swift_metadata,
    test_of: swift_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: swift_resolve_import,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: swift_call_idents,
    // Swift has a flat per-module namespace and no file→file imports, so a
    // bare type/constructor name resolves module-wide (unique-file only).
    module_scoped_resolution: true,
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
    fn computed_properties_are_methods_stored_are_not() {
        // A computed property is an accessor (a getter), so it is emitted as a
        // method — this is what lets a Dart `String get displayName` getter
        // match its Swift `var displayName: String { … }` port. Stored
        // properties (incl. `didSet`-observed ones) are data, not symbols.
        let src = r#"
struct Shift {
    let id: Int
    var name: String = "x"
    var counter: Int = 0 { didSet { } }
    var displayName: String { "Shift \(name)" }
    var durationSeconds: Double {
        get { 3600 }
    }
    func mag() -> Int { id }
}

var globalComputed: Int { 42 }
let globalStored = 7
"#;
        let s = scan(src);
        let methods = qnames(&s, NodeKind::SwiftMethod);
        let funcs = qnames(&s, NodeKind::SwiftFunction);
        assert!(
            methods.contains(&"Shift.displayName".to_string()),
            "expression-bodied computed property is a method: {methods:?}"
        );
        assert!(
            methods.contains(&"Shift.durationSeconds".to_string()),
            "getter-block computed property is a method: {methods:?}"
        );
        // Stored properties (plain + observed) must never become callables.
        for stored in ["Shift.id", "Shift.name", "Shift.counter"] {
            assert!(
                !methods.contains(&stored.to_string()),
                "stored property must not be a method: {stored} in {methods:?}"
            );
        }
        // A global computed var is a free accessor → function; stored → nothing.
        assert!(
            funcs.contains(&"globalComputed".to_string()),
            "global computed var: {funcs:?}"
        );
        assert!(!funcs.contains(&"globalStored".to_string()), "{funcs:?}");
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
    fn type_declarations_capture_inheritance_clause_as_metadata() {
        // The inheritance clause is the only structural signal of *what
        // instantiates a type*. UIKit creates `UIViewController` / cell
        // subclasses from storyboards & XIBs with no in-repo caller, so the
        // dead-code pass needs the supertype list to keep them reachable.
        let s = scan("class FooVC: UIViewController, FooProto {\n  func viewDidLoad() {}\n}\n");
        let sym = s
            .symbols
            .iter()
            .find(|x| x.kind == NodeKind::SwiftClass && x.qualified_name == "FooVC")
            .expect("class node");
        let meta = sym.metadata.as_deref().unwrap_or("");
        assert!(meta.contains("swift_inherits"), "metadata: {meta:?}");
        assert!(meta.contains("UIViewController"), "metadata: {meta:?}");
        assert!(meta.contains("FooProto"), "metadata: {meta:?}");

        // Generic / dotted bases reduce to their bare name.
        let s2 = scan("final class Cell: UIKit.UITableViewCell {}\n");
        let cell = s2
            .symbols
            .iter()
            .find(|x| x.qualified_name == "Cell")
            .unwrap();
        assert!(
            cell.metadata
                .as_deref()
                .unwrap_or("")
                .contains("UITableViewCell"),
            "dotted base bare name: {:?}",
            cell.metadata
        );

        // A type with no inheritance clause carries no metadata (so the
        // dead-code pass never treats a plain value type as framework-owned).
        let s3 = scan("struct Plain { let x: Int }\n");
        let plain = s3
            .symbols
            .iter()
            .find(|x| x.qualified_name == "Plain")
            .unwrap();
        assert_eq!(plain.metadata, None, "plain struct has no supertypes");
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
    fn captures_type_references_in_annotations_metatypes_and_casts() {
        // Models decoded by reflection (ObjectMapper / Codable) and cells
        // registered by metatype have no construction call, so without
        // type-position references they look dead. Capture type names used as
        // annotations, `[T].self` metatypes, `T.self` and `as?` casts.
        let src = r#"
class Holder {
    var items: [WeeksModel] = []
}
func decode(_ d: Decoder) {
    let a = d.mapModels([ExpressCompany].self)
    tableView.register(FooCell.self, forCellReuseIdentifier: "x")
    let z = something as? BarCell
    ScreenManager.shared.refresh()
}
"#;
        let got = refs(&scan(src));
        assert!(
            got.contains(&("Holder".into(), "WeeksModel".into(), RefKind::Reference)),
            "stored-property type annotation → reference on the owning type: {got:?}"
        );
        assert!(
            got.iter()
                .any(|(_, t, k)| t == "ExpressCompany" && *k == RefKind::Reference),
            "metatype inside a generic mapping call: {got:?}"
        );
        assert!(
            got.iter()
                .any(|(_, t, k)| t == "FooCell" && *k == RefKind::Reference),
            "`T.self` metatype argument: {got:?}"
        );
        assert!(
            got.iter()
                .any(|(_, t, k)| t == "BarCell" && *k == RefKind::Reference),
            "`as?` cast target: {got:?}"
        );
        assert!(
            got.iter()
                .any(|(_, t, k)| t == "ScreenManager" && *k == RefKind::Reference),
            "`Type.shared` static-member access references the type: {got:?}"
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
