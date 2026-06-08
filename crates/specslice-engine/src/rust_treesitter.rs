//! P21 — Rust language spec for the generic tree-sitter driver.
//!
//! All the heavy lifting (walking, nesting, ingest, robustness) lives in
//! [`crate::treesitter`]; this file is now just the Rust [`LangSpec`] plus
//! a thin `scan` shim kept for backward compatibility and the original
//! Rust-specific tests. Adding a language is the same exercise: a grammar
//! + a handful of small hooks.

use crate::treesitter::{
    self, body_from_field, keep_callable_kind, name_from_field, no_call_test, no_text, node_text,
    normalise_ws, simple_type_name, LangSpec, RefKind, SymKind, TestKind, MAX_NESTING_DEPTH,
};
use specslice_core::NodeKind;

// Re-export the generic scan types under their historical Rust names so
// existing call sites / tests keep compiling unchanged.
pub use crate::treesitter::{
    Scan as RustScan, ScannedImport as RustImport, ScannedSymbol as RustSymbol,
};

/// Parse Rust `source` into structural symbols + imports.
pub fn scan(source: &str) -> RustScan {
    treesitter::extract(&RUST_SPEC, source)
}

fn rust_language() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}

fn rust_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        "struct_item" | "union_item" => Some(SymKind::Type(NodeKind::RustStruct)),
        "enum_item" => Some(SymKind::Type(NodeKind::RustEnum)),
        "trait_item" => Some(SymKind::Type(NodeKind::RustTrait)),
        "mod_item" => Some(SymKind::Module(NodeKind::RustModule)),
        _ => None,
    }
}

fn rust_is_callable(kind: &str) -> bool {
    matches!(kind, "function_item" | "function_signature_item")
}

fn rust_impl_type_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() != "impl_item" {
        return None;
    }
    node.child_by_field_name("type")
        .and_then(|n| simple_type_name(n, src))
}

fn rust_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "use_declaration" {
        return Vec::new();
    }
    node.child_by_field_name("argument")
        .and_then(|arg| node_text(arg, src))
        .map(normalise_ws)
        .filter(|s| !s.is_empty())
        .into_iter()
        .collect()
}

/// Discover Rust crate source roots: any directory that directly contains a
/// crate entry file (`lib.rs` / `main.rs`). These anchor `crate::` resolution
/// and the crate-name → directory mapping used for cross-crate imports.
/// Deepest-first so the most specific crate root wins as a path prefix.
fn rust_src_roots(all_files: &[String]) -> Vec<String> {
    let mut roots: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for file in all_files {
        for entry in ["lib.rs", "main.rs"] {
            let Some(prefix) = file.strip_suffix(entry) else {
                continue;
            };
            if prefix.is_empty() {
                roots.insert(String::new());
            } else if let Some(dir) = prefix.strip_suffix('/') {
                roots.insert(dir.to_string());
            }
        }
    }
    let mut out: Vec<String> = roots.into_iter().collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.matches('/').count() + usize::from(!r.is_empty())));
    out
}

/// Resolve a Rust `use` path to the repo-relative file that *defines* the
/// nearest enclosing module. Returns `None` for the standard library, external
/// crates and anything that does not map to an in-repo file — those are dropped
/// rather than emitted as dangling import edges. See the unit tests for the
/// exact shapes handled (`crate::` / `self::` / `super::` / cross-crate / globs).
fn rust_resolve_import(
    raw: &str,
    from_file: &str,
    all_files: &[String],
    src_roots: &[String],
) -> Option<String> {
    // Only the module *path* maps to a file; the imported-items group `{..}`
    // and trailing glob `*` are members, not path segments.
    let head = raw.split('{').next().unwrap_or(raw);
    let segs: Vec<&str> = head
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "*")
        .collect();
    let first = *segs.first()?;

    let mut idx = 1usize;
    let base_dir = match first {
        "crate" => rust_crate_root_of(from_file, src_roots)?.clone(),
        "self" => rust_module_dir(from_file),
        "super" => {
            let mut dir = rust_parent_dir(&rust_module_dir(from_file));
            while segs.get(idx) == Some(&"super") {
                dir = rust_parent_dir(&dir);
                idx += 1;
            }
            dir
        }
        // The standard library family never lives in the repo.
        "std" | "core" | "alloc" | "proc_macro" | "test" => return None,
        name => rust_crate_root_for_name(name, src_roots)?.clone(),
    };

    let target = rust_resolve_mod_path(all_files, &base_dir, &segs[idx..], src_roots)?;
    // A file importing itself carries no dependency signal.
    if target == from_file {
        return None;
    }
    Some(target)
}

