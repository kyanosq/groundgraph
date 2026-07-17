//! P22/P23.4 — Go language spec for the generic tree-sitter driver, the
//! **sole structural backend** for Go. `gopls` is demoted to an optional
//! Tier-3 `Calls` / `References` overlay (see [`crate::go_indexer`]).
//!
//! Go-specific irregularities isolated here:
//! - A Go type is a `type_spec` whose *value* (`struct_type` /
//!   `interface_type`) decides its kind, so `go_container_of` inspects
//!   the `type` field rather than the node kind alone.
//! - A method is not lexically nested in its type; the owner comes from
//!   the receiver (`func (r *Repo) M()`), surfaced via
//!   `go_receiver_type` so the generic driver nests it under `Repo`.
//! - `go test` collects free functions by name + signature
//!   (`go_test_of`), and import *paths* map to package *directories*
//!   (`go_resolve_import`).

use crate::treesitter::{
    body_from_field, keep_callable_kind, name_from_field, no_call_test, no_src_roots, no_text,
    node_text, simple_type_name, strip_quotes, CallKind, LangSpec, RefKind, SymKind, TestKind,
};
use groundgraph_core::NodeKind;

fn go_language() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}

fn go_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    if node.kind() != "type_spec" {
        return None;
    }
    match node.child_by_field_name("type").map(|t| t.kind()) {
        Some("struct_type") => Some(SymKind::Type(NodeKind::GoStruct)),
        Some("interface_type") => Some(SymKind::Type(NodeKind::GoInterface)),
        _ => None, // type aliases / named primitives: not a container.
    }
}

fn go_is_callable(kind: &str) -> bool {
    // `method_elem` is an interface method spec (`Area() float64`). It carries
    // no receiver, so the driver nests it under the enclosing interface — the
    // Go analogue of a Java interface method.
    matches!(
        kind,
        "function_declaration" | "method_declaration" | "method_elem"
    )
}

/// Body to recurse into. A type is a `type_spec` whose value sits under the
/// `type` field, not a `body` field, so [`body_from_field`] would never descend
/// into it. Surface an **interface's** method set by returning its
/// `interface_type` value; leave struct bodies closed (their fields are not
/// symbols, and descending would only add reference noise). Everything else
/// (functions, methods) uses the node's own `body` field.
fn go_body_of(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    if node.kind() == "type_spec" {
        let ty = node.child_by_field_name("type")?;
        return (ty.kind() == "interface_type").then_some(ty);
    }
    body_from_field(node)
}

fn go_receiver_type(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    for param in receiver.named_children(&mut cursor) {
        if param.kind() == "parameter_declaration" {
            if let Some(ty) = param.child_by_field_name("type") {
                return simple_type_name(ty, src);
            }
        }
    }
    None
}

fn go_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "import_spec" {
        return Vec::new();
    }
    node.child_by_field_name("path")
        .and_then(|p| node_text(p, src))
        .map(strip_quotes)
        .filter(|s| !s.is_empty())
        .into_iter()
        .collect()
}

fn go_is_transparent(kind: &str) -> bool {
    matches!(
        kind,
        "type_declaration" | "import_declaration" | "import_spec_list"
    )
}

/// Reclassify a Go test function (`func TestXxx(t *testing.T)`,
/// `BenchmarkXxx(b *testing.B)`, `FuzzXxx(f *testing.F)`, `ExampleXxx()`)
/// as a test case, following `go test`'s collection rules: a free function
/// whose name is the prefix optionally followed by a non-lowercase rune.
/// `TestMain` is the package test runner, not a case. `Test`/`Benchmark`/
/// `Fuzz` additionally require a `*testing.*` parameter so ordinary
/// functions like `func Tester()` are never misclassified; examples take no
/// such parameter.
fn go_test_of(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    kind: NodeKind,
    name: &str,
    parent_qualified: Option<&str>,
) -> Option<TestKind> {
    if kind != NodeKind::GoFunction || parent_qualified.is_some() {
        return None;
    }
    if name == "TestMain" {
        return None;
    }
    let prefix = ["Test", "Benchmark", "Fuzz", "Example"]
        .into_iter()
        .find(|p| match name.strip_prefix(p) {
            Some(rest) => rest.chars().next().is_none_or(|c| !c.is_ascii_lowercase()),
            None => false,
        })?;
    if prefix == "Example" || go_has_testing_param(node, src) {
        Some(TestKind::Case)
    } else {
        None
    }
}

