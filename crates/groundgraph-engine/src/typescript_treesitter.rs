//! P22/P23.2 — TypeScript language spec for the generic tree-sitter driver.
//!
//! Since the P23 收敛 this is the **sole structural backend** for TypeScript:
//! [`TYPESCRIPT_SPEC`] owns `.ts` / `.mts` / `.cts` and [`TSX_SPEC`] owns
//! `.tsx` (same grammar crate, JSX-aware dialect). Both share one
//! `language_id` (`typescript`) so symbols, ids, and the `typescript_treesitter`
//! indexer name are uniform across the two passes. The hooks recover:
//!
//! - jest / vitest tests (`describe` / `it` / `test`, incl. `it.only` …),
//! - ESM imports resolved to repo-relative file ids (`./x` → `src/x.ts`,
//!   folder `index` files, and cross-extension `.ts` ↔ `.tsx`).
//!
//! The `typescript-language-server` adapter is demoted to an optional Tier-3
//! overlay that only contributes `Calls` / `References` edges.

use crate::treesitter::{
    body_from_field, keep_callable_kind, name_from_field, no_src_roots, no_test_of, no_text,
    node_text, strip_quotes, CallTestHit, LangSpec, RefKind, SymKind, TestKind, MAX_NESTING_DEPTH,
};
use groundgraph_core::NodeKind;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn tsx_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

fn ts_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        "class_declaration" | "abstract_class_declaration" => {
            Some(SymKind::Type(NodeKind::TypescriptClass))
        }
        "interface_declaration" => Some(SymKind::Type(NodeKind::TypescriptInterface)),
        "enum_declaration" => Some(SymKind::Type(NodeKind::TypescriptEnum)),
        "internal_module" | "module" => Some(SymKind::Module(NodeKind::TypescriptModule)),
        _ => None,
    }
}

fn ts_is_callable(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "generator_function_declaration"
            | "function_signature"
            | "method_definition"
            | "method_signature"
            | "abstract_method_signature"
            // `const NAME = (…) => {…}` / `const NAME = function(){}` — the
            // dominant React/JS pattern. The declarator is only treated as a
            // function symbol when its value is a function expression (gated in
            // `ts_name_of`); other declarators decline a name and fall through
            // to the reference collector.
            | "variable_declarator"
    )
}

/// Names a `variable_declarator` only when it binds a function expression to a
/// plain identifier (`const Foo = () => {}` / `const f = function(){}`), so
/// literal / call / destructuring consts never masquerade as functions. Every
/// other callable kind keeps the standard `name` field.
fn ts_name_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() == "variable_declarator" {
        let value = node.child_by_field_name("value")?;
        if !matches!(
            value.kind(),
            "arrow_function" | "function_expression" | "function"
        ) {
            return None;
        }
        let name = node.child_by_field_name("name")?;
        if name.kind() != "identifier" {
            return None; // object/array destructuring pattern — not a symbol.
        }
        return node_text(name, src).map(str::to_string);
    }
    name_from_field(node, src)
}

/// Body of a callable. For a function-valued `variable_declarator` the body
/// lives one level down in the arrow / function expression, so call extraction
/// and the line span reach into the actual function body.
fn ts_body_of(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    if node.kind() == "variable_declarator" {
        return node
            .child_by_field_name("value")
            .and_then(|v| v.child_by_field_name("body"));
    }
    body_from_field(node)
}

