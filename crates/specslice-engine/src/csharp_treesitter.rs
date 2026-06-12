//! P26 — C# language spec for the generic tree-sitter driver.
//!
//! Owns `.cs` and is the sole structural backend for C#: classes /
//! interfaces / structs / enums / records, methods + constructors, xUnit /
//! NUnit / MSTest attribute test cases, and `using X.Y;` directives resolved
//! to repo-relative files via path-suffix matching (the same source-root
//! agnostic strategy as Java — .NET convention keeps namespace == folder).
//! Output is tagged `indexer = csharp_treesitter`.
//!
//! Shape notes:
//! - `namespace A.B { … }` / file-scoped `namespace A.B;` are *transparent*:
//!   the driver descends through them without emitting a symbol, so
//!   qualified names stay file-local (`Outer.Inner.Method`) exactly like
//!   Java packages.
//! - Constructors share [`NodeKind::CSharpMethod`] — C# names them after
//!   the type, so `Greeter.Greeter` already reads as a constructor.
//! - Properties / fields are not symbols (span noise ≫ navigation value);
//!   their bodies still contribute call references through the enclosing
//!   type's methods only.

use crate::treesitter::{
    body_from_field, name_from_field, no_call_test, no_src_roots, no_text, node_text, normalise_ws,
    simple_type_name, LangSpec, RefKind, SymKind, TestKind, MAX_NESTING_DEPTH,
};
use specslice_core::NodeKind;

fn csharp_language() -> tree_sitter::Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}

fn csharp_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        // `record` is an immutable class with value semantics; collapse it
        // to CSharpClass like Java records collapse to JavaClass.
        "class_declaration" | "record_declaration" => Some(SymKind::Type(NodeKind::CSharpClass)),
        "interface_declaration" => Some(SymKind::Type(NodeKind::CSharpInterface)),
        "struct_declaration" | "record_struct_declaration" => {
            Some(SymKind::Type(NodeKind::CSharpStruct))
        }
        "enum_declaration" => Some(SymKind::Type(NodeKind::CSharpEnum)),
        _ => None,
    }
}

fn csharp_is_callable(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration" | "constructor_declaration" | "local_function_statement"
    )
}

/// Block (`namespace X { … }`) and file-scoped (`namespace X;`) namespaces
/// wrap / precede the types they scope; descend without emitting.
/// `declaration_list` is the `{ … }` body under a block namespace — the
/// walker reaches it as a plain child (containers' own bodies are entered
/// via the `body` field instead), so it must be transparent too.
fn csharp_is_transparent(kind: &str) -> bool {
    matches!(
        kind,
        "namespace_declaration" | "file_scoped_namespace_declaration" | "declaration_list"
    )
}

/// Attribute heads attached to a declaration: `[Fact]` → `Fact`,
/// `[Xunit.Fact]` → `Fact`, `[TestCase(1)]` → `TestCase`. The grammar nests
/// them as `attribute_list > attribute(name: …)` children of the
/// declaration node.
fn csharp_attribute_heads(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "attribute_list" {
            continue;
        }
        let mut ac = child.walk();
        for attr in child.named_children(&mut ac) {
            if attr.kind() != "attribute" {
                continue;
            }
            if let Some(name) = attr
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
    out
}

/// xUnit (`Fact`/`Theory`), NUnit (`Test`/`TestCase`/`TestCaseSource`),
/// MSTest (`TestMethod`/`DataTestMethod`).
fn is_csharp_test_attribute(head: &str) -> bool {
    matches!(
        head,
        "Fact"
            | "Theory"
            | "Test"
            | "TestCase"
            | "TestCaseSource"
            | "TestMethod"
            | "DataTestMethod"
    )
}

fn csharp_test_of(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    kind: NodeKind,
    _name: &str,
    _parent_qualified: Option<&str>,
) -> Option<TestKind> {
    if kind != NodeKind::CSharpMethod {
        return None;
    }
    csharp_attribute_heads(node, src)
        .iter()
        .any(|h| is_csharp_test_attribute(h))
        .then_some(TestKind::Case)
}

/// `using X.Y.Z;` (plus `global using` / `using static` / `using Alias =
/// X.Y.Z;`) → the dotted target. `using (resource)` statements inside
/// method bodies are a different node kind and never reach here.
fn csharp_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "using_directive" {
        return Vec::new();
    }
    let Some(text) = node_text(node, src) else {
        return Vec::new();
    };
    let mut t = text.trim();
    t = t.strip_prefix("global").unwrap_or(t).trim_start();
    t = t.strip_prefix("using").unwrap_or(t).trim_start();
    t = t.strip_prefix("static").unwrap_or(t).trim_start();
    // Alias form: keep the real target on the right of `=`.
    if let Some((_, rhs)) = t.split_once('=') {
        t = rhs.trim_start();
    }
    let cleaned = normalise_ws(t);
    if cleaned.is_empty() {
        Vec::new()
    } else {
        vec![cleaned]
    }
}

