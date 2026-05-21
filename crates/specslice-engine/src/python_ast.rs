//! P16 — minimal Python AST scanner used to补 LSP-derived facts.
//!
//! The scanner is intentionally regex-light and indentation-driven so we
//! can run in pure Rust without pulling in a heavy CPython-compatible
//! parser. It recovers:
//!
//! - module / class / function / method declarations with stable
//!   `qualified_name` (used for symbol IDs and parent linking);
//! - `import foo` and `from foo import bar` edges, mapped to repo-local
//!   files when possible;
//! - pytest test cases / groups (`def test_xxx`, `class Test*:`,
//!   `@pytest.fixture`, `@pytest.mark.parametrize`).
//!
//! It is **not** a substitute for an LSP server. `Calls` / `References`
//! are not emitted here — those rely on a real `callHierarchy` provider.
//! Treat this layer as a "documentSymbol fallback + imports + pytest"
//! supplement that always runs alongside LSP and stays useful when the
//! LSP server is missing.

use specslice_core::NodeKind;

/// One declaration recovered from a Python source file. The scanner
/// preserves source order so callers can rebuild a stable parent tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonSymbol {
    pub kind: NodeKind,
    /// Bare identifier as it appears in the source (e.g. `greet`).
    pub name: String,
    /// Dot-joined qualified path within the module
    /// (e.g. `Greeter.greet`). Root-level functions / classes use just
    /// `name`. Pytest test classes / methods use the same scheme so a
    /// symbol id can be derived as `<file_rel>::<qualified_name>`.
    pub qualified_name: String,
    /// 1-based start / end lines pulled from the `def` / `class` line
    /// and the next outdent.
    pub start_line: u32,
    pub end_line: u32,
    /// Indentation-derived parent. `None` for top-level declarations.
    pub parent_qualified_name: Option<String>,
    /// True iff `@pytest.fixture` (or `@pytest.fixture(...)`) appears
    /// in the decorator stack immediately above the `def`.
    pub is_pytest_fixture: bool,
    /// True iff at least one `@pytest.mark.parametrize(...)` appears in
    /// the decorator stack. Useful as auxiliary metadata; not used to
    /// gate `TestCase` classification.
    pub has_parametrize: bool,
    /// True iff the declaration is `async def` rather than plain `def`.
    /// Mirrors LSP's lack of a distinct async function kind.
    pub is_async: bool,
}

/// `import x.y`, `from x.y import a, b`, `from . import sibling` …
/// `module_path` is the dotted module name; relative-imports keep the
/// leading dots so callers can resolve them against `file_rel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonImport {
    pub module_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PythonScan {
    pub symbols: Vec<PythonSymbol>,
    pub imports: Vec<PythonImport>,
}

