//! P26 — Kotlin language spec for the generic tree-sitter driver
//! (`tree-sitter-kotlin-ng` grammar).
//!
//! Owns `.kt`/`.kts` and is the sole structural backend for Kotlin:
//! classes / interfaces / enums / objects, member + top-level functions,
//! JUnit / kotlin.test `@Test` cases, and `import a.b.C` resolved to repo
//! files by path suffix (the JVM convention shared with Java). Output is
//! tagged `indexer = kotlin_treesitter`.
//!
//! Shape notes (kotlin-ng):
//! - `class`, `interface`, `enum class` and `data class` are all
//!   `class_declaration`; the discriminators are an `interface` keyword
//!   token child and an `enum_class_body` body. `object X` is its own
//!   `object_declaration`.
//! - `companion object { … }` members belong to the enclosing class —
//!   the wrapper is transparent.
//! - `package a.b.c` is a header *sibling* of declarations (like Java), so
//!   qualified names are file-local and need no namespace handling.

use crate::treesitter::{
    name_from_field, no_call_test, no_src_roots, no_text, node_text, normalise_ws, LangSpec,
    RefKind, SymKind, TestKind, MAX_NESTING_DEPTH,
};
use specslice_core::NodeKind;

fn kotlin_language() -> tree_sitter::Language {
    tree_sitter_kotlin_ng::LANGUAGE.into()
}

/// Discriminate the three declaration flavours sharing `class_declaration`:
/// an `enum_class_body` ⇒ enum; an `interface` keyword token ⇒ interface;
/// otherwise a class (incl. `data` / `sealed` / `annotation` classes).
fn kotlin_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        "class_declaration" => {
            let mut cursor = node.walk();
            let mut is_interface = false;
            let mut is_enum = false;
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "interface" => is_interface = true,
                    "enum_class_body" => is_enum = true,
                    _ => {}
                }
            }
            Some(SymKind::Type(if is_enum {
                NodeKind::KotlinEnum
            } else if is_interface {
                NodeKind::KotlinInterface
            } else {
                NodeKind::KotlinClass
            }))
        }
        "object_declaration" => Some(SymKind::Type(NodeKind::KotlinObject)),
        _ => None,
    }
}

fn kotlin_is_callable(kind: &str) -> bool {
    matches!(kind, "function_declaration" | "secondary_constructor")
}

/// `companion object` members attach to the enclosing class. Its
/// `class_body` arrives as a plain child during that transparent descent
/// (named containers enter their body via [`kotlin_body_of`] instead), so
/// the body wrapper must be transparent as well.
fn kotlin_is_transparent(kind: &str) -> bool {
    matches!(kind, "companion_object" | "class_body")
}

/// The body to recurse into: kotlin-ng has no `body` *field*; the body is a
/// named `class_body` / `enum_class_body` / `function_body` child.
fn kotlin_body_of(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    for i in 0..node.named_child_count() {
        let c = node.named_child(u32::try_from(i).unwrap_or(u32::MAX))?;
        if matches!(c.kind(), "class_body" | "enum_class_body" | "function_body") {
            return Some(c);
        }
    }
    None
}

/// Annotation heads from a declaration's `modifiers`: `@Test` → `Test`,
/// `@org.junit.Test` → `Test`. kotlin-ng nests them as
/// `modifiers > annotation > (user_type | constructor_invocation)`.
fn kotlin_annotation_heads(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut mc = child.walk();
        for m in child.named_children(&mut mc) {
            if m.kind() != "annotation" {
                continue;
            }
            if let Some(text) = node_text(m, src) {
                let head = text
                    .trim_start_matches('@')
                    .split(['(', '<'])
                    .next()
                    .unwrap_or("")
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .trim();
                if !head.is_empty() {
                    out.push(head.to_string());
                }
            }
        }
    }
    out
}

/// JUnit 4/5 + kotlin.test share the `Test` head; parameterised/repeated
/// variants follow JUnit 5.
fn is_kotlin_test_annotation(head: &str) -> bool {
    matches!(
        head,
        "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory"
    )
}

fn kotlin_test_of(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    kind: NodeKind,
    _name: &str,
    _parent_qualified: Option<&str>,
) -> Option<TestKind> {
    if kind != NodeKind::KotlinMethod && kind != NodeKind::KotlinFunction {
        return None;
    }
    kotlin_annotation_heads(node, src)
        .iter()
        .any(|h| is_kotlin_test_annotation(h))
        .then_some(TestKind::Case)
}

