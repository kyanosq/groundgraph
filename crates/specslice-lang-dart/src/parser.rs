#![allow(clippy::too_many_arguments)]
//! Lightweight Dart parser. Line-based, brace-aware, no resolved AST.
//!
//! Goals:
//! - Be cheap and predictable. Dart's full grammar is out of scope.
//! - Produce stable artifact IDs and line ranges good enough for PR Impact.
//! - Ignore comments completely. SpecSlice is non-invasive and must not
//!   depend on tool-specific annotations in business code.
//!
//! Non-goals: typedef, mixins, enums, extension methods, type parameters,
//! async modifiers — they are accepted but not modelled as separate kinds.

use specslice_core::artifact_id::{
    dart_class_id, dart_constructor_id, dart_function_id, dart_group_id, dart_method_id,
    dart_test_id, file_id, slugify,
};
use specslice_core::language_batch::{
    AdapterDiagnostic, FileArtifact, ImportEdge, SymbolArtifact, SymbolRange, TestArtifact,
};
use specslice_core::{ArtifactId, NodeKind};

pub struct ParseResult {
    pub file: FileArtifact,
    pub symbols: Vec<SymbolArtifact>,
    pub tests: Vec<TestArtifact>,
    pub imports: Vec<ImportEdge>,
    pub ranges: Vec<SymbolRange>,
    pub diagnostics: Vec<AdapterDiagnostic>,
}

struct ClassFrame {
    id: ArtifactId,
    name: String,
    depth: usize,
}