/// Walk a Python source string and recover the bits we need for the
/// SpecSlice graph. The scanner is tolerant of partial files (e.g.
/// docstring-only modules) and never panics on malformed input.
pub fn scan(source: &str) -> PythonScan {
    let lines: Vec<&str> = source.lines().collect();
    let mut symbols: Vec<PythonSymbol> = Vec::new();
    let mut imports: Vec<PythonImport> = Vec::new();
    // (indent_spaces, qualified_name, kind) — the active enclosing
    // class/function stack. We push when we see a `def`/`class` at
    // strictly deeper indentation, and pop when indent recedes.
    let mut scope_stack: Vec<(usize, String, NodeKind)> = Vec::new();
    // Decorators accumulated for the next non-decorator declaration.
    let mut pending_decorators: Vec<String> = Vec::new();
    // String-literal awareness for `"""docstrings"""` so we do not
    // mis-parse a `def` inside a docstring. We only handle the simple
    // case of standalone triple-quote blocks; mid-line triples are
    // rare in real code and the worst-case is an extra symbol entry.
    let mut in_triple: Option<&'static str> = None;

    for (idx, raw_line) in lines.iter().enumerate() {
        // `idx` is bounded by the number of lines in a file we just
        // read into memory; truncating to u32 only matters past 4G
        // lines of Python (~hundreds of GB of source) which the rest
        // of the engine cannot ingest anyway.
        let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        let line = *raw_line;
        let trimmed = line.trim_start();

        // Triple-quoted string tracking.
        if let Some(closer) = in_triple {
            if line.contains(closer) {
                in_triple = None;
            }
            continue;
        }
        // Detect the *start* of a standalone triple-quoted block. We
        // only enter the block when the closing triple does not appear
        // later on the same line. Strings used as expressions
        // (`x = """..."""`) are unaffected because we look only at the
        // leading non-whitespace token.
        if let Some(opener) = leading_triple(trimmed) {
            let rest = &trimmed[opener.len()..];
            if !rest.contains(opener) {
                in_triple = Some(opener);
                continue;
            }
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - trimmed.len();

        // Pop scopes that are no longer enclosing.
        while let Some((scope_indent, _, _)) = scope_stack.last() {
            if indent <= *scope_indent {
                scope_stack.pop();
            } else {
                break;
            }
        }

        if let Some(rest) = trimmed.strip_prefix('@') {
            pending_decorators.push(rest.trim().to_string());
            continue;
        }

        if let Some(rest) = strip_keyword(trimmed, "import ") {
            for module in parse_import_targets(rest) {
                imports.push(PythonImport {
                    module_path: module,
                });
            }
            pending_decorators.clear();
            continue;
        }
        if let Some(rest) = strip_keyword(trimmed, "from ") {
            if let Some(idx) = rest.find(" import ") {
                let module = rest[..idx].trim().to_string();
                if !module.is_empty() {
                    imports.push(PythonImport {
                        module_path: module,
                    });
                }
            }
            pending_decorators.clear();
            continue;
        }

        let (is_async, def_rest) = match strip_keyword(trimmed, "async def ") {
            Some(rest) => (true, Some(rest)),
            None => (false, strip_keyword(trimmed, "def ")),
        };
        if let Some(rest) = def_rest {
            if let Some(name) = read_identifier(rest) {
                let parent = scope_stack.last().map(|(_, q, _)| q.clone());
                let parent_kind = scope_stack.last().map(|(_, _, k)| *k);
                let kind = if matches!(parent_kind, Some(NodeKind::PythonClass)) {
                    NodeKind::PythonMethod
                } else {
                    NodeKind::PythonFunction
                };
                let qualified = match &parent {
                    Some(q) => format!("{q}.{name}"),
                    None => name.clone(),
                };
                let is_fixture = pending_decorators
                    .iter()
                    .any(|d| is_pytest_fixture_decorator(d));
                let has_parametrize = pending_decorators
                    .iter()
                    .any(|d| is_pytest_parametrize_decorator(d));
                symbols.push(PythonSymbol {
                    kind,
                    name: name.clone(),
                    qualified_name: qualified.clone(),
                    start_line: line_no,
                    end_line: line_no,
                    parent_qualified_name: parent,
                    is_pytest_fixture: is_fixture,
                    has_parametrize,
                    is_async,
                });
                scope_stack.push((indent, qualified, kind));
                pending_decorators.clear();
            }
            continue;
        }

        if let Some(rest) = strip_keyword(trimmed, "class ") {
            if let Some(name) = read_identifier(rest) {
                let parent = scope_stack.last().map(|(_, q, _)| q.clone());
                let qualified = match &parent {
                    Some(q) => format!("{q}.{name}"),
                    None => name.clone(),
                };
                symbols.push(PythonSymbol {
                    kind: NodeKind::PythonClass,
                    name: name.clone(),
                    qualified_name: qualified.clone(),
                    start_line: line_no,
                    end_line: line_no,
                    parent_qualified_name: parent,
                    is_pytest_fixture: false,
                    has_parametrize: false,
                    is_async: false,
                });
                scope_stack.push((indent, qualified, NodeKind::PythonClass));
                pending_decorators.clear();
            }
            continue;
        }

        // Any other code line drops the decorator stack — decorators
        // only attach to the very next class/function declaration.
        pending_decorators.clear();
    }

    // Fill in end_line by scanning forward until indent recedes to or
    // below the symbol's defining indent. We re-walk the lines once to
    // avoid bookkeeping during the first pass.
    fill_end_lines(&mut symbols, &lines);

    PythonScan { symbols, imports }
}

fn fill_end_lines(symbols: &mut [PythonSymbol], lines: &[&str]) {
    if symbols.is_empty() {
        return;
    }
    // We need each symbol's defining indent to compute its body end.
    let mut indents: Vec<usize> = Vec::with_capacity(symbols.len());
    for sym in symbols.iter() {
        let line_idx = sym.start_line as usize - 1;
        let raw = lines.get(line_idx).copied().unwrap_or("");
        indents.push(raw.len() - raw.trim_start().len());
    }
    for (idx, sym) in symbols.iter_mut().enumerate() {
        let start = sym.start_line as usize - 1;
        let own_indent = indents[idx];
        let mut end = sym.start_line;
        for (offset, raw) in lines.iter().enumerate().skip(start + 1) {
            let trimmed = raw.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = raw.len() - trimmed.len();
            if indent <= own_indent {
                break;
            }
            end = u32::try_from(offset + 1).unwrap_or(u32::MAX);
        }
        sym.end_line = end.max(sym.start_line);
    }
}

fn leading_triple(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("\"\"\"") {
        Some("\"\"\"")
    } else if trimmed.starts_with("'''") {
        Some("'''")
    } else {
        None
    }
}

fn strip_keyword<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    let rest = haystack.strip_prefix(needle)?;
    Some(rest)
}

fn read_identifier(rest: &str) -> Option<String> {
    let mut ident = String::new();
    for c in rest.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            ident.push(c);
        } else {
            break;
        }
    }
    if ident.is_empty() || ident.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        None
    } else {
        Some(ident)
    }
}

