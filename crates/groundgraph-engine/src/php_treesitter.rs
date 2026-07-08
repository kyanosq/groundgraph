//! P26 — PHP language spec for the generic tree-sitter driver.
//!
//! Owns `.php` and is the sole structural backend for PHP: classes /
//! interfaces / traits / enums, methods + free functions, PHPUnit `test*`
//! methods and `#[Test]` attributes, and PSR-4 `use A\B\C;` imports
//! resolved by path suffix. Output is tagged `indexer = php_treesitter`.
//!
//! Shape notes:
//! - The grammar root is `program` with a leading `php_tag`; real-world
//!   files always open with `<?php`, and the driver scans whatever nodes
//!   follow.
//! - `namespace App\Services;` is a *sibling* of the declarations it
//!   scopes (statement form), so nothing needs namespace transparency —
//!   qualified names are file-local exactly like Java/C#.
//! - PSR-4 maps namespace segments to directories, so `use App\Models\User`
//!   resolves to `…/Models/User.php` by suffix without composer.json
//!   configuration.

use crate::treesitter::{
    body_from_field, name_from_field, no_call_test, no_src_roots, no_text, node_text, normalise_ws,
    LangSpec, RefKind, SymKind, TestKind, MAX_NESTING_DEPTH,
};
use groundgraph_core::NodeKind;

fn php_language() -> tree_sitter::Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}

fn php_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        "class_declaration" => Some(SymKind::Type(NodeKind::PhpClass)),
        "interface_declaration" => Some(SymKind::Type(NodeKind::PhpInterface)),
        "trait_declaration" => Some(SymKind::Type(NodeKind::PhpTrait)),
        "enum_declaration" => Some(SymKind::Type(NodeKind::PhpEnum)),
        _ => None,
    }
}

fn php_is_callable(kind: &str) -> bool {
    matches!(kind, "method_declaration" | "function_definition")
}

/// PHPUnit: a public `test*` method, or any method carrying a `#[Test]` /
/// `@test`-style attribute (`attribute_list > attribute_group > attribute`).
fn php_test_of(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    kind: NodeKind,
    name: &str,
    _parent_qualified: Option<&str>,
) -> Option<TestKind> {
    if kind != NodeKind::PhpMethod {
        return None;
    }
    // PHPUnit convention: `test` followed by an uppercase letter or `_`.
    // Plain `test` prefixes with a lowercase continuation (`testingHelper`,
    // `testable`) are ordinary methods, not cases.
    if let Some(rest) = name.strip_prefix("test") {
        if rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        {
            return Some(TestKind::Case);
        }
    }
    php_attribute_heads(node, src)
        .iter()
        .any(|h| h == "Test" || h == "DataProvider")
        .then_some(TestKind::Case)
}

/// Attribute heads on a declaration: `#[Test]` → `Test`,
/// `#[PHPUnit\Framework\Attributes\Test]` → `Test`.
fn php_attribute_heads(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "attribute_list" {
            continue;
        }
        let mut gc = child.walk();
        for group in child.named_children(&mut gc) {
            let mut ac = group.walk();
            for attr in group.named_children(&mut ac) {
                if attr.kind() != "attribute" {
                    continue;
                }
                if let Some(text) = attr.named_child(0).and_then(|n| node_text(n, src)) {
                    let head = text.rsplit('\\').next().unwrap_or(text).trim();
                    if !head.is_empty() {
                        out.push(head.to_string());
                    }
                }
            }
        }
    }
    out
}