fn ts_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    match node.kind() {
        // `import ... from "x"` and re-export `export ... from "x"`.
        "import_statement" | "export_statement" => node
            .child_by_field_name("source")
            .and_then(|n| node_text(n, src))
            .map(strip_quotes)
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn ts_is_transparent(kind: &str) -> bool {
    // `namespace X {}` parses as `expression_statement → internal_module`,
    // so we must descend through expression statements to reach it.
    //
    // `object` / `pair` are descended so object-literal *shorthand methods*
    // become symbols. This is what makes Vue 2 Options API components legible:
    // `export default { data() {…}, methods: { foo() {…} } }` nests its
    // business methods two object levels deep (`export_statement → object →
    // pair(methods) → object → method_definition`). Only named `method_definition`
    // nodes are emitted while descending; arrow/function-expression *values*
    // (`foo: () => {}`) carry no name and stay refs-only, so the extra descent
    // adds real methods without flooding the graph with anonymous callbacks.
    matches!(
        kind,
        "export_statement"
            | "ambient_declaration"
            | "expression_statement"
            | "object"
            | "pair"
            // Descend `const`/`let`/`var` declarations so a function-valued
            // `variable_declarator` inside is reached and emitted as a symbol.
            | "lexical_declaration"
            | "variable_declaration"
    )
}

/// Resolve a jest/vitest callee to its root identifier, so `it`, `it.only`,
/// `describe.skip`, `test.concurrent`, … all classify by their base name.
fn ts_callee_root<'a>(func: tree_sitter::Node<'_>, src: &'a [u8]) -> Option<&'a str> {
    match func.kind() {
        "identifier" => node_text(func, src),
        // `it.only` / `describe.each` → recurse into the object side.
        "member_expression" => ts_callee_root(func.child_by_field_name("object")?, src),
        _ => None,
    }
}

/// Detect a call-based test/group: `describe(...)` → group, `it(...)` /
/// `test(...)` → case. Returns the suite name (first string arg) and the
/// callback body so the driver can recurse into nested cases.
fn ts_call_test_of<'a>(node: tree_sitter::Node<'a>, src: &[u8]) -> Option<CallTestHit<'a>> {
    if node.kind() != "call_expression" {
        return None;
    }
    let func = node.child_by_field_name("function")?;
    let kind = match ts_callee_root(func, src)? {
        "describe" | "suite" | "context" => TestKind::Group,
        "it" | "test" | "bench" => TestKind::Case,
        _ => return None,
    };
    let args = node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut name = String::new();
    let mut body = None;
    for arg in args.named_children(&mut cursor) {
        match arg.kind() {
            "string" | "template_string" if name.is_empty() => {
                if let Some(t) = node_text(arg, src) {
                    name = strip_quotes(t);
                }
            }
            "arrow_function" | "function_expression" | "function" => {
                body = arg.child_by_field_name("body");
            }
            _ => {}
        }
    }
    Some(CallTestHit { kind, name, body })
}

/// Resolve a relative ESM specifier (`./x`, `../y/z`) to a repo-relative
/// file path, probing the common implicit extensions and folder `index`
/// files against the resolution universe. Bare npm specifiers (`react`)
/// return `None` so the external `Imports` edge is dropped.
fn ts_resolve_import(
    raw: &str,
    from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let spec = raw.trim();
    if !spec.starts_with('.') && !spec.starts_with('/') {
        return None; // bare npm specifier — external.
    }
    let source_dir = std::path::Path::new(from_file)
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let joined = source_dir.join(spec);
    let mut canonical = String::new();
    for comp in joined.components() {
        use std::path::Component;
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(idx) = canonical.rfind('/') {
                    canonical.truncate(idx);
                } else {
                    canonical.clear();
                }
            }
            Component::Normal(part) => {
                if !canonical.is_empty() {
                    canonical.push('/');
                }
                canonical.push_str(&part.to_string_lossy());
            }
            _ => {}
        }
    }
    let candidates = [
        canonical.clone(),
        format!("{canonical}.ts"),
        format!("{canonical}.tsx"),
        format!("{canonical}.mts"),
        format!("{canonical}.cts"),
        format!("{canonical}.js"),
        format!("{canonical}.jsx"),
        format!("{canonical}.mjs"),
        format!("{canonical}.cjs"),
        format!("{canonical}.vue"),
        format!("{canonical}/index.ts"),
        format!("{canonical}/index.tsx"),
        format!("{canonical}/index.js"),
        format!("{canonical}/index.jsx"),
        format!("{canonical}/index.vue"),
    ];
    candidates
        .into_iter()
        .find(|c| all_files.iter().any(|f| f == c))
}