fn parse_import_targets(rest: &str) -> Vec<String> {
    rest.split(',')
        .map(|part| {
            // Strip trailing comment / `as alias` clauses.
            let head = part.split('#').next().unwrap_or("");
            let head = head.split(" as ").next().unwrap_or(head);
            head.trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn is_pytest_fixture_decorator(decorator: &str) -> bool {
    let bare = decorator.split('(').next().unwrap_or("").trim();
    matches!(
        bare,
        "pytest.fixture" | "fixture" | "pytest_asyncio.fixture"
    )
}

fn is_pytest_parametrize_decorator(decorator: &str) -> bool {
    let bare = decorator.split('(').next().unwrap_or("").trim();
    matches!(bare, "pytest.mark.parametrize" | "parametrize")
}

/// True for `def test_*` or for any method inside a class whose
/// qualified prefix starts with `Test`. Mirrors pytest's default
/// collection rules; we keep the same rules so the AST scanner stays
/// agnostic of pytest configuration.
pub fn is_pytest_test_function(sym: &PythonSymbol) -> bool {
    if sym.is_pytest_fixture {
        return false;
    }
    if !matches!(sym.kind, NodeKind::PythonFunction | NodeKind::PythonMethod) {
        return false;
    }
    if !sym.name.starts_with("test_") {
        return false;
    }
    if matches!(sym.kind, NodeKind::PythonMethod) {
        return sym
            .parent_qualified_name
            .as_deref()
            .map(|p| {
                p.rsplit('.')
                    .next()
                    .map(|tail| tail.starts_with("Test"))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
    }
    true
}

/// True for `class Test*:` declarations at module level. Nested test
/// classes are uncommon; we keep the simple rule for now.
pub fn is_pytest_test_class(sym: &PythonSymbol) -> bool {
    sym.kind == NodeKind::PythonClass
        && sym.name.starts_with("Test")
        && sym.parent_qualified_name.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_module_level_classes_and_functions() {
        let source = r#"
"""module docstring"""

def top_level():
    return 1


class Greeter:
    def greet(self):
        return 'hi'

    async def stream(self):
        return None
"#;
        let scan = scan(source);
        assert_eq!(scan.imports.len(), 0);
        let names: Vec<_> = scan
            .symbols
            .iter()
            .map(|s| {
                (
                    s.kind,
                    s.qualified_name.as_str(),
                    s.parent_qualified_name.as_deref(),
                )
            })
            .collect();
        assert!(names.contains(&(NodeKind::PythonFunction, "top_level", None)));
        assert!(names.contains(&(NodeKind::PythonClass, "Greeter", None)));
        assert!(names.contains(&(NodeKind::PythonMethod, "Greeter.greet", Some("Greeter"))));
        assert!(names.contains(&(NodeKind::PythonMethod, "Greeter.stream", Some("Greeter"))));
        let stream = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "Greeter.stream")
            .unwrap();
        assert!(stream.is_async);
    }

    #[test]
    fn captures_imports_and_from_imports() {
        let source = r#"
import os
from app.greeter import Greeter, make_greeter as mk
from .utils import banner
"#;
        let scan = scan(source);
        let modules: Vec<_> = scan
            .imports
            .iter()
            .map(|i| i.module_path.as_str())
            .collect();
        assert_eq!(modules, vec!["os", "app.greeter", ".utils"]);
    }

    #[test]
    fn detects_pytest_test_functions_and_fixtures() {
        let source = r#"
import pytest


@pytest.fixture
def casual_greeter():
    return None


@pytest.mark.parametrize("name", ["a", "b"])
def test_make_greeter_supports_names(name):
    assert name


class TestGoodbye:
    def test_uses_name(self):
        assert True
"#;
        let scan = scan(source);
        let casual = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "casual_greeter")
            .expect("fixture detected");
        assert!(casual.is_pytest_fixture);
        assert!(!is_pytest_test_function(casual));

        let test_make = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "test_make_greeter_supports_names")
            .expect("test detected");
        assert!(test_make.has_parametrize);
        assert!(is_pytest_test_function(test_make));

        let test_class = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "TestGoodbye")
            .expect("test class detected");
        assert!(is_pytest_test_class(test_class));

        let test_method = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "TestGoodbye.test_uses_name")
            .expect("test method detected");
        assert!(is_pytest_test_function(test_method));
    }

    #[test]
    fn ignores_def_inside_docstrings() {
        let source = r#"
"""
def not_a_function():
    pass
"""

def actual_function():
    return 1
"#;
        let scan = scan(source);
        let names: Vec<_> = scan
            .symbols
            .iter()
            .map(|s| s.qualified_name.as_str())
            .collect();
        assert_eq!(names, vec!["actual_function"]);
    }

    #[test]
    fn fills_end_line_to_last_indented_body_line() {
        let source = "class Foo:\n    def bar(self):\n        x = 1\n        return x\n\ndef baz():\n    return 2\n";
        let scan = scan(source);
        let foo = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "Foo")
            .unwrap();
        let bar = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "Foo.bar")
            .unwrap();
        let baz = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "baz")
            .unwrap();
        assert_eq!(foo.start_line, 1);
        assert_eq!(foo.end_line, 4);
        assert_eq!(bar.start_line, 2);
        assert_eq!(bar.end_line, 4);
        assert_eq!(baz.start_line, 6);
        assert_eq!(baz.end_line, 7);
    }
}