/// Resolve `using A.B.C;` by path suffix: `.NET` convention keeps namespace
/// segments == folder segments, so `MyApp.Services` matches
/// `…/MyApp/Services/<Type>.cs` — but a namespace maps to a *folder*, not a
/// file. Strategy: try `<segments>.cs` (type import via `using static` /
/// alias), then `<segments>/<last>.cs` (folder with a same-named type file),
/// then drop one trailing segment. External namespaces (`System.*`) match
/// nothing and resolve to `None`.
fn csharp_resolve_import(
    raw: &str,
    _from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let dotted = raw.trim().trim_end_matches(';').trim();
    if dotted.is_empty() {
        return None;
    }
    let parts: Vec<&str> = dotted.split('.').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let max = parts.len();
    let min = if max >= 2 { max - 1 } else { 1 };
    for take in (min..=max).rev() {
        let joined = parts[..take].join("/");
        for suffix in [
            format!("{joined}.cs"),
            format!("{}/{}.cs", joined, parts[take - 1]),
        ] {
            let needle = format!("/{suffix}");
            if let Some(hit) = all_files
                .iter()
                .find(|f| **f == suffix || f.ends_with(&needle))
            {
                return Some(hit.clone());
            }
        }
    }
    None
}

/// Outbound identifiers from a callable body:
/// - `Helper()` / `this.Helper()` / `obj.Helper()` → `Call` on the trailing
///   name (`invocation_expression`'s function is an `identifier` or
///   `member_access_expression` whose `name` is the method).
/// - `new Greeter(…)` → `Reference` to the constructed type.
fn csharp_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_csharp_calls(body, src, &mut out, 0);
    out
}

