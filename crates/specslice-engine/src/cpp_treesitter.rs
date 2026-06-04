//! P22 — C++ language spec for the generic tree-sitter driver.
//!
//! Adds namespaces, classes and methods on top of C's structs/enums/
//! functions. Notable handling:
//! - A named `namespace` is a [`SymKind::Module`]; an *anonymous*
//!   namespace emits nothing but is still descended into (via the
//!   `declaration_list` transparent rule) so its contents aren't lost.
//! - `template_declaration` is transparent so templated classes /
//!   functions surface as their underlying declaration.
//! - Method/function names come through the C/C++ [`declarator_name`]
//!   chain; out-of-line definitions (`void T::m() {}`) reduce to their
//!   final component.
//!
//! Known v1 gap: methods that are *declared but not defined* inside a
//! class body (`virtual void draw();`) are only recovered if they have an
//! out-of-line definition. Closing this fully needs body-aware
//! `field_declaration` inspection (or the Tier 3 LSP), tracked for a
//! follow-up.

use crate::treesitter::{
    body_from_field, declarator_name, keep_callable_kind, name_from_field, no_call_test,
    no_src_roots, no_test_of, no_text, node_text, resolve_c_include, simple_type_name,
    strip_quotes, LangSpec, RefKind, SymKind, MAX_NESTING_DEPTH,
};
use specslice_core::NodeKind;

fn cpp_language() -> tree_sitter::Language {
    tree_sitter_cpp::LANGUAGE.into()
}

fn cpp_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    let has_body = node.child_by_field_name("body").is_some();
    match node.kind() {
        // Named namespace only; anonymous falls through to the transparent
        // rule so we still descend into its body.
        "namespace_definition" if node.child_by_field_name("name").is_some() => {
            Some(SymKind::Module(NodeKind::CppNamespace))
        }
        "class_specifier" if has_body => Some(SymKind::Type(NodeKind::CppClass)),
        "struct_specifier" if has_body => Some(SymKind::Type(NodeKind::CppStruct)),
        "union_specifier" if has_body => Some(SymKind::Type(NodeKind::CppStruct)),
        "enum_specifier" if has_body => Some(SymKind::Type(NodeKind::CppEnum)),
        _ => None,
    }
}

fn cpp_is_callable(kind: &str) -> bool {
    kind == "function_definition"
}

fn cpp_name_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() == "function_definition" {
        return node
            .child_by_field_name("declarator")
            .and_then(|d| declarator_name(d, src));
    }
    name_from_field(node, src)
}

fn cpp_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
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

fn cpp_is_transparent(kind: &str) -> bool {
    matches!(
        kind,
        "namespace_definition" // anonymous ones
            | "declaration_list"
            | "template_declaration"
            | "declaration"
            | "type_definition"
            | "linkage_specification"
            | "export_declaration"
            | "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
    )
}

/// Heuristic outbound call / reference identifiers from a C++ callable body
/// (see [`crate::treesitter::resolve_heuristic_refs`]). Every callee is
/// reduced to its **simple trailing name** (never a `::`-qualified path) so
/// resolution stays on the robust bare-name branch — a simple name links to
/// a same-file / included symbol whose own name matches, regardless of the
/// namespace / class it is nested in. Captures:
/// - `helper()` → `Call` to `helper`.
/// - `obj.method()` / `ptr->method()` → `Call` to the field name.
/// - `Class::method()` / `ns::fn()` → `Call` to the trailing identifier.
/// - `foo<T>()` → `Call` to `foo`.
/// - `new Widget(…)` → `Reference` to the constructed type's bare name.
fn cpp_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_cpp_calls(body, src, &mut out, 0);
    out
}

fn collect_cpp_calls(
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
                    .child_by_field_name("function")
                    .and_then(|f| cpp_callee_name(f, src))
                {
                    out.push((name, RefKind::Call));
                }
            }
            "new_expression" => {
                if let Some(name) = child
                    .child_by_field_name("type")
                    .and_then(|t| simple_type_name(t, src))
                {
                    out.push((name, RefKind::Reference));
                }
            }
            _ => {}
        }
        collect_cpp_calls(child, src, out, depth + 1);
    }
}

/// Best-effort *simple* callee name for a C++ call expression's `function`
/// node — qualified paths reduce to their rightmost identifier.
fn cpp_callee_name(func: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" | "field_identifier" => node_text(func, src).map(str::to_string),
        "field_expression" => func
            .child_by_field_name("field")
            .and_then(|f| node_text(f, src))
            .map(str::to_string),
        // `Class::method` / `ns::fn` → rightmost identifier (no `::`).
        // `foo<T>(...)` → the templated function's name.
        "qualified_identifier" | "template_function" => func
            .child_by_field_name("name")
            .and_then(|n| cpp_callee_name(n, src)),
        "parenthesized_expression" => func
            .named_child(0)
            .and_then(|inner| cpp_callee_name(inner, src)),
        _ => None,
    }
}

