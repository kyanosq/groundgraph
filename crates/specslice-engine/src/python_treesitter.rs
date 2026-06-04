//! P22/P23 — Python language spec for the generic tree-sitter driver.
//!
//! Owns `.py` / `.pyi` and is the **sole structural backend** for Python
//! (P23.1 收敛): classes / functions / methods, pytest tests, framework
//! decorator metadata, and `src/`-layout import resolution all flow from
//! here. The LSP adapter (`pyright`/`pylsp`) is demoted to an optional
//! Tier-3 enrichment that only overlays `Calls` / `References` by the same
//! symbol id (see [`crate::python_indexer`]).

use crate::python_frameworks::{classify_decorators, FrameworkRole};
use crate::treesitter::{
    body_from_field, keep_callable_kind, name_from_field, no_call_test, no_text, node_text,
    LangSpec, RefKind, SymKind, TestKind, MAX_NESTING_DEPTH,
};
use specslice_core::NodeKind;

fn python_language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

fn python_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        "class_definition" => Some(SymKind::Type(NodeKind::PythonClass)),
        _ => None,
    }
}

fn python_is_callable(kind: &str) -> bool {
    kind == "function_definition"
}

/// Collect the decorator strings (without the leading `@`) attached to a
/// `class_definition` / `function_definition` node. Decorated defs parse
/// as `decorated_definition → [decorator…, definition]`, so the decorators
/// are siblings reachable via the node's parent.
fn python_decorators(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    if parent.kind() != "decorated_definition" {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cursor = parent.walk();
    for child in parent.named_children(&mut cursor) {
        if child.kind() == "decorator" {
            if let Some(text) = node_text(child, src) {
                let bare = text.trim().trim_start_matches('@').trim();
                if !bare.is_empty() {
                    out.push(bare.to_string());
                }
            }
        }
    }
    out
}

fn is_pytest_fixture_decorator(decorator: &str) -> bool {
    let bare = decorator.split('(').next().unwrap_or("").trim();
    matches!(
        bare,
        "pytest.fixture" | "fixture" | "pytest_asyncio.fixture"
    )
}

/// Reclassify pytest declarations as tests (mirrors pytest's default
/// collection rules): `class Test*` at module level → group; `def test_*`
/// (free function, or method inside a `Test*` group) → case. A
/// `@pytest.fixture` is never a test even if named `test_*`.
fn python_test_of(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    kind: NodeKind,
    name: &str,
    parent_qualified: Option<&str>,
) -> Option<TestKind> {
    if kind == NodeKind::PythonClass {
        if name.starts_with("Test") && parent_qualified.is_none() {
            return Some(TestKind::Group);
        }
        return None;
    }
    if !matches!(kind, NodeKind::PythonFunction | NodeKind::PythonMethod) {
        return None;
    }
    if !name.starts_with("test_") {
        return None;
    }
    if python_decorators(node, src)
        .iter()
        .any(|d| is_pytest_fixture_decorator(d))
    {
        return None;
    }
    if kind == NodeKind::PythonMethod {
        let in_group = parent_qualified
            .and_then(|p| p.rsplit('.').next())
            .map(|tail| tail.starts_with("Test"))
            .unwrap_or(false);
        return in_group.then_some(TestKind::Case);
    }
    Some(TestKind::Case)
}

/// Classify framework decorators (FastAPI route, Celery task, Click/Typer
/// command, dataclass, …) into the JSON [`FrameworkRole`] stored on the
/// symbol node's `metadata_json`. `None` for plain symbols.
fn python_metadata_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    let decorators = python_decorators(node, src);
    if decorators.is_empty() {
        return None;
    }
    let role = classify_decorators(&decorators)?;
    serde_json::to_string(&role).ok()
}

/// True when a symbol's `metadata_json` decodes to a framework entry point
/// — used by [`crate::python_indexer`] to count detected entrypoints
/// without re-parsing the source.
pub fn metadata_is_framework_entrypoint(metadata_json: &str) -> bool {
    serde_json::from_str::<FrameworkRole>(metadata_json)
        .map(|role| role.is_framework_entrypoint())
        .unwrap_or(false)
}

