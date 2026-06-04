//! P23.6 — Dart language spec for the generic tree-sitter driver.
//!
//! Tree-sitter is the **structural backend** for Dart: classes, mixins,
//! enums, extension members, methods / getters / setters, constructors,
//! top-level functions and call-based `test(...)` / `group(...)` cases all
//! flow from here. The Dart analyzer sidecar (and the heuristic lightweight
//! reference scanner) are demoted to optional Tier-3 **semantic** overlays
//! that only contribute `Calls` / `References` / framework edges and the
//! synthetic `route` / `storage` / provider nodes — see
//! [`crate::dart_indexer`].
//!
//! Unlike the other languages, Dart symbols keep the **legacy
//! `dart_class::` / `dart_method::` / `dart_fn::` / `dart_ctor::` id
//! scheme** (see [`dart_extract_structure`]) rather than the generic
//! `dart::<file>::<qname>` scheme, so the analyzer's semantic edges — which
//! are computed against that scheme — bind to the tree-sitter structure with
//! zero translation and the pixcraft golden stays byte-stable.
//!
//! The `UserError/lukepighetti` grammar is irregular; the discriminators
//! below were confirmed empirically against the compiled grammar:
//! - Top-level functions parse as `lambda_expression` whose `parameters:`
//!   is a `function_signature` carrying the `name:` field (anonymous
//!   closures are `function_expression`, never emitted).
//! - Concrete class members nest under `class_member_definition` → either a
//!   `method_signature` (method / getter / setter, with a sibling
//!   `function_body`) or a `declaration` wrapping a `constructor_signature`
//!   (named constructors carry two `name:` fields; unnamed ones a single one
//!   → `<default>`).
//! - Abstract members (no body) parse as `class_member_definition` →
//!   `declaration` → a *bare* `function_signature` / `getter_signature` /
//!   `setter_signature`; these are emitted as methods too.
//! - `extension … on T` is handled like a Rust `impl` block via
//!   [`dart_extension_type`] so its members attach to `T`.
//! - `test` / `group` are call-based; the driver descends into the
//!   conventional `void main() { … }` harness because [`DART_SPEC`] sets
//!   `recurse_callables = true`.

use std::collections::BTreeMap;

use specslice_core::artifact_id::{
    dart_class_id, dart_constructor_id, dart_function_id, dart_group_id, dart_method_id,
    dart_test_id, slugify, ArtifactId,
};
use specslice_core::language_batch::{SymbolArtifact, SymbolRange, TestArtifact};
use specslice_core::NodeKind;

use crate::treesitter::{
    extract, no_call_idents, no_test_of, no_text, node_text, CallTestHit, LangSpec, SymKind,
    TestKind,
};

pub(crate) fn dart_language() -> tree_sitter::Language {
    tree_sitter_dart::language()
}

// ---------------------------------------------------------------------------
// Small grammar helpers
// ---------------------------------------------------------------------------