/// `import a.b.C` / `import a.b.C as D` / `import a.b.*` → the dotted
/// target (alias stripped, wildcard preserved for the resolver to drop).
fn kotlin_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "import" {
        return Vec::new();
    }
    let Some(text) = node_text(node, src) else {
        return Vec::new();
    };
    let mut t = text.trim();
    t = t.strip_prefix("import").unwrap_or(t).trim_start();
    if let Some((lhs, _alias)) = t.split_once(" as ") {
        t = lhs.trim_end();
    }
    let cleaned = normalise_ws(t);
    if cleaned.is_empty() {
        Vec::new()
    } else {
        vec![cleaned]
    }
}

/// JVM path-suffix resolution shared with Java, over `.kt`:
/// `com.example.models.User` → any `…/com/example/models/User.kt`, dropping
/// one trailing segment for member imports. Wildcards / external packages →
/// `None`.
fn kotlin_resolve_import(
    raw: &str,
    _from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let dotted = raw.trim().trim_end_matches(';').trim();
    if dotted.is_empty() || dotted.ends_with('*') {
        return None;
    }
    let parts: Vec<&str> = dotted.split('.').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let max = parts.len();
    let min = if max >= 2 { max - 1 } else { 1 };
    for take in (min..=max).rev() {
        let suffix = format!("{}.kt", parts[..take].join("/"));
        let needle = format!("/{suffix}");
        if let Some(hit) = all_files
            .iter()
            .find(|f| **f == suffix || f.ends_with(&needle))
        {
            return Some(hit.clone());
        }
    }
    None
}

/// Outbound identifiers from a function body:
/// - `helper()` → `Call` (call_expression with identifier callee)
/// - `obj.helper()` → `Call` (navigation_expression callee: the trailing
///   navigation_suffix identifier)
/// - `Greeter(…)` — Kotlin constructs without `new`, so a capitalised bare
///   callee is *also* just a `Call`; the resolver links it to whatever
///   symbol (type or function) carries the name.
fn kotlin_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_kotlin_calls(body, src, &mut out, 0);
    out
}

/// `obj.helper` (a `navigation_expression` callee): kotlin-ng lays the
/// receiver and the member out as sibling identifiers —
/// `(navigation_expression (identifier) (identifier))` — so the invoked
/// method is the *last* identifier child.
fn navigation_call_name(callee: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    let mut last = None;
    for i in 0..callee.named_child_count() {
        let c = callee.named_child(u32::try_from(i).unwrap_or(u32::MAX))?;
        if c.kind() == "identifier" {
            last = node_text(c, src).map(str::to_string);
        }
    }
    last
}

fn collect_kotlin_calls(
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
            if let Some(callee) = child.named_child(0) {
                match callee.kind() {
                    "identifier" => {
                        if let Some(t) = node_text(callee, src) {
                            out.push((t.to_string(), RefKind::Call));
                        }
                    }
                    "navigation_expression" => {
                        if let Some(name) = navigation_call_name(callee, src) {
                            out.push((name, RefKind::Call));
                        }
                    }
                    _ => {}
                }
            }
        }
        collect_kotlin_calls(child, src, out, depth + 1);
    }
}

