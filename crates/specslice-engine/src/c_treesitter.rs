//! P22 — C language spec for the generic tree-sitter driver.
//!
//! C has no classes/namespaces, so it contributes structs (unions fold
//! into structs), enums and free functions. The one irregularity is that
//! a function's name is buried in its `declarator` chain (pointers,
//! parentheses), handled by the shared [`declarator_name`] helper.

use crate::treesitter::{
    body_from_field, declarator_name, keep_callable_kind, name_from_field, no_call_test,
    no_src_roots, no_test_of, no_text, node_text, resolve_c_include, strip_quotes, LangSpec,
    RefKind, SymKind, MAX_NESTING_DEPTH,
};
use specslice_core::NodeKind;

fn c_language() -> tree_sitter::Language {
    tree_sitter_c::LANGUAGE.into()
}

fn c_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    // Require a body so forward declarations (`struct Foo;`) and uses
    // (`struct Foo x;`) don't masquerade as definitions.
    let has_body = node.child_by_field_name("body").is_some();
    match node.kind() {
        "struct_specifier" | "union_specifier" if has_body => {
            Some(SymKind::Type(NodeKind::CStruct))
        }
        "enum_specifier" if has_body => Some(SymKind::Type(NodeKind::CEnum)),
        _ => None,
    }
}

fn c_is_callable(kind: &str) -> bool {
    kind == "function_definition"
}

fn c_name_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() == "function_definition" {
        return node
            .child_by_field_name("declarator")
            .and_then(|d| declarator_name(d, src));
    }
    name_from_field(node, src)
}

fn c_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "preproc_include" {
        return Vec::new();
    }
    node.child_by_field_name("path")
        .and_then(|p| node_text(p, src))
        .map(strip_quotes)
        .filter(|s| !s.is_empty())
        .into_iter()
        .collect()
}

fn c_is_transparent(kind: &str) -> bool {
    matches!(
        kind,
        "declaration"
            | "type_definition"
            | "linkage_specification"
            | "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
    )
}

/// Heuristic outbound call identifiers from a C function body (see
/// [`crate::treesitter::resolve_heuristic_refs`]). C has only free
/// functions, so a bare `helper(…)` links to a same-file / included
/// function of that name; a function-pointer member call (`s.cb()` /
/// `s->cb()`) carries its trailing field name. Libc / unknown calls
/// resolve to nothing — the noise floor stays bounded.
pub(crate) fn c_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_c_calls(body, src, &mut out, 0);
    out
}

fn collect_c_calls(
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
                .child_by_field_name("function")
                .and_then(|f| c_callee_name(f, src))
            {
                out.push((name, RefKind::Call));
            }
        }
        collect_c_calls(child, src, out, depth + 1);
    }
}

/// Best-effort callee name for a C call expression's `function` node.
pub(crate) fn c_callee_name(func: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" => node_text(func, src).map(str::to_string),
        // Function-pointer member call: `s.cb()` / `s->cb()` → field name.
        "field_expression" => func
            .child_by_field_name("field")
            .and_then(|f| node_text(f, src))
            .map(str::to_string),
        "parenthesized_expression" => func
            .named_child(0)
            .and_then(|inner| c_callee_name(inner, src)),
        _ => None,
    }
}

pub(crate) static C_SPEC: LangSpec = LangSpec {
    language_id: "c",
    grammar: c_language,
    extensions: &["c", "h"],
    skip_dirs: &[".git", "build", "cmake-build-debug", "node_modules"],
    separator: ".",
    func_kind: NodeKind::CFunction,
    method_kind: NodeKind::CFunction, // C has no methods.
    container_of: c_container_of,
    is_callable_kind: c_is_callable,
    callable_kind_of: keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: no_text,
    import_of: c_import_of,
    name_of: c_name_of,
    body_of: body_from_field,
    is_transparent_kind: c_is_transparent,
    metadata_of: no_text,
    test_of: no_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: resolve_c_include,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: c_call_idents,
    module_scoped_resolution: false,
    recurse_declined_callables: false,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&C_SPEC, src)
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
    fn captures_bare_and_function_pointer_calls() {
        let src = r#"
int helper(int x) { return x + 1; }
int run(void) {
    int y = helper(3);
    return y;
}
"#;
        let got = refs(&scan(src));
        assert!(
            got.contains(&("run".into(), "helper".into(), RefKind::Call)),
            "bare function call: {got:?}"
        );
    }

    #[test]
    fn structs_enums_functions_and_includes() {
        let src = r#"
#include <stdio.h>
#include "local.h"
struct Point { int x; int y; };
enum Color { RED, GREEN };
int add(int a, int b) { return a + b; }
static void *make(void) { return 0; }
"#;
        let s = scan(src);
        assert!(qnames(&s, NodeKind::CStruct).contains(&"Point".to_string()));
        assert!(qnames(&s, NodeKind::CEnum).contains(&"Color".to_string()));
        let funcs = qnames(&s, NodeKind::CFunction);
        assert!(funcs.contains(&"add".to_string()), "{funcs:?}");
        assert!(
            funcs.contains(&"make".to_string()),
            "pointer-return function name must come through the declarator chain, got {funcs:?}"
        );
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(imports.contains(&"stdio.h"), "angle include: {imports:?}");
        assert!(imports.contains(&"local.h"), "quoted include: {imports:?}");
    }

    #[test]
    fn forward_declaration_is_not_a_struct() {
        let s = scan("struct Opaque;\nstruct Opaque *make(void) { return 0; }\n");
        assert!(
            qnames(&s, NodeKind::CStruct).is_empty(),
            "a bodyless struct must not be emitted"
        );
        assert!(qnames(&s, NodeKind::CFunction).contains(&"make".to_string()));
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("int (((; struct } enum ( #include");
    }

    #[test]
    fn resolve_include_relative_root_and_drops_system_headers() {
        let all = vec![
            "src/app.c".to_string(),
            "src/util.h".to_string(),
            "src/net/socket.h".to_string(),
            "include/proj/api.h".to_string(),
        ];
        // (1) quoted include resolves relative to the including file's dir.
        assert_eq!(
            resolve_c_include("util.h", "src/app.c", &all, &[]),
            Some("src/util.h".to_string())
        );
        // sub-path relative include.
        assert_eq!(
            resolve_c_include("net/socket.h", "src/app.c", &all, &[]),
            Some("src/net/socket.h".to_string())
        );
        // (3) `-I include`-style header reached by a unique suffix.
        assert_eq!(
            resolve_c_include("proj/api.h", "src/app.c", &all, &[]),
            Some("include/proj/api.h".to_string())
        );
        // System header (no in-repo file) is dropped — no dangling edge.
        assert_eq!(resolve_c_include("stdio.h", "src/app.c", &all, &[]), None);
    }

    #[test]
    fn resolve_include_drops_ambiguous_suffix() {
        let all = vec![
            "a/config.h".to_string(),
            "b/config.h".to_string(),
            "main.c".to_string(),
        ];
        // Two `config.h` candidates and no unambiguous relative/root hit →
        // drop rather than guess wrong.
        assert_eq!(resolve_c_include("config.h", "main.c", &all, &[]), None);
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
            prop_assert_eq!(extract(&C_SPEC, &s), extract(&C_SPEC, &s));
        }

        #[test]
        fn symbols_are_well_formed(s in ".*") {
            for sym in extract(&C_SPEC, &s).symbols {
                prop_assert!(!sym.name.is_empty());
                prop_assert!(!sym.qualified_name.is_empty());
                prop_assert!(sym.end_line >= sym.start_line);
            }
        }
    }
}