pub(crate) static CPP_SPEC: LangSpec = LangSpec {
    language_id: "cpp",
    grammar: cpp_language,
    extensions: &["cpp", "cc", "cxx", "hpp", "hh", "hxx", "ipp"],
    skip_dirs: &[".git", "build", "cmake-build-debug", "node_modules"],
    separator: "::",
    func_kind: NodeKind::CppFunction,
    method_kind: NodeKind::CppMethod,
    container_of: cpp_container_of,
    is_callable_kind: cpp_is_callable,
    callable_kind_of: keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: no_text,
    import_of: cpp_import_of,
    name_of: cpp_name_of,
    body_of: body_from_field,
    is_transparent_kind: cpp_is_transparent,
    metadata_of: no_text,
    test_of: no_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: resolve_c_include,
    recurse_callables: false,
    call_idents_of: cpp_call_idents,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&CPP_SPEC, src)
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
    fn captures_simple_member_qualified_and_new_calls() {
        let src = r#"
namespace geo {
  int helper(int v) { return v + 1; }
  int run() { return helper(2); }

  struct Shape {
    int area() const { return compute(); }
    int compute() const { return 0; }
  };

  int make() {
    Shape* s = new Shape();
    return s->area();
  }
}
"#;
        let got = refs(&scan(src));
        assert!(
            got.contains(&("geo::run".into(), "helper".into(), RefKind::Call)),
            "bare call resolves by simple name: {got:?}"
        );
        assert!(
            got.contains(&("geo::Shape::area".into(), "compute".into(), RefKind::Call)),
            "in-class bare call: {got:?}"
        );
        assert!(
            got.contains(&("geo::make".into(), "Shape".into(), RefKind::Reference)),
            "new-expression reference (bare type name): {got:?}"
        );
        assert!(
            got.contains(&("geo::make".into(), "area".into(), RefKind::Call)),
            "arrow member call → field name: {got:?}"
        );
    }

    #[test]
    fn namespace_class_method_struct_enum_and_include() {
        let src = r#"
#include <vector>
namespace geo {
  class Shape {
  public:
    double area() const { return 0; }
  };
  struct P { int x; };
  enum E { A, B };
  int freefn() { return 0; }
}
"#;
        let s = scan(src);
        assert!(qnames(&s, NodeKind::CppNamespace).contains(&"geo".to_string()));
        assert!(qnames(&s, NodeKind::CppClass).contains(&"geo::Shape".to_string()));
        let methods = qnames(&s, NodeKind::CppMethod);
        assert!(
            methods.contains(&"geo::Shape::area".to_string()),
            "in-class defined method should nest under the class, got {methods:?}"
        );
        assert!(qnames(&s, NodeKind::CppStruct).contains(&"geo::P".to_string()));
        assert!(qnames(&s, NodeKind::CppEnum).contains(&"geo::E".to_string()));
        assert!(
            qnames(&s, NodeKind::CppFunction).contains(&"geo::freefn".to_string()),
            "namespaced free function should qualify"
        );
        assert!(s.imports.iter().any(|i| i.path == "vector"));
    }

    #[test]
    fn anonymous_namespace_contents_are_still_found() {
        let s = scan("namespace { void helper() {} }\n");
        assert!(
            qnames(&s, NodeKind::CppFunction).contains(&"helper".to_string()),
            "contents of an anonymous namespace must not be dropped"
        );
        // Anonymous namespace itself emits no module symbol.
        assert!(qnames(&s, NodeKind::CppNamespace).is_empty());
    }

    #[test]
    fn templated_function_is_found() {
        let s = scan("template <typename T> T identity(T v) { return v; }\n");
        assert!(qnames(&s, NodeKind::CppFunction).contains(&"identity".to_string()));
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("namespace class template <<< :: :: struct {");
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
            prop_assert_eq!(extract(&CPP_SPEC, &s), extract(&CPP_SPEC, &s));
        }

        #[test]
        fn symbols_are_well_formed(s in ".*") {
            for sym in extract(&CPP_SPEC, &s).symbols {
                prop_assert!(!sym.name.is_empty());
                prop_assert!(!sym.qualified_name.is_empty());
                prop_assert!(sym.end_line >= sym.start_line);
            }
        }
    }
}