/// Collect heuristic call / reference identifiers from a callable body so the
/// generic resolver can link them to same-file or imported symbols (medium
/// confidence). TypeScript has no `::` paths, so every name is a bare simple
/// name the resolver matches against the per-file symbol index:
///
/// - `helper()` → `Call` to `helper`.
/// - `this.greet()` / `obj.method()` → `Call` to the *property* name (links
///   to a same-file method of that name).
/// - `new Widget()` → `Reference` to the constructor identifier `Widget`.
///
/// Built-ins (`console.log`, `arr.map`, …) resolve to nothing because the
/// resolver only links names that exist as local / imported symbols, so the
/// noise floor stays bounded — exactly as for the Rust resolver.
fn ts_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_ts_calls(body, src, &mut out, 0);
    out
}

fn collect_ts_calls(
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
                if let Some(func) = child.child_by_field_name("function") {
                    if let Some(name) = ts_callee_name(func, src) {
                        out.push((name, RefKind::Call));
                    }
                }
            }
            // `new Widget()` → reference to the constructed type.
            "new_expression" => {
                if let Some(ctor) = child.child_by_field_name("constructor") {
                    if let Some(name) = ts_callee_name(ctor, src) {
                        out.push((name, RefKind::Reference));
                    }
                }
            }
            _ => {}
        }
        // Always descend: calls nest inside arguments, arrow bodies, JSX
        // expressions, etc. Nested arrow / function expressions fold into the
        // enclosing callable (they are not separately indexed symbols).
        collect_ts_calls(child, src, out, depth + 1);
    }
}

/// Best-effort callee name for a call / new expression's function node.
fn ts_callee_name(func: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" => node_text(func, src).map(str::to_string),
        // `x.method(...)` / `this.method(...)` → the trailing property name.
        "member_expression" => func
            .child_by_field_name("property")
            .and_then(|p| node_text(p, src))
            .map(str::to_string),
        // `foo<T>(...)` — the call grammar exposes the inner callee directly,
        // but defensively unwrap an instantiation expression if present.
        "instantiation_expression" | "non_null_expression" | "parenthesized_expression" => func
            .child_by_field_name("function")
            .or_else(|| func.named_child(0))
            .and_then(|inner| ts_callee_name(inner, src)),
        _ => None,
    }
}

/// Skip directories shared by the `.ts` and `.tsx` specs.
const TS_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    "coverage",
];

/// Build a TypeScript-family spec parameterised only by grammar +
/// extensions so the `.ts` and `.tsx` dialects stay byte-for-byte in sync.
const fn ts_family_spec(
    grammar: fn() -> tree_sitter::Language,
    extensions: &'static [&'static str],
) -> LangSpec {
    LangSpec {
        language_id: "typescript",
        grammar,
        extensions,
        skip_dirs: TS_SKIP_DIRS,
        separator: ".",
        func_kind: NodeKind::TypescriptFunction,
        method_kind: NodeKind::TypescriptMethod,
        container_of: ts_container_of,
        is_callable_kind: ts_is_callable,
        callable_kind_of: keep_callable_kind,
        callable_span_of: crate::treesitter::callable_node_is_span,
        impl_type_of: no_text,
        receiver_type_of: no_text,
        import_of: ts_import_of,
        name_of: ts_name_of,
        body_of: ts_body_of,
        is_transparent_kind: ts_is_transparent,
        metadata_of: no_text,
        test_of: no_test_of,
        call_test_of: ts_call_test_of,
        src_roots_of: no_src_roots,
        resolve_import: ts_resolve_import,
        recurse_callables: false,
        emit_nested_callables_with_metadata_only: false,
        call_idents_of: ts_call_idents,
        module_scoped_resolution: false,
        recurse_declined_callables: true,
        claims_path: None,
    }
}

pub(crate) static TYPESCRIPT_SPEC: LangSpec =
    ts_family_spec(ts_language, &["ts", "mts", "cts", "mjs", "cjs"]);

