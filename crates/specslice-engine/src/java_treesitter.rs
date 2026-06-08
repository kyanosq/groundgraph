//! P23.3 — Java language spec for the generic tree-sitter driver.
//!
//! Owns `.java` and is the **sole structural backend** for Java: classes /
//! interfaces / enums / records, methods + constructors, JUnit `@Test`
//! cases, and `import x.y.Z;` resolved to repo-relative file ids all flow
//! from here. Output is tagged `indexer = java_treesitter`.
//!
//! The `jdtls` LSP adapter is demoted to an optional Tier-3 enrichment that
//! only overlays `Calls` / `References` by the same symbol id (see
//! [`crate::java_indexer`]).
//!
//! Notes on Java's irregular shape, and how the data-driven driver handles
//! it without special cases:
//! - `package com.example;` is **not** an AST ancestor of the types it
//!   scopes, so qualified names are file-local (`Outer.Inner.method`) and
//!   the unified `java::<file>::<qname>` id keeps them globally unique.
//!   Package identity is recovered for *import resolution* via path suffix
//!   matching, which needs no source-root configuration.
//! - Constructors share the callable path but keep the distinct
//!   [`NodeKind::JavaConstructor`] via the driver's `callable_kind_of` hook.
//! - Methods declared inside an `enum` sit under an `enum_body_declarations`
//!   wrapper, so that node is marked *transparent* for the driver to
//!   descend through it.

use crate::treesitter::{
    body_from_field, name_from_field, no_call_test, no_src_roots, no_text, node_text, normalise_ws,
    simple_type_name, LangSpec, RefKind, SymKind, TestKind, MAX_NESTING_DEPTH,
};
use specslice_core::NodeKind;

fn java_language() -> tree_sitter::Language {
    tree_sitter_java::LANGUAGE.into()
}

fn java_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        // `record` is structurally an immutable class (auto accessors), so
        // it collapses to JavaClass; `@interface` is recorded as interface.
        "class_declaration" | "record_declaration" => Some(SymKind::Type(NodeKind::JavaClass)),
        "interface_declaration" | "annotation_type_declaration" => {
            Some(SymKind::Type(NodeKind::JavaInterface))
        }
        "enum_declaration" => Some(SymKind::Type(NodeKind::JavaEnum)),
        _ => None,
    }
}

fn java_is_callable(kind: &str) -> bool {
    matches!(kind, "method_declaration" | "constructor_declaration")
}

/// Constructors keep their own [`NodeKind`]; everything else stays whatever
/// the driver chose (always [`NodeKind::JavaMethod`] for Java callables).
fn java_callable_kind(node: tree_sitter::Node<'_>, _src: &[u8], default: NodeKind) -> NodeKind {
    if node.kind() == "constructor_declaration" {
        NodeKind::JavaConstructor
    } else {
        default
    }
}

/// `enum_body_declarations` wraps the method/field members of an enum; the
/// driver must descend through it to reach those declarations.
fn java_is_transparent(kind: &str) -> bool {
    kind == "enum_body_declarations"
}

/// Collect the bare annotation heads attached to a declaration via its
/// `modifiers` child: `@Test` → `Test`, `@org.junit.jupiter.api.Test` →
/// `Test`, `@ParameterizedTest(name = …)` → `ParameterizedTest`.
fn java_annotation_heads(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut mc = child.walk();
        for m in child.named_children(&mut mc) {
            if matches!(m.kind(), "marker_annotation" | "annotation") {
                if let Some(name) = m
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, src))
                {
                    let head = name.rsplit('.').next().unwrap_or(name).trim();
                    if !head.is_empty() {
                        out.push(head.to_string());
                    }
                }
            }
        }
    }
    out
}

/// JUnit 4 / 5 test-method annotations (`@Test` and its parameterised /
/// repeated / factory / template variants, plus legacy `@Theory`).
fn is_junit_annotation(head: &str) -> bool {
    matches!(
        head,
        "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate" | "Theory"
    )
}

/// Reclassify a JUnit-annotated method as a test case. The enclosing class
/// stays a structural [`NodeKind::JavaClass`] (JUnit needs no class-level
/// annotation), so the driver parents the case onto it.
fn java_test_of(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    kind: NodeKind,
    _name: &str,
    _parent_qualified: Option<&str>,
) -> Option<TestKind> {
    if kind != NodeKind::JavaMethod {
        return None;
    }
    java_annotation_heads(node, src)
        .iter()
        .any(|h| is_junit_annotation(h))
        .then_some(TestKind::Case)
}