/// `use A\B\C;` / `use A\B\{C, D};` / `use A\B\C as D;` → dotted targets
/// (`function` / `const` flavours included; the keywords are stripped).
/// Method-level trait `use X;` inside a class body is a different node kind
/// (`use_declaration`) and is *not* captured here.
fn php_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "namespace_use_declaration" {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for clause in node.named_children(&mut cursor) {
        match clause.kind() {
            "namespace_use_clause" => {
                if let Some(name) = clause.named_child(0).and_then(|n| node_text(n, src)) {
                    let cleaned = normalise_ws(name);
                    if !cleaned.is_empty() {
                        out.push(cleaned);
                    }
                }
            }
            // Group form: `use A\B\{C, D as E};` — prefix + group entries.
            "namespace_use_group" => {
                let prefix = node
                    .named_children(&mut node.walk())
                    .find(|c| c.kind() == "namespace_name")
                    .and_then(|n| node_text(n, src))
                    .unwrap_or("");
                let mut gc = clause.walk();
                for entry in clause.named_children(&mut gc) {
                    if entry.kind() == "namespace_use_clause" {
                        if let Some(name) = entry.named_child(0).and_then(|n| node_text(n, src)) {
                            let joined = if prefix.is_empty() {
                                name.to_string()
                            } else {
                                format!("{prefix}\\{name}")
                            };
                            out.push(normalise_ws(&joined));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// PSR-4 resolution by *tail* alignment: the namespace's leading segments
/// map to a configured root that rarely matches its directory name casing
/// (`App\` → `app/` in Laravel, `src/` elsewhere), so the match anchors at
/// the tail instead — `App\Models\User` hits any `…/Models/User.php`, then
/// progressively shorter tails down to `…/User.php`. A second round drops
/// the last segment so `use App\Util\slugify` (function import) lands on
/// `…/Util.php`-style files. Vendor namespaces match nothing → `None`.
fn php_resolve_import(
    raw: &str,
    _from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let cleaned = raw.trim().trim_end_matches(';').trim();
    if cleaned.is_empty() {
        return None;
    }
    let parts: Vec<&str> = cleaned.split('\\').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let find_by_tail = |segs: &[&str]| -> Option<String> {
        if segs.is_empty() {
            return None;
        }
        // Longest tail first; require ≥2 segments before falling back to the
        // bare filename so `use Vendor\Pkg\User` doesn't grab an unrelated
        // local `User.php` unless nothing longer matched.
        for skip in 0..segs.len() {
            let suffix = format!("{}.php", segs[skip..].join("/"));
            let needle = format!("/{suffix}");
            if let Some(hit) = all_files
                .iter()
                .find(|f| **f == suffix || f.ends_with(&needle))
            {
                return Some(hit.clone());
            }
        }
        None
    };
    find_by_tail(&parts).or_else(|| {
        // Member / function import: drop the trailing segment.
        (parts.len() >= 2)
            .then(|| find_by_tail(&parts[..parts.len() - 1]))
            .flatten()
    })
}

/// Outbound identifiers from a callable body:
/// - `helper()` → `Call` (function_call_expression, function: name)
/// - `$this->helper()` / `$obj->helper()` → `Call` (member_call_expression)
/// - `Greeter::run()` → `Call` (scoped_call_expression)
/// - `new Greeter(…)` → `Reference` (object_creation_expression)
fn php_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    collect_php_calls(body, src, &mut out, 0);
    out
}

fn collect_php_calls(
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
            "function_call_expression" => {
                if let Some(f) = child.child_by_field_name("function") {
                    if matches!(f.kind(), "name" | "qualified_name") {
                        if let Some(t) = node_text(f, src) {
                            let bare = t.rsplit('\\').next().unwrap_or(t);
                            out.push((bare.to_string(), RefKind::Call));
                        }
                    }
                }
            }
            "member_call_expression" | "scoped_call_expression" => {
                if let Some(name) = child
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, src))
                {
                    out.push((name.to_string(), RefKind::Call));
                }
            }
            "object_creation_expression" => {
                for i in 0..child.named_child_count() {
                    let Some(c) = child.named_child(u32::try_from(i).unwrap_or(u32::MAX)) else {
                        break;
                    };
                    if matches!(c.kind(), "name" | "qualified_name") {
                        if let Some(t) = node_text(c, src) {
                            let bare = t.rsplit('\\').next().unwrap_or(t);
                            out.push((bare.to_string(), RefKind::Reference));
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
        collect_php_calls(child, src, out, depth + 1);
    }
}

pub(crate) static PHP_SPEC: LangSpec = LangSpec {
    language_id: "php",
    grammar: php_language,
    extensions: &["php"],
    skip_dirs: &[
        ".git",
        "vendor",
        "node_modules",
        "storage",
        "cache",
        ".phpunit.cache",
    ],
    separator: "::",
    func_kind: NodeKind::PhpFunction,
    method_kind: NodeKind::PhpMethod,
    container_of: php_container_of,
    is_callable_kind: php_is_callable,
    callable_kind_of: crate::treesitter::keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: no_text,
    import_of: php_import_of,
    name_of: name_from_field,
    body_of: body_from_field,
    is_transparent_kind: crate::treesitter::never,
    metadata_of: no_text,
    test_of: php_test_of,
    call_test_of: no_call_test,
    src_roots_of: no_src_roots,
    resolve_import: php_resolve_import,
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: php_call_idents,
    module_scoped_resolution: false,
    recurse_declined_callables: false,
    claims_path: None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&PHP_SPEC, src)
    }
    fn qnames(scan: &Scan, kind: NodeKind) -> Vec<String> {
        scan.symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.qualified_name.clone())
            .collect()
    }

    #[test]
    fn containers_methods_and_free_functions() {
        let src = r#"<?php
namespace App\Services;

interface GreeterInterface {
    public function greet(string $name): string;
}

class Greeter implements GreeterInterface {
    public function __construct(int $count) {}
    public function greet(string $name): string { return "hi"; }
    public static function run(): void {}
}

trait Loggable {
    public function log(string $msg): void {}
}

enum Status: string {
    case Active = 'active';
}

function top_level(int $x): int { return $x * 2; }
"#;
        let s = scan(src);
        assert!(qnames(&s, NodeKind::PhpClass).contains(&"Greeter".to_string()));
        assert!(qnames(&s, NodeKind::PhpInterface).contains(&"GreeterInterface".to_string()));
        assert!(qnames(&s, NodeKind::PhpTrait).contains(&"Loggable".to_string()));
        assert!(qnames(&s, NodeKind::PhpEnum).contains(&"Status".to_string()));
        let methods = qnames(&s, NodeKind::PhpMethod);
        assert!(
            methods.contains(&"Greeter::greet".to_string()),
            "{methods:?}"
        );
        assert!(
            methods.contains(&"Greeter::__construct".to_string()),
            "{methods:?}"
        );
        assert!(
            methods.contains(&"Loggable::log".to_string()),
            "trait methods attach to the trait: {methods:?}"
        );
        assert!(
            qnames(&s, NodeKind::PhpFunction).contains(&"top_level".to_string()),
            "free functions"
        );
    }

    #[test]
    fn php_methods_with_lowercase_after_test_stay_structural() {
        // PHPUnit's convention is `test` followed by an uppercase letter or
        // `_` (`testGreets`, `test_greets`). Methods like `testingHelper` /
        // `testable` are ordinary business code and must NOT be reclassified
        // as test cases (which would drop them from the structural symbols).
        let src = r#"<?php
class Widget {
    public function testingHelper(): void {}
    public function testable(): void {}
    public function testGreets(): void {}
}
"#;
        let s = scan(src);
        let methods = qnames(&s, NodeKind::PhpMethod);
        assert!(
            methods.contains(&"Widget::testingHelper".to_string()),
            "testingHelper should stay structural: {methods:?}"
        );
        assert!(
            methods.contains(&"Widget::testable".to_string()),
            "testable should stay structural: {methods:?}"
        );
        let cases: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(
            cases.contains(&"Widget::testGreets"),
            "real PHPUnit case still detected: {cases:?}"
        );
        assert!(
            !cases.contains(&"Widget::testingHelper"),
            "testingHelper is not a case: {cases:?}"
        );
        assert!(
            !cases.contains(&"Widget::testable"),
            "testable is not a case: {cases:?}"
        );
    }

    #[test]
    fn phpunit_test_methods_and_attributes() {
        let src = r#"<?php
class GreeterTest extends TestCase {
    public function testGreets(): void {}
    #[Test]
    public function greetsWithAttribute(): void {}
    public function helperThing(): void {}
}
"#;
        let s = scan(src);
        let cases: Vec<&str> = s
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(cases.contains(&"GreeterTest::testGreets"), "{cases:?}");
        assert!(
            cases.contains(&"GreeterTest::greetsWithAttribute"),
            "#[Test] attribute: {cases:?}"
        );
        assert!(
            qnames(&s, NodeKind::PhpMethod).contains(&"GreeterTest::helperThing".to_string()),
            "helpers stay structural"
        );
    }

    #[test]
    fn use_declarations_are_captured_and_resolved() {
        let src = r#"<?php
namespace App;

use App\Models\User;
use App\Models\Order as PurchaseOrder;
use function App\Util\slugify;
"#;
        let s = scan(src);
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(imports.contains(&"App\\Models\\User"), "{imports:?}");
        assert!(
            imports.contains(&"App\\Models\\Order"),
            "alias keeps the real target: {imports:?}"
        );
        assert!(
            imports.contains(&"App\\Util\\slugify"),
            "function imports keep their path: {imports:?}"
        );

        let files = vec![
            "app/Models/User.php".to_string(),
            "app/Models/Order.php".to_string(),
            "app/Util/Util.php".to_string(),
        ];
        assert_eq!(
            php_resolve_import("App\\Models\\User", "x", &files, &[]),
            Some("app/Models/User.php".to_string())
        );
        // Function import: drop the trailing member, still no file → None
        // (Util.php is named after the dir, not the namespace tail).
        assert_eq!(
            php_resolve_import("Vendor\\Pkg\\Thing", "x", &files, &[]),
            None
        );
    }

    #[test]
    fn captures_calls_and_object_creation() {
        let src = r#"<?php
class App {
    public function run(): void {
        $g = new Greeter(1);
        $g->greet("x");
        Greeter::run();
        helper();
    }
}
function helper(): void {}
class Greeter {
    public function __construct(int $n) {}
    public function greet(string $name): string { return "hi"; }
    public static function run(): void {}
}
"#;
        let s = scan(src);
        let refs: Vec<(String, String, RefKind)> = s
            .references
            .iter()
            .map(|r| (r.from_qualified.clone(), r.to_name.clone(), r.kind))
            .collect();
        assert!(
            refs.contains(&("App::run".into(), "Greeter".into(), RefKind::Reference)),
            "new Greeter: {refs:?}"
        );
        assert!(
            refs.contains(&("App::run".into(), "greet".into(), RefKind::Call)),
            "member call: {refs:?}"
        );
        assert!(
            refs.contains(&("App::run".into(), "run".into(), RefKind::Call)),
            "scoped call: {refs:?}"
        );
        assert!(
            refs.contains(&("App::run".into(), "helper".into(), RefKind::Call)),
            "free function call: {refs:?}"
        );
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("<?php class class {{{ function ((( ");
        let _ = scan("<?php class 名前 { function 方法() {} }");
        // No php tag at all — html-only file.
        let _ = scan("<html><body>hello</body></html>");
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
            prop_assert_eq!(extract(&PHP_SPEC, &s), extract(&PHP_SPEC, &s));
        }
    }
}