/// First *named* child of the given kind.
fn child_of_kind<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// First descendant of the given kind, breadth-bounded so a pathological
/// tree cannot loop forever.
fn first_descendant<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
    max_depth: usize,
) -> Option<tree_sitter::Node<'a>> {
    if max_depth == 0 {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
        if let Some(found) = first_descendant(child, kind, max_depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Strip a string literal's surrounding quotes (`'…'` / `"…"`).
fn unquote(text: &str) -> String {
    let trimmed = text.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' || first == b'"') && last == first {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

/// Bare type name: drop any generic arguments and leading scope so
/// `extension X on Map<String,int>` attaches members to `Map`.
fn bare_type(text: &str) -> String {
    let head = text.split('<').next().unwrap_or(text).trim();
    head.rsplit('.').next().unwrap_or(head).trim().to_string()
}

/// Split a qualified name at the first separator: `Class.member` →
/// `("Class", "member")`. No separator → `(whole, "")`.
fn split_first_dot(qualified: &str) -> (&str, &str) {
    match qualified.split_once('.') {
        Some((a, b)) => (a, b),
        None => (qualified, ""),
    }
}

// ---------------------------------------------------------------------------
// LangSpec hooks
// ---------------------------------------------------------------------------

fn dart_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        // class / mixin / enum all collapse onto DartClass — the core node
        // model has no distinct mixin/enum kind and the analyzer treats them
        // the same structurally.
        "class_definition" | "mixin_declaration" | "enum_declaration" => {
            Some(SymKind::Type(NodeKind::DartClass))
        }
        _ => None,
    }
}

/// `extension … on T { … }`: not itself a symbol; its members attach to the
/// extended type `T` (Rust-`impl` analogy).
fn dart_extension_type(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() != "extension_declaration" {
        return None;
    }
    let t = node.child_by_field_name("class")?;
    Some(bare_type(node_text(t, src)?))
}

fn dart_is_callable(kind: &str) -> bool {
    matches!(
        kind,
        // Concrete members carry a `method_signature` (+ a sibling
        // `function_body`); abstract members (no body) parse as a *bare*
        // `function_signature` / `getter_signature` / `setter_signature`
        // under a `declaration` wrapper. Both forms are callables. The bare
        // signatures only ever surface here for abstract members: the inner
        // `function_signature` of a concrete `method_signature` (and of a
        // top-level `lambda_expression`) is never re-walked, so there is no
        // double emit.
        "method_signature"
            | "function_signature"
            | "getter_signature"
            | "setter_signature"
            | "constructor_signature"
            | "lambda_expression"
    )
}

fn dart_callable_kind(node: tree_sitter::Node<'_>, _src: &[u8], base: NodeKind) -> NodeKind {
    if node.kind() == "constructor_signature" {
        NodeKind::DartConstructor
    } else {
        base
    }
}

/// Methods / constructors: the grammar splits the signature from the
/// `function_body` into sibling children of a `class_member_definition`
/// wrapper that spans both, so measure that wrapper (confirmed empirically:
/// `class_member_definition [2..4] → method_signature [2..2] + function_body
/// [2..4]`). Top-level functions are a `lambda_expression` whose body is a
/// child, so they already span correctly → `None` (use self).
fn dart_callable_span(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    match node.kind() {
        "method_signature" | "constructor_signature" => {
            let mut cur = node.parent();
            for _ in 0..4 {
                let p = cur?;
                if p.kind() == "class_member_definition" {
                    return Some(p);
                }
                cur = p.parent();
            }
            None
        }
        _ => None,
    }
}

fn dart_name_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        // `name:` field is the simple identifier.
        "class_definition" | "enum_declaration" | "extension_declaration" => node
            .child_by_field_name("name")
            .and_then(|n| node_text(n, src))
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        // mixin: name is the first identifier child (no `name:` field).
        "mixin_declaration" => child_of_kind(node, "identifier")
            .and_then(|n| node_text(n, src))
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        // concrete method / getter / setter: dig into the inner signature.
        "method_signature" => {
            let mut cursor = node.walk();
            let inner = node
                .named_children(&mut cursor)
                .find(|c| c.kind().ends_with("_signature"))?;
            inner
                .child_by_field_name("name")
                .or_else(|| child_of_kind(inner, "identifier"))
                .and_then(|n| node_text(n, src))
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        }
        // abstract member: a bare signature whose name is the `name:` field
        // (falling back to the first identifier child for grammar variants).
        "function_signature" | "getter_signature" | "setter_signature" => node
            .child_by_field_name("name")
            .or_else(|| child_of_kind(node, "identifier"))
            .and_then(|n| node_text(n, src))
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        // constructor: two `name:` fields → named (`Class.named`); one →
        // unnamed (`Class.<default>`).
        "constructor_signature" => {
            let mut cursor = node.walk();
            let names: Vec<_> = node.children_by_field_name("name", &mut cursor).collect();
            if names.len() >= 2 {
                node_text(names[names.len() - 1], src)
                    .map(str::to_string)
                    .filter(|s| !s.is_empty())
            } else {
                Some("<default>".to_string())
            }
        }
        // top-level function: `parameters:` is a named `function_signature`.
        "lambda_expression" => {
            let params = node.child_by_field_name("parameters")?;
            if params.kind() == "function_signature" {
                params
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, src))
                    .map(str::to_string)
                    .filter(|s| !s.is_empty())
            } else {
                None // anonymous closure → not a symbol
            }
        }
        _ => None,
    }
}

fn dart_body_of(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    match node.kind() {
        "class_definition" | "enum_declaration" | "extension_declaration" => {
            node.child_by_field_name("body")
        }
        "mixin_declaration" => child_of_kind(node, "class_body"),
        // For the call-test recursion into `void main() { … }` we want the
        // statement block, not the wrapping `function_body`.
        "lambda_expression" => {
            let body = node.child_by_field_name("body")?;
            child_of_kind(body, "block")
        }
        _ => None,
    }
}

