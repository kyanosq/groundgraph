//! P20 — minimal Java AST scanner used to supplement LSP-derived facts.
//!
//! Same shape as `typescript_ast`: a tolerant line-based scanner that
//! recovers exactly what SpecSlice needs in the absence of `jdtls`:
//!
//! - one [`JavaPackage`] per source file (from `package x.y.z;`);
//! - import edges (`import x.y.Z;`, `import static x.y.Z.method;`);
//! - top-level + nested `class` / `interface` / `enum` declarations;
//! - method bodies (`public Foo bar(...)`), constructors, and
//!   JUnit test methods (`@Test`, `@ParameterizedTest`,
//!   `@RepeatedTest`, `@TestFactory`).
//!
//! Anything that requires resolution (call edges, references, generics
//! across files) is left to the LSP pass — exactly like the TS/Python
//! adapters.

use specslice_core::NodeKind;

/// One declaration recovered from a Java source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaSymbol {
    pub kind: NodeKind,
    pub name: String,
    /// Dot-joined path within the file (e.g. `Greeter.greet`). Top-level
    /// declarations use just `name`.
    pub qualified_name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub parent_qualified_name: Option<String>,
    /// Annotations attached to this declaration, captured raw without
    /// the leading `@` (e.g. `Test`, `GetMapping("/api/hello")`).
    pub annotations: Vec<String>,
}

/// `import x.y.Z;` — `module_specifier` is the dotted path between
/// `import` and the trailing `;`. `is_static` is true for
/// `import static`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaImport {
    pub module_specifier: String,
    pub is_static: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JavaScan {
    pub package_name: Option<String>,
    pub symbols: Vec<JavaSymbol>,
    pub imports: Vec<JavaImport>,
}

/// Walk a Java source string and recover the bits we need.
pub fn scan(source: &str) -> JavaScan {
    let lines: Vec<&str> = source.lines().collect();
    let mut symbols: Vec<JavaSymbol> = Vec::new();
    let mut imports: Vec<JavaImport> = Vec::new();
    let mut package_name: Option<String> = None;

    let mut class_stack: Vec<(String, i32)> = Vec::new();
    let mut brace_depth: i32 = 0;
    let mut pending_annotations: Vec<String> = Vec::new();
    // (symbol_idx_in_symbols, brace_depth_at_open)
    let mut open_blocks: Vec<(usize, i32)> = Vec::new();

    for (idx, raw) in lines.iter().enumerate() {
        let line_no = u32::try_from(idx).unwrap_or(u32::MAX).saturating_add(1);
        let stripped = strip_inline_comment(raw);
        let trimmed = stripped.trim_start();

        // Package declaration. Only the first one wins.
        if package_name.is_none() {
            if let Some(rest) = trimmed.strip_prefix("package ") {
                let pkg = rest.trim().trim_end_matches(';').trim().to_string();
                if !pkg.is_empty() {
                    package_name = Some(pkg);
                }
                update_braces(&stripped, &mut brace_depth);
                continue;
            }
        }

        // Imports.
        if let Some(spec) = parse_import(trimmed) {
            imports.push(spec);
            update_braces(&stripped, &mut brace_depth);
            continue;
        }

        // Annotations: `@Foo` / `@Foo("/api")` (and ignore `@Override`
        // semantically — we still attach it as an annotation so callers
        // can audit later).
        if let Some(rest) = trimmed.strip_prefix('@') {
            if let Some(first) = rest.chars().next() {
                if first.is_ascii_alphabetic() {
                    pending_annotations.push(rest.trim_end_matches(';').trim().to_string());
                    update_braces(&stripped, &mut brace_depth);
                    continue;
                }
            }
        }

        // Declarations.
        let parent = class_stack.last().map(|(q, _)| q.clone());
        if let Some(decl) = parse_declaration(trimmed) {
            let qualified_name = match parent.as_deref() {
                Some(p) => format!("{p}.{}", decl.name),
                None => decl.name.clone(),
            };
            // JUnit test methods get rewritten to TestCase even though
            // the syntactic kind would be Method.
            let mut kind = decl.kind;
            let annotations = std::mem::take(&mut pending_annotations);
            if matches!(kind, NodeKind::JavaMethod) && is_junit_test(&annotations) {
                kind = NodeKind::TestCase;
            }
            let symbol = JavaSymbol {
                kind,
                name: decl.name.clone(),
                qualified_name: qualified_name.clone(),
                start_line: line_no,
                end_line: line_no,
                parent_qualified_name: parent.clone(),
                annotations,
            };
            let sym_idx = symbols.len();
            symbols.push(symbol);

            if decl.opens_block {
                let entry_depth = brace_depth;
                if is_type_scope(decl.kind) {
                    class_stack.push((qualified_name.clone(), entry_depth));
                }
                update_braces(&stripped, &mut brace_depth);
                open_blocks.push((sym_idx, entry_depth));
                while let Some(&(s_idx, e_depth)) = open_blocks.last() {
                    if brace_depth <= e_depth {
                        symbols[s_idx].end_line = line_no;
                        open_blocks.pop();
                        if is_type_scope(symbols[s_idx].kind)
                            && class_stack.last().map(|(_, d)| *d) == Some(e_depth)
                        {
                            class_stack.pop();
                        }
                    } else {
                        break;
                    }
                }
                continue;
            }
        }

        update_braces(&stripped, &mut brace_depth);
        while let Some(&(s_idx, e_depth)) = open_blocks.last() {
            if brace_depth <= e_depth {
                symbols[s_idx].end_line = line_no;
                open_blocks.pop();
                if is_type_scope(symbols[s_idx].kind)
                    && class_stack.last().map(|(_, d)| *d) == Some(e_depth)
                {
                    class_stack.pop();
                }
            } else {
                break;
            }
        }
    }

    let last_line = u32::try_from(lines.len().max(1)).unwrap_or(u32::MAX);
    for (s_idx, _) in open_blocks.drain(..) {
        symbols[s_idx].end_line = last_line;
    }

    JavaScan {
        package_name,
        symbols,
        imports,
    }
}