/// Extract the dotted target from `import x.y.Z;` / `import static
/// x.y.Z.member;` / `import x.y.*;` — the keywords and trailing `;` are
/// stripped; the wildcard / static markers are preserved in the text so
/// [`java_resolve_import`] can act on them.
fn java_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "import_declaration" {
        return Vec::new();
    }
    let Some(text) = node_text(node, src) else {
        return Vec::new();
    };
    let mut t = text.trim();
    t = t.strip_prefix("import").unwrap_or(t).trim_start();
    t = t.strip_prefix("static").unwrap_or(t).trim_start();
    let cleaned = normalise_ws(t);
    if cleaned.is_empty() {
        Vec::new()
    } else {
        vec![cleaned]
    }
}

/// Resolve a Java import target to a repo-relative file by matching the
/// package path as a suffix (`com.example.Greeter` → any
/// `…/com/example/Greeter.java`). Drops one trailing segment as a fallback
/// so `import static pkg.Class.member;` and `import pkg.Outer.Inner;`
/// (nested type living in `Outer.java`) still land on the right file. JDK /
/// third-party packages and `import pkg.*;` wildcards resolve to `None`, so
/// they never create dangling file nodes. Source-root agnostic: works for
/// `src/main/java`, `app/src`, flat layouts, … alike.
fn java_resolve_import(
    raw: &str,
    _from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let dotted = raw.trim().trim_end_matches(';').trim();
    if dotted.is_empty() || dotted.ends_with('*') {
        return None; // wildcard package import — no single target file.
    }
    let parts: Vec<&str> = dotted.split('.').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let max = parts.len();
    let min = if max >= 2 { max - 1 } else { 1 };
    for take in (min..=max).rev() {
        let suffix = format!("{}.java", parts[..take].join("/"));
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

/// Heuristic outbound call / reference identifiers from a Java callable
/// body (see [`crate::treesitter::resolve_heuristic_refs`]). Captures:
/// - `helper()` / `this.helper()` / `obj.helper()` → `Call` to the
///   invocation `name` (links to a same-file / imported method).
/// - `new Greeter(…)` → `Reference` to the constructed type.
///
/// Qualified stdlib / third-party invocations (`List.of`,
/// `Collections.emptyList`) carry only their trailing method name, so they
/// resolve to nothing unless a local symbol of that name exists — keeping
/// the noise floor bounded.
fn java_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_java_calls(body, src, &mut out, 0);
    out
}

fn collect_java_calls(
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
            "method_invocation" => {
                if let Some(name) = child
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, src))
                {
                    out.push((name.to_string(), RefKind::Call));
                }
            }
            "object_creation_expression" => {
                if let Some(name) = child
                    .child_by_field_name("type")
                    .and_then(|t| simple_type_name(t, src))
                {
                    out.push((name, RefKind::Reference));
                }
            }
            _ => {}
        }
        collect_java_calls(child, src, out, depth + 1);
    }
}