/// Directory under which the current module's *submodules* live: the parent
/// directory for `mod.rs` / `lib.rs` / `main.rs`, otherwise the `foo.rs` stem.
fn rust_module_dir(from_file: &str) -> String {
    let (dir, file) = match from_file.rfind('/') {
        Some(i) => (&from_file[..i], &from_file[i + 1..]),
        None => ("", from_file),
    };
    match file {
        "mod.rs" | "lib.rs" | "main.rs" => dir.to_string(),
        other => {
            let stem = other.strip_suffix(".rs").unwrap_or(other);
            if dir.is_empty() {
                stem.to_string()
            } else {
                format!("{dir}/{stem}")
            }
        }
    }
}

fn rust_parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// The most specific crate src root that contains `from_file` (roots are
/// deepest-first, so the first prefix match is the right one).
fn rust_crate_root_of<'a>(from_file: &str, src_roots: &'a [String]) -> Option<&'a String> {
    src_roots
        .iter()
        .find(|root| root.is_empty() || from_file.starts_with(&format!("{root}/")))
}

/// Crate name derived from a src root (`crates/specslice-engine/src` →
/// `specslice_engine`), normalised the way `use` paths spell it.
fn rust_crate_name(root: &str) -> Option<String> {
    let dir = root.strip_suffix("/src").unwrap_or(root);
    let name = dir.rsplit('/').next().unwrap_or(dir);
    if name.is_empty() {
        return None;
    }
    Some(name.replace('-', "_"))
}

fn rust_crate_root_for_name<'a>(name: &str, src_roots: &'a [String]) -> Option<&'a String> {
    src_roots
        .iter()
        .find(|root| rust_crate_name(root).as_deref() == Some(name))
}

/// Walk `segs` against `base_dir`, returning the longest module prefix that
/// resolves to a `foo.rs` / `foo/mod.rs` file. Trailing segments are items, not
/// modules. Falls back to the crate entry file for items re-exported from the
/// crate root.
fn rust_resolve_mod_path(
    all_files: &[String],
    base_dir: &str,
    segs: &[&str],
    src_roots: &[String],
) -> Option<String> {
    for k in (0..=segs.len()).rev() {
        let mut p = base_dir.to_string();
        for s in &segs[..k] {
            if !p.is_empty() {
                p.push('/');
            }
            p.push_str(s);
        }
        for cand in [format!("{p}.rs"), format!("{p}/mod.rs")] {
            if !cand.is_empty() && all_files.iter().any(|f| f == &cand) {
                return Some(cand);
            }
        }
    }
    if src_roots.iter().any(|r| r == base_dir) {
        for entry in ["lib.rs", "main.rs"] {
            let cand = if base_dir.is_empty() {
                entry.to_string()
            } else {
                format!("{base_dir}/{entry}")
            };
            if all_files.iter().any(|f| f == &cand) {
                return Some(cand);
            }
        }
    }
    None
}