pub fn parse_dart(path: &str, source: &str, content_hash: &str) -> ParseResult {
    let mut symbols = Vec::new();
    let mut tests = Vec::new();
    let mut imports = Vec::new();
    let mut ranges = Vec::new();
    let diagnostics = Vec::new();

    let file_artifact = FileArtifact {
        id: file_id(path),
        path: path.to_string(),
        language: "dart".into(),
        content_hash: content_hash.to_string(),
    };

    let mut class_stack: Vec<ClassFrame> = Vec::new();
    let mut open_symbol_starts: Vec<(ArtifactId, u32, usize)> = Vec::new();
    let mut depth = 0usize;
    let mut total_lines = 0u32;

    for (idx, raw_line) in source.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        total_lines = line_no;
        let line = strip_inline_string_braces(raw_line);
        let trimmed = line.trim_start();

        // Ignore comments completely. Links are declared outside business
        // source files in `.specslice/links.yaml`.
        if trimmed.starts_with("///") {
            continue;
        }
        // Skip blank lines without clearing doc buffer.
        if trimmed.is_empty() {
            // Update depth and continue.
            update_depth(&line, &mut depth);
            close_symbols(
                &mut open_symbol_starts,
                &mut class_stack,
                &mut ranges,
                &mut symbols,
                &mut tests,
                depth,
                line_no,
                path,
            );
            continue;
        }

        // Import directives.
        if let Some(target) = parse_import(trimmed) {
            imports.push(ImportEdge {
                from_file: file_artifact.id.clone(),
                to_path: target,
            });
            update_depth(&line, &mut depth);
            close_symbols(
                &mut open_symbol_starts,
                &mut class_stack,
                &mut ranges,
                &mut symbols,
                &mut tests,
                depth,
                line_no,
                path,
            );
            continue;
        }

        let current_class = class_stack
            .last()
            .map(|c| (c.id.clone(), c.name.clone(), c.depth));

        // Class declarations.
        if let Some(class_name) = parse_class_header(trimmed) {
            let class_id = dart_class_id(path, &class_name);
            let sym = SymbolArtifact {
                id: class_id.clone(),
                kind: NodeKind::DartClass,
                path: path.to_string(),
                name: class_name.clone(),
                qualified_name: class_name.clone(),
                start_line: line_no,
                end_line: line_no,
                parent_symbol_id: None,
            };
            symbols.push(sym);
            let opens_brace = line.contains('{');
            update_depth(&line, &mut depth);
            if opens_brace {
                class_stack.push(ClassFrame {
                    id: class_id.clone(),
                    name: class_name.clone(),
                    depth,
                });
                open_symbol_starts.push((class_id, line_no, depth));
            }
            close_symbols(
                &mut open_symbol_starts,
                &mut class_stack,
                &mut ranges,
                &mut symbols,
                &mut tests,
                depth,
                line_no,
                path,
            );
            continue;
        }

        // test() / group() calls.
        if let Some(name) = parse_test_call(trimmed) {
            let slug = slugify(&name);
            let id = dart_test_id(path, &slug);
            tests.push(TestArtifact {
                id: id.clone(),
                kind: NodeKind::TestCase,
                path: path.to_string(),
                name: name.clone(),
                start_line: line_no,
                end_line: line_no,
                parent_symbol_id: current_class.as_ref().map(|c| c.0.clone()),
            });
            update_depth(&line, &mut depth);
            close_symbols(
                &mut open_symbol_starts,
                &mut class_stack,
                &mut ranges,
                &mut symbols,
                &mut tests,
                depth,
                line_no,
                path,
            );
            continue;
        }
        if let Some(name) = parse_group_call(trimmed) {
            let slug = slugify(&name);
            let id = dart_group_id(path, &slug);
            tests.push(TestArtifact {
                id: id.clone(),
                kind: NodeKind::TestGroup,
                path: path.to_string(),
                name,
                start_line: line_no,
                end_line: line_no,
                parent_symbol_id: current_class.as_ref().map(|c| c.0.clone()),
            });
            update_depth(&line, &mut depth);
            close_symbols(
                &mut open_symbol_starts,
                &mut class_stack,
                &mut ranges,
                &mut symbols,
                &mut tests,
                depth,
                line_no,
                path,
            );
            continue;
        }

        // Constructor and method/function declarations.
        // Only parse declarations at the class's *direct* child layer.
        // Anything deeper (`depth > class_depth`) is inside a method body
        // and must not be re-parsed — otherwise expressions like
        // `candidates.sort(...)` get misidentified as method declarations.
        let inside_class = current_class
            .as_ref()
            .map(|(_, _, class_depth)| depth == *class_depth)
            .unwrap_or(false);

        if inside_class {
            let class = current_class.as_ref().unwrap();
            if let Some(ctor_name) = parse_constructor(trimmed, &class.1) {
                let id = dart_constructor_id(path, &class.1, &ctor_name);
                let qname = if ctor_name.is_empty() {
                    class.1.clone()
                } else {
                    format!("{}.{}", class.1, ctor_name)
                };
                symbols.push(SymbolArtifact {
                    id: id.clone(),
                    kind: NodeKind::DartConstructor,
                    path: path.to_string(),
                    name: ctor_name.clone(),
                    qualified_name: qname,
                    start_line: line_no,
                    end_line: line_no,
                    parent_symbol_id: Some(class.0.clone()),
                });
                if line.contains('{') {
                    update_depth(&line, &mut depth);
                    open_symbol_starts.push((id, line_no, depth));
                } else {
                    update_depth(&line, &mut depth);
                }
                close_symbols(
                    &mut open_symbol_starts,
                    &mut class_stack,
                    &mut ranges,
                    &mut symbols,
                    &mut tests,
                    depth,
                    line_no,
                    path,
                );
                continue;
            }
            if let Some(method_name) = parse_method(trimmed) {
                let id = dart_method_id(path, &class.1, &method_name);
                symbols.push(SymbolArtifact {
                    id: id.clone(),
                    kind: NodeKind::DartMethod,
                    path: path.to_string(),
                    name: method_name.clone(),
                    qualified_name: format!("{}.{}", class.1, method_name),
                    start_line: line_no,
                    end_line: line_no,
                    parent_symbol_id: Some(class.0.clone()),
                });
                if line.contains('{') {
                    update_depth(&line, &mut depth);
                    open_symbol_starts.push((id, line_no, depth));
                } else {
                    update_depth(&line, &mut depth);
                }
                close_symbols(
                    &mut open_symbol_starts,
                    &mut class_stack,
                    &mut ranges,
                    &mut symbols,
                    &mut tests,
                    depth,
                    line_no,
                    path,
                );
                continue;
            }
        } else if depth == 0 {
            if let Some(name) = parse_top_level_function(trimmed) {
                let id = dart_function_id(path, &name);
                symbols.push(SymbolArtifact {
                    id: id.clone(),
                    kind: NodeKind::DartFunction,
                    path: path.to_string(),
                    name: name.clone(),
                    qualified_name: name.clone(),
                    start_line: line_no,
                    end_line: line_no,
                    parent_symbol_id: None,
                });
                if line.contains('{') {
                    update_depth(&line, &mut depth);
                    open_symbol_starts.push((id, line_no, depth));
                } else {
                    update_depth(&line, &mut depth);
                }
                close_symbols(
                    &mut open_symbol_starts,
                    &mut class_stack,
                    &mut ranges,
                    &mut symbols,
                    &mut tests,
                    depth,
                    line_no,
                    path,
                );
                continue;
            }
        }

        // Default: just update brace depth.
        update_depth(&line, &mut depth);
        close_symbols(
            &mut open_symbol_starts,
            &mut class_stack,
            &mut ranges,
            &mut symbols,
            &mut tests,
            depth,
            line_no,
            path,
        );
    }

    // Anything still open closes at EOF.
    while let Some((id, start, _depth)) = open_symbol_starts.pop() {
        finalize_symbol(
            &id,
            start,
            total_lines,
            path,
            &mut ranges,
            &mut symbols,
            &mut tests,
            &class_stack,
        );
    }

    ParseResult {
        file: file_artifact,
        symbols,
        tests,
        imports,
        ranges,
        diagnostics,
    }
}