struct Declaration {
    kind: NodeKind,
    name: String,
    opens_block: bool,
}

/// Returns true for the Java declarations that open a *type* scope
/// (class / interface / enum). Records currently collapse to
/// `JavaClass`; methods inside any of these get parented to the
/// enclosing qualified name.
fn is_type_scope(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::JavaClass | NodeKind::JavaInterface | NodeKind::JavaEnum
    )
}

fn parse_declaration(trimmed: &str) -> Option<Declaration> {
    // Strip leading modifiers (`public`, `private`, `protected`,
    // `static`, `final`, `abstract`, `default`, `synchronized`,
    // `native`, `transient`, `volatile`, `strictfp`). These can stack
    // in any order so we strip in a loop.
    let mut rest = trimmed;
    loop {
        let before = rest;
        for keyword in [
            "public ",
            "private ",
            "protected ",
            "static ",
            "final ",
            "abstract ",
            "default ",
            "synchronized ",
            "native ",
            "transient ",
            "volatile ",
            "strictfp ",
        ] {
            if let Some(after) = rest.strip_prefix(keyword) {
                rest = after.trim_start();
                break;
            }
        }
        if std::ptr::eq(before, rest) {
            break;
        }
    }

    // `class Foo` / `interface Foo` / `enum Foo` / `record Foo` /
    // `@interface Foo` (annotation type — we record as interface).
    //
    // P20 fixup — `enum` gets its own `JavaEnum` kind so the graph
    // view / search filters can distinguish enum cases from plain
    // class declarations. `record` keeps `JavaClass` for now because
    // structurally (immutable POJO + auto-generated accessors) it
    // behaves like a class.
    for (prefix, kind) in [
        ("class ", NodeKind::JavaClass),
        ("interface ", NodeKind::JavaInterface),
        ("enum ", NodeKind::JavaEnum),
        ("record ", NodeKind::JavaClass),
    ] {
        if let Some(after) = rest.strip_prefix(prefix) {
            let name = take_identifier(after)?;
            return Some(Declaration {
                kind,
                name,
                opens_block: rest.contains('{'),
            });
        }
    }

    // Method / constructor:  `ReturnType name(args) {`  or
    // `name(args) {`.
    if let Some((name, is_constructor)) = parse_method_or_constructor(rest) {
        let kind = if is_constructor {
            NodeKind::JavaConstructor
        } else {
            NodeKind::JavaMethod
        };
        return Some(Declaration {
            kind,
            name,
            opens_block: rest.contains('{'),
        });
    }
    None
}