fn collect_csharp_calls(
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
            "invocation_expression" => {
                if let Some(f) = child.child_by_field_name("function") {
                    let name = match f.kind() {
                        "identifier" => node_text(f, src).map(str::to_string),
                        "member_access_expression" => f
                            .child_by_field_name("name")
                            .and_then(|n| node_text(n, src))
                            .map(str::to_string),
                        _ => None,
                    };
                    if let Some(name) = name {
                        out.push((name, RefKind::Call));
                    }
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
        collect_csharp_calls(child, src, out, depth + 1);
    }
}

pub(crate) static CSHARP_SPEC: LangSpec = LangSpec {
    language_id: "csharp",
    grammar: csharp_language,
    extensions: &["cs"],
    skip_dirs: &[
        ".git",
        "bin",
        "obj",
        "packages",
        "node_modules",
        ".vs",
        "artifacts",
    ],
    separator: ".",
    func_kind: NodeKind::CSharpFunction,
    method_kind: NodeKind::CSharpMethod,
    container_of: csharp_container_of,
    is_callable_kind: csharp_is_callable,
    callable_kind_of: crate::treesitter::keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: no_text,
    import_of: csharp_import_of,
    name_of: name_from_field,
    body_of: body_from_field,
    is_transparent_kind: csharp_is_transparent,
    metadata_of: no_text,
    test_of: csharp_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: csharp_resolve_import,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: csharp_call_idents,
    module_scoped_resolution: false,
    recurse_declined_callables: false,
    claims_path: None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&CSHARP_SPEC, src)
    }
    fn qnames(scan: &Scan, kind: NodeKind) -> Vec<String> {
        scan.symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.qualified_name.clone())
            .collect()
    }

    #[test]
    fn containers_methods_and_namespace_transparency() {
        let src = r#"
using System;

namespace MyApp.Services
{
    public interface IGreeter { string Greet(string name); }

    public class Greeter : IGreeter
    {
        public Greeter(int count) { }
        public string Greet(string name) => "hi";
        public class Inner { public void Ping() { } }
    }

    public struct Point { }
    public enum Status { Active, Done }
    public record User(string Name);
}
"#;
        let s = scan(src);
        // Namespace is transparent: qualified names start at the type.
        assert!(
            qnames(&s, NodeKind::CSharpClass).contains(&"Greeter".to_string()),
            "{:?}",
            qnames(&s, NodeKind::CSharpClass)
        );
        assert!(qnames(&s, NodeKind::CSharpInterface).contains(&"IGreeter".to_string()));
        assert!(qnames(&s, NodeKind::CSharpStruct).contains(&"Point".to_string()));
        assert!(qnames(&s, NodeKind::CSharpEnum).contains(&"Status".to_string()));
        assert!(
            qnames(&s, NodeKind::CSharpClass).contains(&"User".to_string()),
            "record collapses to class"
        );
        let methods = qnames(&s, NodeKind::CSharpMethod);
        assert!(
            methods.contains(&"Greeter.Greet".to_string()),
            "{methods:?}"
        );
        assert!(
            methods.contains(&"Greeter.Greeter".to_string()),
            "constructor is a method named after the type: {methods:?}"
        );
        assert!(
            methods.contains(&"Greeter.Inner.Ping".to_string()),
            "nested classes qualify through the outer: {methods:?}"
        );
    }

    #[test]
    fn file_scoped_namespace_is_transparent_too() {
        let src = "namespace MyApp.Services;\n\npublic class Greeter { public void Go() {} }\n";
        let s = scan(src);
        assert!(
            qnames(&s, NodeKind::CSharpClass).contains(&"Greeter".to_string()),
            "{:?}",
            qnames(&s, NodeKind::CSharpClass)
        );
        assert!(qnames(&s, NodeKind::CSharpMethod).contains(&"Greeter.Go".to_string()));
    }

    #[test]
    fn xunit_nunit_mstest_attributes_become_test_cases() {
        let src = r#"
public class GreeterTests
{
    [Fact]
    public void Greets() { }
    [Theory]
    public void GreetsMany() { }
    [Test]
    public void NUnitStyle() { }
    [TestMethod]
    public void MsTestStyle() { }
    public void Helper() { }
}
"#;
        let s = scan(src);
        let cases: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(cases.contains(&"GreeterTests.Greets"), "{cases:?}");
        assert!(cases.contains(&"GreeterTests.GreetsMany"), "{cases:?}");
        assert!(cases.contains(&"GreeterTests.NUnitStyle"), "{cases:?}");
        assert!(cases.contains(&"GreeterTests.MsTestStyle"), "{cases:?}");
        let methods = qnames(&s, NodeKind::CSharpMethod);
        assert!(
            methods.contains(&"GreeterTests.Helper".to_string()),
            "non-test methods stay structural: {methods:?}"
        );
        assert!(
            !methods.contains(&"GreeterTests.Greets".to_string()),
            "test methods leave the structural bucket: {methods:?}"
        );
    }

    #[test]
    fn using_directives_are_captured_and_resolved_by_suffix() {
        let s = scan(
            "using System;\nusing MyApp.Models;\nusing static MyApp.Util.Strings;\nglobal using MyApp.Core;\nusing PO = MyApp.Models.Order;\n",
        );
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(imports.contains(&"System"), "{imports:?}");
        assert!(imports.contains(&"MyApp.Models"), "{imports:?}");
        assert!(imports.contains(&"MyApp.Util.Strings"), "{imports:?}");
        assert!(imports.contains(&"MyApp.Core"), "{imports:?}");
        assert!(
            imports.contains(&"MyApp.Models.Order"),
            "alias keeps the real target: {imports:?}"
        );

        let files = vec![
            "src/MyApp/Models/User.cs".to_string(),
            "src/MyApp/Models/Models.cs".to_string(),
            "src/MyApp/Util/Strings.cs".to_string(),
        ];
        // Type-suffix hit (`using static` / alias style).
        assert_eq!(
            csharp_resolve_import("MyApp.Util.Strings", "x", &files, &[]),
            Some("src/MyApp/Util/Strings.cs".to_string())
        );
        // Namespace folder with a same-named file.
        assert_eq!(
            csharp_resolve_import("MyApp.Models", "x", &files, &[]),
            Some("src/MyApp/Models/Models.cs".to_string())
        );
        // External namespace → None.
        assert_eq!(csharp_resolve_import("System.Linq", "x", &files, &[]), None);
    }

    #[test]
    fn captures_invocations_and_object_creation() {
        let src = r#"
public class App
{
    public void Run()
    {
        var g = new Greeter(1);
        g.Greet("x");
        Helper();
    }
    void Helper() { }
}
public class Greeter
{
    public Greeter(int n) { }
    public string Greet(string name) => "hi";
}
"#;
        let s = scan(src);
        let refs: Vec<(String, String, RefKind)> = s
            .references
            .iter()
            .map(|r| (r.from_qualified.clone(), r.to_name.clone(), r.kind))
            .collect();
        assert!(
            refs.contains(&("App.Run".into(), "Greeter".into(), RefKind::Reference)),
            "object creation: {refs:?}"
        );
        assert!(
            refs.contains(&("App.Run".into(), "Greet".into(), RefKind::Call)),
            "member invocation: {refs:?}"
        );
        assert!(
            refs.contains(&("App.Run".into(), "Helper".into(), RefKind::Call)),
            "bare invocation: {refs:?}"
        );
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("class class { [[[ void ((( }");
        let _ = scan("namespace 名前;\nclass 名前 { void 方法() {} }\n");
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
            prop_assert_eq!(extract(&CSHARP_SPEC, &s), extract(&CSHARP_SPEC, &s));
        }
    }
}