/// JSX-aware dialect: identical structure / hooks as [`TYPESCRIPT_SPEC`] but
/// backed by the JSX grammar. Owns `.tsx` plus plain JavaScript `.js` / `.jsx`
/// — the JSX grammar is a superset that parses non-JSX JS correctly, and many
/// real-world `.js` files embed JSX, so routing them here maximises coverage.
/// Shares `language_id = "typescript"` so every pass lands in the same node /
/// id / indexer namespace.
/// `.vue` Single-File Components are routed here too: their `<script>` blocks
/// are plain JS/JSX (Vue 2 Options API), which the JSX-superset grammar parses
/// correctly once the surrounding `<template>`/`<style>` is stripped upstream
/// (see `treesitter::preprocess_source`).
pub(crate) static TSX_SPEC: LangSpec = ts_family_spec(tsx_language, &["tsx", "js", "jsx", "vue"]);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&TYPESCRIPT_SPEC, src)
    }
    fn qnames(scan: &Scan, kind: NodeKind) -> Vec<String> {
        scan.symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.qualified_name.clone())
            .collect()
    }

    #[test]
    fn classes_methods_interfaces_enums_and_namespaces() {
        let src = r#"
import { Foo } from "./foo";
export class Animal {
  constructor(name: string) {}
  speak(): string { return "rawr"; }
}
export interface Named { name(): string; }
export enum Color { Red, Green }
export function helper(): void {}
namespace Geo {
  export function area(): number { return 0; }
}
"#;
        let s = scan(src);
        assert_eq!(qnames(&s, NodeKind::TypescriptClass), vec!["Animal"]);
        let methods = qnames(&s, NodeKind::TypescriptMethod);
        assert!(methods.contains(&"Animal.speak".to_string()), "{methods:?}");
        assert_eq!(qnames(&s, NodeKind::TypescriptInterface), vec!["Named"]);
        assert!(
            qnames(&s, NodeKind::TypescriptMethod).contains(&"Named.name".to_string()),
            "interface methods should nest under the interface"
        );
        assert_eq!(qnames(&s, NodeKind::TypescriptEnum), vec!["Color"]);
        assert!(qnames(&s, NodeKind::TypescriptFunction).contains(&"helper".to_string()));
        assert!(qnames(&s, NodeKind::TypescriptModule).contains(&"Geo".to_string()));
        assert!(
            qnames(&s, NodeKind::TypescriptFunction).contains(&"Geo.area".to_string()),
            "namespaced function should qualify"
        );
        assert!(s.imports.iter().any(|i| i.path == "./foo"));
    }

    #[test]
    fn object_literal_methods_in_named_const_become_callables() {
        // `export const api = { async login() {} }` — the service-object module
        // shape behind most axios/fetch clients. The declarator is callable-kind
        // (so `const f = () => {}` works) but *declines* its name when the value
        // is an object, which previously stranded the methods two levels down
        // (`lexical_declaration → variable_declarator → object → method`). They
        // must surface as callables — matching `export default { … }` object
        // methods — so call graphs and route-consumer links reach them.
        let src = r#"
const http = makeClient();
export const api = {
  async login(body: LoginBody) { return http.post("/admin/login", body); },
  getUser(id: string) { return http.get(`/admin/users/${id}`); },
};
"#;
        let s = scan(src);
        let fns = qnames(&s, NodeKind::TypescriptFunction);
        assert!(
            fns.contains(&"login".to_string()),
            "object-literal method `login` should be a callable: {fns:?}"
        );
        assert!(
            fns.contains(&"getUser".to_string()),
            "object-literal method `getUser` should be a callable: {fns:?}"
        );
    }

    #[test]
    fn arrow_and_function_expression_consts_are_captured_as_functions() {
        // The dominant modern TS/JS pattern: components, hooks and utilities are
        // `const NAME = (…) => {…}` / `const NAME = function(){}`, not
        // `function NAME(){}`. Missing these made a 79-file React frontend report
        // only 64 symbols (zero components). The arrow body's calls must also be
        // attributed to the const so the call graph reaches into it.
        use crate::treesitter::RefKind;
        let src = r#"
import React from "react";
export const EditUser: React.FC<Props> = ({ user }) => {
  save(user);
  return null;
};
const helper = async () => { return 1; };
const legacy = function () { return 2; };
const NUM = 5;
const { a, b } = obj;
const memo = useMemo(() => 1, []);
function save(u: any) {}
"#;
        let s = extract(&TSX_SPEC, src);
        let fns = qnames(&s, NodeKind::TypescriptFunction);
        assert!(
            fns.contains(&"EditUser".to_string()),
            "arrow const component: {fns:?}"
        );
        assert!(
            fns.contains(&"helper".to_string()),
            "async arrow const: {fns:?}"
        );
        assert!(
            fns.contains(&"legacy".to_string()),
            "function-expression const: {fns:?}"
        );
        assert!(
            fns.contains(&"save".to_string()),
            "plain function still works: {fns:?}"
        );
        // Non-function consts must NOT be mistaken for functions.
        assert!(
            !fns.contains(&"NUM".to_string()),
            "literal const is not a function: {fns:?}"
        );
        assert!(
            !fns.contains(&"memo".to_string()),
            "call-valued const is not a function: {fns:?}"
        );
        assert!(
            !fns.iter().any(|n| n == "a" || n == "b"),
            "destructuring binding is not a function: {fns:?}"
        );
        // The arrow body's call is attributed to the const.
        let from_edit: Vec<&str> = s
            .references
            .iter()
            .filter(|r| r.from_qualified == "EditUser" && r.kind == RefKind::Call)
            .map(|r| r.to_name.as_str())
            .collect();
        assert!(
            from_edit.contains(&"save"),
            "arrow body call save(user) should be attributed to EditUser, got {from_edit:?}"
        );
    }

    #[test]
    fn string_literals_are_not_symbols_and_garbage_is_safe() {
        let s = scan(r#"const x = "class Fake {}"; function real() {}"#);
        assert_eq!(qnames(&s, NodeKind::TypescriptFunction), vec!["real"]);
        assert!(qnames(&s, NodeKind::TypescriptClass).is_empty());
        assert_eq!(scan(""), Scan::default());
        let _ = scan("class class class function (((");
    }

    #[test]
    fn captures_bare_and_member_call_identifiers() {
        use crate::treesitter::RefKind;
        let src = r#"
function helper(): void {}
export function run(): void {
  helper();
  this.greet();
}
"#;
        let s = scan(src);
        let from_run: Vec<&str> = s
            .references
            .iter()
            .filter(|r| r.from_qualified == "run" && r.kind == RefKind::Call)
            .map(|r| r.to_name.as_str())
            .collect();
        assert!(
            from_run.contains(&"helper"),
            "bare call helper() should be captured, got {from_run:?}"
        );
        assert!(
            from_run.contains(&"greet"),
            "member call this.greet() should be captured by property, got {from_run:?}"
        );
    }

    #[test]
    fn captures_constructor_references() {
        use crate::treesitter::RefKind;
        let src = r#"
class Widget {}
export function build(): void {
  const w = new Widget();
}
"#;
        let s = scan(src);
        let refs: Vec<&str> = s
            .references
            .iter()
            .filter(|r| r.from_qualified == "build" && r.kind == RefKind::Reference)
            .map(|r| r.to_name.as_str())
            .collect();
        assert!(
            refs.contains(&"Widget"),
            "new Widget() should record a reference to the class, got {refs:?}"
        );
    }

    fn test_qnames(scan: &Scan, kind: TestKind) -> Vec<String> {
        scan.tests
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.qualified_name.clone())
            .collect()
    }

    #[test]
    fn vitest_jest_describe_it_test_become_nested_tests() {
        let src = r#"
import { describe, it, test } from "vitest";
describe("greeter", () => {
  it("greets", () => {});
  test("falls back", () => {});
  it.only("focused", () => {});
  describe("nested", () => {
    it("deep", () => {});
  });
});
"#;
        let s = scan(src);
        let groups = test_qnames(&s, TestKind::Group);
        let cases = test_qnames(&s, TestKind::Case);
        assert!(groups.contains(&"greeter".to_string()), "{groups:?}");
        assert!(groups.contains(&"greeter.nested".to_string()), "{groups:?}");
        assert!(cases.contains(&"greeter.greets".to_string()), "{cases:?}");
        assert!(
            cases.contains(&"greeter.falls back".to_string()),
            "{cases:?}"
        );
        assert!(
            cases.contains(&"greeter.focused".to_string()),
            "it.only should classify as a case: {cases:?}"
        );
        assert!(
            cases.contains(&"greeter.nested.deep".to_string()),
            "deeply nested it() should qualify under both describes: {cases:?}"
        );
    }

    #[test]
    fn relative_imports_resolve_with_extensions_and_drop_bare() {
        let all = [
            "src/greeter.ts".to_string(),
            "src/util/index.ts".to_string(),
            "src/app/Widget.tsx".to_string(),
        ];
        // `.ts` sibling.
        assert_eq!(
            ts_resolve_import("./greeter", "src/index.ts", &all, &[]),
            Some("src/greeter.ts".into())
        );
        // Folder import → `index.ts`.
        assert_eq!(
            ts_resolve_import("./util", "src/index.ts", &all, &[]),
            Some("src/util/index.ts".into())
        );
        // Parent traversal + cross-extension `.ts` → `.tsx` (the `.tsx`
        // universe is supplied by the adapter via `resolution_paths`).
        assert_eq!(
            ts_resolve_import("../app/Widget", "src/sub/index.ts", &all, &[]),
            Some("src/app/Widget.tsx".into())
        );
        // Bare npm specifier → dropped.
        assert_eq!(ts_resolve_import("react", "src/index.ts", &all, &[]), None);
        // Unresolvable relative → dropped (never fabricate a node).
        assert_eq!(
            ts_resolve_import("./missing", "src/index.ts", &all, &[]),
            None
        );
    }

    #[test]
    fn tsx_spec_parses_components_and_keeps_test_hooks() {
        let src = r#"
import React from "react";
export function Button(props: { label: string }) {
  return <button>{props.label}</button>;
}
export class Panel extends React.Component {
  render() { return <div><Button label="x" /></div>; }
}
"#;
        let s = extract(&TSX_SPEC, src);
        assert!(qnames(&s, NodeKind::TypescriptFunction).contains(&"Button".to_string()));
        assert!(qnames(&s, NodeKind::TypescriptClass).contains(&"Panel".to_string()));
        assert!(
            qnames(&s, NodeKind::TypescriptMethod).contains(&"Panel.render".to_string()),
            "JSX-returning method should still parse"
        );
        // `react` is bare → no resolvable import edge, but the raw import is
        // still captured by the scanner.
        assert!(s.imports.iter().any(|i| i.path == "react"));
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
            prop_assert_eq!(extract(&TYPESCRIPT_SPEC, &s), extract(&TYPESCRIPT_SPEC, &s));
            prop_assert_eq!(extract(&TSX_SPEC, &s), extract(&TSX_SPEC, &s));
        }

        #[test]
        fn symbols_are_well_formed(s in ".*") {
            for spec in [&TYPESCRIPT_SPEC, &TSX_SPEC] {
                let scan = extract(spec, &s);
                for sym in scan.symbols {
                    prop_assert!(!sym.name.is_empty());
                    prop_assert!(!sym.qualified_name.is_empty());
                    prop_assert!(sym.end_line >= sym.start_line);
                }
                for t in scan.tests {
                    prop_assert!(!t.name.is_empty());
                    prop_assert!(!t.qualified_name.is_empty());
                    prop_assert!(t.end_line >= t.start_line);
                }
            }
        }
    }
}