fn close_symbols(
    open_symbols: &mut Vec<(ArtifactId, u32, usize)>,
    class_stack: &mut Vec<ClassFrame>,
    ranges: &mut Vec<SymbolRange>,
    symbols: &mut [SymbolArtifact],
    tests: &mut [TestArtifact],
    depth: usize,
    line_no: u32,
    path: &str,
) {
    while let Some(&(_, _, sym_depth)) = open_symbols.last() {
        if depth < sym_depth {
            let (id, start, _) = open_symbols.pop().unwrap();
            finalize_symbol(
                &id,
                start,
                line_no,
                path,
                ranges,
                symbols,
                tests,
                class_stack,
            );
        } else {
            break;
        }
    }
    while let Some(frame) = class_stack.last() {
        if depth < frame.depth {
            let frame = class_stack.pop().unwrap();
            // The class symbol's end_line is set when we close its
            // corresponding open_symbol entry above.
            let _ = frame;
        } else {
            break;
        }
    }
}

fn finalize_symbol(
    id: &ArtifactId,
    start_line: u32,
    end_line: u32,
    path: &str,
    ranges: &mut Vec<SymbolRange>,
    symbols: &mut [SymbolArtifact],
    tests: &mut [TestArtifact],
    class_stack: &[ClassFrame],
) {
    let (kind, qname, parent) = if let Some(sym) = symbols.iter_mut().find(|s| s.id == *id) {
        sym.end_line = end_line;
        (
            sym.kind,
            sym.qualified_name.clone(),
            sym.parent_symbol_id.clone(),
        )
    } else if let Some(t) = tests.iter_mut().find(|t| t.id == *id) {
        t.end_line = end_line;
        (t.kind, t.name.clone(), t.parent_symbol_id.clone())
    } else if let Some(frame) = class_stack.iter().find(|f| f.id == *id) {
        (NodeKind::DartClass, frame.name.clone(), None)
    } else {
        return;
    };

    ranges.push(SymbolRange {
        file_path: path.to_string(),
        symbol_id: id.clone(),
        start_line,
        end_line,
        symbol_kind: kind,
        qualified_name: qname,
        parent_symbol_id: parent,
    });
}