pub(crate) static RUST_SPEC: LangSpec = LangSpec {
    language_id: "rust",
    grammar: rust_language,
    extensions: &["rs"],
    skip_dirs: &[".git", "target", "out", ".idea"],
    separator: "::",
    func_kind: NodeKind::RustFunction,
    method_kind: NodeKind::RustMethod,
    container_of: rust_container_of,
    is_callable_kind: rust_is_callable,
    callable_kind_of: keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: rust_impl_type_of,
    receiver_type_of: no_text,
    import_of: rust_import_of,
    name_of: name_from_field,
    body_of: body_from_field,
    is_transparent_kind: treesitter::never,
    metadata_of: no_text,
    test_of: rust_test_of,
    call_test_of: no_call_test,
    src_roots_of: rust_src_roots,
    resolve_import: rust_resolve_import,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: rust_call_idents,
    module_scoped_resolution: false,
    recurse_declined_callables: false,
    claims_path: None,
};

/// Recognise `#[test]` functions (and derivatives such as `#[tokio::test]`)
/// as test cases. Rust was previously the only tree-sitter language without
/// a `test_of` hook, so its tests were indexed as ordinary functions and
/// never used as dead-code reachability roots. In `tree-sitter-rust` an
/// attribute is an `attribute_item` *sibling* preceding the `function_item`,
/// so we walk back over the leading attributes / comments.
fn rust_test_of(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    _kind: NodeKind,
    _name: &str,
    _parent: Option<&str>,
) -> Option<TestKind> {
    if node.kind() != "function_item" {
        return None;
    }
    let mut sib = node.prev_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "attribute_item" => {
                if node_text(s, src).is_some_and(rust_attr_is_test) {
                    return Some(TestKind::Case);
                }
            }
            // Attributes and doc/line comments may sit between the marker
            // and the `fn`; keep scanning past them.
            "line_comment" | "block_comment" => {}
            // Any other preceding sibling ends the attribute run.
            _ => break,
        }
        sib = s.prev_sibling();
    }
    None
}

/// `#[test]` / `#[tokio::test]` → true; `#[cfg(test)]`, `#[ignore]`,
/// `#[should_panic]` → false. Matches on the attribute *path* (the part
/// before any `(`) so `test` appearing only as an argument never counts.
fn rust_attr_is_test(attr_text: &str) -> bool {
    let t = attr_text.trim();
    let t = t.strip_prefix('#').unwrap_or(t).trim_start();
    let t = t.strip_prefix('[').unwrap_or(t);
    let t = t.strip_suffix(']').unwrap_or(t);
    let name = t.split('(').next().unwrap_or(t).trim();
    name == "test" || name.ends_with("::test")
}

/// Collect outbound call / reference identifiers from a Rust callable body.
///
/// Conservative on purpose — only signals a deterministic parser can stand
/// behind, all resolved to in-repo symbols downstream:
/// - `call_expression` callee → a `Calls` target (`foo()`, `self.foo()`,
///   `Type::assoc()`). Method calls contribute the bare method name; the
///   receiver type is unknown without type inference, so name resolution
///   picks the same/imported-file method of that name.
/// - `Type::assoc` paths keep the full `Type::assoc` string so the resolver
///   can verify `Type` is local before linking (drops `HashMap::new`, etc.).
///
/// Macros and bare value identifiers are intentionally skipped (too noisy
/// without a resolver); they can be added behind the same medium tier later.
fn rust_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_rust_calls(body, src, &mut out, 0);
    out
}

fn collect_rust_calls(
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
            if let Some(func) = child.child_by_field_name("function") {
                if let Some(name) = rust_callee_name(func, src) {
                    out.push((name, RefKind::Call));
                }
            }
        }
        // Always descend: calls nest inside arguments, blocks, closures,
        // match arms, etc. Nested-fn calls fold into the enclosing callable
        // (acceptable — nested fns are not separately indexed).
        collect_rust_calls(child, src, out, depth + 1);
    }
}