/// True when a function declares a parameter referencing the `testing`
/// package (`*testing.T` / `*testing.B` / `*testing.F` / `*testing.M`).
fn go_has_testing_param(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    let Some(params) = node.child_by_field_name("parameters") else {
        return false;
    };
    let mut cursor = params.walk();
    for param in params.named_children(&mut cursor) {
        if param.kind() != "parameter_declaration" {
            continue;
        }
        if let Some(ty) = param.child_by_field_name("type") {
            if node_text(ty, src).is_some_and(|t| t.contains("testing.")) {
                return true;
            }
        }
    }
    false
}

/// Resolve a Go import path to a repo-relative file. Go packages are
/// directories, not single files, so we connect the importing file to the
/// lexicographically-first `.go` file in the target package (a stable
/// representative of that package). The package directory is found by
/// matching the longest suffix of the import path (`mymod/internal/store`
/// → a `…/internal/store/*.go` directory), which needs no `go.mod` parsing
/// and works for any module layout. Stdlib / third-party packages that do
/// not correspond to a repo directory resolve to `None`, so they never
/// create dangling external nodes.
fn go_resolve_import(
    raw: &str,
    _from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let path = strip_quotes(raw);
    let comps: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if comps.is_empty() {
        return None;
    }
    for start in 0..comps.len() {
        let dir = comps[start..].join("/");
        if let Some(file) = representative_go_file(all_files, &dir) {
            return Some(file);
        }
    }
    None
}

/// The lexicographically-first `.go` file living *directly* in a directory
/// whose path equals or ends with `dir` (the package dir). Deterministic so
/// re-indexing is stable.
fn representative_go_file(all_files: &[String], dir: &str) -> Option<String> {
    let needle = format!("/{dir}");
    all_files
        .iter()
        .filter(|f| match f.rsplit_once('/') {
            Some((parent, _)) => parent == dir || parent.ends_with(&needle),
            None => false,
        })
        .min()
        .cloned()
}

/// Heuristic outbound call / reference identifiers from a Go callable body
/// (see [`crate::treesitter::resolve_heuristic_refs`]). Captures:
/// - `helper()` → `Call` to `helper`.
/// - `r.Method()` / `pkg.Fn()` → `Call` to the trailing selector name
///   (links to a same-file / imported function or method of that name).
/// - `Repo{…}` / `&Repo{…}` → `Reference` to the composite-literal type.
///
/// Package-qualified stdlib / third-party calls (`fmt.Sprintf`,
/// `strings.TrimSpace`) carry only their trailing name, so they resolve to
/// nothing unless a local symbol of that name exists — keeping the noise
/// floor bounded, exactly like the Rust / TypeScript resolvers.
fn go_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    crate::treesitter::collect_calls(body, src, &mut out, 0, GO_CALL_KINDS);
    out
}

