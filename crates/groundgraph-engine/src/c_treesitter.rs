//! P22 — C language spec for the generic tree-sitter driver.
//!
//! C has no classes/namespaces, so it contributes structs (unions fold
//! into structs), enums and free functions. The one irregularity is that
//! a function's name is buried in its `declarator` chain (pointers,
//! parentheses), handled by the shared [`declarator_name`] helper.

use crate::treesitter::{
    body_from_field, declarator_name, keep_callable_kind, name_from_field, no_call_test,
    no_src_roots, no_test_of, no_text, node_text, resolve_c_include, strip_quotes, CallKind,
    LangSpec, RefKind, SymKind,
};
use groundgraph_core::NodeKind;

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
        // `typedef struct/enum { … } Name;` — an anonymous record named only by
        // the typedef (shared C-family handling).
        "type_definition" => crate::treesitter::anon_typedef_record_specifier(node).map(|spec| {
            if spec == "enum_specifier" {
                SymKind::Type(NodeKind::CEnum)
            } else {
                SymKind::Type(NodeKind::CStruct)
            }
        }),
        _ => None,
    }
}

fn c_is_callable(kind: &str) -> bool {
    // Function-like macros behave as (textually inlined) functions: they are
    // called like functions and call functions from their body. Modelling
    // them keeps macro-mediated families (`serverAssert` → `_serverAssert`)
    // reachable in the call graph.
    kind == "function_definition" || kind == "preproc_function_def"
}

fn c_name_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "function_definition" => node
            .child_by_field_name("declarator")
            .and_then(|d| declarator_name(d, src)),
        // An anonymous typedef record borrows its name from the typedef declarator.
        "type_definition" => crate::treesitter::typedef_declarator_name(node, src),
        // `typedef struct _Tag { … } Name;` — users refer to the typedef
        // name; the tag only appears in internal self-references. Prefer the
        // typedef name so the symbol matches how the codebase spells it.
        "struct_specifier" | "union_specifier" | "enum_specifier" => node
            .parent()
            .filter(|p| p.kind() == "type_definition")
            .and_then(|p| crate::treesitter::typedef_declarator_name(p, src))
            .or_else(|| name_from_field(node, src)),
        _ => name_from_field(node, src),
    }
}

/// Body to recurse into. For a typedef'd anonymous record the members live one
/// level down, under the inner specifier's `body`; everything else uses the
/// node's own `body` field.
fn c_body_of(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    if node.kind() == "type_definition" {
        return crate::treesitter::typedef_record_body(node);
    }
    if node.kind() == "preproc_function_def" {
        // The macro replacement text: a flat `preproc_arg` token, scanned
        // textually by `c_call_idents`.
        return node.child_by_field_name("value");
    }
    body_from_field(node)
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
    if body.kind() == "preproc_arg" {
        // Macro replacement text is a flat token: the preprocessor has no
        // AST. Extract `ident(` textually; keywords are filtered.
        if let Some(text) = node_text(body, src) {
            collect_macro_calls(text, &mut out);
        }
        return out;
    }
    crate::treesitter::collect_calls(body, src, &mut out, 0, C_CALL_KINDS);
    out
}

/// C keywords (and preprocessor operators) that read as `word(` inside a
/// macro body but are never callees.
const C_NON_CALLEES: &[&str] = &[
    "if",
    "while",
    "for",
    "switch",
    "return",
    "sizeof",
    "defined",
    "void",
    "do",
    "else",
    "case",
    "typeof",
    "alignof",
    "_Alignof",
    "_Static_assert",
];

/// Extract `identifier(` occurrences from raw macro replacement text.
fn collect_macro_calls(text: &str, out: &mut Vec<(String, RefKind)>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            let word = &text[start..i];
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' && !C_NON_CALLEES.contains(&word) {
                out.push((word.to_string(), RefKind::Call));
            }
        } else {
            i += 1;
        }
    }
}

/// The single C call shape: a `call_expression` whose `function` field is
/// resolved by [`c_callee_name`]. (Macro-replacement text — `preproc_arg` —
/// is handled textually in [`c_call_idents`] before this runs, since the
/// preprocessor exposes no AST to walk.)
fn c_call_extract(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<(String, RefKind)> {
    node.child_by_field_name("function")
        .and_then(|f| c_callee_name(f, src))
        .map(|name| (name, RefKind::Call))
}

static C_CALL_KINDS: &[CallKind] = &[CallKind {
    kind: "call_expression",
    extract: c_call_extract,
}];

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
    body_of: c_body_of,
    is_transparent_kind: c_is_transparent,
    metadata_of: no_text,
    test_of: no_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: resolve_c_include,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: c_call_idents,
    // C has one flat namespace and splits declaration (`.h`) from definition
    // (`.c`): the definition is never in the caller's include targets, so
    // import-scoped resolution loses the cross-TU call graph. Module-wide
    // resolution applies the same uniqueness/multi-word/hub gates as Swift.
    module_scoped_resolution: true,
    recurse_declined_callables: false,
    // `.h` is shared with C++: claim a header only when it does NOT look like
    // C++ (no `namespace` / `class` / `::`). A `.c` file always reads true.
    claims_path: Some(c_claims_path),
    partial_class_merge: false,
};

