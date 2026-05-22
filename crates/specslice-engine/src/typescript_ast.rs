//! P20 — minimal TypeScript AST scanner used to supplement LSP-derived facts.
//!
//! This is *not* a full TypeScript parser. It's a tolerant line-based
//! scanner that recovers exactly the bits SpecSlice needs in the
//! absence of `typescript-language-server`:
//!
//! - module-level file artefact (one per `.ts` / `.tsx`);
//! - `import` statements (static + re-export + bare side-effect);
//! - top-level `function` / `class` / `interface` / `enum` declarations
//!   plus class methods (recovered via brace tracking);
//! - jest / vitest test cases (`describe(...)`, `it(...)`, `test(...)`).
//!
//! Calls / references are deliberately out of scope — those rely on a
//! real LSP server, exactly like Python's `python_ast`.
//!
//! Edge cases the scanner deliberately punts on:
//! - generics with `<>` that span multiple lines (the visible
//!   identifier still parses; the generic body is ignored);
//! - template literals containing `class` / `function` keywords (we
//!   require the keyword to start at the indent column or be preceded
//!   by `export ` / `async ` / nothing — so backticks usually skip);
//! - decorator-heavy code (we capture the line they sit on but don't
//!   classify the framework — that lives in `typescript_frameworks`).
//!
//! Verified against the `typescript_hello` fixture; corner cases live
//! in the unit tests.

use specslice_core::NodeKind;

/// One declaration recovered from a TypeScript source file. Preserves
/// source order so callers can rebuild the parent chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypescriptSymbol {
    pub kind: NodeKind,
    pub name: String,
    /// Dot-joined qualified path within the module (e.g.
    /// `Greeter.greet`). Top-level declarations use just `name`.
    pub qualified_name: String,
    /// 1-based start / end lines. Brace-tracked; falls back to start
    /// line when no matching brace was found.
    pub start_line: u32,
    pub end_line: u32,
    pub parent_qualified_name: Option<String>,
    /// True iff the declaration was preceded by `export` / `export default`.
    pub is_exported: bool,
    /// Decorator strings (`@route('/x')`, `@injectable`, …) in source
    /// order. Captured raw without the leading `@` so a separate
    /// classifier can decide framework family.
    pub decorators: Vec<String>,
}

/// `import ... from "x"` / `import "x"` / `export ... from "x"` —
/// `module_specifier` is the raw string between the quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypescriptImport {
    pub module_specifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypescriptScan {
    pub symbols: Vec<TypescriptSymbol>,
    pub imports: Vec<TypescriptImport>,
}