fn update_depth(line: &str, depth: &mut usize) {
    let mut in_string = false;
    let mut quote_char = '"';
    let mut prev = ' ';
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            if ch == quote_char && prev != '\\' {
                in_string = false;
            }
            prev = ch;
            continue;
        }
        match ch {
            '\'' | '"' => {
                in_string = true;
                quote_char = ch;
            }
            '/' if chars.peek() == Some(&'/') => break, // line comment
            '{' => *depth += 1,
            '}' => {
                if *depth > 0 {
                    *depth -= 1;
                }
            }
            _ => {}
        }
        prev = ch;
    }
}

fn strip_inline_string_braces(line: &str) -> String {
    // Conservative pass: keep the line as-is; `update_depth` already skips
    // braces inside string literals.
    line.to_string()
}

fn parse_class_header(line: &str) -> Option<String> {
    let stripped = line
        .trim_start_matches("abstract ")
        .trim_start_matches("base ")
        .trim_start_matches("final ")
        .trim_start_matches("sealed ")
        .trim_start_matches("interface ")
        .trim_start_matches("class ")
        .trim_start();
    if line.trim_start().starts_with("class ")
        || line.trim_start().starts_with("abstract class ")
        || line.trim_start().starts_with("sealed class ")
        || line.trim_start().starts_with("final class ")
        || line.trim_start().starts_with("base class ")
        || line.trim_start().starts_with("interface class ")
    {
        let name: String = stripped
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

fn parse_import(line: &str) -> Option<String> {
    let rest = line.strip_prefix("import ")?;
    let rest = rest.trim_start();
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let end = rest[1..].find(quote)?;
    Some(rest[1..1 + end].to_string())
}

fn parse_test_call(line: &str) -> Option<String> {
    extract_call_arg(line, "test(")
}

fn parse_group_call(line: &str) -> Option<String> {
    extract_call_arg(line, "group(")
}

fn extract_call_arg(line: &str, prefix: &str) -> Option<String> {
    let pos = line.find(prefix)?;
    // Must be at start or preceded by non-identifier char.
    if pos > 0 {
        let prev = line.as_bytes()[pos - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return None;
        }
    }
    let after = &line[pos + prefix.len()..];
    let after = after.trim_start();
    let quote = after.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let inside = &after[1..];
    let end = inside.find(quote)?;
    Some(inside[..end].to_string())
}

fn parse_method(line: &str) -> Option<String> {
    // <return-type>? <name>(args). We skip lines starting with reserved
    // statements (`if`, `for`, `while`, etc).
    if line.starts_with("//") || line.starts_with("///") {
        return None;
    }
    for kw in [
        "if(", "if ", "for(", "for ", "while(", "while ", "switch", "return ", "return;",
    ] {
        if line.starts_with(kw) {
            return None;
        }
    }
    let paren = line.find('(')?;
    let head = &line[..paren];
    let head_trim = head.trim_end();
    let name_part = head_trim.split_whitespace().last()?;
    let cleaned: String = name_part
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    // Filter out `test`/`group` callsites (already detected separately).
    if matches!(cleaned.as_str(), "test" | "group") {
        return None;
    }
    Some(cleaned)
}

fn parse_top_level_function(line: &str) -> Option<String> {
    // Same heuristic as method, but at depth 0.
    parse_method(line)
}

fn parse_constructor(line: &str, class_name: &str) -> Option<String> {
    let trimmed = line.trim_start_matches("const ").trim_start();
    if !trimmed.starts_with(class_name) {
        return None;
    }
    let after = &trimmed[class_name.len()..];
    let mut chars = after.chars();
    match chars.next() {
        Some('(') => Some(String::new()),
        Some('.') => {
            let name: String = chars
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            // Must be followed by `(`.
            let after_name = &after[1 + name.len()..];
            if after_name.starts_with('(') {
                Some(name)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> ParseResult {
        parse_dart("lib/a.dart", src, "hash")
    }

    #[test]
    fn extracts_class_and_methods_ignoring_comments() {
        let src = "/// Ordinary API docs\nclass Foo {\n  Foo();\n  void bar() {}\n}\n";
        let r = parse(src);
        assert_eq!(r.symbols.len(), 3, "class + ctor + method");
        let class = r
            .symbols
            .iter()
            .find(|s| s.kind == NodeKind::DartClass)
            .unwrap();
        assert_eq!(class.name, "Foo");
        let method = r
            .symbols
            .iter()
            .find(|s| s.kind == NodeKind::DartMethod)
            .unwrap();
        assert_eq!(method.name, "bar");
        assert_eq!(method.parent_symbol_id.as_ref().unwrap(), &class.id);
    }

    #[test]
    fn extracts_top_level_function() {
        let src = "void main() {}\n";
        let r = parse(src);
        assert!(r
            .symbols
            .iter()
            .any(|s| s.kind == NodeKind::DartFunction && s.name == "main"));
    }

    #[test]
    fn extracts_named_constructor() {
        let src = "class Foo {\n  Foo.named();\n}\n";
        let r = parse(src);
        let ctor = r
            .symbols
            .iter()
            .find(|s| s.kind == NodeKind::DartConstructor)
            .unwrap();
        assert_eq!(ctor.name, "named");
    }

    #[test]
    fn extracts_test_and_group_ignoring_comments() {
        let src = "void main() {\n  group('Outer', () {\n    /// Ordinary test docs\n    test('inside', () {});\n  });\n}\n";
        let r = parse(src);
        let test = r
            .tests
            .iter()
            .find(|t| t.kind == NodeKind::TestCase)
            .unwrap();
        assert_eq!(test.name, "inside");
        let group = r
            .tests
            .iter()
            .find(|t| t.kind == NodeKind::TestGroup)
            .unwrap();
        assert_eq!(group.name, "Outer");
    }

    #[test]
    fn extracts_imports() {
        let src = "import 'package:foo/bar.dart';\nimport \"package:baz/qux.dart\";\n";
        let r = parse(src);
        assert_eq!(r.imports.len(), 2);
        assert_eq!(r.imports[0].to_path, "package:foo/bar.dart");
        assert_eq!(r.imports[1].to_path, "package:baz/qux.dart");
    }

    #[test]
    fn symbol_ranges_cover_method_and_class() {
        let src = "class Foo {\n  void bar() {\n    return;\n  }\n}\n";
        let r = parse(src);
        let class_range = r
            .ranges
            .iter()
            .find(|rg| rg.symbol_kind == NodeKind::DartClass)
            .unwrap();
        assert_eq!(class_range.start_line, 1);
        assert!(class_range.end_line >= 4);
        let method_range = r
            .ranges
            .iter()
            .find(|rg| rg.symbol_kind == NodeKind::DartMethod)
            .unwrap();
        assert_eq!(method_range.start_line, 2);
        assert!(method_range.end_line >= 3);
        assert_eq!(
            method_range.parent_symbol_id.as_ref().unwrap(),
            &class_range.symbol_id
        );
    }

    #[test]
    fn parse_class_header_recognises_modifiers() {
        assert_eq!(parse_class_header("class Foo {").as_deref(), Some("Foo"));
        assert_eq!(
            parse_class_header("abstract class Bar {").as_deref(),
            Some("Bar")
        );
        assert_eq!(
            parse_class_header("sealed class Baz {").as_deref(),
            Some("Baz")
        );
        assert_eq!(parse_class_header("not a class"), None);
    }

    #[test]
    fn parse_import_handles_both_quote_styles() {
        assert_eq!(parse_import("import 'a.dart';").as_deref(), Some("a.dart"));
        assert_eq!(
            parse_import("import \"b.dart\";").as_deref(),
            Some("b.dart")
        );
        assert_eq!(parse_import("not import"), None);
    }

    #[test]
    fn extract_call_arg_ignores_substring_matches() {
        assert!(parse_test_call("xtest('foo', ...)").is_none());
        assert_eq!(parse_test_call("test('foo', ...)").as_deref(), Some("foo"));
        assert_eq!(parse_group_call("  group('g', ...)").as_deref(), Some("g"));
    }

    #[test]
    fn unterminated_quote_in_test_call_is_ignored() {
        assert!(parse_test_call("test('foo, ...)").is_none());
    }

    #[test]
    fn constructor_requires_class_name_match() {
        assert_eq!(parse_constructor("Foo();", "Foo").as_deref(), Some(""));
        assert_eq!(
            parse_constructor("Foo.named();", "Foo").as_deref(),
            Some("named")
        );
        assert!(parse_constructor("Foo.weird", "Foo").is_none());
        assert!(parse_constructor("Other()", "Foo").is_none());
    }

    #[test]
    fn method_filter_skips_control_flow_keywords() {
        assert!(parse_method("if (x) {").is_none());
        assert!(parse_method("return foo();").is_none());
        assert!(parse_method("while(true) {").is_none());
        assert!(parse_method("// comment").is_none());
    }

    #[test]
    fn imports_inside_strings_are_not_extracted() {
        let r = parse("import 'a.dart';\nvoid main() { var s = 'import not-a-file'; }\n");
        assert_eq!(r.imports.len(), 1);
        assert_eq!(r.imports[0].to_path, "a.dart");
    }

    #[test]
    fn class_without_brace_is_recorded_without_body_range() {
        let r = parse("abstract class Foo;\n");
        let class = r
            .symbols
            .iter()
            .find(|s| s.kind == NodeKind::DartClass)
            .unwrap();
        assert_eq!(class.name, "Foo");
        // No body so we should not emit a range for the class.
        assert!(r
            .ranges
            .iter()
            .all(|rg| rg.symbol_kind != NodeKind::DartClass));
    }

    #[test]
    fn empty_source_returns_only_file_artifact() {
        let r = parse("");
        assert!(r.symbols.is_empty());
        assert!(r.tests.is_empty());
        assert!(r.imports.is_empty());
        assert!(r.ranges.is_empty());
        assert_eq!(r.file.path, "lib/a.dart");
    }

    #[test]
    fn method_body_calls_are_not_misdetected_as_method_declarations() {
        // Regression: `depth >= class_depth` used to let `candidates.sort(...)`
        // inside a method body be parsed as a sibling method of the enclosing
        // class. The only legitimate method declarations live at the class's
        // *direct* child layer (depth == class_depth).
        let src = "class AutoPlacementService {\n  AutoPlacementService();\n\n  PlacementCandidate placeWatermark(List<PlacementCandidate> candidates) {\n    candidates.sort((a, b) => b.score.compareTo(a.score));\n    return candidates.first;\n  }\n\n  double scoreCandidate(PlacementCandidate candidate) {\n    return candidate.score;\n  }\n}\n";
        let r = parse(src);
        let method_names: Vec<_> = r
            .symbols
            .iter()
            .filter(|s| s.kind == NodeKind::DartMethod)
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(
            method_names,
            vec!["placeWatermark".to_string(), "scoreCandidate".to_string()],
            "no body-call noise allowed, got {method_names:?}"
        );
    }

    #[test]
    fn nested_blocks_do_not_emit_phantom_methods() {
        // `if (...) { ... }` inside a method body must not introduce a new
        // method symbol just because the outer brace depth increased.
        let src =
            "class Box {\n  void open() {\n    if (locked) {\n      throw 'no';\n    }\n  }\n}\n";
        let r = parse(src);
        let methods: Vec<_> = r
            .symbols
            .iter()
            .filter(|s| s.kind == NodeKind::DartMethod)
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(methods, vec!["open".to_string()]);
    }

    #[test]
    fn update_depth_skips_braces_inside_strings_and_line_comments() {
        let mut d = 0usize;
        update_depth("var x = '{{{'; // }", &mut d);
        assert_eq!(d, 0);
        update_depth("class X {", &mut d);
        assert_eq!(d, 1);
        update_depth("}", &mut d);
        assert_eq!(d, 0);
        // Underflow safety.
        update_depth("}", &mut d);
        assert_eq!(d, 0);
    }
}