fn python_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    match node.kind() {
        "import_statement" => {
            let mut out = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "dotted_name" => {
                        if let Some(t) = node_text(child, src) {
                            out.push(t.to_string());
                        }
                    }
                    "aliased_import" => {
                        if let Some(name) = child.child_by_field_name("name") {
                            if let Some(t) = node_text(name, src) {
                                out.push(t.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
            out
        }
        "import_from_statement" => node
            .child_by_field_name("module_name")
            .and_then(|m| node_text(m, src))
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn python_is_transparent(kind: &str) -> bool {
    kind == "decorated_definition"
}

/// Resolve a Python module target to a repo-relative file. Handles flat
/// layouts (`app.foo` → `app/foo.py`), `src/`-style layouts (via the
/// discovered source roots), package `__init__.py`, and relative imports
/// (`.utils`, `..pkg.mod`). External deps (`os`, `fastapi`) resolve to
/// `None` so they never inject dangling file nodes.
fn python_resolve_import(
    raw: &str,
    from_file: &str,
    all_files: &[String],
    src_roots: &[String],
) -> Option<String> {
    let module = raw.trim();
    if module.is_empty() {
        return None;
    }
    if let Some(stripped) = module.strip_prefix('.') {
        let mut dots = 1usize;
        let mut tail = stripped;
        while let Some(rest) = tail.strip_prefix('.') {
            dots += 1;
            tail = rest;
        }
        let from_parts: Vec<&str> = from_file.split('/').collect();
        let pkg_len = from_parts.len().saturating_sub(1);
        if dots > pkg_len {
            return None;
        }
        let mut base: Vec<&str> = from_parts[..pkg_len.saturating_sub(dots - 1)].to_vec();
        if !tail.is_empty() {
            base.extend(tail.split('.'));
        }
        let candidate = base.join("/");
        return python_resolve_candidate(all_files, &candidate);
    }
    let base = module.replace('.', "/");
    if let Some(hit) = python_resolve_candidate(all_files, &base) {
        return Some(hit);
    }
    for root in src_roots {
        let candidate = if root.is_empty() {
            base.clone()
        } else {
            format!("{root}/{base}")
        };
        if candidate == base {
            continue;
        }
        if let Some(hit) = python_resolve_candidate(all_files, &candidate) {
            return Some(hit);
        }
    }
    None
}

fn python_resolve_candidate(all_files: &[String], base: &str) -> Option<String> {
    let module_file = format!("{base}.py");
    let package_init = format!("{base}/__init__.py");
    all_files
        .iter()
        .find(|f| **f == module_file || **f == package_init)
        .cloned()
}

/// Discover Python source roots from the `__init__.py` chain: any directory
/// that is *not* itself a package but whose child is becomes a source root
/// (the `src/`-layout case). The empty root is always included so flat
/// layouts keep resolving. Deepest-first so the most specific root wins.
fn python_src_roots(all_files: &[String]) -> Vec<String> {
    let mut init_dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for file in all_files {
        let trimmed = file.strip_suffix("/__init__.py").or({
            if file == "__init__.py" {
                Some("")
            } else {
                None
            }
        });
        if let Some(dir) = trimmed {
            init_dirs.insert(dir.to_string());
        }
    }
    let mut roots: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    roots.insert(String::new());
    for dir in &init_dirs {
        let mut cur = dir.clone();
        loop {
            let parent = match cur.rfind('/') {
                Some(idx) => cur[..idx].to_string(),
                None => String::new(),
            };
            if !init_dirs.contains(&parent) {
                roots.insert(parent);
                break;
            }
            // The repo root is itself a package (`parent == cur == ""`):
            // there is no higher directory to climb to, so stop. Without
            // this guard the walk loops forever on a top-level `__init__.py`.
            if parent == cur {
                break;
            }
            cur = parent;
        }
    }
    let mut out: Vec<String> = roots.into_iter().collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.matches('/').count() + usize::from(!r.is_empty())));
    out
}

/// Heuristic outbound call / reference identifiers from a Python callable
/// body (see [`crate::treesitter::resolve_heuristic_refs`]). Captures:
/// - `helper()` / `Widget()` → `Call` to `helper` / the class `Widget`
///   (Python construction *is* a call, so the class becomes reachable).
/// - `self.method()` / `obj.method()` → `Call` to the trailing attribute
///   name (links to a same-file / imported function or method).
///
/// Dotted stdlib / third-party calls (`os.getcwd`, `json.loads`) carry only
/// their trailing name, so they resolve to nothing unless a local symbol of
/// that name exists — keeping the noise floor bounded.
fn py_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_py_calls(body, src, &mut out, 0);
    out
}

fn collect_py_calls(
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
        if child.kind() == "call" {
            if let Some(name) = child
                .child_by_field_name("function")
                .and_then(|func| py_callee_name(func, src))
            {
                out.push((name, RefKind::Call));
            }
        }
        collect_py_calls(child, src, out, depth + 1);
    }
}

/// Best-effort callee name for a Python `call` node's `function`.
fn py_callee_name(func: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" => node_text(func, src).map(str::to_string),
        // `obj.method(...)` / `self.method(...)` → trailing attribute name.
        "attribute" => func
            .child_by_field_name("attribute")
            .and_then(|a| node_text(a, src))
            .map(str::to_string),
        _ => None,
    }
}