/// Best-effort detection of either `ReturnType name(args)` (method) or
/// `Name(args)` (constructor). Returns `(name, is_constructor)`.
fn parse_method_or_constructor(line: &str) -> Option<(String, bool)> {
    // Banned starts: control flow / fields / assignments. These never
    // begin a method declaration.
    for banned in [
        "if ", "for ", "while ", "switch ", "return ", "throw ", "case ", "do ", "try ", "catch ",
        "finally ", "new ",
    ] {
        if line.starts_with(banned) {
            return None;
        }
    }
    // Pure assignment / cast lines: `Foo = ...`, `Foo bar = ...;`.
    let paren_pos = line.find('(')?;
    let head = &line[..paren_pos];
    // Equal-sign before paren => assignment, not a method.
    if head.contains('=') {
        return None;
    }
    let head = head.trim_end();
    // We want either:
    //   <tokens>... NAME      → method, NAME is the last identifier
    //   NAME                  → constructor, head is the identifier
    let tokens: Vec<&str> = head.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    // Disqualify keywords masquerading as method names.
    let last = tokens.last().copied()?;
    // Trim generics: `<T>`.
    let last = trim_generics(last);
    if !is_identifier(last) {
        return None;
    }
    if reserved_word(last) {
        return None;
    }
    // Closing paren on the same line is not required (multi-line
    // signatures), but if it exists we must see either `{`, `throws`,
    // or `;` (interface stub) after it.
    let after = &line[paren_pos..];
    let ok = after.contains('{')
        || after.contains("throws ")
        || after.trim_end().ends_with(';')
        || !after.contains(')');
    if !ok {
        return None;
    }
    let is_constructor = tokens.len() == 1;
    Some((last.to_string(), is_constructor))
}

fn trim_generics(s: &str) -> &str {
    if let Some(end) = s.find('<') {
        &s[..end]
    } else {
        s
    }
}

fn reserved_word(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "switch"
            | "return"
            | "throw"
            | "case"
            | "do"
            | "try"
            | "catch"
            | "finally"
            | "new"
            | "class"
            | "interface"
            | "enum"
            | "record"
            | "public"
            | "private"
            | "protected"
            | "static"
            | "final"
            | "abstract"
            | "package"
            | "import"
    )
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    for ch in chars {
        if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '$') {
            return false;
        }
    }
    true
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

fn parse_import(trimmed: &str) -> Option<JavaImport> {
    let rest = trimmed.strip_prefix("import ")?;
    let rest = rest.trim_start();
    let (is_static, body) = if let Some(after) = rest.strip_prefix("static ") {
        (true, after.trim_start())
    } else {
        (false, rest)
    };
    let body = body.trim_end_matches(';').trim();
    if body.is_empty() {
        return None;
    }
    Some(JavaImport {
        module_specifier: body.to_string(),
        is_static,
    })
}

fn is_junit_test(annotations: &[String]) -> bool {
    annotations.iter().any(|a| {
        let head = a.split('(').next().unwrap_or(a).trim();
        matches!(
            head,
            "Test"
                | "ParameterizedTest"
                | "RepeatedTest"
                | "TestFactory"
                | "TestTemplate"
                | "Theory"
        )
    })
}