fn dart_is_transparent(kind: &str) -> bool {
    matches!(kind, "class_member_definition" | "declaration")
}

fn dart_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "import_or_export" {
        return Vec::new();
    }
    match first_descendant(node, "string_literal", 6) {
        Some(s) => match node_text(s, src) {
            Some(t) => vec![unquote(t)],
            None => Vec::new(),
        },
        None => Vec::new(),
    }
}

fn dart_call_test_of<'a>(node: tree_sitter::Node<'a>, src: &[u8]) -> Option<CallTestHit<'a>> {
    let ma = match node.kind() {
        "member_access" => node,
        "expression_statement" => child_of_kind(node, "member_access")?,
        _ => return None,
    };
    let callee = ma.named_child(0)?;
    if callee.kind() != "identifier" {
        return None;
    }
    let kind = match node_text(callee, src)? {
        "test" | "testWidgets" => TestKind::Case,
        "group" => TestKind::Group,
        _ => return None,
    };
    let selector = child_of_kind(ma, "selector")?;
    let arg_part = child_of_kind(selector, "argument_part")?;
    let args = child_of_kind(arg_part, "arguments")?;

    let mut description: Option<String> = None;
    let mut body: Option<tree_sitter::Node<'a>> = None;
    let mut cursor = args.walk();
    for arg in args.named_children(&mut cursor) {
        if arg.kind() != "argument" {
            continue;
        }
        let Some(inner) = arg.named_child(0) else {
            continue;
        };
        if description.is_none() && inner.kind() == "string_literal" {
            description = node_text(inner, src).map(unquote);
        } else if inner.kind() == "function_expression" {
            body = inner
                .child_by_field_name("body")
                .and_then(|b| child_of_kind(b, "block"));
        }
    }
    let name = description?;
    if name.is_empty() {
        return None;
    }
    Some(CallTestHit { kind, name, body })
}

/// Resolve a Dart import target to a repo-relative `.dart` path. SDK
/// (`dart:`) imports drop; `package:<pkg>/<p>` maps to `lib/<p>` of the
/// current package; relative imports resolve against the importer's dir.
/// Anything that does not land on a known file is dropped (external dep).
pub(crate) fn dart_resolve_import(
    raw: &str,
    from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with("dart:") {
        return None;
    }
    let candidate = if let Some(rest) = raw.strip_prefix("package:") {
        // package:<pkg>/<path> → lib/<path> (same-package only).
        let path = rest.split_once('/').map(|(_, p)| p)?;
        format!("lib/{path}")
    } else {
        // Relative import: resolve against the importing file's directory.
        let base = match from_file.rsplit_once('/') {
            Some((dir, _)) => dir,
            None => "",
        };
        normalize_relative(base, raw)
    };
    all_files.iter().find(|f| f.as_str() == candidate).cloned()
}