pub(crate) static PYTHON_SPEC: LangSpec = LangSpec {
    language_id: "python",
    grammar: python_language,
    extensions: &["py", "pyi"],
    skip_dirs: &[
        ".git",
        "__pycache__",
        ".venv",
        "venv",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".tox",
        ".eggs",
        "build",
        "dist",
        "node_modules",
        "site-packages",
    ],
    separator: ".",
    func_kind: NodeKind::PythonFunction,
    method_kind: NodeKind::PythonMethod,
    container_of: python_container_of,
    is_callable_kind: python_is_callable,
    callable_kind_of: keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: no_text,
    import_of: python_import_of,
    name_of: name_from_field,
    body_of: body_from_field,
    is_transparent_kind: python_is_transparent,
    metadata_of: python_metadata_of,
    test_of: python_test_of,
    call_test_of: no_call_test,
    src_roots_of: python_src_roots,
    resolve_import: python_resolve_import,
    recurse_callables: false,
    call_idents_of: py_call_idents,
    module_scoped_resolution: false,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&PYTHON_SPEC, src)
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
    fn captures_bare_attribute_and_construction_calls() {
        let src = "\
class Animal:
    def speak(self):
        return self.noise()

    def noise(self):
        return \"rawr\"


def make():
    a = Animal()
    return a.speak()
";
        let got = refs(&scan(src));
        assert!(
            got.contains(&("Animal.speak".into(), "noise".into(), RefKind::Call)),
            "self-method call via attribute: {got:?}"
        );
        assert!(
            got.contains(&("make".into(), "Animal".into(), RefKind::Call)),
            "construction is a call to the class: {got:?}"
        );
        assert!(
            got.contains(&("make".into(), "speak".into(), RefKind::Call)),
            "attribute call on a local: {got:?}"
        );
    }

    #[test]
    fn captures_module_level_and_class_body_references() {
        let src = "\
def _amihud(window):
    return window

class Spec:
    pass

FACTORS = [_amihud(20)]

class Registry:
    default = Spec()
";
        let got = refs(&scan(src));
        // Module-level registration: not owned by any symbol, so it is
        // anchored on the empty (file) scope.
        assert!(
            got.contains(&(String::new(), "_amihud".into(), RefKind::Call)),
            "module-level call should be captured at file scope: {got:?}"
        );
        // Class-body field initializer: attributed to the enclosing class so
        // the constructed type stays reachable when the class is.
        assert!(
            got.contains(&("Registry".into(), "Spec".into(), RefKind::Call)),
            "class-body construction should attribute to the class: {got:?}"
        );
    }

    #[test]
    fn classes_methods_functions_and_imports() {
        let src = r#"
import os
import sys, json
from typing import List

class Animal:
    def __init__(self):
        pass
    def speak(self):
        return "rawr"

@some.decorator
class Decorated:
    def m(self):
        pass

def top_level():
    pass
"#;
        let s = scan(src);
        let classes = qnames(&s, NodeKind::PythonClass);
        assert!(classes.contains(&"Animal".to_string()), "{classes:?}");
        assert!(classes.contains(&"Decorated".to_string()), "{classes:?}");
        let methods = qnames(&s, NodeKind::PythonMethod);
        assert!(
            methods.contains(&"Animal.__init__".to_string()),
            "{methods:?}"
        );
        assert!(methods.contains(&"Animal.speak".to_string()), "{methods:?}");
        assert!(
            methods.contains(&"Decorated.m".to_string()),
            "decorated class methods should nest, got {methods:?}"
        );
        assert!(qnames(&s, NodeKind::PythonFunction).contains(&"top_level".to_string()));
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        for want in ["os", "sys", "json", "typing"] {
            assert!(
                imports.contains(&want),
                "missing import {want}: {imports:?}"
            );
        }
    }

    #[test]
    fn nested_function_in_method_is_not_a_top_level_symbol() {
        let src = "class A:\n    def outer(self):\n        def inner():\n            pass\n";
        let s = scan(src);
        // inner() lives in a function body we deliberately don't descend.
        assert!(!qnames(&s, NodeKind::PythonFunction).contains(&"inner".to_string()));
        assert!(qnames(&s, NodeKind::PythonMethod).contains(&"A.outer".to_string()));
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("def def def class : : :");
        let _ = scan("class 名前:\n    def 方法(self): pass\n");
    }

    #[test]
    fn pytest_declarations_become_tests_and_others_stay_symbols() {
        let src = "\
import pytest


def test_top():
    pass


@pytest.fixture
def test_looks_like_a_test_but_is_a_fixture():
    return None


class TestThing:
    def test_inside(self):
        pass

    def helper(self):
        pass
";
        let s = scan(src);
        let cases: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        let groups: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Group)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(cases.contains(&"test_top"), "{cases:?}");
        assert!(groups.contains(&"TestThing"), "{groups:?}");
        assert!(cases.contains(&"TestThing.test_inside"), "{cases:?}");
        // The non-test method stays a structural symbol.
        assert!(qnames(&s, NodeKind::PythonMethod).contains(&"TestThing.helper".to_string()));
        // A @pytest.fixture is never a test even when named `test_*`.
        assert!(
            qnames(&s, NodeKind::PythonFunction)
                .contains(&"test_looks_like_a_test_but_is_a_fixture".to_string()),
            "fixture must remain a function symbol, not a test"
        );
    }

    #[test]
    fn framework_decorators_attach_metadata_to_symbols() {
        let src = "\
@router.get(\"/items\")
def list_items():
    return []


@dataclass
class Item:
    id: int
";
        let s = scan(src);
        let list_items = s
            .symbols
            .iter()
            .find(|sym| sym.qualified_name == "list_items")
            .expect("list_items symbol");
        let meta = list_items
            .metadata
            .as_deref()
            .expect("metadata present for fastapi route");
        assert!(metadata_is_framework_entrypoint(meta), "{meta}");
        let role: FrameworkRole = serde_json::from_str(meta).unwrap();
        assert_eq!(role.family(), "fastapi_route");
        // dataclass attaches metadata but is not an entrypoint.
        let item = s
            .symbols
            .iter()
            .find(|sym| sym.qualified_name == "Item")
            .expect("Item symbol");
        let item_meta = item.metadata.as_deref().expect("dataclass metadata");
        assert!(!metadata_is_framework_entrypoint(item_meta));
    }

    #[test]
    fn src_roots_terminate_when_repo_root_is_itself_a_package() {
        // A repo whose top level is a package (`__init__.py` at the very
        // root) puts "" into the init-dir set. The source-root walk must
        // still terminate — historically the parent-of-"" is "" which is
        // also a package, so a naive loop spun forever (regression guard).
        let files = vec![
            "__init__.py".to_string(),
            "config/__init__.py".to_string(),
            "config/loader.py".to_string(),
            "workflow.py".to_string(),
        ];
        let roots = python_src_roots(&files);
        // The empty root is always a valid source root.
        assert!(roots.contains(&String::new()), "{roots:?}");
        // And a relative resolution against it still works.
        assert_eq!(
            python_resolve_import("config.loader", "workflow.py", &files, &roots),
            Some("config/loader.py".to_string())
        );
    }

    #[test]
    fn import_resolution_handles_flat_relative_src_layout_and_drops_external() {
        let files = vec![
            "app/__init__.py".to_string(),
            "app/greeter.py".to_string(),
            "app/utils.py".to_string(),
            "backend/app/__init__.py".to_string(),
            "backend/app/core/__init__.py".to_string(),
            "backend/app/core/config.py".to_string(),
        ];
        let roots = python_src_roots(&files);
        assert!(roots.contains(&String::new()), "{roots:?}");
        assert!(roots.contains(&"backend".to_string()), "{roots:?}");
        // Flat package import.
        assert_eq!(
            python_resolve_import("app.greeter", "tests/test_greeter.py", &files, &roots),
            Some("app/greeter.py".to_string())
        );
        // Bare package → __init__.py.
        assert_eq!(
            python_resolve_import("app", "tests/test_greeter.py", &files, &roots),
            Some("app/__init__.py".to_string())
        );
        // Relative import resolves against the importer's package.
        assert_eq!(
            python_resolve_import(".utils", "app/greeter.py", &files, &roots),
            Some("app/utils.py".to_string())
        );
        // src/-layout import via the discovered `backend` root.
        assert_eq!(
            python_resolve_import("app.core.config", "backend/tests/test_x.py", &files, &roots),
            Some("backend/app/core/config.py".to_string())
        );
        // External dependency drops (no dangling node).
        assert_eq!(
            python_resolve_import("os", "app/greeter.py", &files, &roots),
            None
        );
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
            prop_assert_eq!(extract(&PYTHON_SPEC, &s), extract(&PYTHON_SPEC, &s));
        }

        #[test]
        fn symbols_are_well_formed(s in ".*") {
            for sym in extract(&PYTHON_SPEC, &s).symbols {
                prop_assert!(!sym.name.is_empty());
                prop_assert!(!sym.qualified_name.is_empty());
                prop_assert!(sym.end_line >= sym.start_line);
            }
        }
    }
}