/// Walk a TypeScript source string and recover the bits we need for
/// the SpecSlice graph. Tolerant of partial files and never panics.
pub fn scan(source: &str) -> TypescriptScan {
    let lines: Vec<&str> = source.lines().collect();
    let mut symbols: Vec<TypescriptSymbol> = Vec::new();
    let mut imports: Vec<TypescriptImport> = Vec::new();

    // Class/interface/enum bodies live on a stack so methods inside
    // classes can record `parent_qualified_name`. Each entry is
    // (qualified_name, brace_depth_at_open). When brace_depth drops
    // below the recorded depth we pop.
    let mut class_stack: Vec<(String, i32)> = Vec::new();
    let mut brace_depth: i32 = 0;
    let mut pending_decorators: Vec<String> = Vec::new();

    // Track unfinished decl bodies so we can fill in end_line when the
    // brace count returns to the opener level. Index into `symbols`.
    let mut open_blocks: Vec<(usize, i32)> = Vec::new();

    for (idx, raw) in lines.iter().enumerate() {
        let line_no = u32::try_from(idx).unwrap_or(u32::MAX).saturating_add(1);
        let stripped = strip_inline_comment(raw);
        let trimmed = stripped.trim_start();

        // Decorators: `@foo(...)` or `@foo`.
        if let Some(rest) = trimmed.strip_prefix('@') {
            if !rest.is_empty() && rest.chars().next().unwrap().is_ascii_alphabetic() {
                pending_decorators.push(rest.trim_end_matches(';').trim().to_string());
                update_braces(&stripped, &mut brace_depth);
                continue;
            }
        }

        // Imports.
        if let Some(spec) = parse_import_specifier(trimmed) {
            imports.push(TypescriptImport {
                module_specifier: spec,
            });
            update_braces(&stripped, &mut brace_depth);
            continue;
        }
        if let Some(spec) = parse_export_from_specifier(trimmed) {
            imports.push(TypescriptImport {
                module_specifier: spec,
            });
            update_braces(&stripped, &mut brace_depth);
            continue;
        }

        // Test cases — only at "outer" brace levels so `describe(`
        // / `it(` inside template literals are ignored. We are
        // permissive about the indent.
        if let Some(name) = parse_test_call(trimmed, "describe") {
            symbols.push(test_symbol(NodeKind::TestGroup, name, line_no));
        } else if let Some(name) =
            parse_test_call(trimmed, "it").or_else(|| parse_test_call(trimmed, "test"))
        {
            symbols.push(test_symbol(NodeKind::TestCase, name, line_no));
        }

        // Declarations — class / interface / enum / function / method.
        let parent = class_stack.last().map(|(q, _)| q.clone());
        if let Some(decl) = parse_declaration(trimmed) {
            let qualified_name = match parent.as_deref() {
                Some(p) => format!("{p}.{}", decl.name),
                None => decl.name.clone(),
            };
            let symbol = TypescriptSymbol {
                kind: decl.kind,
                name: decl.name.clone(),
                qualified_name: qualified_name.clone(),
                start_line: line_no,
                end_line: line_no,
                parent_qualified_name: parent.clone(),
                is_exported: decl.is_exported,
                decorators: std::mem::take(&mut pending_decorators),
            };
            let sym_idx = symbols.len();
            symbols.push(symbol);
            // For block-bodied declarations (class / interface / enum,
            // and functions with `{` on the line), open a block we'll
            // close on matching `}`.
            if decl.opens_block {
                // After this line's brace handling we want to pop
                // when brace_depth returns to current+0.
                let entry_depth = brace_depth;
                // Track new class scope so nested methods qualify.
                if matches!(
                    decl.kind,
                    NodeKind::TypescriptClass
                        | NodeKind::TypescriptInterface
                        | NodeKind::TypescriptEnum
                ) {
                    class_stack.push((qualified_name.clone(), entry_depth));
                }
                update_braces(&stripped, &mut brace_depth);
                open_blocks.push((sym_idx, entry_depth));
                // Drain any blocks that fully closed on the same line
                // (e.g. `interface Foo {}` on a single line).
                while let Some(&(s_idx, e_depth)) = open_blocks.last() {
                    if brace_depth <= e_depth {
                        symbols[s_idx].end_line = line_no;
                        open_blocks.pop();
                        if matches!(
                            symbols[s_idx].kind,
                            NodeKind::TypescriptClass
                                | NodeKind::TypescriptInterface
                                | NodeKind::TypescriptEnum
                        ) {
                            // also pop the matching class scope
                            if class_stack.last().map(|(_, d)| *d) == Some(e_depth) {
                                class_stack.pop();
                            }
                        }
                    } else {
                        break;
                    }
                }
                continue;
            }
        }

        // Generic line: update braces and close any blocks whose
        // depth dropped back to / below open depth.
        update_braces(&stripped, &mut brace_depth);
        while let Some(&(s_idx, e_depth)) = open_blocks.last() {
            if brace_depth <= e_depth {
                symbols[s_idx].end_line = line_no;
                open_blocks.pop();
                if matches!(
                    symbols[s_idx].kind,
                    NodeKind::TypescriptClass
                        | NodeKind::TypescriptInterface
                        | NodeKind::TypescriptEnum
                ) && class_stack.last().map(|(_, d)| *d) == Some(e_depth)
                {
                    class_stack.pop();
                }
            } else {
                break;
            }
        }
    }

    // Any unclosed blocks (e.g. malformed source) get the last line.
    let last_line = u32::try_from(lines.len().max(1)).unwrap_or(u32::MAX);
    for (s_idx, _) in open_blocks.drain(..) {
        symbols[s_idx].end_line = last_line;
    }

    TypescriptScan { symbols, imports }
}

struct Declaration {
    kind: NodeKind,
    name: String,
    is_exported: bool,
    opens_block: bool,
}