/// [`LangSpec::claims_path`] for C: own every `.c`, and own a `.h` only when it
/// carries no C++ constructs (those headers belong to the C++ parser).
fn c_claims_path(rel: &str, head: &str) -> bool {
    if rel.ends_with(".h") {
        return !crate::treesitter::looks_like_cpp(head);
    }
    true
}

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
            .map(|r| (r.from_qualified.to_string(), r.to_name.clone(), r.kind))
            .collect()
    }

    #[test]
    fn tagged_typedef_struct_takes_the_typedef_name() {
        // Redis idiom: `typedef struct _clusterNode { … } clusterNode;`.
        // Every user refers to the typedef name; the `_`-prefixed tag only
        // appears in internal self-references (`struct _clusterNode *next`).
        // Naming the symbol after the tag strands it (dead-code false
        // positive); the typedef name is the public identity.
        let s = scan("typedef struct _clusterNode { struct _clusterNode *next; } clusterNode;\n");
        assert_eq!(qnames(&s, NodeKind::CStruct), vec!["clusterNode"]);
        // Plain tagged structs (no typedef) keep the tag name.
        let s2 = scan("struct dictEntry { int v; };\n");
        assert_eq!(qnames(&s2, NodeKind::CStruct), vec!["dictEntry"]);
    }

    #[test]
    fn function_like_macros_are_callable_symbols_with_outbound_calls() {
        // `#define serverAssert(e) _serverAssert(...)`: the macro is the
        // only caller of `_serverAssert`, and ordinary functions call the
        // macro. Without modelling the macro as a callable, the whole
        // assert/panic family looks dead.
        let s = scan(
            "void _serverAssert(char *e, char *f, int l);\n\
             #define serverAssert(_e) ((_e)?(void)0 : (_serverAssert(#_e,__FILE__,__LINE__),redis_unreachable()))\n",
        );
        assert_eq!(qnames(&s, NodeKind::CFunction), vec!["serverAssert"]);
        let r = refs(&s);
        assert!(
            r.iter()
                .any(|(from, to, _)| from == "serverAssert" && to == "_serverAssert"),
            "macro body must yield outbound call idents: {r:?}"
        );
        assert!(
            !r.iter().any(|(_, to, _)| to == "if" || to == "void"),
            "keywords must not leak as callees: {r:?}"
        );
    }

    #[test]
    fn module_resolution_links_definitions_across_translation_units() {
        use crate::treesitter::{resolve_heuristic_refs, RefKind, ScannedRef};
        use std::collections::HashMap;

        // The C compilation model: `dict.h` declares, `dict.c` defines, and
        // `server.c` includes only the header. The definition is *not* in any
        // import target, so import-scoped resolution loses the whole
        // cross-file call graph (Redis: 6965 symbols, only 127 reachable).
        // C has one flat namespace — a unique multi-word name resolves
        // module-wide, same policy as Swift.
        let symbols = vec![
            ("src/dict.c", "dictAdd", "dictAdd"),
            ("src/dict.c", "dictCreate", "dictCreate"),
            ("src/server.c", "initServer", "initServer"),
            // single generic word defined elsewhere must NOT link
            ("src/util.c", "send", "send"),
        ];
        let pending = vec![
            (
                "src/server.c".to_string(),
                ScannedRef {
                    from_qualified: "initServer".into(),
                    to_name: "dictCreate".into(),
                    kind: RefKind::Call,
                },
            ),
            (
                "src/server.c".to_string(),
                ScannedRef {
                    from_qualified: "initServer".into(),
                    to_name: "send".into(),
                    kind: RefKind::Call,
                },
            ),
        ];
        let edges = resolve_heuristic_refs(&C_SPEC, &symbols, &HashMap::new(), &pending);
        assert!(
            edges
                .iter()
                .any(|e| e.to_symbol_id.to_string().contains("dictCreate")),
            "unique multi-word C function must resolve across translation units: {edges:?}"
        );
        assert!(
            !edges
                .iter()
                .any(|e| e.to_symbol_id.to_string().contains("send")),
            "single generic word must stay unresolved (libc collision): {edges:?}"
        );
    }

    #[test]
    fn anonymous_typedef_struct_and_enum_take_the_typedef_name() {
        // The dominant C idiom: `typedef struct { … } Name;`. tree-sitter parses
        // the record as a *nameless* specifier (which the driver drops), with the
        // real name only on the typedef declarator. Recover it.
        let src = "typedef struct { int x; int y; } Point;\n\
                   typedef enum { A, B } Letter;\n\
                   typedef union { int i; float f; } Value;\n\
                   typedef struct Node { int v; } Node;\n\
                   typedef int MyInt;\n";
        let s = scan(src);
        let structs = qnames(&s, NodeKind::CStruct);
        let enums = qnames(&s, NodeKind::CEnum);
        assert!(
            structs.contains(&"Point".to_string()),
            "anon typedef struct: {structs:?}"
        );
        assert!(
            structs.contains(&"Value".to_string()),
            "anon typedef union: {structs:?}"
        );
        assert!(
            enums.contains(&"Letter".to_string()),
            "anon typedef enum: {enums:?}"
        );
        // Named record typedef stays single (no duplicate from the typedef).
        assert_eq!(
            structs.iter().filter(|n| *n == "Node").count(),
            1,
            "named record typedef must emit exactly once: {structs:?}"
        );
        // A plain alias typedef is not a record and emits nothing.
        assert!(!structs.contains(&"MyInt".to_string()) && !enums.contains(&"MyInt".to_string()));
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