fn strip_inline_comment(line: &str) -> String {
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
            b'"' | b'\'' => in_string = Some(b),
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
            b'"' | b'\'' => in_string = Some(b),
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
    fn package_and_imports_are_recovered() {
        let src = r#"
package com.example.app;

import java.util.List;
import static java.util.Collections.emptyList;
"#;
        let s = scan(src);
        assert_eq!(s.package_name.as_deref(), Some("com.example.app"));
        assert_eq!(s.imports.len(), 2);
        assert_eq!(s.imports[0].module_specifier, "java.util.List");
        assert!(!s.imports[0].is_static);
        assert_eq!(
            s.imports[1].module_specifier,
            "java.util.Collections.emptyList"
        );
        assert!(s.imports[1].is_static);
    }

    #[test]
    fn class_with_methods_and_constructor() {
        let src = r#"
package com.example;

public class Greeter {
    private final String name;

    public Greeter(String name) {
        this.name = name;
    }

    public String greet() {
        return "hi " + name;
    }
}
"#;
        let s = scan(src);
        let class = s
            .symbols
            .iter()
            .find(|sy| sy.kind == NodeKind::JavaClass)
            .unwrap();
        assert_eq!(class.name, "Greeter");
        let methods: Vec<_> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::JavaMethod)
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "greet");
        assert_eq!(methods[0].parent_qualified_name.as_deref(), Some("Greeter"));

        let ctors: Vec<_> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::JavaConstructor)
            .collect();
        assert_eq!(ctors.len(), 1);
        assert_eq!(ctors[0].name, "Greeter");
        assert_eq!(ctors[0].parent_qualified_name.as_deref(), Some("Greeter"));
    }

    #[test]
    fn junit_annotated_methods_become_test_cases() {
        let src = r#"
package com.example;

import org.junit.jupiter.api.Test;

class GreeterTest {
    @Test
    void greetsByName() {}

    @ParameterizedTest
    void greetsAnyone() {}
}
"#;
        let s = scan(src);
        let tests: Vec<_> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::TestCase)
            .collect();
        assert_eq!(tests.len(), 2);
        let names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"greetsByName"));
        assert!(names.contains(&"greetsAnyone"));
        // Non-test methods should disappear from the JavaMethod bucket.
        assert!(s.symbols.iter().all(|sy| sy.kind != NodeKind::JavaMethod));
    }

    #[test]
    fn enum_declares_distinct_kind_and_parents_methods() {
        let src = r#"
package com.example;

public enum Status {
    ACTIVE,
    PAUSED,
    DELETED;

    public boolean isLive() {
        return this == ACTIVE;
    }
}
"#;
        let s = scan(src);
        let enums: Vec<&JavaSymbol> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::JavaEnum)
            .collect();
        assert_eq!(enums.len(), 1, "expected one JavaEnum, got {:?}", enums);
        assert_eq!(enums[0].name, "Status");
        // No JavaClass should be created for `enum`.
        assert!(
            s.symbols
                .iter()
                .all(|sy| sy.kind != NodeKind::JavaClass || sy.name != "Status"),
            "enum should not collapse to JavaClass"
        );
        // Methods declared inside the enum get parented to it.
        let methods: Vec<&JavaSymbol> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::JavaMethod)
            .collect();
        assert!(
            methods
                .iter()
                .any(|m| m.name == "isLive" && m.parent_qualified_name.as_deref() == Some("Status")),
            "expected `isLive` parented to Status, got {:?}",
            methods
        );
    }

    #[test]
    fn interface_and_nested_class() {
        let src = r#"
package com.example;

interface Walker {
    void walk();
}

class Outer {
    static class Inner {
        void ping() {}
    }
}
"#;
        let s = scan(src);
        let kinds: Vec<NodeKind> = s.symbols.iter().map(|sy| sy.kind).collect();
        assert!(kinds.contains(&NodeKind::JavaInterface));
        let classes: Vec<&JavaSymbol> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::JavaClass)
            .collect();
        assert!(classes
            .iter()
            .any(|c| c.name == "Outer" && c.parent_qualified_name.is_none()));
        assert!(classes
            .iter()
            .any(|c| c.name == "Inner" && c.parent_qualified_name.as_deref() == Some("Outer")));
        let pings: Vec<_> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::JavaMethod)
            .collect();
        assert!(
            pings
                .iter()
                .any(|m| m.name == "ping"
                    && m.parent_qualified_name.as_deref() == Some("Outer.Inner"))
        );
    }

    #[test]
    fn annotations_attach_to_following_declaration() {
        let src = r#"
package com.example;

@RestController
@RequestMapping("/api")
public class HelloController {
    @GetMapping("/hello")
    public String hello() { return "hi"; }
}
"#;
        let s = scan(src);
        let cls = s
            .symbols
            .iter()
            .find(|sy| sy.kind == NodeKind::JavaClass)
            .unwrap();
        assert_eq!(
            cls.annotations,
            vec![
                "RestController".to_string(),
                "RequestMapping(\"/api\")".into(),
            ]
        );
        let method = s
            .symbols
            .iter()
            .find(|sy| sy.kind == NodeKind::JavaMethod)
            .unwrap();
        assert_eq!(method.name, "hello");
        assert!(method
            .annotations
            .iter()
            .any(|a| a.starts_with("GetMapping(")));
    }

    #[test]
    fn package_only_files_still_scan() {
        let s = scan("package com.example;\n");
        assert_eq!(s.package_name.as_deref(), Some("com.example"));
        assert!(s.symbols.is_empty());
        assert!(s.imports.is_empty());
    }

    #[test]
    fn ignores_class_keyword_inside_strings() {
        let s = scan("class Foo { String s = \"class Bar {\"; }\n");
        let class_names: Vec<_> = s
            .symbols
            .iter()
            .filter(|sy| sy.kind == NodeKind::JavaClass)
            .map(|sy| sy.name.as_str())
            .collect();
        assert_eq!(class_names, vec!["Foo"]);
    }
}