/// `call_expression` whose `function` field names the callee (bare name /
/// selector / generic operand), resolved by [`go_callee_name`].
fn go_call_extract(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<(String, RefKind)> {
    node.child_by_field_name("function")
        .and_then(|func| go_callee_name(func, src))
        .map(|name| (name, RefKind::Call))
}

/// `Repo{…}` / `&Repo{…}` — reference the constructed type's bare name.
fn go_composite_extract(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<(String, RefKind)> {
    node.child_by_field_name("type")
        .and_then(|ty| simple_type_name(ty, src))
        .map(|name| (name, RefKind::Reference))
}

static GO_CALL_KINDS: &[CallKind] = &[
    CallKind {
        kind: "call_expression",
        extract: go_call_extract,
    },
    CallKind {
        kind: "composite_literal",
        extract: go_composite_extract,
    },
];

/// Best-effort callee name for a Go call expression's `function` node.
fn go_callee_name(func: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" => node_text(func, src).map(str::to_string),
        // `recv.Method(...)` / `pkg.Fn(...)` → trailing selector name.
        "selector_expression" => func
            .child_by_field_name("field")
            .and_then(|f| node_text(f, src))
            .map(str::to_string),
        // Generic call `Foo[T](...)` / parenthesised callee — unwrap to the
        // underlying operand.
        "index_expression" | "parenthesized_expression" => func
            .child_by_field_name("operand")
            .or_else(|| func.named_child(0))
            .and_then(|inner| go_callee_name(inner, src)),
        _ => None,
    }
}

pub(crate) static GO_SPEC: LangSpec = LangSpec {
    language_id: "go",
    grammar: go_language,
    extensions: &["go"],
    skip_dirs: &[".git", "vendor", "node_modules"],
    separator: ".",
    func_kind: NodeKind::GoFunction,
    method_kind: NodeKind::GoMethod,
    container_of: go_container_of,
    is_callable_kind: go_is_callable,
    callable_kind_of: keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: go_receiver_type,
    import_of: go_import_of,
    name_of: name_from_field,
    body_of: go_body_of,
    is_transparent_kind: go_is_transparent,
    metadata_of: no_text,
    test_of: go_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: go_resolve_import,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: go_call_idents,
    module_scoped_resolution: false,
    recurse_declined_callables: false,
    claims_path: None,
    partial_class_merge: false,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&GO_SPEC, src)
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
    fn captures_call_and_construction_identifiers() {
        let src = r#"
package repo

type Repo struct{ Name string }

func (r *Repo) Greet() string { return r.Hello() }

func (r *Repo) Hello() string { return "hi" }

func New() *Repo { return &Repo{} }

func Run() {
	n := New()
	n.Greet()
}
"#;
        let got = refs(&scan(src));
        assert!(
            got.contains(&("Repo.Greet".into(), "Hello".into(), RefKind::Call)),
            "method-to-method via receiver selector: {got:?}"
        );
        assert!(
            got.contains(&("Run".into(), "New".into(), RefKind::Call)),
            "bare call: {got:?}"
        );
        assert!(
            got.contains(&("Run".into(), "Greet".into(), RefKind::Call)),
            "value selector call: {got:?}"
        );
        assert!(
            got.contains(&("New".into(), "Repo".into(), RefKind::Reference)),
            "composite-literal construction reference: {got:?}"
        );
    }

    #[test]
    fn structs_interfaces_methods_functions_and_imports() {
        let src = r#"
package repo

import (
	"fmt"
	"strings"
)

type Repo struct {
	Name string
}

type Store interface {
	Get() string
}

func (r *Repo) Greet() string {
	return fmt.Sprintf("hi %s", strings.TrimSpace(r.Name))
}

func New() *Repo {
	return &Repo{}
}
"#;
        let s = scan(src);
        assert!(qnames(&s, NodeKind::GoStruct).contains(&"Repo".to_string()));
        assert!(qnames(&s, NodeKind::GoInterface).contains(&"Store".to_string()));
        let methods = qnames(&s, NodeKind::GoMethod);
        assert!(
            methods.contains(&"Repo.Greet".to_string()),
            "pointer-receiver method should nest under Repo, got {methods:?}"
        );
        let greet = s.symbols.iter().find(|x| x.name == "Greet").unwrap();
        assert_eq!(greet.parent_qualified_name.as_deref(), Some("Repo"));
        assert!(qnames(&s, NodeKind::GoFunction).contains(&"New".to_string()));
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(
            imports.contains(&"fmt") && imports.contains(&"strings"),
            "{imports:?}"
        );
    }

    #[test]
    fn interface_method_specs_nest_under_the_interface() {
        // An interface's method set is its contract — the analogue of Java's
        // interface methods, which *are* captured. tree-sitter parses each as a
        // `method_elem` inside `interface_type`; the driver must descend into the
        // interface body and surface them as methods nested under the interface.
        let src = "package p\n\
                   type Shape interface {\n\
                   \tArea() float64\n\
                   \tPerimeter() float64\n\
                   \tio.Reader\n\
                   }\n";
        let s = scan(src);
        let methods = qnames(&s, NodeKind::GoMethod);
        assert!(
            methods.contains(&"Shape.Area".to_string()),
            "interface method should nest under the interface: {methods:?}"
        );
        assert!(
            methods.contains(&"Shape.Perimeter".to_string()),
            "interface method should nest under the interface: {methods:?}"
        );
        // The embedded interface (`io.Reader`) is a `type_elem`, not a method —
        // it must not masquerade as one.
        assert!(
            !methods.iter().any(|m| m.contains("Reader")),
            "embedded interface is not a method: {methods:?}"
        );
    }

    #[test]
    fn generic_and_constraint_interfaces_behave() {
        // A generic interface still has a method set; a constraint interface
        // (type set, no methods) and an empty interface must yield no methods.
        let src = "package p\n\
                   type Container[T any] interface {\n\tGet() T\n\tPut(v T)\n}\n\
                   type Number interface {\n\t~int | ~float64\n}\n\
                   type Any interface{}\n";
        let s = scan(src);
        let methods = qnames(&s, NodeKind::GoMethod);
        assert!(
            methods.contains(&"Container.Get".to_string()),
            "{methods:?}"
        );
        assert!(
            methods.contains(&"Container.Put".to_string()),
            "{methods:?}"
        );
        assert_eq!(
            methods.len(),
            2,
            "constraint/empty interfaces add no methods: {methods:?}"
        );
    }

    #[test]
    fn struct_fields_do_not_become_methods_or_functions() {
        // Descending into interface bodies must not also start emitting struct
        // fields as callables (fields are `field_declaration`, not callables).
        let src = "package p\ntype T struct {\n\tName string\n\tFn func() int\n}\n";
        let s = scan(src);
        assert!(
            qnames(&s, NodeKind::GoMethod).is_empty(),
            "no methods from a struct"
        );
        assert!(
            qnames(&s, NodeKind::GoFunction).is_empty(),
            "no functions from struct fields"
        );
        assert!(qnames(&s, NodeKind::GoStruct).contains(&"T".to_string()));
    }

    #[test]
    fn value_receiver_and_single_import() {
        let src = "package p\nimport \"os\"\ntype T struct{}\nfunc (t T) M() {}\n";
        let s = scan(src);
        assert!(qnames(&s, NodeKind::GoMethod).contains(&"T.M".to_string()));
        assert!(s.imports.iter().any(|i| i.path == "os"));
    }

    #[test]
    fn test_functions_are_collected_by_signature_and_name() {
        let src = r#"
package repo

import "testing"

func TestGreet(t *testing.T) {}

func BenchmarkGreet(b *testing.B) {}

func ExampleGreet() {}

func TestMain(m *testing.M) {}

func Tester() {}

func TestNoParam() {}
"#;
        let s = scan(src);
        let cases: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(cases.contains(&"TestGreet"), "{cases:?}");
        assert!(cases.contains(&"BenchmarkGreet"), "{cases:?}");
        assert!(
            cases.contains(&"ExampleGreet"),
            "examples need no param: {cases:?}"
        );
        // TestMain is the runner, not a case.
        assert!(!cases.contains(&"TestMain"), "{cases:?}");
        // `Tester` doesn't match the prefix rule (lowercase tail).
        assert!(
            qnames(&s, NodeKind::GoFunction).contains(&"Tester".to_string()),
            "Tester stays a normal function"
        );
        // `TestNoParam` matches the name but lacks a *testing.T param.
        assert!(
            qnames(&s, NodeKind::GoFunction).contains(&"TestNoParam".to_string()),
            "name-only match without testing param stays a function"
        );
    }

    #[test]
    fn imports_resolve_to_package_representative_file_and_drop_external() {
        let files = vec![
            "cmd/app/main.go".to_string(),
            "internal/store/cache.go".to_string(),
            "internal/store/store.go".to_string(),
        ];
        // Full module path → package dir via suffix match; representative is
        // the lexicographically-first file (cache.go < store.go).
        assert_eq!(
            go_resolve_import("\"mymod/internal/store\"", "cmd/app/main.go", &files, &[]),
            Some("internal/store/cache.go".to_string())
        );
        // Stdlib / third-party → dropped (no repo package directory).
        assert_eq!(
            go_resolve_import("\"fmt\"", "cmd/app/main.go", &files, &[]),
            None
        );
        assert_eq!(
            go_resolve_import("\"github.com/x/zzz\"", "cmd/app/main.go", &files, &[]),
            None
        );
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("package func type struct interface (((");
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
            prop_assert_eq!(extract(&GO_SPEC, &s), extract(&GO_SPEC, &s));
        }

        #[test]
        fn symbols_are_well_formed(s in ".*") {
            for sym in extract(&GO_SPEC, &s).symbols {
                prop_assert!(!sym.name.is_empty());
                prop_assert!(!sym.qualified_name.is_empty());
                prop_assert!(sym.end_line >= sym.start_line);
            }
        }
    }
}