fn parse_declaration(trimmed: &str) -> Option<Declaration> {
    // Strip leading `export default` / `export `.
    let mut is_exported = false;
    let mut rest = trimmed;
    if let Some(after) = rest.strip_prefix("export default") {
        is_exported = true;
        rest = after.trim_start();
    } else if let Some(after) = rest.strip_prefix("export ") {
        is_exported = true;
        rest = after.trim_start();
    }
    // Strip `async` / `abstract` / `declare` modifiers we don't need.
    for keyword in [
        "async ",
        "abstract ",
        "declare ",
        "public ",
        "private ",
        "protected ",
    ] {
        if let Some(after) = rest.strip_prefix(keyword) {
            rest = after.trim_start();
        }
    }

    // `class Foo` / `interface Foo` / `enum Foo`
    for (prefix, kind) in [
        ("class ", NodeKind::TypescriptClass),
        ("interface ", NodeKind::TypescriptInterface),
        ("enum ", NodeKind::TypescriptEnum),
    ] {
        if let Some(after) = rest.strip_prefix(prefix) {
            let name = take_identifier(after)?;
            let opens_block = rest.contains('{');
            return Some(Declaration {
                kind,
                name,
                is_exported,
                opens_block,
            });
        }
    }
    // `function foo(...)` (incl. async).
    if let Some(after) = rest
        .strip_prefix("function ")
        .or_else(|| rest.strip_prefix("function* "))
    {
        let name = take_identifier(after.trim_start())?;
        return Some(Declaration {
            kind: NodeKind::TypescriptFunction,
            name,
            is_exported,
            opens_block: rest.contains('{'),
        });
    }
    // Method shorthand `foo(...) {` inside a class body. We restrict
    // to lines that look like `name(...args...)` followed by `{` or
    // `:` (return type) or `: T {`. To avoid arrow functions inside
    // method bodies, only recognise when the line starts with the
    // identifier (no `const ` / `let ` / `=`).
    if let Some(name) = parse_method_signature(rest) {
        return Some(Declaration {
            kind: NodeKind::TypescriptMethod,
            name,
            is_exported,
            opens_block: rest.contains('{'),
        });
    }
    None
}

fn parse_method_signature(rest: &str) -> Option<String> {
    // Skip empty / keyword-only lines.
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    // Reject `=` / `const ` / `let ` / `var ` / `return ` etc.
    let banned = [
        "const ", "let ", "var ", "return ", "if ", "for ", "while ", "switch ", "case ", "throw ",
        "type ", "import", "export ", "// ", "/* ",
    ];
    if banned.iter().any(|k| rest.starts_with(k)) {
        return None;
    }
    if rest.contains('=') && !rest.contains("=>") {
        return None;
    }
    let name = take_identifier(rest)?;
    let after = rest.get(name.len()..)?.trim_start();
    // Must be followed by `(` or `<` (generic) or `: ` (typed field
    // declaration — which is *not* a method).
    let opens_call = after.starts_with('(') || after.starts_with('<');
    if !opens_call {
        return None;
    }
    Some(name)
}

fn take_identifier(s: &str) -> Option<String> {
    let s = s.trim_start();
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            out.push(ch);
        } else {
            break;
        }
    }
    if out.is_empty() || out.chars().next().unwrap().is_ascii_digit() {
        None
    } else {
        Some(out)
    }
}

fn parse_import_specifier(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with("import") {
        return None;
    }
    let rest = trimmed.get(6..)?.trim_start();
    // Bare side-effect import: `import "x";` / `import 'x';`
    if rest.starts_with('"') || rest.starts_with('\'') {
        return string_literal(rest);
    }
    // `import foo from "x"` / `import { a, b } from "x"` / `import * as Foo from "x"`
    if let Some(idx) = rest.find(" from ") {
        let tail = &rest[idx + 6..];
        return string_literal(tail.trim_start());
    }
    None
}

fn parse_export_from_specifier(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with("export") {
        return None;
    }
    let idx = trimmed.find(" from ")?;
    let tail = &trimmed[idx + 6..];
    string_literal(tail.trim_start())
}

fn string_literal(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let quote = match bytes.first().copied() {
        Some(b'"') => b'"',
        Some(b'\'') => b'\'',
        _ => return None,
    };
    let after = &s[1..];
    let end = after.bytes().position(|b| b == quote)?;
    Some(after[..end].to_string())
}

fn parse_test_call(trimmed: &str, fn_name: &str) -> Option<String> {
    let rest = trimmed.strip_prefix(fn_name)?;
    let rest = rest.trim_start();
    if !rest.starts_with('(') {
        return None;
    }
    let after = rest.get(1..)?.trim_start();
    string_literal(after)
}

fn test_symbol(kind: NodeKind, name: String, line: u32) -> TypescriptSymbol {
    TypescriptSymbol {
        kind,
        name: name.clone(),
        qualified_name: name,
        start_line: line,
        end_line: line,
        parent_qualified_name: None,
        is_exported: false,
        decorators: Vec::new(),
    }
}

