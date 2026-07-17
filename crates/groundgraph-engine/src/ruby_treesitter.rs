//! P26 — Ruby language spec for the generic tree-sitter driver.
//!
//! Owns `.rb` and is the sole structural backend for Ruby: `module` /
//! `class` containers, `def` methods (`def self.x` singleton methods
//! included), Minitest `test_*` methods and RSpec `describe`/`it` blocks as
//! tests, and `require` / `require_relative` resolved to repo files.
//! Output is tagged `indexer = ruby_treesitter`.
//!
//! Shape notes:
//! - `class Foo::Bar` carries a `scope_resolution` name; the raw text
//!   (`Foo::Bar`) is kept as the symbol name — Ruby's own spelling.
//! - `def self.build` (a `singleton_method` node) is a *class-level*
//!   method; both flavours share [`NodeKind::RubyMethod`] under a type and
//!   [`NodeKind::RubyFunction`] at top level.
//! - RSpec's DSL is call-based (`describe "x" do … end`), so it flows
//!   through the driver's `call_test_of` hook with body recursion the same
//!   way Jest's `describe`/`it` does for TypeScript.

use crate::treesitter::{
    body_from_field, name_from_field, no_src_roots, no_text, node_text, CallKind, CallTestHit,
    LangSpec, RefKind, SymKind, TestKind,
};
use groundgraph_core::NodeKind;

fn ruby_language() -> tree_sitter::Language {
    tree_sitter_ruby::LANGUAGE.into()
}

fn ruby_container_of(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<SymKind> {
    match node.kind() {
        // Ruby modules are namespaces *and* mixins; either way their `def`s
        // are method-like (they become instance methods when included), so
        // Type — not Module — keeps nested callables as RubyMethod.
        "module" => Some(SymKind::Type(NodeKind::RubyModule)),
        "class" => Some(SymKind::Type(NodeKind::RubyClass)),
        _ => None,
    }
}

fn ruby_is_callable(kind: &str) -> bool {
    matches!(kind, "method" | "singleton_method")
}

/// `class << self … end` opens the singleton class; its `def`s are class
/// methods of the enclosing class. `do_block` / `block` / `body_statement`
/// are the statement wrappers the walker meets when descending into an
/// RSpec `describe … do` body (the *container* bodies enter via the `body`
/// field instead, so this only fires under transparent / test recursion).
fn ruby_is_transparent(kind: &str) -> bool {
    matches!(
        kind,
        "singleton_class" | "do_block" | "block" | "body_statement"
    )
}

/// Minitest / test-unit convention: a `test_*` method inside a `*Test` /
/// `*::TestCase`-superclassed class. Superclass text is not visible at the
/// method node, so the heuristic is the method-name prefix plus an enclosing
/// type — top-level `test_*` functions are *not* reclassified (scripts often
/// define helpers named `test_…`).
fn ruby_test_of(
    _node: tree_sitter::Node<'_>,
    _src: &[u8],
    kind: NodeKind,
    name: &str,
    parent_qualified: Option<&str>,
) -> Option<TestKind> {
    if kind != NodeKind::RubyMethod {
        return None;
    }
    (name.starts_with("test_") && parent_qualified.is_some()).then_some(TestKind::Case)
}

/// RSpec / Minitest::Spec DSL: `describe` / `context` open groups,
/// `it` / `specify` / `test` open cases. The description string (first
/// argument) names the node; the `do … end` block is recursed for nesting.
/// `RSpec.describe Foo do` is a `call` with receiver `RSpec`.
fn ruby_call_test<'a>(node: tree_sitter::Node<'a>, src: &[u8]) -> Option<CallTestHit<'a>> {
    if node.kind() != "call" {
        return None;
    }
    let method = node
        .child_by_field_name("method")
        .and_then(|m| node_text(m, src))?;
    let kind = match method {
        "describe" | "context" => TestKind::Group,
        "it" | "specify" => TestKind::Case,
        _ => return None,
    };
    // First argument: a string ("charges the gateway") or a constant
    // (RSpec.describe Billing::Invoice).
    let args = node.child_by_field_name("arguments")?;
    let mut name = None;
    for i in 0..args.named_child_count() {
        let Some(a) = args.named_child(u32::try_from(i).unwrap_or(u32::MAX)) else {
            break;
        };
        name = match a.kind() {
            "string" => string_content_text(a, src),
            "constant" | "scope_resolution" => node_text(a, src).map(str::to_string),
            _ => None,
        };
        if name.is_some() {
            break;
        }
    }
    let name = name?;
    let body = node.child_by_field_name("block");
    Some(CallTestHit { kind, name, body })
}