pub(crate) static JAVA_SPEC: LangSpec = LangSpec {
    language_id: "java",
    grammar: java_language,
    extensions: &["java"],
    skip_dirs: &[
        ".git",
        "target",
        "build",
        "out",
        ".idea",
        ".gradle",
        "bin",
        "node_modules",
    ],
    separator: ".",
    func_kind: NodeKind::JavaMethod,
    method_kind: NodeKind::JavaMethod,
    container_of: java_container_of,
    is_callable_kind: java_is_callable,
    callable_kind_of: java_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: no_text,
    import_of: java_import_of,
    name_of: name_from_field,
    body_of: body_from_field,
    is_transparent_kind: java_is_transparent,
    metadata_of: no_text,
    test_of: java_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: java_resolve_import,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: java_call_idents,
    module_scoped_resolution: false,
    recurse_declined_callables: false,
    claims_path: None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&JAVA_SPEC, src)
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
    fn captures_invocation_and_object_creation() {
        let src = r#"
package com.example;

class Greeter {
    String greet() {
        return build();
    }
    String build() {
        return "hi";
    }
}

class App {
    void run() {
        Greeter g = new Greeter();
        g.greet();
    }
}
"#;
        let got = refs(&scan(src));
        assert!(
            got.contains(&("Greeter.greet".into(), "build".into(), RefKind::Call)),
            "bare method invocation: {got:?}"
        );
        assert!(
            got.contains(&("App.run".into(), "Greeter".into(), RefKind::Reference)),
            "object creation reference: {got:?}"
        );
        assert!(
            got.contains(&("App.run".into(), "greet".into(), RefKind::Call)),
            "qualified invocation keeps the trailing name: {got:?}"
        );
    }

    #[test]
    fn classes_methods_constructors_and_nesting() {
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
        assert!(
            qnames(&s, NodeKind::JavaClass).contains(&"Greeter".to_string()),
            "{:?}",
            qnames(&s, NodeKind::JavaClass)
        );
        // Package is not part of the qualified name (file id disambiguates).
        let methods = qnames(&s, NodeKind::JavaMethod);
        assert!(
            methods.contains(&"Greeter.greet".to_string()),
            "{methods:?}"
        );
        let ctors = qnames(&s, NodeKind::JavaConstructor);
        assert!(
            ctors.contains(&"Greeter.Greeter".to_string()),
            "constructor keeps its own kind, nested under the class: {ctors:?}"
        );
        assert!(
            qnames(&s, NodeKind::JavaInterface).contains(&"Walker".to_string()),
            "interfaces are captured"
        );
        // Nested static class qualifies under its outer, and its method
        // under both.
        assert!(qnames(&s, NodeKind::JavaClass).contains(&"Outer.Inner".to_string()));
        assert!(
            methods.contains(&"Outer.Inner.ping".to_string()),
            "{methods:?}"
        );
    }

    #[test]
    fn enum_keeps_distinct_kind_and_parents_methods() {
        let src = r#"
package com.example;

public enum Status {
    ACTIVE,
    PAUSED;

    public boolean isLive() {
        return this == ACTIVE;
    }
}
"#;
        let s = scan(src);
        assert_eq!(
            qnames(&s, NodeKind::JavaEnum),
            vec!["Status".to_string()],
            "exactly one JavaEnum, never a JavaClass"
        );
        assert!(
            qnames(&s, NodeKind::JavaMethod).contains(&"Status.isLive".to_string()),
            "enum methods nest through enum_body_declarations: {:?}",
            qnames(&s, NodeKind::JavaMethod)
        );
    }

    #[test]
    fn record_collapses_to_class() {
        let s = scan("package com.example;\npublic record Point(int x, int y) {}\n");
        assert!(qnames(&s, NodeKind::JavaClass).contains(&"Point".to_string()));
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

    void helper() {}
}
"#;
        let s = scan(src);
        let cases: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(cases.contains(&"GreeterTest.greetsByName"), "{cases:?}");
        assert!(cases.contains(&"GreeterTest.greetsAnyone"), "{cases:?}");
        // The non-test method stays a structural symbol; test methods leave
        // the JavaMethod bucket.
        let methods = qnames(&s, NodeKind::JavaMethod);
        assert!(
            methods.contains(&"GreeterTest.helper".to_string()),
            "{methods:?}"
        );
        assert!(
            !methods.contains(&"GreeterTest.greetsByName".to_string()),
            "JUnit method must not also be a structural method: {methods:?}"
        );
        // The test case parents onto its enclosing class.
        let case = s
            .tests
            .iter()
            .find(|t| t.qualified_name == "GreeterTest.greetsByName")
            .unwrap();
        assert_eq!(case.parent_qualified_name.as_deref(), Some("GreeterTest"));
    }

    #[test]
    fn imports_resolve_by_path_suffix_and_drop_external() {
        let files = vec![
            "src/main/java/com/example/Greeter.java".to_string(),
            "src/main/java/com/example/util/Strings.java".to_string(),
            "src/test/java/com/example/GreeterTest.java".to_string(),
        ];
        // Plain class import → its file under any source root.
        assert_eq!(
            java_resolve_import(
                "com.example.Greeter",
                "src/test/java/com/example/GreeterTest.java",
                &files,
                &[]
            ),
            Some("src/main/java/com/example/Greeter.java".to_string())
        );
        // Static member import → drop the trailing member, land on the class.
        assert_eq!(
            java_resolve_import("com.example.util.Strings.join", "x", &files, &[]),
            Some("src/main/java/com/example/util/Strings.java".to_string())
        );
        // Wildcard package import → no single file.
        assert_eq!(java_resolve_import("com.example.*", "x", &files, &[]), None);
        // JDK / third-party → dropped (never a dangling node).
        assert_eq!(
            java_resolve_import("java.util.List", "x", &files, &[]),
            None
        );
    }

    #[test]
    fn import_of_strips_keywords_and_wildcards() {
        let s = scan(
            "package com.example;\nimport java.util.List;\nimport static java.util.Collections.emptyList;\nimport com.example.*;\n",
        );
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(imports.contains(&"java.util.List"), "{imports:?}");
        assert!(
            imports.contains(&"java.util.Collections.emptyList"),
            "static import keeps its dotted member: {imports:?}"
        );
        assert!(imports.contains(&"com.example.*"), "{imports:?}");
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("class class class { @ @ @ void (((");
        let _ = scan("package 名前;\nclass 名前 { void 方法() {} }\n");
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
            prop_assert_eq!(extract(&JAVA_SPEC, &s), extract(&JAVA_SPEC, &s));
        }

        #[test]
        fn symbols_are_well_formed(s in ".*") {
            for sym in extract(&JAVA_SPEC, &s).symbols {
                prop_assert!(!sym.name.is_empty());
                prop_assert!(!sym.qualified_name.is_empty());
                prop_assert!(sym.end_line >= sym.start_line);
            }
        }
    }
}