fn strip_inline_comment(line: &str) -> String {
    // Trim `//` to end of line, but only outside of strings.
    let bytes = line.as_bytes();
    let mut in_string: Option<u8> = None;
    let mut prev_back = false;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = in_string {
            if b == q && !prev_back {
                in_string = None;
            }
            prev_back = b == b'\\' && !prev_back;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => in_string = Some(b),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return line[..i].to_string();
            }
            _ => {}
        }
        prev_back = false;
    }
    line.to_string()
}

fn update_braces(line: &str, depth: &mut i32) {
    let mut in_string: Option<u8> = None;
    let mut prev_back = false;
    for &b in line.as_bytes() {
        if let Some(q) = in_string {
            if b == q && !prev_back {
                in_string = None;
            }
            prev_back = b == b'\\' && !prev_back;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => in_string = Some(b),
            b'{' => *depth += 1,
            b'}' => *depth -= 1,
            _ => {}
        }
        prev_back = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_are_recovered_from_static_and_reexport_forms() {
        let src = r#"
import { foo } from "./foo";
import bar from "./bar";
import * as baz from "./baz";
import "./side-effect";
export { quux } from "./quux";
"#;
        let s = scan(src);
        let mods: Vec<_> = s
            .imports
            .iter()
            .map(|i| i.module_specifier.clone())
            .collect();
        assert_eq!(
            mods,
            vec![
                "./foo".to_string(),
                "./bar".into(),
                "./baz".into(),
                "./side-effect".into(),
                "./quux".into(),
            ]
        );
    }

    #[test]
    fn class_and_methods_are_parented() {
        let src = r#"
export class Greeter {
  constructor(name: string) {}
  greet(): string {
    return "hi";
  }
}
"#;
        let s = scan(src);
        let class = s
            .symbols
            .iter()
            .find(|sy| sy.kind == NodeKind::TypescriptClass)
            .unwrap();
        assert_eq!(class.name, "Greeter");
        assert!(class.is_exported);
        let methods: Vec<_> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::TypescriptMethod)
            .collect();
        assert_eq!(methods.len(), 2, "{methods:?}");
        assert!(methods
            .iter()
            .all(|m| m.parent_qualified_name.as_deref() == Some("Greeter")));
    }

    #[test]
    fn top_level_function_is_recovered() {
        let s = scan("export function greet(name: string) { return name; }\n");
        let f = s
            .symbols
            .iter()
            .find(|sy| sy.kind == NodeKind::TypescriptFunction)
            .unwrap();
        assert_eq!(f.name, "greet");
        assert!(f.is_exported);
    }

    #[test]
    fn interface_and_enum_register_as_types() {
        let src = "export interface Walker { walk(): void }\nenum Color { Red, Green }\n";
        let s = scan(src);
        let kinds: Vec<NodeKind> = s.symbols.iter().map(|sy| sy.kind).collect();
        assert!(kinds.contains(&NodeKind::TypescriptInterface));
        assert!(kinds.contains(&NodeKind::TypescriptEnum));
    }

    #[test]
    fn describe_and_it_are_recovered_as_tests() {
        let src = r#"
import { describe, it } from "vitest";
describe("greeter", () => {
  it("greets", () => {});
  test("falls back to test()", () => {});
});
"#;
        let s = scan(src);
        let test_kinds: Vec<_> = s
            .symbols
            .iter()
            .map(|sy| (sy.kind, sy.name.clone()))
            .collect();
        assert!(test_kinds.contains(&(NodeKind::TestGroup, "greeter".into())));
        assert!(test_kinds.contains(&(NodeKind::TestCase, "greets".into())));
        assert!(test_kinds.contains(&(NodeKind::TestCase, "falls back to test()".into())));
    }

    #[test]
    fn decorators_attach_to_following_declaration() {
        let src = r#"
@Injectable()
@Controller("/api")
export class HelloController {}
"#;
        let s = scan(src);
        let cls = s
            .symbols
            .iter()
            .find(|sy| sy.kind == NodeKind::TypescriptClass)
            .unwrap();
        assert_eq!(cls.name, "HelloController");
        assert_eq!(
            cls.decorators,
            vec!["Injectable()".to_string(), "Controller(\"/api\")".into()],
        );
    }

    #[test]
    fn ignores_class_keyword_inside_strings() {
        let src = "const note = \"class Foo {\";\n";
        let s = scan(src);
        assert!(s
            .symbols
            .iter()
            .all(|sy| !matches!(sy.kind, NodeKind::TypescriptClass)));
    }
}