/// The text of a string literal's `string_content` child (`'json'` → `json`).
fn string_content_text(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    for i in 0..node.named_child_count() {
        let c = node.named_child(u32::try_from(i).unwrap_or(u32::MAX))?;
        if c.kind() == "string_content" {
            return node_text(c, src).map(str::to_string);
        }
    }
    None
}

/// `require 'json'` / `require_relative 'lib/helper'` → the path string.
/// `require_relative` keeps a `./` prefix so the resolver knows to anchor at
/// the requiring file's directory.
fn ruby_import_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() != "call" {
        return Vec::new();
    }
    let Some(method) = node
        .child_by_field_name("method")
        .and_then(|m| node_text(m, src))
    else {
        return Vec::new();
    };
    if !matches!(method, "require" | "require_relative") {
        return Vec::new();
    }
    let Some(args) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut text = None;
    for i in 0..args.named_child_count() {
        let Some(a) = args.named_child(u32::try_from(i).unwrap_or(u32::MAX)) else {
            break;
        };
        if a.kind() == "string" {
            text = string_content_text(a, src);
            break;
        }
    }
    let Some(text) = text else {
        return Vec::new();
    };
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if method == "require_relative" {
        vec![format!("./{text}")]
    } else {
        vec![text.to_string()]
    }
}

/// Resolve a require target to a repo file:
/// - `./…` (require_relative) → relative to the requiring file's directory,
///   `.rb` appended.
/// - bare names (require) → gem-style load-path probe: `lib/<name>.rb`
///   first (the canonical gem layout), then `<name>.rb` from the repo root.
///   Stdlib / external gems match nothing → `None`.
fn ruby_resolve_import(
    raw: &str,
    from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let target = raw.trim();
    if target.is_empty() {
        return None;
    }
    let with_ext = |p: &str| {
        if p.ends_with(".rb") {
            p.to_string()
        } else {
            format!("{p}.rb")
        }
    };
    if let Some(rel) = target.strip_prefix("./") {
        let dir = std::path::Path::new(from_file).parent()?;
        let joined = dir.join(with_ext(rel));
        let normal = crate::treesitter::canonicalize_rel_path(&joined);
        return all_files.contains(&normal).then_some(normal);
    }
    let candidate = with_ext(target);
    for probe in [format!("lib/{candidate}"), candidate.clone()] {
        if all_files.contains(&probe) {
            return Some(probe);
        }
    }
    // Monorepo gems: any `…/lib/<name>.rb` (e.g. `gems/foo/lib/foo.rb`).
    let needle = format!("/lib/{candidate}");
    all_files.iter().find(|f| f.ends_with(&needle)).cloned()
}

/// Outbound identifiers from a method body:
/// - `helper(x)` / `obj.helper(x)` → `Call` on the method name.
/// - `Foo.new` / `Foo::Bar.new` → `Reference` to the constructed constant
///   (Ruby's object creation).
fn ruby_call_idents(body: tree_sitter::Node<'_>, src: &[u8]) -> Vec<(String, RefKind)> {
    let mut out = Vec::new();
    crate::treesitter::collect_calls(body, src, &mut out, 0, RUBY_CALL_KINDS);
    out
}