pub(crate) static KOTLIN_SPEC: LangSpec = LangSpec {
    language_id: "kotlin",
    grammar: kotlin_language,
    extensions: &["kt", "kts"],
    skip_dirs: &[
        ".git",
        "build",
        "out",
        ".gradle",
        ".idea",
        "node_modules",
    ],
    separator: ".",
    func_kind: NodeKind::KotlinFunction,
    method_kind: NodeKind::KotlinMethod,
    container_of: kotlin_container_of,
    is_callable_kind: kotlin_is_callable,
    callable_kind_of: crate::treesitter::keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: no_text,
    import_of: kotlin_import_of,
    name_of: name_from_field,
    body_of: kotlin_body_of,
    is_transparent_kind: kotlin_is_transparent,
    metadata_of: no_text,
    test_of: kotlin_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: kotlin_resolve_import,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: kotlin_call_idents,
    module_scoped_resolution: false,
    recurse_declined_callables: false,
    claims_path: None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&KOTLIN_SPEC, src)
    }
    fn qnames(scan: &Scan, kind: NodeKind) -> Vec<String> {
        scan.symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.qualified_name.clone())
            .collect()
    }

    #[test]
    fn classes_interfaces_enums_objects_and_functions() {
        let src = r#"
package com.example.billing

interface Greeter {
    fun greet(name: String): String
}

class DefaultGreeter(private val count: Int) : Greeter {
    override fun greet(name: String): String = "hi"
    fun helper() { greet("x") }
    companion object {
        fun build(): DefaultGreeter = DefaultGreeter(0)
    }
}

object Singleton { fun touch() {} }

data class Point(val x: Int, val y: Int)

enum class Status { ACTIVE, DONE }

fun topLevel(x: Int): Int = x * 2
"#;
        let s = scan(src);
        assert!(
            qnames(&s, NodeKind::KotlinInterface).contains(&"Greeter".to_string()),
            "{:?}",
            qnames(&s, NodeKind::KotlinInterface)
        );
        assert!(qnames(&s, NodeKind::KotlinClass).contains(&"DefaultGreeter".to_string()));
        assert!(
            qnames(&s, NodeKind::KotlinClass).contains(&"Point".to_string()),
            "data class is a class"
        );
        assert!(qnames(&s, NodeKind::KotlinEnum).contains(&"Status".to_string()));
        assert!(qnames(&s, NodeKind::KotlinObject).contains(&"Singleton".to_string()));
        let methods = qnames(&s, NodeKind::KotlinMethod);
        assert!(
            methods.contains(&"DefaultGreeter.greet".to_string()),
            "{methods:?}"
        );
        assert!(
            methods.contains(&"DefaultGreeter.build".to_string()),
            "companion object members attach to the class: {methods:?}"
        );
        assert!(
            methods.contains(&"Singleton.touch".to_string()),
            "{methods:?}"
        );
        assert!(
            qnames(&s, NodeKind::KotlinFunction).contains(&"topLevel".to_string()),
            "top-level functions"
        );
    }

    #[test]
    fn junit_annotated_functions_become_test_cases() {
        let src = r#"
package com.example

import org.junit.jupiter.api.Test

class GreeterTest {
    @Test
    fun greetsPolitely() {}

    fun helper() {}
}
"#;
        let s = scan(src);
        let cases: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(cases.contains(&"GreeterTest.greetsPolitely"), "{cases:?}");
        let methods = qnames(&s, NodeKind::KotlinMethod);
        assert!(
            methods.contains(&"GreeterTest.helper".to_string()),
            "{methods:?}"
        );
        assert!(
            !methods.contains(&"GreeterTest.greetsPolitely".to_string()),
            "test functions leave the structural bucket: {methods:?}"
        );
    }

    #[test]
    fn imports_are_captured_and_resolved_by_suffix() {
        let s = scan(
            "package com.example\n\nimport com.example.models.User\nimport com.example.util.Strings as Str\nimport org.junit.jupiter.api.Test\nimport com.example.models.*\n",
        );
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(imports.contains(&"com.example.models.User"), "{imports:?}");
        assert!(
            imports.contains(&"com.example.util.Strings"),
            "alias keeps target: {imports:?}"
        );
        assert!(imports.contains(&"com.example.models.*"), "{imports:?}");

        let files = vec![
            "src/main/kotlin/com/example/models/User.kt".to_string(),
            "src/main/kotlin/com/example/util/Strings.kt".to_string(),
        ];
        assert_eq!(
            kotlin_resolve_import("com.example.models.User", "x", &files, &[]),
            Some("src/main/kotlin/com/example/models/User.kt".to_string())
        );
        assert_eq!(
            kotlin_resolve_import("com.example.models.*", "x", &files, &[]),
            None
        );
        assert_eq!(
            kotlin_resolve_import("org.junit.jupiter.api.Test", "x", &files, &[]),
            None
        );
    }

    #[test]
    fn captures_calls_and_constructions() {
        let src = r#"
class App {
    fun run() {
        val g = DefaultGreeter(1)
        g.greet("x")
        helper()
    }
    fun helper() {}
}
class DefaultGreeter(val n: Int) {
    fun greet(name: String): String = "hi"
}
"#;
        let s = scan(src);
        let refs: Vec<(String, String, RefKind)> = s
            .references
            .iter()
            .map(|r| (r.from_qualified.clone(), r.to_name.clone(), r.kind))
            .collect();
        assert!(
            refs.contains(&("App.run".into(), "DefaultGreeter".into(), RefKind::Call)),
            "constructor call (no `new` in Kotlin): {refs:?}"
        );
        assert!(
            refs.contains(&("App.run".into(), "greet".into(), RefKind::Call)),
            "navigation call: {refs:?}"
        );
        assert!(
            refs.contains(&("App.run".into(), "helper".into(), RefKind::Call)),
            "bare call: {refs:?}"
        );
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("class class fun ((( }");
        let _ = scan("class 名前 { fun 方法() {} }");
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
            prop_assert_eq!(extract(&KOTLIN_SPEC, &s), extract(&KOTLIN_SPEC, &s));
        }
    }
}