/// Join `base` (a directory) with a relative import `rel`, collapsing
/// `.`/`..` segments. Pure string math — never touches the filesystem.
fn normalize_relative(base: &str, rel: &str) -> String {
    let mut stack: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

pub(crate) static DART_SPEC: LangSpec = LangSpec {
    language_id: "dart",
    grammar: dart_language,
    extensions: &["dart"],
    skip_dirs: &[".dart_tool", "build", ".git"],
    separator: ".",
    func_kind: NodeKind::DartFunction,
    method_kind: NodeKind::DartMethod,
    container_of: dart_container_of,
    is_callable_kind: dart_is_callable,
    callable_kind_of: dart_callable_kind,
    callable_span_of: dart_callable_span,
    impl_type_of: dart_extension_type,
    receiver_type_of: no_text,
    import_of: dart_import_of,
    name_of: dart_name_of,
    body_of: dart_body_of,
    is_transparent_kind: dart_is_transparent,
    metadata_of: no_text,
    test_of: no_test_of,
    call_test_of: dart_call_test_of,
    src_roots_of: crate::treesitter::no_src_roots,
    resolve_import: dart_resolve_import,
    recurse_callables: true,
    call_idents_of: no_call_idents,
    module_scoped_resolution: false,
};

// ---------------------------------------------------------------------------
// Legacy-id structural mapping
// ---------------------------------------------------------------------------

/// Structural facts extracted from one Dart file, addressed with the legacy
/// `dart_*::` id scheme so the analyzer/lightweight semantic overlays bind
/// without translation. `raw_imports` are unresolved targets — the caller
/// resolves them against the full repo file set via [`dart_resolve_import`].
#[derive(Debug, Clone, Default)]
pub struct DartFileStructure {
    pub symbols: Vec<SymbolArtifact>,
    pub tests: Vec<TestArtifact>,
    pub ranges: Vec<SymbolRange>,
    pub raw_imports: Vec<String>,
}

fn test_node_kind(kind: TestKind) -> NodeKind {
    match kind {
        TestKind::Case => NodeKind::TestCase,
        TestKind::Group => NodeKind::TestGroup,
    }
}

fn dart_symbol_id(path: &str, kind: NodeKind, qualified: &str) -> ArtifactId {
    match kind {
        NodeKind::DartClass => dart_class_id(path, qualified),
        NodeKind::DartFunction => dart_function_id(path, qualified),
        NodeKind::DartMethod => {
            let (cls, member) = split_first_dot(qualified);
            dart_method_id(path, cls, member)
        }
        NodeKind::DartConstructor => {
            let (cls, ctor) = split_first_dot(qualified);
            dart_constructor_id(path, cls, ctor)
        }
        _ => ArtifactId::new(format!("dart::{path}::{qualified}")),
    }
}

fn dart_test_node_id(path: &str, kind: TestKind, name: &str) -> ArtifactId {
    match kind {
        TestKind::Case => dart_test_id(path, &slugify(name)),
        TestKind::Group => dart_group_id(path, &slugify(name)),
    }
}

/// Run the tree-sitter driver over one Dart file and lower the generic
/// [`crate::treesitter::Scan`] into legacy-id [`SymbolArtifact`] /
/// [`TestArtifact`] / [`SymbolRange`]s.
pub fn dart_extract_structure(file_rel: &str, source: &str) -> DartFileStructure {
    let scan = extract(&DART_SPEC, source);

    // qualified-name → legacy id, for both symbols and tests, so parent
    // links resolve to the actual node we emit (and never dangle).
    let mut id_of: BTreeMap<String, ArtifactId> = BTreeMap::new();
    for s in &scan.symbols {
        id_of
            .entry(s.qualified_name.clone())
            .or_insert_with(|| dart_symbol_id(file_rel, s.kind, &s.qualified_name));
    }
    for t in &scan.tests {
        id_of
            .entry(t.qualified_name.clone())
            .or_insert_with(|| dart_test_node_id(file_rel, t.kind, &t.name));
    }

    let mut out = DartFileStructure {
        raw_imports: scan.imports.iter().map(|i| i.path.clone()).collect(),
        ..Default::default()
    };

    for s in &scan.symbols {
        let id = id_of[&s.qualified_name].clone();
        let parent = s
            .parent_qualified_name
            .as_ref()
            .and_then(|p| id_of.get(p).cloned());
        out.symbols.push(SymbolArtifact {
            id: id.clone(),
            kind: s.kind,
            path: file_rel.to_string(),
            name: s.name.clone(),
            qualified_name: s.qualified_name.clone(),
            start_line: s.start_line,
            end_line: s.end_line,
            parent_symbol_id: parent.clone(),
            metadata_json: s.metadata.clone(),
        });
        out.ranges.push(SymbolRange {
            file_path: file_rel.to_string(),
            symbol_id: id,
            start_line: s.start_line,
            end_line: s.end_line,
            symbol_kind: s.kind,
            qualified_name: s.qualified_name.clone(),
            parent_symbol_id: parent,
        });
    }

    for t in &scan.tests {
        let id = id_of[&t.qualified_name].clone();
        let parent = t
            .parent_qualified_name
            .as_ref()
            .and_then(|p| id_of.get(p).cloned());
        out.tests.push(TestArtifact {
            id,
            kind: test_node_kind(t.kind),
            path: file_rel.to_string(),
            name: t.name.clone(),
            start_line: t.start_line,
            end_line: t.end_line,
            parent_symbol_id: parent,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(s: &DartFileStructure) -> Vec<String> {
        s.symbols.iter().map(|x| x.id.to_string()).collect()
    }

    #[test]
    fn classes_methods_constructors_and_functions_get_legacy_ids() {
        let src = r#"
class EditorController {
  EditorController();
  EditorController.named(this.x);
  int get tool => 0;
  void applyTool(Tool t) { _persist(); }
  void _persist() {}
}

void main() {}
"#;
        let s = dart_extract_structure("lib/features/editor/editor_controller.dart", src);
        let got = ids(&s);
        assert!(got.contains(
            &"dart_class::lib/features/editor/editor_controller.dart#EditorController".to_string()
        ));
        assert!(got.contains(
            &"dart_method::lib/features/editor/editor_controller.dart#EditorController.applyTool"
                .to_string()
        ));
        assert!(got.contains(
            &"dart_method::lib/features/editor/editor_controller.dart#EditorController.tool"
                .to_string()
        ));
        assert!(got.contains(
            &"dart_ctor::lib/features/editor/editor_controller.dart#EditorController.<default>"
                .to_string()
        ));
        assert!(got.contains(
            &"dart_ctor::lib/features/editor/editor_controller.dart#EditorController.named"
                .to_string()
        ));
        assert!(
            got.contains(&"dart_fn::lib/features/editor/editor_controller.dart#main".to_string())
        );

        // applyTool nests under the class.
        let apply = s
            .symbols
            .iter()
            .find(|x| x.name == "applyTool")
            .expect("applyTool symbol");
        assert_eq!(
            apply.parent_symbol_id.as_ref().map(|p| p.to_string()),
            Some(
                "dart_class::lib/features/editor/editor_controller.dart#EditorController"
                    .to_string()
            )
        );
    }

    #[test]
    fn abstract_members_without_bodies_are_emitted_as_methods() {
        // Abstract members parse as a *bare* function/getter/setter signature
        // (no `method_signature`, no `function_body`); they must still emit a
        // method symbol so semantic edges and impact have an anchor.
        let src = "abstract class P {\n  void render();\n  int get tier;\n  set name(String v);\n  void concrete() {}\n}\n";
        let s = dart_extract_structure("lib/p.dart", src);
        let got = ids(&s);
        assert!(
            got.contains(&"dart_method::lib/p.dart#P.render".to_string()),
            "abstract method missing: {got:?}"
        );
        assert!(
            got.contains(&"dart_method::lib/p.dart#P.tier".to_string()),
            "abstract getter missing: {got:?}"
        );
        assert!(
            got.contains(&"dart_method::lib/p.dart#P.name".to_string()),
            "abstract setter missing: {got:?}"
        );
        // The concrete sibling is still emitted exactly once (no double-count
        // of the inner function_signature).
        let render_count = s.symbols.iter().filter(|x| x.name == "render").count();
        assert_eq!(render_count, 1, "abstract method emitted twice: {got:?}");
        let concrete_count = s.symbols.iter().filter(|x| x.name == "concrete").count();
        assert_eq!(concrete_count, 1, "concrete method emitted twice: {got:?}");
    }

    #[test]
    fn method_and_constructor_ranges_cover_their_bodies() {
        // The grammar splits a method's signature from its body; the symbol
        // range must still span the body so point-in-range / impact queries
        // resolve a line inside the body to the method (regression: the
        // range used to collapse onto the signature line).
        let src = "class S {\n  S();\n  int m(int a) {\n    return a;\n  }\n}\n";
        let s = dart_extract_structure("lib/s.dart", src);
        let m = s.symbols.iter().find(|x| x.name == "m").expect("method m");
        // signature is line 3, body close is line 5 (1-indexed).
        assert_eq!(m.start_line, 3, "{m:?}");
        assert!(m.end_line >= 5, "method range must cover its body: {m:?}");
        // the matching range mirrors the symbol.
        let r = s
            .ranges
            .iter()
            .find(|r| r.qualified_name == "S.m")
            .expect("range S.m");
        assert_eq!((r.start_line, r.end_line), (m.start_line, m.end_line));
    }

    #[test]
    fn mixins_and_extensions_attach_members() {
        let src = r#"
mixin Logger {
  void log(String m) {}
}

extension StringX on String {
  String shout() => toUpperCase();
}
"#;
        let s = dart_extract_structure("lib/util.dart", src);
        let got = ids(&s);
        // mixin → DartClass + method nested under it.
        assert!(got.contains(&"dart_class::lib/util.dart#Logger".to_string()));
        assert!(got.contains(&"dart_method::lib/util.dart#Logger.log".to_string()));
        // extension members attach to the extended type `String`.
        assert!(got.contains(&"dart_method::lib/util.dart#String.shout".to_string()));
    }

    #[test]
    fn private_extension_on_private_type_attaches_members() {
        // Regression (turing dogfood): a private extension on a private state
        // class inside a part file. Its members must attach to the `on` type
        // (`dart_method::<file>#_GameScreenState._showSnackBar`) so the
        // analyzer sidecar's usage edges resolve. Previously these surfaced as
        // top-level `dart_fn::…#_showSnackBar` nodes and were flagged as
        // high-confidence dead code despite many in-file callers.
        let src = r#"
part of '../game_screen.dart';

extension _GameScreenEditor on _GameScreenState {
  void _showSnackBar(SnackBar snackBar) {
    final messenger = ScaffoldMessenger.of(context);
    messenger.showSnackBar(snackBar);
  }
}
"#;
        let s = dart_extract_structure("lib/screens/game_screen/game_screen_editor.dart", src);
        let got = ids(&s);
        assert!(
            got.contains(
                &"dart_method::lib/screens/game_screen/game_screen_editor.dart#_GameScreenState._showSnackBar"
                    .to_string()
            ),
            "extension member must attach to the `on` type, got: {got:?}"
        );
        assert!(
            !got.contains(
                &"dart_fn::lib/screens/game_screen/game_screen_editor.dart#_showSnackBar"
                    .to_string()
            ),
            "extension member must NOT be emitted as a top-level function, got: {got:?}"
        );
    }

    #[test]
    fn call_based_tests_and_groups_are_detected_inside_main() {
        let src = r#"
import 'package:test/test.dart';

void main() {
  group('auto placement', () {
    test('places watermark outside face region', () {
      expect(1, 1);
    });
  });
  test('top level case', () {});
}
"#;
        let s = dart_extract_structure("test/watermark_test.dart", src);
        let test_ids: Vec<String> = s.tests.iter().map(|t| t.id.to_string()).collect();
        assert!(test_ids.contains(
            &"dart_test::test/watermark_test.dart#places-watermark-outside-face-region".to_string()
        ));
        assert!(
            test_ids.contains(&"dart_test::test/watermark_test.dart#top-level-case".to_string())
        );
        assert!(
            test_ids.contains(&"dart_group::test/watermark_test.dart#auto-placement".to_string())
        );

        // The nested case is parented onto the group.
        let nested = s
            .tests
            .iter()
            .find(|t| t.name == "places watermark outside face region")
            .expect("nested test");
        assert_eq!(
            nested.parent_symbol_id.as_ref().map(|p| p.to_string()),
            Some("dart_group::test/watermark_test.dart#auto-placement".to_string())
        );
    }

    #[test]
    fn imports_resolve_relative_and_package_drop_sdk_and_external() {
        let files = vec![
            "lib/a.dart".to_string(),
            "lib/sub/b.dart".to_string(),
            "lib/features/c.dart".to_string(),
        ];
        // package:<this>/sub/b.dart → lib/sub/b.dart
        assert_eq!(
            dart_resolve_import("package:myapp/sub/b.dart", "lib/a.dart", &files, &[]),
            Some("lib/sub/b.dart".to_string())
        );
        // relative from lib/features/c.dart → ../a.dart = lib/a.dart
        assert_eq!(
            dart_resolve_import("../a.dart", "lib/features/c.dart", &files, &[]),
            Some("lib/a.dart".to_string())
        );
        // dart: SDK import drops.
        assert_eq!(
            dart_resolve_import("dart:async", "lib/a.dart", &files, &[]),
            None
        );
        // unknown external package drops.
        assert_eq!(
            dart_resolve_import("package:other/x.dart", "lib/a.dart", &files, &[]),
            None
        );
    }

    #[test]
    fn driver_is_total_on_arbitrary_input() {
        // Totality guard: never panics, regardless of input shape.
        for src in [
            "",
            "class",
            "void main() {",
            "extension on {{{",
            "class A { A.",
            "test('x', () {",
            "ⓤⓝⓘⓒⓞⓓⓔ class 名 { void 方法() {} }",
            &"class A { void m() {} } ".repeat(500),
        ] {
            let _ = dart_extract_structure("lib/x.dart", src);
        }
    }
}