/// The single Ruby call shape (tree-sitter-ruby spells it `call`): a
/// `Foo.new` construction is a `Reference` to the constant's bare name; any
/// other method (`foo`, `obj.bar`) is a `Call` on the method name.
fn ruby_call_extract(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<(String, RefKind)> {
    let method = node
        .child_by_field_name("method")
        .and_then(|m| node_text(m, src));
    let receiver = node.child_by_field_name("receiver");
    match (method, receiver) {
        (Some("new"), Some(r)) if matches!(r.kind(), "constant" | "scope_resolution") => {
            let t = node_text(r, src)?;
            let bare = t.rsplit("::").next().unwrap_or(t);
            Some((bare.to_string(), RefKind::Reference))
        }
        (Some(m), _) if m != "new" => Some((m.to_string(), RefKind::Call)),
        _ => None,
    }
}

static RUBY_CALL_KINDS: &[CallKind] = &[CallKind {
    kind: "call",
    extract: ruby_call_extract,
}];

pub(crate) static RUBY_SPEC: LangSpec = LangSpec {
    language_id: "ruby",
    grammar: ruby_language,
    extensions: &["rb", "rake"],
    skip_dirs: &[
        ".git",
        "vendor",
        "node_modules",
        "tmp",
        "log",
        ".bundle",
        "coverage",
    ],
    separator: "::",
    func_kind: NodeKind::RubyFunction,
    method_kind: NodeKind::RubyMethod,
    container_of: ruby_container_of,
    is_callable_kind: ruby_is_callable,
    callable_kind_of: crate::treesitter::keep_callable_kind,
    callable_span_of: crate::treesitter::callable_node_is_span,
    impl_type_of: no_text,
    receiver_type_of: no_text,
    import_of: ruby_import_of,
    name_of: name_from_field,
    body_of: body_from_field,
    is_transparent_kind: ruby_is_transparent,
    metadata_of: no_text,
    test_of: ruby_test_of,
    call_test_of: ruby_call_test,
    src_roots_of: no_src_roots,
    resolve_import: ruby_resolve_import,
    // RSpec files put `describe`/`it` at top level but Minitest cases sit
    // *inside* methods of spec-style DSLs too; recursing callables lets the
    // call-test walker find nested blocks (mirrors Dart's `main() { test() }`).
    recurse_callables: false,
    emit_nested_callables_with_metadata_only: false,
    call_idents_of: ruby_call_idents,
    module_scoped_resolution: false,
    recurse_declined_callables: false,
    claims_path: None,
    partial_class_merge: false,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::{extract, Scan};

    fn scan(src: &str) -> Scan {
        extract(&RUBY_SPEC, src)
    }
    fn qnames(scan: &Scan, kind: NodeKind) -> Vec<String> {
        scan.symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.qualified_name.clone())
            .collect()
    }

    #[test]
    fn modules_classes_methods_and_singletons() {
        let src = r#"
module Billing
  class Invoice
    def initialize(total)
      @total = total
    end

    def charge!(gateway)
      gateway.charge(@total)
    end

    def self.build
      new(0)
    end

    class << self
      def default_currency
        :usd
      end
    end
  end
end

def top_level_helper(x)
  x * 2
end
"#;
        let s = scan(src);
        assert!(qnames(&s, NodeKind::RubyModule).contains(&"Billing".to_string()));
        assert!(qnames(&s, NodeKind::RubyClass).contains(&"Billing::Invoice".to_string()));
        let methods = qnames(&s, NodeKind::RubyMethod);
        assert!(
            methods.contains(&"Billing::Invoice::charge!".to_string()),
            "{methods:?}"
        );
        assert!(
            methods.contains(&"Billing::Invoice::build".to_string()),
            "def self.x is a method of the class: {methods:?}"
        );
        assert!(
            methods.contains(&"Billing::Invoice::default_currency".to_string()),
            "class << self is transparent: {methods:?}"
        );
        assert!(
            qnames(&s, NodeKind::RubyFunction).contains(&"top_level_helper".to_string()),
            "top-level def is a function"
        );
    }

    #[test]
    fn compound_class_names_keep_their_ruby_spelling() {
        let s = scan("class Foo::Bar\n  def go\n  end\nend\n");
        assert!(
            qnames(&s, NodeKind::RubyClass).contains(&"Foo::Bar".to_string()),
            "{:?}",
            qnames(&s, NodeKind::RubyClass)
        );
    }

    #[test]
    fn minitest_methods_and_rspec_blocks_become_tests() {
        let src = r#"
class InvoiceTest < Minitest::Test
  def test_charges
    assert true
  end

  def helper_method
  end
end

RSpec.describe Billing::Invoice do
  it "charges the gateway" do
  end

  describe "when empty" do
    it "charges nothing" do
    end
  end
end
"#;
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
        assert!(
            cases.contains(&"InvoiceTest::test_charges"),
            "minitest test_*: {cases:?}"
        );
        assert!(
            groups.contains(&"Billing::Invoice"),
            "RSpec.describe Constant opens a group: {groups:?}"
        );
        assert!(
            cases.iter().any(|c| c.ends_with("charges the gateway")),
            "it 'desc' opens a case: {cases:?}"
        );
        assert!(
            groups.iter().any(|g| g.ends_with("when empty")),
            "nested describe: {groups:?}"
        );
        // helper stays structural.
        assert!(
            qnames(&s, NodeKind::RubyMethod).contains(&"InvoiceTest::helper_method".to_string())
        );
    }

    #[test]
    fn requires_are_captured_and_resolved() {
        let s = scan("require 'json'\nrequire_relative 'helper'\nrequire 'billing/invoice'\n");
        let imports: Vec<&str> = s.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(imports.contains(&"json"), "{imports:?}");
        assert!(
            imports.contains(&"./helper"),
            "require_relative keeps its anchor: {imports:?}"
        );
        assert!(imports.contains(&"billing/invoice"), "{imports:?}");

        let files = vec![
            "lib/billing/invoice.rb".to_string(),
            "lib/billing.rb".to_string(),
            "spec/billing/helper.rb".to_string(),
            "spec/billing/invoice_spec.rb".to_string(),
        ];
        // Gem layout: bare require probes lib/.
        assert_eq!(
            ruby_resolve_import("billing/invoice", "app.rb", &files, &[]),
            Some("lib/billing/invoice.rb".to_string())
        );
        // require_relative anchors at the requiring file.
        assert_eq!(
            ruby_resolve_import("./helper", "spec/billing/invoice_spec.rb", &files, &[]),
            Some("spec/billing/helper.rb".to_string())
        );
        // Stdlib → None.
        assert_eq!(ruby_resolve_import("json", "app.rb", &files, &[]), None);
    }

    #[test]
    fn captures_calls_and_constant_construction() {
        let src = r#"
class App
  def run
    g = Greeter.new
    g.greet("x")
    helper(1)
  end

  def helper(x)
  end
end

class Greeter
  def greet(name)
    "hi"
  end
end
"#;
        let s = scan(src);
        let refs: Vec<(String, String, RefKind)> = s
            .references
            .iter()
            .map(|r| (r.from_qualified.to_string(), r.to_name.clone(), r.kind))
            .collect();
        assert!(
            refs.contains(&("App::run".into(), "Greeter".into(), RefKind::Reference)),
            "Foo.new is a construction reference: {refs:?}"
        );
        assert!(
            refs.contains(&("App::run".into(), "greet".into(), RefKind::Call)),
            "{refs:?}"
        );
        // NB. a bare no-paren `helper` parses as `identifier` (ambiguous with
        // a local variable read), so only parenthesised / receiver calls are
        // captured — hence `helper(1)` above.
        assert!(
            refs.contains(&("App::run".into(), "helper".into(), RefKind::Call)),
            "{refs:?}"
        );
    }

    #[test]
    fn garbage_is_safe() {
        assert_eq!(scan(""), Scan::default());
        let _ = scan("class class def ((( end end end");
        let _ = scan("module 名前\n  def 方法\n  end\nend\n");
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
            prop_assert_eq!(extract(&RUBY_SPEC, &s), extract(&RUBY_SPEC, &s));
        }
    }
}