/// Best-effort callee name for a `call_expression`'s `function` node.
fn rust_callee_name(func: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" => node_text(func, src).map(str::to_string),
        // `x.method(...)` / `self.method(...)` → the method identifier.
        "field_expression" => func
            .child_by_field_name("field")
            .and_then(|f| node_text(f, src))
            .map(str::to_string),
        // `Type::assoc` / `module::func` → keep the path; the resolver
        // splits it and only links when the head is a local type.
        "scoped_identifier" => node_text(func, src).map(normalise_ws),
        // `foo::<T>()` — unwrap the turbofish wrapper and retry.
        "generic_function" => func
            .child_by_field_name("function")
            .and_then(|inner| rust_callee_name(inner, src)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_of(scan: &RustScan, kind: NodeKind) -> Vec<String> {
        scan.symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.qualified_name.clone())
            .collect()
    }

    #[test]
    fn struct_and_impl_method_nest_under_type() {
        let src = r#"
pub struct Greeter {
    name: String,
}

impl Greeter {
    pub fn new(name: String) -> Self { Self { name } }
    pub fn greet(&self) -> String { format!("hi {}", self.name) }
}
"#;
        let scan = scan(src);
        assert_eq!(names_of(&scan, NodeKind::RustStruct), vec!["Greeter"]);
        let methods = names_of(&scan, NodeKind::RustMethod);
        assert!(
            methods.contains(&"Greeter::new".to_string())
                && methods.contains(&"Greeter::greet".to_string()),
            "expected Greeter::new + Greeter::greet, got {methods:?}"
        );
        let greet = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "Greeter::greet")
            .unwrap();
        assert_eq!(greet.parent_qualified_name.as_deref(), Some("Greeter"));
    }

    #[test]
    fn captures_same_file_call_identifiers() {
        let src = "fn a() { b(); c(); }\nfn b() {}\nfn c() {}\n";
        let scan = scan(src);
        let from_a: Vec<&str> = scan
            .references
            .iter()
            .filter(|r| r.from_qualified == "a" && r.kind == RefKind::Call)
            .map(|r| r.to_name.as_str())
            .collect();
        assert!(
            from_a.contains(&"b") && from_a.contains(&"c"),
            "expected a → b, c calls, got {:?}",
            scan.references
        );
    }

    #[test]
    fn captures_method_and_scoped_call_identifiers() {
        let src = r#"
pub struct Foo;
impl Foo {
    pub fn new() -> Self { Foo }
    pub fn run(&self) { self.helper(); let _ = Foo::new(); }
    fn helper(&self) {}
}
"#;
        let scan = scan(src);
        let from_run: Vec<&str> = scan
            .references
            .iter()
            .filter(|r| r.from_qualified == "Foo::run")
            .map(|r| r.to_name.as_str())
            .collect();
        assert!(
            from_run.contains(&"helper"),
            "method call self.helper() should be captured, got {from_run:?}"
        );
        assert!(
            from_run.contains(&"Foo::new"),
            "scoped Foo::new() should be captured verbatim, got {from_run:?}"
        );
    }

    #[test]
    fn test_attribute_functions_are_classified_as_tests() {
        let src = r#"
pub fn helper() {}

#[test]
fn it_adds() { helper(); }

#[tokio::test]
async fn it_awaits() {}

#[cfg(test)]
fn only_a_helper() {}
"#;
        let scan = scan(src);
        let test_names: Vec<&str> = scan.tests.iter().map(|t| t.name.as_str()).collect();
        assert!(
            test_names.contains(&"it_adds") && test_names.contains(&"it_awaits"),
            "#[test] and #[tokio::test] should be tests, got {test_names:?}"
        );
        // `#[cfg(test)]` is *not* a test marker — `test` is only an argument.
        assert!(
            !test_names.contains(&"only_a_helper"),
            "#[cfg(test)] must not be treated as a test, got {test_names:?}"
        );
        let sym_names: Vec<&str> = scan.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            !sym_names.contains(&"it_adds"),
            "a reclassified test must not also be a structural symbol, got {sym_names:?}"
        );
        // A test body still seeds outbound call edges (reachability roots).
        assert!(
            scan.references
                .iter()
                .any(|r| r.from_qualified == "it_adds" && r.to_name == "helper"),
            "test body calls should be captured, got {:?}",
            scan.references
        );
    }

    #[test]
    fn attr_is_test_matches_only_test_paths() {
        assert!(rust_attr_is_test("#[test]"));
        assert!(rust_attr_is_test("#[tokio::test]"));
        assert!(rust_attr_is_test("#[ test ]"));
        assert!(!rust_attr_is_test("#[cfg(test)]"));
        assert!(!rust_attr_is_test("#[ignore]"));
        assert!(!rust_attr_is_test("#[should_panic]"));
    }

    #[test]
    fn every_language_spec_opts_into_the_call_resolver() {
        // The capture hook is opt-in via `call_idents_of`: the
        // `no_call_idents` default yields an empty `references` set, keeping
        // a brand-new language silent until it wires a resolver. Every
        // language shipped today has opted in, so the medium-confidence
        // Calls / References overlay is available uniformly across the
        // tree-sitter backends.
        use crate::treesitter::{no_call_idents, LangSpec};
        let default = no_call_idents as usize;
        let specs: [(&str, &LangSpec); 9] = [
            ("rust", &RUST_SPEC),
            ("python", &crate::python_treesitter::PYTHON_SPEC),
            ("go", &crate::go_treesitter::GO_SPEC),
            ("java", &crate::java_treesitter::JAVA_SPEC),
            ("c", &crate::c_treesitter::C_SPEC),
            ("cpp", &crate::cpp_treesitter::CPP_SPEC),
            ("swift", &crate::swift_treesitter::SWIFT_SPEC),
            ("typescript", &crate::typescript_treesitter::TYPESCRIPT_SPEC),
            ("tsx", &crate::typescript_treesitter::TSX_SPEC),
        ];
        for (name, spec) in specs {
            assert!(
                (spec.call_idents_of as usize) != default,
                "{name} must opt into the call resolver"
            );
        }
    }

    #[test]
    fn enum_trait_and_trait_default_method() {
        let src = r#"
pub enum Mode { Fast, Slow }

pub trait Runnable {
    fn run(&self);
    fn describe(&self) -> &str { "runnable" }
}
"#;
        let scan = scan(src);
        assert_eq!(names_of(&scan, NodeKind::RustEnum), vec!["Mode"]);
        assert_eq!(names_of(&scan, NodeKind::RustTrait), vec!["Runnable"]);
        let methods = names_of(&scan, NodeKind::RustMethod);
        assert!(
            methods.contains(&"Runnable::run".to_string())
                && methods.contains(&"Runnable::describe".to_string()),
            "trait signature + default method should both be methods, got {methods:?}"
        );
    }

    #[test]
    fn nested_module_qualifies_free_functions() {
        let src = r#"
mod outer {
    pub fn top() {}
    mod inner {
        pub fn helper() {}
    }
}

pub fn root_fn() {}
"#;
        let scan = scan(src);
        let modules = names_of(&scan, NodeKind::RustModule);
        assert!(modules.contains(&"outer".to_string()));
        assert!(modules.contains(&"outer::inner".to_string()));
        let funcs = names_of(&scan, NodeKind::RustFunction);
        assert!(funcs.contains(&"outer::top".to_string()), "got {funcs:?}");
        assert!(
            funcs.contains(&"outer::inner::helper".to_string()),
            "got {funcs:?}"
        );
        assert!(funcs.contains(&"root_fn".to_string()), "got {funcs:?}");
    }

    #[test]
    fn use_declarations_become_imports() {
        let src = r#"
use std::collections::HashMap;
use crate::foo::{Alpha, Beta};
use super::bar::*;
"#;
        let scan = scan(src);
        let paths: Vec<&str> = scan.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(
            paths.contains(&"std::collections::HashMap"),
            "got {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.starts_with("crate::foo::{")),
            "grouped use should be captured, got {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.contains("bar::*")),
            "glob use should be captured, got {paths:?}"
        );
    }

    fn workspace_files() -> Vec<String> {
        [
            "crates/specslice-cli/src/main.rs",
            "crates/specslice-cli/src/commands/mod.rs",
            "crates/specslice-cli/src/commands/search.rs",
            "crates/specslice-cli/src/commands/graph_mermaid.rs",
            "crates/specslice-engine/src/lib.rs",
            "crates/specslice-engine/src/graph.rs",
            "crates/specslice-engine/src/slice.rs",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    #[test]
    fn src_roots_are_crate_entry_dirs_deepest_first() {
        let roots = rust_src_roots(&workspace_files());
        assert!(
            roots.contains(&"crates/specslice-cli/src".to_string()),
            "{roots:?}"
        );
        assert!(
            roots.contains(&"crates/specslice-engine/src".to_string()),
            "{roots:?}"
        );
        // A non-entry directory must never be reported as a crate root.
        assert!(!roots.contains(&"crates/specslice-cli/src/commands".to_string()));
    }

    #[test]
    fn crate_name_strips_src_and_normalises_dashes() {
        assert_eq!(
            rust_crate_name("crates/specslice-engine/src").as_deref(),
            Some("specslice_engine")
        );
    }

    #[test]
    fn resolve_crate_relative_to_sibling_module() {
        let files = workspace_files();
        let roots = rust_src_roots(&files);
        let got = rust_resolve_import(
            "crate::commands::graph_mermaid::{render_parts, MermaidNode}",
            "crates/specslice-cli/src/commands/search.rs",
            &files,
            &roots,
        );
        assert_eq!(
            got.as_deref(),
            Some("crates/specslice-cli/src/commands/graph_mermaid.rs")
        );
    }

    #[test]
    fn resolve_super_glob_to_parent_module_file() {
        let files = workspace_files();
        let roots = rust_src_roots(&files);
        // From `commands/search.rs`, `super` is the `commands` module (mod.rs).
        let got = rust_resolve_import(
            "super::*",
            "crates/specslice-cli/src/commands/search.rs",
            &files,
            &roots,
        );
        assert_eq!(
            got.as_deref(),
            Some("crates/specslice-cli/src/commands/mod.rs")
        );
    }

    #[test]
    fn resolve_cross_crate_import_by_crate_name() {
        let files = workspace_files();
        let roots = rust_src_roots(&files);
        let got = rust_resolve_import(
            "specslice_engine::graph::GraphLayer",
            "crates/specslice-cli/src/commands/search.rs",
            &files,
            &roots,
        );
        assert_eq!(got.as_deref(), Some("crates/specslice-engine/src/graph.rs"));
    }

    #[test]
    fn cross_crate_reexport_falls_back_to_crate_entry() {
        let files = workspace_files();
        let roots = rust_src_roots(&files);
        // `GraphLayer` re-exported from the engine crate root resolves to lib.rs.
        let got = rust_resolve_import(
            "specslice_engine::GraphLayer",
            "crates/specslice-cli/src/commands/search.rs",
            &files,
            &roots,
        );
        assert_eq!(got.as_deref(), Some("crates/specslice-engine/src/lib.rs"));
    }

    #[test]
    fn std_and_external_crates_are_dropped() {
        let files = workspace_files();
        let roots = rust_src_roots(&files);
        for raw in [
            "std::path::Path",
            "std::path::{Path, PathBuf}",
            "core::fmt::Debug",
            "anyhow::{Context, Result}",
            "serde::Serialize",
        ] {
            assert_eq!(
                rust_resolve_import(
                    raw,
                    "crates/specslice-cli/src/commands/search.rs",
                    &files,
                    &roots
                ),
                None,
                "external import should be dropped: {raw}"
            );
        }
    }

    #[test]
    fn self_import_is_not_emitted() {
        let files = workspace_files();
        let roots = rust_src_roots(&files);
        // `commands/mod.rs` importing `crate::commands` would point at itself.
        let got = rust_resolve_import(
            "crate::commands::search::SearchRunArgs",
            "crates/specslice-cli/src/commands/mod.rs",
            &files,
            &roots,
        );
        // resolves to a *different* file, so it is kept …
        assert_eq!(
            got.as_deref(),
            Some("crates/specslice-cli/src/commands/search.rs")
        );
        // … but a literal self-reference is dropped.
        assert_eq!(
            rust_resolve_import(
                "crate::commands",
                "crates/specslice-cli/src/commands/mod.rs",
                &files,
                &roots
            ),
            None
        );
    }

    #[test]
    fn string_literal_is_not_a_symbol() {
        let src = r#"
pub fn real() -> &'static str {
    let _ = "pub fn fake() {}";
    // struct AlsoFake {}
    "trait Nope {}"
}
"#;
        let scan = scan(src);
        let funcs = names_of(&scan, NodeKind::RustFunction);
        assert_eq!(funcs, vec!["real"], "only the real fn should be found");
        assert!(names_of(&scan, NodeKind::RustStruct).is_empty());
        assert!(names_of(&scan, NodeKind::RustTrait).is_empty());
    }

    #[test]
    fn trait_impl_for_type_attaches_methods_to_type() {
        let src = r#"
struct Widget;
impl std::fmt::Display for Widget {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) }
}
"#;
        let scan = scan(src);
        let fmt = scan
            .symbols
            .iter()
            .find(|s| s.name == "fmt")
            .expect("fmt method present");
        assert_eq!(fmt.kind, NodeKind::RustMethod);
        assert_eq!(fmt.parent_qualified_name.as_deref(), Some("Widget"));
    }

    #[test]
    fn empty_and_garbage_inputs_are_safe() {
        assert_eq!(scan(""), RustScan::default());
        let _ = scan("fn (((");
        let _ = scan("impl impl impl trait fn fn");
        let _ = scan("ä̲̅ unicode 💥 fn 名前() {}");
    }

    #[test]
    fn pathologically_deep_nesting_does_not_overflow() {
        let depth = 5_000;
        let mut src = String::new();
        for i in 0..depth {
            src.push_str(&format!("mod m{i} {{\n"));
        }
        src.push_str("fn deep() {}\n");
        for _ in 0..depth {
            src.push('}');
        }
        let _ = scan(&src);
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn scan_never_panics_on_arbitrary_input(s in ".*") {
            let _ = scan(&s);
        }

        #[test]
        fn scan_is_deterministic(s in ".*") {
            prop_assert_eq!(scan(&s), scan(&s));
        }

        #[test]
        fn emitted_symbols_are_well_formed(s in ".*") {
            let scanned = scan(&s);
            for sym in &scanned.symbols {
                prop_assert!(!sym.name.is_empty(), "empty symbol name");
                prop_assert!(!sym.qualified_name.is_empty(), "empty qualified name");
                prop_assert!(sym.start_line >= 1, "1-based start line");
                prop_assert!(
                    sym.end_line >= sym.start_line,
                    "end_line {} < start_line {}",
                    sym.end_line,
                    sym.start_line
                );
                let last = sym.qualified_name.rsplit("::").next().unwrap_or("");
                prop_assert_eq!(last, sym.name.as_str());
            }
        }

        #[test]
        fn imports_are_normalised(s in ".*") {
            let scanned = scan(&s);
            for imp in &scanned.imports {
                prop_assert_eq!(imp.path.trim(), imp.path.as_str());
                prop_assert!(!imp.path.ends_with(';'), "import keeps trailing ;");
            }
        }
    }
}
