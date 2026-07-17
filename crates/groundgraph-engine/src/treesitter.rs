//! Generic in-process tree-sitter breadth backend (Tier 2).
//!
//! One driver, many languages. Instead of a hand-written scanner +
//! indexer per language (which would be six near-identical files), every
//! language is reduced to a data-driven [`LangSpec`]: a grammar, a set of
//! file extensions, and a handful of small functions that map this
//! grammar's node kinds onto GroundGraph [`NodeKind`]s. The generic
//! [`extract`] walker and [`index_repo_with_spec`] indexer are written
//! and tested once and shared by all languages.
//!
//! Design notes / why this shape:
//! - Nesting (`Outer::Inner::method`) is derived from the *actual* AST
//!   ancestry during the walk, so it works uniformly across languages
//!   without per-language "container lists".
//! - The two genuinely irregular cases — Rust `impl` blocks (methods
//!   attach to a type declared elsewhere) and Go method receivers
//!   (`func (r *T) m()`) — are isolated behind the `impl_type_of` /
//!   `receiver_type_of` hooks so the common path stays simple.
//! - The driver is **total and panic-free** on any input (pinned by the
//!   per-language property tests), and recursion is depth-capped so a
//!   pathologically nested file cannot blow the stack.

use std::path::{Path, PathBuf};

use std::sync::Arc;

use anyhow::{Context, Result};
use groundgraph_core::artifact_id::{file_id, ArtifactId};
use groundgraph_core::language_batch::{
    FileArtifact, ImportEdge, LanguageIndexBatch, ReferenceEdge, SymbolArtifact, TestArtifact,
};
use groundgraph_core::{EdgeKind, NodeKind};
use groundgraph_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;

/// Hard cap on recursion into nested bodies. Real code never approaches
/// this; the bound exists purely so a maliciously deep input cannot blow
/// the stack (we simply stop descending — never panic, never abort).
pub const MAX_NESTING_DEPTH: usize = 256;

/// What a "container" declaration is, structurally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymKind {
    /// A user-defined type (class / struct / enum / trait / interface /
    /// protocol). Callables nested inside its body become *methods*.
    Type(NodeKind),
    /// A module / namespace / package. Nested callables stay *functions*.
    Module(NodeKind),
}

impl SymKind {
    fn node_kind(self) -> NodeKind {
        match self {
            SymKind::Type(k) | SymKind::Module(k) => k,
        }
    }
    fn is_type(self) -> bool {
        matches!(self, SymKind::Type(_))
    }
}

/// A structural symbol recovered from a source file. Language-agnostic;
/// `rust_treesitter` etc. re-export this under their own names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedSymbol {
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub parent_qualified_name: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    /// Optional structured JSON published by a language's `metadata_of`
    /// hook (e.g. Python framework facts). `None` for the common case.
    pub metadata: Option<String>,
}

/// Coarse test role used to classify a declaration or a call-based test
/// case onto a GroundGraph [`NodeKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    /// A single test case (`def test_x`, Go `TestX`, JUnit `@Test`, JS `it`).
    Case,
    /// A test group / suite (`class Test*`, JS `describe`).
    Group,
}

impl TestKind {
    fn node_kind(self) -> NodeKind {
        match self {
            TestKind::Case => NodeKind::TestCase,
            TestKind::Group => NodeKind::TestGroup,
        }
    }
}

/// A test node recovered from a source file (either by reclassifying a
/// declaration via `test_of`, or by walking a call-based framework via
/// `call_test_of`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedTest {
    pub kind: TestKind,
    pub name: String,
    pub qualified_name: String,
    pub parent_qualified_name: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
}

/// Result of a `call_test_of` hook: a call-based test/group plus the body
/// node to recurse into for nested cases (`describe` → `it`).
pub struct CallTestHit<'a> {
    pub kind: TestKind,
    pub name: String,
    pub body: Option<tree_sitter::Node<'a>>,
}

/// An import / use target, stored verbatim (already whitespace-collapsed
/// and stripped of any trailing `;`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedImport {
    pub path: String,
}

/// Whether a captured body identifier is a call target or a plain type /
/// value reference. Mapped to [`EdgeKind::Calls`] / [`EdgeKind::References`]
/// once resolved to an in-repo symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Call,
    Reference,
}

impl RefKind {
    fn edge_kind(self) -> EdgeKind {
        match self {
            RefKind::Call => EdgeKind::Calls,
            RefKind::Reference => EdgeKind::References,
        }
    }
}

/// A body-level outbound reference captured during the walk: from the
/// enclosing callable (`from_qualified`) to a still-unresolved name
/// (`to_name`, possibly a `::`-qualified path for Rust associated calls).
/// [`index_repo_with_spec`] resolves `to_name` to a concrete in-repo
/// symbol id (same-file first, then via resolved imports) and emits a
/// medium-confidence edge. Only populated by languages whose
/// [`LangSpec::call_idents_of`] opts in (today: Rust); empty otherwise so
/// every other language's scan output is byte-for-byte unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedRef {
    pub from_qualified: Arc<str>,
    pub to_name: String,
    pub kind: RefKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub symbols: Vec<ScannedSymbol>,
    pub imports: Vec<ScannedImport>,
    pub tests: Vec<ScannedTest>,
    /// Heuristic body-level call/reference identifiers (see [`ScannedRef`]).
    pub references: Vec<ScannedRef>,
    /// The parse exceeded the per-file budget and was abandoned (the scan is
    /// empty). Counted by the file loop and surfaced as an index warning.
    pub parse_timed_out: bool,
}

// Function-pointer hook types. Using fn pointers (not trait objects)
// keeps a `LangSpec` a plain `const`/`static` with zero allocation.
type ContainerFn = for<'a, 'b> fn(tree_sitter::Node<'a>, &'b [u8]) -> Option<SymKind>;
type TextFn = for<'a, 'b> fn(tree_sitter::Node<'a>, &'b [u8]) -> Option<String>;
/// Import hook: one source node may yield several targets (`import a, b`).
type ImportsFn = for<'a, 'b> fn(tree_sitter::Node<'a>, &'b [u8]) -> Vec<String>;
type BodyFn = for<'a> fn(tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>>;
type KindBoolFn = fn(&str) -> bool;
/// Override the [`NodeKind`] assigned to a callable based on its concrete
/// node (e.g. Java `constructor_declaration` → `JavaConstructor`). Receives
/// the callable node, source, and the default kind (method / free function)
/// the driver would otherwise assign. Default [`keep_callable_kind`].
type CallableKindFn = for<'a, 'b> fn(tree_sitter::Node<'a>, &'b [u8], NodeKind) -> NodeKind;
/// Reclassify a *declaration* (function / method / class) as a test.
/// Receives the declaration node, source, the resolved [`NodeKind`], the
/// bare name, and the effective parent qualified name. Returns the test
/// role, or `None` to keep it a normal structural symbol.
type TestOfFn = for<'a, 'b> fn(
    tree_sitter::Node<'a>,
    &'b [u8],
    NodeKind,
    &str,
    Option<&str>,
) -> Option<TestKind>;
/// Detect a call-based test/group (`describe`/`it`/`test`, Dart
/// `test`/`group`). `None` for any non-test call.
type CallTestFn = for<'a, 'b> fn(tree_sitter::Node<'a>, &'b [u8]) -> Option<CallTestHit<'a>>;
/// Discover source roots from the full relative-path file set (Python
/// `src/`-layout, Java source dirs, …). Empty for languages that resolve
/// imports without one.
type SrcRootsFn = fn(&[String]) -> Vec<String>;
/// Resolve one raw import target to a repo-relative file path so the
/// `Imports` edge connects file → file. `None` drops the edge (external
/// dependency / unresolved).
type ResolveImportFn = fn(&str, &str, &[String], &[String]) -> Option<String>;
/// Extract outbound call / reference identifiers from a callable's body
/// node. Each pair is `(name, kind)` where `name` may be a `::`-qualified
/// path (Rust `Type::assoc`). Returns empty for languages that have not
/// opted into the heuristic call resolver. Default [`no_call_idents`].
type CallIdentsFn = for<'a, 'b> fn(tree_sitter::Node<'a>, &'b [u8]) -> Vec<(String, RefKind)>;
/// Decide whether a spec owns a discovered file by *content*, used only to
/// split an extension shared between dialects (`.h` is claimed by both C and
/// C++). Receives `(relative_path, head_of_file)` and returns whether THIS
/// spec should parse it. `None` on [`LangSpec::claims_path`] = own every file
/// whose extension matches (the default for every single-dialect language).
type PathClaimFn = fn(&str, &str) -> bool;

/// Heuristic: does this header text carry C++-only constructs? Used to route a
/// `.h` file (claimed by both C and C++) to the right grammar. Keyed on signals
/// the C grammar can never produce — scope resolution `::`, `namespace`,
/// `template<…>`, access-specifier labels, and `class …` declarations — so a
/// plain C header (structs / typedefs / functions only) reads as C while a
/// header-only C++ library reads as C++.
pub(crate) fn looks_like_cpp(head: &str) -> bool {
    head.contains("::")
        || head.contains("namespace ")
        || head.contains("template<")
        || head.contains("template <")
        || head.contains("public:")
        || head.contains("private:")
        || head.contains("protected:")
        || head.contains("class ")
        // `extern "C"` / `extern "C++"` linkage is C++-only syntax. tree-sitter's
        // C grammar cannot parse the wrapping block and drops every declaration
        // inside it, whereas the C++ grammar handles the linkage block *and* the
        // C declarations within. So an `extern "C"`-guarded `.h` (the universal
        // dual-use header idiom) must be routed to the C++ parser.
        || head.contains("extern \"C")
}

/// For a `type_definition` that is `typedef struct/union/enum { … } Name;` — an
/// **anonymous** record named only through the typedef — return the inner
/// specifier's kind (`"struct_specifier"` / `"union_specifier"` /
/// `"enum_specifier"`). tree-sitter parses the record as a *nameless* specifier
/// (which the driver drops for lack of a name), leaving the real name on the
/// typedef declarator. Shared by the C and C++ specs so the dominant C struct
/// idiom enters the graph under its typedef name.
///
/// Returns `None` for a *named* record typedef (`typedef struct Node {…} Node;`,
/// where the inner named specifier already emits it — avoiding a duplicate) and
/// for plain alias typedefs (`typedef int MyInt;`, which name no record).
pub(crate) fn anon_typedef_record_specifier(node: tree_sitter::Node<'_>) -> Option<&'static str> {
    let ty = node.child_by_field_name("type")?;
    if ty.child_by_field_name("body").is_none() || ty.child_by_field_name("name").is_some() {
        return None;
    }
    match ty.kind() {
        k @ ("struct_specifier" | "union_specifier" | "enum_specifier") => Some(k),
        _ => None,
    }
}

/// The name a typedef'd anonymous record borrows from its typedef declarator
/// (`typedef struct { … } Point;` → `Point`). Shared by the C and C++ specs.
pub(crate) fn typedef_declarator_name(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    node.child_by_field_name("declarator")
        .and_then(|d| declarator_name(d, src))
}

/// The body to recurse into for a typedef'd anonymous record: its members live
/// one level down, under the inner specifier's `body`. Shared by C and C++.
pub(crate) fn typedef_record_body(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    node.child_by_field_name("type")
        .and_then(|t| t.child_by_field_name("body"))
}

/// Everything the generic driver needs to index one language.
pub struct LangSpec {
    /// Stable identifier used in artifact ids + the indexer name
    /// (`<language_id>_treesitter`). E.g. `rust`, `typescript`.
    pub language_id: &'static str,
    /// Returns the compiled tree-sitter grammar.
    pub grammar: fn() -> tree_sitter::Language,
    /// File extensions (without dot) this language owns.
    pub extensions: &'static [&'static str],
    /// Directory names to skip during discovery.
    pub skip_dirs: &'static [&'static str],
    /// Path separator for qualified names (`::` for Rust/C++, `.` else).
    pub separator: &'static str,
    /// NodeKind for a free function and for a method (callable nested in
    /// a `Type`).
    pub func_kind: NodeKind,
    pub method_kind: NodeKind,
    /// Classify a node as a type/module container (or `None`).
    pub container_of: ContainerFn,
    /// Is this node kind a callable declaration?
    pub is_callable_kind: KindBoolFn,
    /// Refine a callable's [`NodeKind`] from its concrete node (Java
    /// constructors, …). Default [`keep_callable_kind`] (identity).
    pub callable_kind_of: CallableKindFn,
    /// Given a callable's *signature* node, return the node whose source
    /// span should define the callable's line range. Default
    /// [`callable_node_is_span`] (`None` → measure the signature node
    /// itself, which is correct whenever the body is a child of the
    /// signature). Languages whose grammar splits the signature and the
    /// body into *sibling* nodes — Dart `class_member_definition` →
    /// `method_signature` + `function_body` — override this so a method's
    /// range covers its body (impact slicing keys off the body lines).
    pub callable_span_of: BodyFn,
    /// Rust-style `impl` block: returns the implemented type's bare name
    /// (the block is not itself a symbol; its body's callables attach to
    /// that type). `None` for non-impl nodes / other languages.
    pub impl_type_of: TextFn,
    /// Go-style method receiver: returns the receiver type's bare name so
    /// the method nests under it. `None` otherwise.
    pub receiver_type_of: TextFn,
    /// Returns the import target(s) if this node is an import (one node
    /// can declare several, e.g. Python `import a, b`).
    pub import_of: ImportsFn,
    /// Extracts the declared name from a definition node.
    pub name_of: TextFn,
    /// Finds the body node to recurse into (default: field `body`).
    pub body_of: BodyFn,
    /// "Transparent" wrappers to descend through without emitting a
    /// symbol (e.g. Python `decorated_definition`, TS `export_statement`).
    pub is_transparent_kind: KindBoolFn,
    /// Structured JSON metadata for a symbol (framework facts, …). Default
    /// [`no_text`] (no metadata).
    pub metadata_of: TextFn,
    /// Reclassify a declaration as a test. Default [`no_test_of`].
    pub test_of: TestOfFn,
    /// Detect call-based tests (`describe`/`it`). Default [`no_call_test`].
    pub call_test_of: CallTestFn,
    /// Discover import source roots. Default [`no_src_roots`].
    pub src_roots_of: SrcRootsFn,
    /// Resolve an import target to a repo-relative file. Default
    /// [`keep_raw_import`] (store the raw target verbatim).
    pub resolve_import: ResolveImportFn,
    /// Descend into a callable's body during the walk. Off by default so
    /// existing languages keep their behaviour (callable bodies are *not*
    /// re-walked, so local/nested declarations are not emitted). Dart turns
    /// this on so call-based tests (`test`/`group`) written inside the
    /// conventional `void main() { … }` harness are discovered.
    pub recurse_callables: bool,
    /// Pair with `recurse_callables`: when descending into a callable body,
    /// only emit a *nested* callable that carries framework metadata
    /// (`metadata_of` returns `Some`). Off by default, so Dart keeps
    /// emitting every nested declaration. Python turns this on so a FastAPI
    /// handler defined inside an app factory (`def create_app(): @app.get(…)
    /// def handler(): …`) becomes a real symbol while plain local closures
    /// stay invisible — the decorator is what makes the inner `def` an
    /// addressable entry point worth a node.
    pub emit_nested_callables_with_metadata_only: bool,
    /// Capture body-level call / reference identifiers for the heuristic
    /// call resolver. Default [`no_call_idents`] (a no-op: no references,
    /// no behaviour change). Languages that opt in (Rust) emit
    /// medium-confidence `Calls` / `References` edges resolved by name.
    pub call_idents_of: CallIdentsFn,
    /// Opt into **whole-module** name resolution for a flat-namespace
    /// language. Default `false` (same-file → imported-file only).
    ///
    /// Swift `import`s a *module*, not a file, so there are no file→file
    /// import edges to follow; without this, every cross-file call stays
    /// unresolved and the file graph collapses into one blob. When `true`,
    /// a bare name that resolves nowhere same-file/imported falls back to
    /// the *single* definition site anywhere in the indexed module — and
    /// only when that name maps to exactly one file, so ubiquitous method
    /// names (`viewDidLoad`…) defined in many files never link unrelated
    /// files together. Type/constructor names (usually unique) carry the
    /// signal that drives [`crate::feature_cluster`].
    pub module_scoped_resolution: bool,
    /// Descend into a callable node that the spec *declined to name* (so its
    /// nested symbols become reachable) instead of routing it straight to the
    /// reference collector. Default `false` — declined callables stay refs-only
    /// (Swift stored properties keep their type-position references attached to
    /// the enclosing scope). TypeScript turns this on so a named object module
    /// `const api = { login() {} }` reaches its methods: the declarator is
    /// callable-kind (for `const f = () => {}`) yet declines its name when the
    /// value is an object literal, which would otherwise strand the methods two
    /// levels down (`variable_declarator → object → method_definition`).
    pub recurse_declined_callables: bool,
    /// Content gate for an extension shared between dialects. `None` (the
    /// default) means "own every file whose extension matches". C and C++ set
    /// this to split the shared `.h`: C claims a header only when it does *not*
    /// look like C++, C++ claims it only when it does — so exactly one parser
    /// owns each header and nothing is double-indexed.
    pub claims_path: Option<PathClaimFn>,
    /// Merge `partial class` / `partial struct` halves across files
    /// (issues.md #125). When on, a bare name that resolves neither same-file
    /// nor via an imported file falls back to a module-wide lookup whose
    /// target shares the caller's owning-type prefix — the same-named partial
    /// class half in a companion file. C# turns this on; every other language
    /// leaves it off so unrelated same-named methods across files never link.
    pub partial_class_merge: bool,
}

/// Default [`LangSpec::call_idents_of`]: capture nothing. Languages that
/// have not built a body identifier extractor stay structural-only.
pub fn no_call_idents(_body: tree_sitter::Node<'_>, _source: &[u8]) -> Vec<(String, RefKind)> {
    Vec::new()
}

/// Extract one outbound `(callee_name, ref_kind)` pair from a single call /
/// constructor / reference site, or `None` to skip it. The node passed in is
/// the grammar-specific call site itself (`call_expression`,
/// `method_invocation`, `new_expression`, …), so each language reads whatever
/// field / child its grammar spells the callee with — see [`CallKind`].
pub(crate) type CallExtractFn =
    for<'a, 'b> fn(tree_sitter::Node<'a>, &'b [u8]) -> Option<(String, RefKind)>;

/// One shape of call site a tree-sitter adapter collects from a body: the
/// tree-sitter node kind, paired with the function that pulls the callee
/// name (and whether it is a [`RefKind::Call`] or [`RefKind::Reference`])
/// out of a node of that kind.
///
/// A language hands the shared [`collect_calls`] walker a `&[CallKind]`
/// slice; the recursion, the [`MAX_NESTING_DEPTH`] cap, and the unconditional
/// "always descend into every child" semantics live in that walker, written
/// and tested once. Adding a new call shape (a language's `new` form, a
/// type-position reference, …) is one more slice entry — not a fresh
/// hand-written recursion per adapter (issues.md #130).
#[derive(Clone, Copy)]
pub(crate) struct CallKind {
    /// The tree-sitter node kind this entry matches (e.g. `"call_expression"`).
    pub kind: &'static str,
    /// Pull `(name, ref_kind)` out of a node of [`CallKind::kind`], or `None`.
    pub extract: CallExtractFn,
}

/// Generic call / reference collector shared by every tree-sitter adapter's
/// [`LangSpec::call_idents_of`] hook (issues.md #130).
///
/// Walks `node`'s named children; for each child whose kind matches a
/// registered [`CallKind`] it runs that kind's extractor and pushes the
/// result, then **always** recurses into the child (`depth + 1`) so calls
/// nested in arguments / blocks / closures are still found. A hard
/// [`MAX_NESTING_DEPTH`] cap stops a pathologically deep input from blowing
/// the stack. Behaviour is byte-for-byte identical to the former per-language
/// `collect_<lang>_calls` recursions: the only thing that varied across them
/// was *which* kinds match and *how* the callee name is read — now expressed
/// as the `call_kinds` slice.
pub(crate) fn collect_calls(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    out: &mut Vec<(String, RefKind)>,
    depth: usize,
    call_kinds: &[CallKind],
) {
    if depth > MAX_NESTING_DEPTH {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        // A node matches at most one call kind: the kinds in a language's
        // slice are mutually exclusive grammar categories, so the first hit
        // is the only hit (mirrors the prior `match child.kind()` arms).
        for ck in call_kinds {
            if kind == ck.kind {
                if let Some((name, ref_kind)) = (ck.extract)(child, src) {
                    out.push((name, ref_kind));
                }
                break;
            }
        }
        collect_calls(child, src, out, depth + 1, call_kinds);
    }
}

/// Per-file parse budget. Clean source parses in single-digit milliseconds —
/// the 3.2 MB `checker.ts` (largest real-world source we've met) completes
/// well under 500 ms — while what blows past the budget is tree-sitter's
/// error-recovery on files that are not really code (compiler test fixtures
/// with intentional syntax errors — the TypeScript repo's `tests/cases`
/// pushed a structural index from ~3 min to 20+ min at no budget, and still
/// burned ~30% of the wall clock at 2 s). Measured on that repo, 500 ms vs
/// 2 s keeps 99.98% of symbols and cuts the index from 24.2 s to 16.4 s. A
/// timed-out file keeps its File node but yields no symbols; raise via
/// `GROUNDGRAPH_PARSE_BUDGET_MS` on slow machines if fixtures matter.
const DEFAULT_PARSE_BUDGET_MS: u64 = 500;
const PARSE_BUDGET_ENV: &str = "GROUNDGRAPH_PARSE_BUDGET_MS";
const TIMING_ENV: &str = "GROUNDGRAPH_TIMING";

fn parse_budget() -> std::time::Duration {
    let ms = std::env::var(PARSE_BUDGET_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_PARSE_BUDGET_MS);
    std::time::Duration::from_millis(ms)
}

/// Parse `source` with `spec`'s grammar and return its structural symbols
/// and imports. Total and panic-free: any parser failure yields an empty
/// scan rather than aborting the index run. Files whose parse exceeds the
/// budget yield an empty scan with [`Scan::parse_timed_out`] set so the
/// caller can surface a count.
pub fn extract(spec: &LangSpec, source: &str) -> Scan {
    let mut scan = Scan::default();
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&(spec.grammar)()).is_err() {
        return scan;
    }
    let bytes = source.as_bytes();
    let start = std::time::Instant::now();
    let budget = parse_budget();
    let mut on_progress = move |_state: &tree_sitter::ParseState| {
        if start.elapsed() > budget {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    let tree = parser.parse_with_options(
        &mut |offset, _point| {
            if offset < bytes.len() {
                &bytes[offset..]
            } else {
                &[]
            }
        },
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut on_progress)),
    );
    let Some(tree) = tree else {
        scan.parse_timed_out = true;
        return scan;
    };
    walk(
        tree.root_node(),
        bytes,
        spec,
        None,
        false,
        false,
        0,
        &mut scan,
    );
    scan
}

/// Map a source file to the byte stream the grammar should actually parse.
///
/// Most languages parse their file verbatim. Container formats whose code is
/// embedded in markup (today: Vue `.vue` SFCs) are reduced to just the
/// embedded code, with every other byte blanked so recovered spans still
/// index correctly into the original file. The single extension point keeps
/// the `LangSpec`s — and the generic file loop — unaware of any one format.
pub(crate) fn preprocess_source<'a>(rel_path: &str, source: &'a str) -> std::borrow::Cow<'a, str> {
    if rel_path.ends_with(".vue") {
        return std::borrow::Cow::Owned(vue_script_only(source));
    }
    // C/C++: neutralise an export macro between the `class`/`struct` keyword and
    // the type name (`class UTILS_PUBLIC Foo {…}`) so the grammar parses the
    // whole record — *with its members* — instead of mis-reading it as a
    // function. Offsets are preserved, so a `.h` claimed by C (no `class`) is a
    // no-op and never reallocates.
    if is_c_family_path(rel_path) {
        if let Some(blanked) = blank_cpp_export_macros(source) {
            return std::borrow::Cow::Owned(blanked);
        }
    }
    std::borrow::Cow::Borrowed(source)
}

/// Does this path belong to the C / C++ family (where an export macro can sit
/// between `class`/`struct` and the type name)?
fn is_c_family_path(rel_path: &str) -> bool {
    matches!(
        rel_path.rsplit('.').next(),
        Some("c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ipp")
    )
}

/// Blank an ALL-CAPS export macro wedged between a `class`/`struct` keyword and
/// the type name — `class UTILS_PUBLIC Foo {` → `class              Foo {`.
///
/// Each blanked byte becomes an ASCII space, so byte length and every newline
/// offset are preserved 1:1 and recovered spans still map onto the original
/// file. Returns `None` when nothing matched (the overwhelmingly common case),
/// so non-macro'd C/C++ files are never reallocated.
///
/// The pattern is intentionally narrow to avoid touching real code: a
/// word-boundaried `class`/`struct`, then an identifier that is **all upper-case
/// / digits / underscore** (the export-macro convention — real type names are
/// not), then **another identifier** (the true type name), then `{` or `:`
/// (definition / base-clause). A genuine `class Foo {` has only one identifier
/// and never matches; `enum class FOO_T {` has no second identifier and is left
/// alone.
pub(crate) fn blank_cpp_export_macros(source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let is_ident_start = |b: u8| b.is_ascii_alphabetic() || b == b'_';
    let skip_ws = |mut i: usize| {
        while i < n && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let ident_end = |mut i: usize| {
        while i < n && is_ident(bytes[i]) {
            i += 1;
        }
        i
    };

    let mut out: Option<Vec<u8>> = None;
    let mut i = 0usize;
    while i < n {
        // Match a word-boundaried `class` / `struct` keyword.
        let kw_len = if bytes[i..].starts_with(b"class") {
            5
        } else if bytes[i..].starts_with(b"struct") {
            6
        } else {
            0
        };
        let boundary_before = i == 0 || !is_ident(bytes[i - 1]);
        let boundary_after = i + kw_len < n && bytes[i + kw_len].is_ascii_whitespace();
        if kw_len == 0 || !boundary_before || !boundary_after {
            i += 1;
            continue;
        }
        // id1: the candidate macro.
        let id1_start = skip_ws(i + kw_len);
        if id1_start >= n || !is_ident_start(bytes[id1_start]) {
            i += kw_len;
            continue;
        }
        let id1_end = ident_end(id1_start);
        let id1 = &bytes[id1_start..id1_end];
        let is_macroish = id1.len() >= 2
            && id1
                .iter()
                .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            && id1.iter().any(|&b| b.is_ascii_uppercase());
        // id2: the real type name, which must exist.
        let id2_start = skip_ws(id1_end);
        let has_id2 = id2_start < n && is_ident_start(bytes[id2_start]);
        if !is_macroish || !has_id2 {
            i += kw_len;
            continue;
        }
        // After id2, the next significant byte must open a definition (`{`) or a
        // base-clause (`:`) — never a `(`/`;` (that would be a declaration).
        let after_id2 = skip_ws(ident_end(id2_start));
        if after_id2 >= n || (bytes[after_id2] != b'{' && bytes[after_id2] != b':') {
            i += kw_len;
            continue;
        }
        // Blank id1 in place (lazily allocate the owned copy on first hit).
        let buf = out.get_or_insert_with(|| bytes.to_vec());
        for b in &mut buf[id1_start..id1_end] {
            *b = b' ';
        }
        i = id1_end;
    }
    out.map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Blank everything outside `<script>…</script>` in a Vue SFC, preserving byte
/// length and newline positions so tree-sitter spans map 1:1 back onto the
/// original file. All `<script>` blocks (e.g. `<script>` + `<script setup>`)
/// are retained verbatim; `<template>`/`<style>` and any non-ASCII markup
/// collapse to ASCII spaces (each byte → one space) so the result stays valid
/// UTF-8 without shifting a single offset.
pub(crate) fn vue_script_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out: Vec<u8> = bytes
        .iter()
        .map(|&b| if b == b'\n' { b'\n' } else { b' ' })
        .collect();
    // ASCII-lowercased copy for case-insensitive tag scanning; `to_ascii_lowercase`
    // preserves byte length so indices stay aligned with `bytes`.
    let lower = source.to_ascii_lowercase();
    let lower = lower.as_bytes();
    let find = |hay: &[u8], needle: &[u8], from: usize| -> Option<usize> {
        if from >= hay.len() {
            return None;
        }
        hay[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| from + p)
    };
    let mut search = 0usize;
    while let Some(tag_start) = find(lower, b"<script", search) {
        let Some(gt) = find(lower, b">", tag_start) else {
            break;
        };
        let content_start = gt + 1;
        let Some(close) = find(lower, b"</script", content_start) else {
            break;
        };
        out[content_start..close].copy_from_slice(&bytes[content_start..close]);
        search = close + b"</script".len();
    }
    // Only original bytes (at their original offsets) and ASCII blanks are
    // present, so the buffer is guaranteed valid UTF-8.
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

#[allow(clippy::too_many_arguments)]
fn walk(
    container: tree_sitter::Node<'_>,
    source: &[u8],
    spec: &LangSpec,
    parent_qualified: Option<&str>,
    parent_is_type: bool,
    in_callable_body: bool,
    depth: usize,
    scan: &mut Scan,
) {
    if depth > MAX_NESTING_DEPTH {
        return;
    }
    let mut cursor = container.walk();
    for child in container.named_children(&mut cursor) {
        let kind = child.kind();

        // 1. Imports (one node may declare several targets).
        let imports = (spec.import_of)(child, source);
        if !imports.is_empty() {
            for path in imports {
                if !path.is_empty() {
                    scan.imports.push(ScannedImport { path });
                }
            }
            continue;
        }

        // 2. Impl-like blocks: not a symbol; body attaches to a type.
        if let Some(type_name) = (spec.impl_type_of)(child, source) {
            let nested = combine(parent_qualified, &type_name, spec.separator);
            if let Some(body) = (spec.body_of)(child) {
                walk(
                    body,
                    source,
                    spec,
                    Some(&nested),
                    true,
                    false,
                    depth + 1,
                    scan,
                );
            }
            continue;
        }

        // 3. Type / module containers.
        if let Some(sym) = (spec.container_of)(child, source) {
            // Inside a callable body, a language may suppress nested
            // declarations that are not framework entry points (e.g. a Python
            // local class defined in a function). Those were never reachable
            // before `recurse_callables`, so skipping keeps prior behaviour.
            if in_callable_body && spec.emit_nested_callables_with_metadata_only {
                continue;
            }
            if let Some(name) = (spec.name_of)(child, source) {
                let qualified = combine(parent_qualified, &name, spec.separator);
                match (spec.test_of)(child, source, sym.node_kind(), &name, parent_qualified) {
                    Some(role) => {
                        push_test(scan, role, &name, &qualified, parent_qualified, child);
                    }
                    None => {
                        let metadata = (spec.metadata_of)(child, source);
                        push_symbol(
                            scan,
                            sym.node_kind(),
                            &name,
                            &qualified,
                            parent_qualified,
                            child,
                            metadata,
                        );
                    }
                }
                if let Some(body) = (spec.body_of)(child) {
                    walk(
                        body,
                        source,
                        spec,
                        Some(&qualified),
                        sym.is_type(),
                        false,
                        depth + 1,
                        scan,
                    );
                }
            }
            continue;
        }

        // 4. Callables (function / method).
        if (spec.is_callable_kind)(kind) {
            if let Some(name) = (spec.name_of)(child, source) {
                let (eff_parent, eff_is_type) = match (spec.receiver_type_of)(child, source) {
                    Some(recv) => (Some(combine(parent_qualified, &recv, spec.separator)), true),
                    None => (parent_qualified.map(str::to_string), parent_is_type),
                };
                let base_kind = if eff_is_type {
                    spec.method_kind
                } else {
                    spec.func_kind
                };
                let kind = (spec.callable_kind_of)(child, source, base_kind);
                let qualified = combine(eff_parent.as_deref(), &name, spec.separator);
                // Hooks inspect the *signature* node (`child`); the line
                // range is taken from the (possibly wider) span node so a
                // method whose body is a grammar sibling still covers it.
                let span = (spec.callable_span_of)(child).unwrap_or(child);
                let metadata = (spec.metadata_of)(child, source);
                // A nested callable is only a real symbol when the language
                // descends into bodies *and* either accepts every nested
                // declaration (Dart) or this one carries framework metadata
                // (Python: a decorated FastAPI handler vs. a plain closure).
                let emit = !in_callable_body
                    || !spec.emit_nested_callables_with_metadata_only
                    || metadata.is_some();
                if emit {
                    match (spec.test_of)(child, source, kind, &name, eff_parent.as_deref()) {
                        Some(role) => {
                            push_test(scan, role, &name, &qualified, eff_parent.as_deref(), span);
                        }
                        None => {
                            push_symbol(
                                scan,
                                kind,
                                &name,
                                &qualified,
                                eff_parent.as_deref(),
                                span,
                                metadata,
                            );
                        }
                    }
                    // Heuristic call resolver: capture outbound call / reference
                    // identifiers from this callable's body, keyed by its
                    // qualified name. Done for tests too so a test seeds
                    // reachability into the code it exercises. No-op (empty) for
                    // languages that have not opted in via `call_idents_of`.
                    if let Some(body) = (spec.body_of)(child) {
                        // Share one Arc<str> per callable (#160): every
                        // body-level call ident used to deep-clone the
                        // qualified String; an Arc clone is allocation-free.
                        let from_qualified = Arc::<str>::from(qualified.as_str());
                        for (to_name, ref_kind) in (spec.call_idents_of)(body, source) {
                            if !to_name.is_empty() {
                                scan.references.push(ScannedRef {
                                    from_qualified: Arc::clone(&from_qualified),
                                    to_name,
                                    kind: ref_kind,
                                });
                            }
                        }
                    }
                }
                // Optional: descend into the callable body so call-based tests
                // hosted inside a function (Dart's `void main() { test(…); }`)
                // and decorated nested handlers (Python app factories) are
                // discovered. The body's nodes attach to *this callable's*
                // parent (file / module), not to the callable, matching how the
                // Dart analyzer files top-level `test(...)` nodes under the file.
                if spec.recurse_callables {
                    if let Some(body) = (spec.body_of)(child) {
                        walk(
                            body,
                            source,
                            spec,
                            parent_qualified,
                            false,
                            true,
                            depth + 1,
                            scan,
                        );
                    }
                }
                continue;
            }
            // A callable node the spec declined to name — e.g. a Swift *stored*
            // property routed here so *computed* properties can be emitted (see
            // `swift_name_of`). It is not a symbol; fall through to the general
            // reference collector (section 6) so its type-position references
            // stay attached to the enclosing scope instead of being dropped.
            //
            // Opt-in: a language may instead ask to *descend* into the declined
            // callable so nested symbols become reachable. TypeScript uses this
            // for `const api = { login() {} }` — the declarator declines its name
            // (object value) but its methods, one transparent `object` level
            // down, are real callables. Recursion stays in the enclosing scope
            // and still collects the children's references via section 6.
            if spec.recurse_declined_callables {
                walk(
                    child,
                    source,
                    spec,
                    parent_qualified,
                    parent_is_type,
                    in_callable_body,
                    depth + 1,
                    scan,
                );
                continue;
            }
        }

        // 4b. Call-based tests (`describe`/`it`, Dart `test`/`group`).
        if let Some(hit) = (spec.call_test_of)(child, source) {
            if !hit.name.is_empty() {
                let qualified = combine(parent_qualified, &hit.name, spec.separator);
                push_test(
                    scan,
                    hit.kind,
                    &hit.name,
                    &qualified,
                    parent_qualified,
                    child,
                );
                if let Some(body) = hit.body {
                    walk(
                        body,
                        source,
                        spec,
                        Some(&qualified),
                        false,
                        false,
                        depth + 1,
                        scan,
                    );
                }
                continue;
            }
        }

        // 5. Transparent wrappers — descend without emitting.
        if (spec.is_transparent_kind)(kind) {
            walk(
                child,
                source,
                spec,
                parent_qualified,
                parent_is_type,
                in_callable_body,
                depth + 1,
                scan,
            );
            continue;
        }

        // 6. Everything else at this scope is a plain statement, not a
        // symbol: module-level registrations (`FACTORS = [_amihud(20)]`),
        // class-body field initializers (`handler = _dispatch()`), top-level
        // expression statements, etc. These never become nodes, but they can
        // *reference* callables, and missing those references is the largest
        // source of dead-code false positives for heuristic languages. Capture
        // the call / reference identifiers and attribute them to the enclosing
        // scope — the containing type for a class body, or the file itself for
        // module scope (empty `from_qualified`, resolved to the file node in
        // `resolve_heuristic_refs`). No-op for languages that have not opted
        // into `call_idents_of`.
        for (to_name, ref_kind) in (spec.call_idents_of)(child, source) {
            if !to_name.is_empty() {
                scan.references.push(ScannedRef {
                    from_qualified: Arc::from(parent_qualified.unwrap_or_default()),
                    to_name,
                    kind: ref_kind,
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_symbol(
    scan: &mut Scan,
    kind: NodeKind,
    name: &str,
    qualified: &str,
    parent_qualified: Option<&str>,
    node: tree_sitter::Node<'_>,
    metadata: Option<String>,
) {
    if name.is_empty() {
        return;
    }
    scan.symbols.push(ScannedSymbol {
        kind,
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        parent_qualified_name: parent_qualified.map(str::to_string),
        start_line: line_no(node.start_position().row),
        end_line: line_no(node.end_position().row),
        metadata,
    });
}

fn push_test(
    scan: &mut Scan,
    kind: TestKind,
    name: &str,
    qualified: &str,
    parent_qualified: Option<&str>,
    node: tree_sitter::Node<'_>,
) {
    if name.is_empty() {
        return;
    }
    scan.tests.push(ScannedTest {
        kind,
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        parent_qualified_name: parent_qualified.map(str::to_string),
        start_line: line_no(node.start_position().row),
        end_line: line_no(node.end_position().row),
    });
}

fn combine(parent: Option<&str>, name: &str, sep: &str) -> String {
    match parent {
        Some(p) => format!("{p}{sep}{name}"),
        None => name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers usable by individual `LangSpec`s.
// ---------------------------------------------------------------------------

/// Text of a node, or `None` if it is not valid UTF-8.
pub fn node_text<'a>(node: tree_sitter::Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

/// Default name extractor: the `name` field, non-empty.
pub fn name_from_field(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| node_text(n, source))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Default body finder: the `body` field.
pub fn body_from_field(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    node.child_by_field_name("body")
}

/// Default `callable_span_of`: the callable signature node already spans
/// its body (the common case), so no override is needed.
pub fn callable_node_is_span(_node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    None
}

/// `None`-returning text hook (for specs that don't use a given slot).
pub fn no_text(_node: tree_sitter::Node<'_>, _source: &[u8]) -> Option<String> {
    None
}

/// Empty import hook (for specs handled entirely elsewhere).
pub fn no_imports(_node: tree_sitter::Node<'_>, _source: &[u8]) -> Vec<String> {
    Vec::new()
}

/// `None`-returning container hook.
pub fn no_container(_node: tree_sitter::Node<'_>, _source: &[u8]) -> Option<SymKind> {
    None
}

/// Always-false kind predicate.
pub fn never(_kind: &str) -> bool {
    false
}

/// Default `callable_kind_of`: keep whatever kind the driver chose.
pub fn keep_callable_kind(
    _node: tree_sitter::Node<'_>,
    _source: &[u8],
    default: NodeKind,
) -> NodeKind {
    default
}

/// Default `test_of`: nothing is a test.
pub fn no_test_of(
    _node: tree_sitter::Node<'_>,
    _source: &[u8],
    _kind: NodeKind,
    _name: &str,
    _parent: Option<&str>,
) -> Option<TestKind> {
    None
}

/// Default `call_test_of`: no call-based tests.
pub fn no_call_test<'a>(_node: tree_sitter::Node<'a>, _source: &[u8]) -> Option<CallTestHit<'a>> {
    None
}

/// Default `src_roots_of`: no special source roots.
pub fn no_src_roots(_files: &[String]) -> Vec<String> {
    Vec::new()
}

/// Default `resolve_import`: keep the raw target verbatim (the historical
/// behaviour — the `Imports` edge points at `file::<raw-target>`).
pub fn keep_raw_import(
    raw: &str,
    _from_file: &str,
    _all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Expand a Java `import a.b.c.*;` wildcard to the repo files that are *direct*
/// members of package `a.b.c` (any `…/a/b/c/<Name>.java`). Used for name
/// resolution scope only — not for file→file import edges. Returns empty for a
/// non-wildcard or JDK/third-party package with no indexed files.
fn java_wildcard_package_files(raw: &str, all_files: &[String]) -> Vec<String> {
    let dotted = raw.trim().trim_end_matches(';').trim();
    let Some(pkg) = dotted.strip_suffix(".*") else {
        return Vec::new();
    };
    let pkg = pkg.trim_end_matches('.').trim();
    if pkg.is_empty() {
        return Vec::new();
    }
    let pkgdir = pkg.replace('.', "/");
    let mut out = Vec::new();
    for f in all_files {
        let Some(slash) = f.rfind('/') else {
            continue;
        };
        let (parent, name) = (&f[..slash], &f[slash + 1..]);
        // Direct member of the package dir (source-root agnostic: the package
        // path is a suffix of the file's parent dir, or the whole parent for a
        // root-level layout), and a Java source file.
        if name.ends_with(".java") && (parent == pkgdir || parent.ends_with(&format!("/{pkgdir}")))
        {
            out.push(f.clone());
        }
    }
    out
}

/// Expand a resolved Go import (a single *representative* package file, e.g.
/// `internal/repo/admin.go`) to every `.go` file that is a direct member of
/// that package directory. A Go package's symbols are spread across all its
/// files, but [`go_resolve_import`](crate::go_treesitter) collapses the package
/// to one representative; a bare cross-package call (`s.repo.GetX()` collects
/// `GetX`) may target a method defined in any sibling file. Used for the name-
/// resolution scope only — the file→file `ImportEdge` keeps the single
/// representative, so the file graph and import stats are unchanged. Returns the
/// package's files (including the representative); empty when the target has no
/// parent dir.
fn go_package_sibling_files(target: &str, all_files: &[String]) -> Vec<String> {
    let Some(slash) = target.rfind('/') else {
        // Root-level package: members are the root-level `.go` files.
        return all_files
            .iter()
            .filter(|f| !f.contains('/') && f.ends_with(".go"))
            .cloned()
            .collect();
    };
    let pkgdir = &target[..slash];
    all_files
        .iter()
        .filter(|f| {
            f.ends_with(".go")
                && matches!(f.rsplit_once('/'), Some((parent, _)) if parent == pkgdir)
        })
        .cloned()
        .collect()
}

/// Collapse `.`/`..` segments in a path into a clean repo-relative string
/// using `/` separators. Leading `..` that would escape the root simply
/// clears the accumulator (we never emit paths above the repo root).
pub fn canonicalize_rel_path(path: &std::path::Path) -> String {
    use std::path::Component;
    let mut canonical = String::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(idx) = canonical.rfind('/') {
                    canonical.truncate(idx);
                } else {
                    canonical.clear();
                }
            }
            Component::Normal(part) => {
                if !canonical.is_empty() {
                    canonical.push('/');
                }
                canonical.push_str(&part.to_string_lossy());
            }
            _ => {}
        }
    }
    canonical
}

/// Resolve a C/C++ `#include` target to a repo-relative file, or `None`
/// (system header / not vendored in-repo / ambiguous) so the `Imports` edge
/// is dropped rather than left dangling — the same correctness bar the Rust
/// and TypeScript resolvers hold. The angle-vs-quote distinction is already
/// erased upstream by `strip_quotes`, so resolution is purely by existence:
///   1. relative to the including file's directory (`"util/x.h"`, `"../a.h"`);
///   2. as a repo-root-relative path;
///   3. a unique file whose path ends with the include's written sub-path
///      (covers project headers reached via an `-I <dir>` include root).
pub fn resolve_c_include(
    raw: &str,
    from_file: &str,
    all_files: &[String],
    _src_roots: &[String],
) -> Option<String> {
    let spec = raw.trim().replace('\\', "/");
    if spec.is_empty() {
        return None;
    }
    // (1) Relative to the including file's directory.
    let source_dir = std::path::Path::new(from_file)
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let rel = canonicalize_rel_path(&source_dir.join(&spec));
    if !rel.is_empty() && all_files.iter().any(|f| f == &rel) {
        return Some(rel);
    }
    // (2) Repo-root-relative.
    let rooted = canonicalize_rel_path(std::path::Path::new(&spec));
    if !rooted.is_empty() && all_files.iter().any(|f| f == &rooted) {
        return Some(rooted);
    }
    // (3) Unique suffix match for `-I`-rooted project headers.
    let needle = format!("/{}", spec.trim_start_matches("./"));
    let mut hits = all_files.iter().filter(|f| f.ends_with(&needle));
    let first = hits.next()?;
    if hits.next().is_none() {
        return Some(first.clone());
    }
    None
}

/// Reduce a possibly-generic, possibly-scoped type reference to its bare
/// name: `Vec<T>` → `Vec`, `crate::a::Foo<'x>` → `Foo`, `*T` → `T`.
pub fn simple_type_name(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let text = node_text(node, source)?;
    let before_generics = text.split('<').next().unwrap_or(text);
    let bare = before_generics
        .rsplit("::")
        .next()
        .unwrap_or(before_generics)
        .trim()
        .trim_start_matches(['*', '&', ' ']);
    let bare = bare.trim();
    if bare.is_empty() {
        None
    } else {
        Some(bare.to_string())
    }
}

/// Collapse internal whitespace and strip a trailing `;` — used by import
/// hooks so the stored target is stable regardless of source formatting.
pub fn normalise_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().trim_end_matches(';').trim().to_string()
}

fn line_no(row: usize) -> u32 {
    u32::try_from(row).unwrap_or(u32::MAX).saturating_add(1)
}

/// Strip one layer of surrounding quotes / angle brackets from an import
/// literal: `"foo.h"` → `foo.h`, `<stdio.h>` → `stdio.h`, `'pkg'` → `pkg`.
pub fn strip_quotes(text: &str) -> String {
    let t = text.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        let paired = matches!(
            (first, last),
            (b'"', b'"') | (b'\'', b'\'') | (b'<', b'>') | (b'`', b'`')
        );
        if paired {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// Resolve the declared name of a C/C++ `function_definition` by walking
/// down its `declarator` chain (pointers, parentheses, references) to the
/// innermost identifier. `qualified_identifier` (`Foo::bar`) collapses to
/// its last component. Bounded so a malformed declarator can't loop.
pub fn declarator_name(declarator: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut cur = declarator;
    for _ in 0..64 {
        match cur.kind() {
            "identifier"
            | "field_identifier"
            | "type_identifier"
            | "namespace_identifier"
            | "destructor_name"
            | "operator_name"
            | "operator_cast" => {
                return node_text(cur, source)
                    .map(str::to_string)
                    .filter(|s| !s.is_empty());
            }
            "qualified_identifier" | "template_function" | "template_type" => {
                if let Some(name) = cur.child_by_field_name("name") {
                    cur = name;
                    continue;
                }
                return simple_type_name(cur, source);
            }
            _ => {
                if let Some(inner) = cur.child_by_field_name("declarator") {
                    cur = inner;
                    continue;
                }
                match cur.named_child(0) {
                    Some(child) => cur = child,
                    None => return None,
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Generic repo indexer.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct TsIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    /// Extra repo-relative paths to widen the *import-resolution* universe
    /// beyond the files this spec actually parses. Lets a sibling spec
    /// (e.g. TypeScript ↔ TSX) resolve cross-extension imports without the
    /// driver having to parse the other grammar. Empty for every language
    /// that resolves within its own extension set.
    pub resolution_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TsIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub imports: usize,
    /// Test cases + groups recovered (pytest, JUnit, `describe`/`it`, …).
    #[serde(default)]
    pub tests: usize,
    /// Medium-confidence heuristic `Calls` / `References` edges resolved by
    /// the in-process call resolver (Rust today). 0 for languages that rely
    /// on an LSP / analyzer sidecar for semantic edges.
    #[serde(default)]
    pub references: usize,
    /// Files whose parse blew the per-file budget (error-recovery on
    /// fixture/corpus files) and were skipped. Surfaced as a CLI warning.
    #[serde(default)]
    pub parse_timeouts: usize,
    /// Files skipped because they exceeded `crate::source_text::MAX_INDEX_FILE_BYTES`
    /// — a capacity gate (read before the time gate) bounding peak memory.
    #[serde(default)]
    pub skipped_oversized: usize,
    /// `<language_id>_treesitter` when anything was produced.
    pub resolver_used: String,
}

/// The indexer name a given language writes under.
pub fn indexer_name(spec: &LangSpec) -> String {
    format!("{}_treesitter", spec.language_id)
}

/// Every language the in-process tree-sitter breadth backend supports.
/// Order is the canonical render / iteration order.
pub const SUPPORTED_LANGUAGES: &[&str] = &[
    "rust",
    "typescript",
    "python",
    "go",
    "java",
    "swift",
    "c",
    "cpp",
    "csharp",
    "ruby",
    "php",
    "kotlin",
];

/// Resolve a configured language id (with a few common aliases) to its
/// static [`LangSpec`]. Returns `None` for anything unrecognised so the
/// engine can skip it without aborting the whole run.
pub fn spec_for_language(language_id: &str) -> Option<&'static LangSpec> {
    match language_id.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => Some(&crate::rust_treesitter::RUST_SPEC),
        "typescript" | "ts" | "javascript" | "js" => {
            Some(&crate::typescript_treesitter::TYPESCRIPT_SPEC)
        }
        "python" | "py" => Some(&crate::python_treesitter::PYTHON_SPEC),
        "go" | "golang" => Some(&crate::go_treesitter::GO_SPEC),
        "java" => Some(&crate::java_treesitter::JAVA_SPEC),
        "swift" => Some(&crate::swift_treesitter::SWIFT_SPEC),
        "c" => Some(&crate::c_treesitter::C_SPEC),
        "cpp" | "c++" | "cxx" => Some(&crate::cpp_treesitter::CPP_SPEC),
        "csharp" | "c#" | "cs" => Some(&crate::csharp_treesitter::CSHARP_SPEC),
        "ruby" | "rb" => Some(&crate::ruby_treesitter::RUBY_SPEC),
        "php" => Some(&crate::php_treesitter::PHP_SPEC),
        "kotlin" | "kt" => Some(&crate::kotlin_treesitter::KOTLIN_SPEC),
        _ => None,
    }
}

/// Deferred heuristic-resolution inputs handed back by
/// [`index_repo_with_spec_collect`] so a multi-dialect adapter (TypeScript's
/// `.ts` + `.tsx` passes) can resolve body identifiers against the *union* of
/// every pass's symbols instead of each pass in isolation. Holds owned strings
/// because the originating batch is consumed (ingested) before resolution.
#[derive(Default)]
pub(crate) struct RefResolutionInputs {
    /// `(path, name, qualified_name)` for every emitted symbol.
    pub symbols: Vec<(String, String, String)>,
    /// Per-file resolved import target paths.
    pub import_targets: std::collections::HashMap<String, Vec<String>>,
    /// Captured body references awaiting resolution.
    pub pending: Vec<(String, ScannedRef)>,
}

/// Discover → parse → ingest one language across a repo. Reused by every
/// per-language `index_*` wrapper and by the unified engine pass. Resolves
/// heuristic call/reference edges inline against this single pass's symbols.
pub fn index_repo_with_spec(
    store: &mut Store,
    spec: &LangSpec,
    options: &TsIndexOptions,
) -> Result<TsIndexResult> {
    let (result, _inputs) = index_repo_with_spec_impl(store, spec, options, true)?;
    Ok(result)
}

/// Like [`index_repo_with_spec`] but *defers* heuristic call/reference
/// resolution: structure (symbols / imports / tests) is still ingested, while
/// the resolution inputs are returned so the caller can resolve them against a
/// larger symbol universe (e.g. the TS adapter's merged `.ts` + `.tsx` set).
pub(crate) fn index_repo_with_spec_collect(
    store: &mut Store,
    spec: &LangSpec,
    options: &TsIndexOptions,
) -> Result<(TsIndexResult, RefResolutionInputs)> {
    index_repo_with_spec_impl(store, spec, options, false)
}

fn index_repo_with_spec_impl(
    store: &mut Store,
    spec: &LangSpec,
    options: &TsIndexOptions,
    resolve_inline: bool,
) -> Result<(TsIndexResult, RefResolutionInputs)> {
    let timing = std::env::var_os(TIMING_ENV).is_some();
    let mut t = std::time::Instant::now();
    let mut mark = move |phase: &str, lang: &str| {
        if timing {
            eprintln!(
                "[timing]   {lang}/{phase}: {:.2}s",
                t.elapsed().as_secs_f64()
            );
        }
        t = std::time::Instant::now();
    };
    let files = discover_files(
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
        spec.extensions,
        spec.skip_dirs,
        spec.claims_path,
    )?;
    mark("discover", spec.language_id);
    if files.is_empty() {
        return Ok((TsIndexResult::default(), RefResolutionInputs::default()));
    }

    let mut result = TsIndexResult::default();
    let mut batch = LanguageIndexBatch {
        language: spec.language_id.into(),
        ..Default::default()
    };

    // Resolution context: the full relative-path set (plus any caller-
    // supplied cross-extension paths) and any import source roots a
    // language wants (Python `src/`-layout, …). Computed once so per-file
    // import resolution stays O(files) overall.
    let mut all_files: Vec<String> = files.iter().map(|f| f.relative.clone()).collect();
    if !options.resolution_paths.is_empty() {
        all_files.extend(options.resolution_paths.iter().cloned());
        all_files.sort();
        all_files.dedup();
    }
    let src_roots = (spec.src_roots_of)(&all_files);

    // Heuristic call-resolver state. Populated only when a language opts in
    // via `call_idents_of` (today: Rust); empty + zero extra work for every
    // other language, whose `scanned.references` is always empty.
    let mut pending_refs: Vec<(String, ScannedRef)> = Vec::new();
    let mut import_targets: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    // Parse in parallel, merge serially — as a *pipeline*, not in chunked
    // lock-step. The old chunked design (parse 256, then merge 256) made
    // every worker wait at the chunk barrier for the slowest parse; on repos
    // whose fixtures trigger heavy error-recovery (TypeScript `tests/cases`)
    // the profile showed workers parked in cvwait longer than they parsed.
    // Here workers stream results through a bounded channel while the
    // caller's thread merges concurrently; the channel bound keeps peak
    // memory in check (only ~1k sources alive at once).
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut parse_timeouts: usize = 0;
    let skipped_oversized = AtomicUsize::new(0);
    let skipped_ref = &skipped_oversized;
    let files_ref: &[DiscoveredFile] = &files;
    let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, String, Scan)>(1024);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            files_ref
                .par_iter()
                .enumerate()
                .for_each_with(tx, |tx, (i, file)| {
                    // Capacity gate before the time gate: never read a file
                    // past the size budget into memory.
                    if crate::source_text::is_oversized_source(&file.absolute) {
                        skipped_ref.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    let Ok(source) = std::fs::read_to_string(&file.absolute) else {
                        return;
                    };
                    // Container formats (Vue SFCs) wrap parseable code in a
                    // `<script>` block; hand the grammar only that region
                    // (offsets preserved) so the rest of the file —
                    // `<template>` HTML, `<style>` CSS — never reaches the
                    // parser.
                    let parse_source = preprocess_source(&file.relative, &source);
                    let scanned = extract(spec, &parse_source);
                    let _ = tx.send((i, source, scanned));
                });
        });
        for (i, source, scanned) in rx.iter() {
            let file = &files[i];
            if scanned.parse_timed_out {
                parse_timeouts += 1;
            }
            result.files += 1;

            let file_artifact_id = file_id(&file.relative);
            batch.files.push(FileArtifact {
                id: file_artifact_id.clone(),
                path: file.relative.clone(),
                language: spec.language_id.into(),
                content_hash: sha256_hex(source.as_bytes()),
            });

            // Only link to a parent we actually emitted in *this* file so we
            // never create a dangling `contains` edge. Tests can parent onto
            // either a structural symbol (JUnit `@Test` in a class) or another
            // test (a pytest method in a `Test*` group), so both sets count.
            let emitted: std::collections::BTreeSet<&str> = scanned
                .symbols
                .iter()
                .map(|s| s.qualified_name.as_str())
                .collect();
            let emitted_tests: std::collections::BTreeSet<&str> = scanned
                .tests
                .iter()
                .map(|t| t.qualified_name.as_str())
                .collect();
            let parent_id = |p: &str| -> Option<ArtifactId> {
                (emitted.contains(p) || emitted_tests.contains(p))
                    .then(|| symbol_id(spec, &file.relative, p))
            };

            for sym in &scanned.symbols {
                let id = symbol_id(spec, &file.relative, &sym.qualified_name);
                let parent_symbol_id = sym.parent_qualified_name.as_deref().and_then(parent_id);
                batch.symbols.push(SymbolArtifact {
                    id,
                    kind: sym.kind,
                    path: file.relative.clone(),
                    name: sym.name.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    start_line: sym.start_line,
                    end_line: sym.end_line,
                    parent_symbol_id,
                    metadata_json: sym.metadata.clone(),
                });
                result.symbols += 1;
            }

            for t in &scanned.tests {
                let id = symbol_id(spec, &file.relative, &t.qualified_name);
                let parent_symbol_id = t.parent_qualified_name.as_deref().and_then(parent_id);
                batch.tests.push(TestArtifact {
                    id,
                    kind: t.kind.node_kind(),
                    path: file.relative.clone(),
                    name: t.name.clone(),
                    start_line: t.start_line,
                    end_line: t.end_line,
                    parent_symbol_id,
                });
                result.tests += 1;
            }

            for imp in &scanned.imports {
                if let Some(target) =
                    (spec.resolve_import)(&imp.path, &file.relative, &all_files, &src_roots)
                {
                    if !scanned.references.is_empty() {
                        let entry = import_targets.entry(file.relative.clone()).or_default();
                        if spec.language_id == "go" {
                            // A Go import names a *package* (directory); the resolved
                            // representative is one of its files. Feed every sibling
                            // file of that package into the resolution scope so a
                            // bare cross-package call reaches a method defined in any
                            // of them (resolution only — the ImportEdge below keeps
                            // the single representative, so import stats are unchanged).
                            entry.extend(go_package_sibling_files(&target, &all_files));
                        } else {
                            entry.push(target.clone());
                        }
                    }
                    batch.imports.push(ImportEdge {
                        from_file: file_artifact_id.clone(),
                        to_path: target,
                    });
                    result.imports += 1;
                } else if spec.language_id == "java" && !scanned.references.is_empty() {
                    // `import a.b.c.*;` — a wildcard package import resolves to no
                    // single file, but the package's symbols ARE in scope: a bare
                    // call (`baseMapper.selectX()` collects `selectX`) must reach a
                    // method defined in any `…/a/b/c/<Name>.java`. Feed those files
                    // into name resolution ONLY (no file→file ImportEdge, so the
                    // file graph and import stats stay byte-identical to before).
                    for target in java_wildcard_package_files(&imp.path, &all_files) {
                        import_targets
                            .entry(file.relative.clone())
                            .or_default()
                            .push(target);
                    }
                }
            }

            for r in &scanned.references {
                pending_refs.push((file.relative.clone(), r.clone()));
            }
        }
    });
    mark("parse+merge", spec.language_id);
    if parse_timeouts > 0 {
        result.parse_timeouts = parse_timeouts;
    }
    result.skipped_oversized = skipped_oversized.load(Ordering::Relaxed);

    // Resolve captured body identifiers to concrete in-repo symbols and
    // append medium-confidence Calls / References edges. The ingestion path
    // tiers these as `medium` (indexer name is `<lang>_treesitter`, neither
    // `_lsp` nor `dart_analyzer`), so they enrich `callers` / `dead-code` /
    // `impact` without ever claiming compiler-grade certainty.
    //
    // When `resolve_inline` is false the caller resolves later against a wider
    // symbol set (TS merges `.ts` + `.tsx`); we only ingest structure here and
    // hand the inputs back.
    let mut inputs = RefResolutionInputs::default();
    if resolve_inline {
        if !pending_refs.is_empty() {
            let view: Vec<(&str, &str, &str)> = batch
                .symbols
                .iter()
                .map(|s| (s.path.as_str(), s.name.as_str(), s.qualified_name.as_str()))
                .collect();
            let edges = resolve_heuristic_refs(spec, &view, &import_targets, &pending_refs);
            result.references = edges.len();
            batch.references.extend(edges);
        }
    } else {
        inputs.symbols = batch
            .symbols
            .iter()
            .map(|s| (s.path.clone(), s.name.clone(), s.qualified_name.clone()))
            .collect();
        inputs.import_targets = import_targets;
        inputs.pending = pending_refs;
    }

    mark("resolve", spec.language_id);

    if result.files > 0 {
        let name = indexer_name(spec);
        ingest_language_batch_minimal(store, &batch, &name)
            .with_context(|| format!("ingesting {} tree-sitter batch", spec.language_id))?;
        result.resolver_used = name;
    }
    mark("ingest", spec.language_id);
    Ok((result, inputs))
}

/// Upper bound on resolved targets emitted for a single captured
/// identifier. Bounds the blast radius of a name that happens to match
/// many symbols (e.g. a ubiquitous `new`) so the resolver can never
/// quadratically explode the edge set.
const MAX_REF_TARGETS: usize = 16;

/// Fan-in cap for [`LangSpec::module_scoped_resolution`]: a uniquely-named
/// type referenced (module-wide) by more distinct files than this is treated
/// as cross-cutting infrastructure (a base class / Theme / Router) rather
/// than a feature boundary, and is *not* linked — otherwise every file would
/// couple to it and the file graph would collapse into one community.
pub(crate) const MODULE_HUB_FANIN_CAP: usize = 32;

/// Resolve heuristic body identifiers (`pending`) to concrete symbol ids
/// using only in-batch facts: a name → qualified-name index over the
/// emitted symbols, and the per-file resolved import targets.
///
/// Resolution order, by construction conservative:
/// 1. A bare `name` resolves to a same-file symbol of that name; failing
///    that, a symbol of that name in a directly-imported file.
/// 2. A `Type::assoc` path resolves only when `Type` is a *local* type in
///    the same / imported file — external paths (`HashMap::new`) are
///    dropped so they never mislink to an unrelated local `new`. When it
///    resolves we also emit a `References` edge to the `Type` itself.
pub(crate) fn resolve_heuristic_refs(
    spec: &LangSpec,
    symbols: &[(&str, &str, &str)],
    import_targets: &std::collections::HashMap<String, Vec<String>>,
    pending: &[(String, ScannedRef)],
) -> Vec<ReferenceEdge> {
    use std::collections::{HashMap, HashSet};

    // file -> (simple name -> [qualified name]) over every emitted symbol.
    // The `(path, name, qualified)` view lets the same resolver run over a
    // single in-memory batch (generic path) or the union of several passes
    // (the TS adapter merges `.ts` + `.tsx` symbols before resolving).
    let mut by_file: HashMap<&str, HashMap<&str, Vec<&str>>> = HashMap::new();
    for (path, name, qualified) in symbols {
        by_file
            .entry(path)
            .or_default()
            .entry(name)
            .or_default()
            .push(qualified);
    }

    // Whole-module name index for flat-namespace languages (Swift): simple
    // name → its definition sites `(file, qualified)`. Only consulted when
    // same-file / imported-file resolution fails *and* the name maps to a
    // single file, so unique types/constructors link across files while
    // ubiquitous method names stay local.
    let module_index: HashMap<&str, Vec<(&str, &str)>> =
        if spec.module_scoped_resolution || spec.partial_class_merge {
            let mut idx: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
            for (path, name, qualified) in symbols {
                idx.entry(name).or_default().push((path, qualified));
            }
            idx
        } else {
            HashMap::new()
        };
    // Pre-pass: names whose module-wide fan-in (distinct referencing files)
    // exceeds the cap are infrastructure hubs and excluded from module-wide
    // resolution (see [`MODULE_HUB_FANIN_CAP`]).
    let module_hubs: HashSet<&str> = if spec.module_scoped_resolution {
        let mut fanin: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (file, r) in pending {
            if !r.to_name.contains(spec.separator) {
                fanin
                    .entry(r.to_name.as_str())
                    .or_default()
                    .insert(file.as_str());
            }
        }
        fanin
            .into_iter()
            .filter(|(_, files)| files.len() > MODULE_HUB_FANIN_CAP)
            .map(|(name, _)| name)
            .collect()
    } else {
        HashSet::new()
    };

    let sep = spec.separator;
    let mut out: Vec<ReferenceEdge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (file, r) in pending {
        // An empty `from_qualified` marks a module/file-scope reference
        // (a top-level statement that belongs to no symbol). Anchor it on
        // the file node so dead-code reachability can follow it once the
        // file is proven reachable.
        let from_id = if r.from_qualified.is_empty() {
            file_id(file)
        } else {
            symbol_id(spec, file, &r.from_qualified[..])
        };
        // (target_file, target_qualified, edge_kind)
        let mut targets: Vec<(&str, String, EdgeKind)> = Vec::new();

        if r.to_name.contains(sep) {
            let parts: Vec<&str> = r.to_name.split(sep).filter(|p| !p.is_empty()).collect();
            if parts.len() >= 2 {
                // `parts.len() >= 2` (checked above) guarantees both ends; the
                // `expect` messages document that invariant in place (#206).
                let head = *parts.first().expect("parts.len() >= 2 verified above");
                let leaf = *parts.last().expect("parts.len() >= 2 verified above");
                let want = format!("{head}{sep}{leaf}");
                let mut search: Vec<&str> = vec![file.as_str()];
                if let Some(t) = import_targets.get(file) {
                    search.extend(t.iter().map(String::as_str));
                }
                for f in search {
                    let Some(byname) = by_file.get(f) else {
                        continue;
                    };
                    // `head` must be a local type in this file, otherwise the
                    // path is external (std / third-party) and we drop it.
                    if !byname.get(head).is_some_and(|q| q.contains(&head)) {
                        continue;
                    }
                    if byname
                        .get(leaf)
                        .is_some_and(|q| q.iter().any(|q| *q == want))
                    {
                        targets.push((f, want.clone(), r.kind.edge_kind()));
                    }
                    // Keep the owning type reachable too.
                    targets.push((f, head.to_string(), EdgeKind::References));
                }
            }
        } else {
            let name = r.to_name.as_str();
            if let Some(q) = by_file.get(file.as_str()).and_then(|m| m.get(name)) {
                for qn in q {
                    // Skip a self-match (`*qn == from_qualified`): a bare call
                    // whose name equals the caller's own qualified name is either
                    // self-recursion (whose self edge is dropped at emit anyway)
                    // or a `recv.Name()` delegation where the receiver field
                    // shares the method's name. Pushing the self target here would
                    // wrongly suppress the import-target fallback below and lose
                    // the real cross-package edge.
                    if *qn == &r.from_qualified[..] {
                        continue;
                    }
                    targets.push((file.as_str(), (*qn).to_string(), r.kind.edge_kind()));
                }
            }
            if targets.is_empty() {
                if let Some(tfiles) = import_targets.get(file) {
                    for tf in tfiles {
                        if let Some(q) = by_file.get(tf.as_str()).and_then(|m| m.get(name)) {
                            for qn in q {
                                targets.push((tf.as_str(), (*qn).to_string(), r.kind.edge_kind()));
                            }
                        }
                    }
                }
            }
            // Flat-namespace fallback (Swift): resolve module-wide, but only
            // for a name that is (1) project-shaped — PascalCase (a type or
            // constructor) or a multi-word lowerCamel method
            // (`checkFirstLaunchIntroduction`); single generic words (`save`,
            // `update`) collide with stdlib/UIKit and stay local; (2) defined
            // in exactly one *other* file — unique; and (3) not a high fan-in
            // hub. These together keep the edges feature-shaped instead of
            // gluing every file to shared infra.
            if targets.is_empty()
                && spec.module_scoped_resolution
                && (name.chars().next().is_some_and(char::is_uppercase)
                    || crate::source_text::is_multi_word_identifier(name))
                && !module_hubs.contains(name)
            {
                if let Some(defs) = module_index.get(name) {
                    let distinct_files: HashSet<&str> = defs.iter().map(|(f, _)| *f).collect();
                    if distinct_files.len() == 1 {
                        let tf = *distinct_files
                            .iter()
                            .next()
                            .expect("len == 1 verified above");
                        if tf != file.as_str() {
                            for (_, qn) in defs {
                                targets.push((tf, (*qn).to_string(), r.kind.edge_kind()));
                            }
                        }
                    }
                }
            }
            // C# partial-class merge (issues.md #125): a bare name unresolved
            // same-file / via `using` falls back to the partial companion — a
            // same-named class half in another file. Gate on the owning-type
            // prefix matching the caller's, so unrelated same-named methods in
            // different classes never link (only a partial peer qualifies).
            if targets.is_empty() && spec.partial_class_merge {
                let from_owner = r.from_qualified.split(sep).next().unwrap_or("");
                if !from_owner.is_empty() {
                    if let Some(defs) = module_index.get(name) {
                        for (tf, qn) in defs {
                            let target_owner = qn.split(sep).next().unwrap_or("");
                            if target_owner == from_owner && *tf != file.as_str() {
                                targets.push((*tf, (*qn).to_string(), r.kind.edge_kind()));
                            }
                        }
                    }
                }
            }
        }

        // Dedup BEFORE applying MAX_REF_TARGETS so the cap counts *unique*
        // targets. Otherwise duplicate (file, qualified-name) entries — e.g.
        // several imports resolving to the same definition — fill the budget
        // and starve real edges past index MAX_REF_TARGETS (#124).
        let mut unique_targets = 0usize;
        let mut local_seen: HashSet<String> = HashSet::new();
        for (tf, tq, kind) in targets {
            let to_id = symbol_id(spec, tf, &tq);
            if to_id == from_id {
                continue;
            }
            if !local_seen.insert(format!("{to_id}\u{1}{kind:?}")) {
                continue;
            }
            unique_targets += 1;
            if unique_targets > MAX_REF_TARGETS {
                break;
            }
            let dedup = format!("{from_id}\u{1}{to_id}\u{1}{kind:?}");
            if !seen.insert(dedup) {
                continue;
            }
            out.push(ReferenceEdge {
                from_symbol_id: from_id.clone(),
                to_symbol_id: to_id,
                kind,
                source_file: file.clone(),
                line: 0,
                snippet: String::new(),
                resolver: format!("{}_treesitter_heuristic", spec.language_id),
            });
        }
    }

    out
}

/// `<language_id>::<file-relative-path>::<qualified-name>` — file-scoped
/// so two same-named items in different files never collide without full
/// cross-file resolution.
fn symbol_id(spec: &LangSpec, file_rel: &str, qualified: &str) -> ArtifactId {
    ArtifactId::new(format!("{}::{file_rel}::{qualified}", spec.language_id))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for b in out {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

#[derive(Debug, Clone)]
struct DiscoveredFile {
    relative: String,
    absolute: PathBuf,
}

/// Discover repo-relative paths for the given extensions, reusing the
/// same walk / skip / exclude rules as the parser. Useful for building a
/// cross-extension import-resolution universe (see
/// [`TsIndexOptions::resolution_paths`]).
pub fn discover_relative_paths(
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
    extensions: &[&str],
    skip_dirs: &[&str],
) -> Result<Vec<String>> {
    Ok(discover_files(
        repo_root,
        code_roots,
        exclude_globs,
        extensions,
        skip_dirs,
        None,
    )?
    .into_iter()
    .map(|f| f.relative)
    .collect())
}

/// How many leading bytes of a file a [`PathClaimFn`] sniffs. Generous enough
/// to clear license headers / includes and reach the first declaration, small
/// enough to stay cheap.
const CLAIM_SNIFF_BYTES: usize = 8192;

/// Read up to `max` bytes from a file as lossy UTF-8. Returns an empty string
/// on any I/O error — a file we cannot read claims nothing, which keeps
/// discovery total and panic-free.
fn read_head(path: &Path, max: usize) -> String {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = vec![0u8; max];
    match f.read(&mut buf) {
        Ok(n) => String::from_utf8_lossy(&buf[..n]).into_owned(),
        Err(_) => String::new(),
    }
}

fn discover_files(
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
    extensions: &[&str],
    skip_dirs: &[&str],
    claims_path: Option<PathClaimFn>,
) -> Result<Vec<DiscoveredFile>> {
    let mut out: Vec<DiscoveredFile> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    // Compile the (constant) exclude globs once instead of re-collecting each
    // pattern's chars for every candidate file (#142).
    let exclude = crate::lsp_indexer::ExcludeGlobs::compile(exclude_globs);
    // Borrow `code_roots` directly; only synthesise a default `["."]` when the
    // caller passed none, avoiding a clone of the whole root list (#159).
    let default_root = [PathBuf::from(".")];
    let roots: &[PathBuf] = if code_roots.is_empty() {
        &default_root
    } else {
        code_roots
    };
    for root in roots {
        let abs = repo_root.join(root);
        if !abs.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&abs)
            .into_iter()
            .filter_entry(|e| !is_skip_dir(e, skip_dirs))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if !extensions.contains(&ext) {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if exclude.matches(&rel) {
                continue;
            }
            // Content gate for dialects that share an extension (C/C++ `.h`):
            // sniff the file head and let the spec disown a file it shouldn't
            // parse. Only invoked when a spec opts in, so single-dialect
            // languages pay no extra I/O.
            if let Some(claims) = claims_path {
                let head = read_head(path, CLAIM_SNIFF_BYTES);
                if !claims(&rel, &head) {
                    continue;
                }
            }
            if !seen.insert(rel.clone()) {
                continue;
            }
            out.push(DiscoveredFile {
                relative: rel,
                absolute: repo_root.join(path.strip_prefix(repo_root).unwrap_or(path)),
            });
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

/// Directories that are never first-party source for *any* language and must
/// be pruned by every walk: VCS metadata, agent worktrees (`.claude` holds full
/// repo copies that would duplicate every symbol), vendored dependencies, and
/// Python virtualenvs / installed packages / caches (an installed FastAPI's
/// docstring examples would otherwise be parsed as real code). Shared by the
/// tree-sitter discovery and the schema indexer so both see one file universe
/// — the single source of truth for "noise" directories. Per-language
/// `LangSpec::skip_dirs` adds only build-output dirs specific to that toolchain.
pub const ALWAYS_SKIP_DIRS: &[&str] = &[
    // VCS / tooling / agent worktrees
    ".git",
    ".hg",
    ".svn",
    ".claude",
    // Vendored dependencies / build caches (non-hidden names)
    "node_modules",
    "vendor",
    "Pods",
    "Carthage",
    "DerivedData",
    ".dart_tool",
    // Go toolchain convention (adopted industry-wide): `testdata/` holds
    // fixtures — often intentionally malformed or generated source — that
    // the compiler itself ignores. Indexing them floods dead-code/search.
    // `test_data` is the same convention with an underscore (rust-analyzer's
    // parser fixtures: ~470 deliberately broken `.rs` files).
    "testdata",
    "test_data",
    // Python virtualenvs / installed packages / caches
    ".venv",
    "venv",
    "site-packages",
    "__pycache__",
    ".tox",
    ".eggs",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

fn is_skip_dir(entry: &walkdir::DirEntry, skip_dirs: &[&str]) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    // Hidden directories (below the root) are tooling / build / cache output —
    // DerivedData variants (`.derivedData-codex`), SwiftPM `.build`/`.swiftpm`,
    // `.venv`, `.gradle`, `.idea` … — never first-party source, exactly the
    // default ripgrep / `ignore` apply. The depth>0 guard keeps a repository
    // whose own root happens to be hidden (cloned into `.foo`) indexable.
    if entry.depth() > 0 && name.starts_with('.') {
        return true;
    }
    // An embedded git repository (a sub-dir holding its own `.git/` dir, below
    // the root) is a *different project* — vendored upstreams, reference clones
    // — whose files git does not even track here. Prune the whole subtree so its
    // symbols never masquerade as first-party. Only a `.git` *directory* counts:
    // a submodule's `.git` *file* (a gitlink the parent declares) stays indexed.
    if entry.depth() > 0 && entry.path().join(".git").is_dir() {
        return true;
    }
    ALWAYS_SKIP_DIRS.contains(&name) || skip_dirs.contains(&name)
}

// ---------------------------------------------------------------------------
// P23.0 — generic driver capability tests.
//
// These exercise the new `metadata_of` / `test_of` / `call_test_of` /
// `resolve_import` / `src_roots_of` hooks against *throwaway* specs built by
// functionally updating a real production spec, so the machinery is proven
// once here and every language only has to wire its hook in its own phase.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod driver_capability_tests {
    use super::*;

    #[test]
    fn collect_calls_walks_registered_kinds_and_always_descends() {
        // The generic call walker (issues.md #130) must, for every named
        // child: run the extractor of the first registered CallKind whose
        // `kind` matches, push its result, and *always* recurse so a call
        // nested in another call's arguments is still found. We drive it
        // against the Rust grammar with a throwaway CallKind so the test
        // stays independent of any per-adapter migration.
        fn extract_call(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<(String, RefKind)> {
            node.child_by_field_name("function")
                .and_then(|f| node_text(f, src))
                .map(|n| (n.to_string(), RefKind::Call))
        }
        let kinds = &[CallKind {
            kind: "call_expression",
            extract: extract_call,
        }];
        let src = "fn body() { foo(bar()); }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let body = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .child_by_field_name("body")
            .unwrap();
        let mut out = Vec::new();
        collect_calls(body, src.as_bytes(), &mut out, 0, kinds);
        let names: Vec<String> = out.iter().map(|(n, _)| n.clone()).collect();
        // `bar()` nests inside `foo()`'s argument list — finding both proves
        // the walker descends unconditionally rather than stopping at the
        // outer call.
        assert!(
            names.contains(&"foo".to_string()) && names.contains(&"bar".to_string()),
            "expected foo + bar (always-descend), got {names:?}"
        );
        // Past the nesting cap the walker must be a no-op: the first thing it
        // does is bail when `depth > MAX_NESTING_DEPTH`.
        let mut capped = Vec::new();
        collect_calls(
            body,
            src.as_bytes(),
            &mut capped,
            MAX_NESTING_DEPTH + 1,
            kinds,
        );
        assert!(
            capped.is_empty(),
            "depth > MAX_NESTING_DEPTH must stop the walk immediately"
        );
    }

    #[test]
    fn collect_calls_with_empty_kinds_is_a_clean_noop() {
        // A brand-new language wires no call kinds yet: the walker still runs
        // (and recurses), but with no extractor to fire it collects nothing —
        // the safe default that keeps an unwired language silent.
        let src = "fn body() { foo(bar()); }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let body = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .child_by_field_name("body")
            .unwrap();
        let mut out = Vec::new();
        collect_calls(body, src.as_bytes(), &mut out, 0, &[]);
        assert!(out.is_empty(), "no registered kinds ⇒ no captures");
    }

    #[test]
    fn oversized_source_files_are_skipped() {
        use groundgraph_store::Store;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Small.java"), "class Small { void run() {} }").unwrap();
        // A file past the size budget must be skipped *before* it is read
        // into memory (rayon reads one file per core — an accidental giant
        // generated/vendored file would otherwise risk OOM).
        std::fs::write(
            root.join("Big.java"),
            vec![b'x'; usize::try_from(crate::source_text::MAX_INDEX_FILE_BYTES + 1).unwrap()],
        )
        .unwrap();
        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let result = index_repo_with_spec(
            &mut store,
            &crate::java_treesitter::JAVA_SPEC,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec![],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        )
        .unwrap();
        assert_eq!(result.skipped_oversized, 1, "Big.java must be skipped");
        assert_eq!(result.files, 1, "only Small.java is parsed");
    }

    #[test]
    fn supported_languages_matches_spec_for_language() {
        // `SUPPORTED_LANGUAGES` is the public "languages the engine parses"
        // enumeration. It must stay in lock-step with `spec_for_language`,
        // otherwise downstream code (CLI listings, docs, capability probes)
        // silently hides languages the engine actually supports.
        let mut listed: Vec<&str> = SUPPORTED_LANGUAGES.to_vec();
        listed.sort_unstable();
        let mut want = vec![
            "rust",
            "typescript",
            "python",
            "go",
            "java",
            "swift",
            "c",
            "cpp",
            "csharp",
            "ruby",
            "php",
            "kotlin",
        ];
        want.sort_unstable();
        assert_eq!(
            listed, want,
            "SUPPORTED_LANGUAGES drifted from the set spec_for_language resolves"
        );
        // Every listed id resolves, and its canonical id round-trips.
        for lang in SUPPORTED_LANGUAGES {
            let spec = spec_for_language(lang).unwrap_or_else(|| {
                panic!("SUPPORTED_LANGUAGES lists `{lang}` but spec_for_language returns None")
            });
            assert_eq!(spec.language_id, *lang, "canonical id mismatch for {lang}");
        }
    }

    // --- A Python spec wired with declaration-based test detection,
    //     metadata, and src-root import resolution. ---

    fn cap_test_of(
        _node: tree_sitter::Node<'_>,
        _src: &[u8],
        kind: NodeKind,
        name: &str,
        parent: Option<&str>,
    ) -> Option<TestKind> {
        if kind == NodeKind::PythonClass && name.starts_with("Test") && parent.is_none() {
            return Some(TestKind::Group);
        }
        if matches!(kind, NodeKind::PythonFunction | NodeKind::PythonMethod)
            && name.starts_with("test_")
        {
            if kind == NodeKind::PythonMethod {
                let in_group = parent
                    .and_then(|p| p.rsplit('.').next())
                    .map(|tail| tail.starts_with("Test"))
                    .unwrap_or(false);
                return in_group.then_some(TestKind::Case);
            }
            return Some(TestKind::Case);
        }
        None
    }

    fn cap_metadata(node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<String> {
        (node.kind() == "class_definition").then(|| r#"{"tag":"py"}"#.to_string())
    }

    fn cap_src_roots(_files: &[String]) -> Vec<String> {
        vec![String::new()]
    }

    fn cap_resolve(
        raw: &str,
        _from_file: &str,
        all_files: &[String],
        src_roots: &[String],
    ) -> Option<String> {
        let rel = raw.trim_start_matches('.').replace('.', "/");
        if rel.is_empty() {
            return None;
        }
        for root in src_roots {
            let cand = if root.is_empty() {
                format!("{rel}.py")
            } else {
                format!("{root}/{rel}.py")
            };
            if all_files.iter().any(|f| f == &cand) {
                return Some(cand);
            }
        }
        None
    }

    fn py_cap_spec() -> LangSpec {
        LangSpec {
            test_of: cap_test_of,
            metadata_of: cap_metadata,
            src_roots_of: cap_src_roots,
            resolve_import: cap_resolve,
            ..crate::python_treesitter::PYTHON_SPEC
        }
    }

    #[test]
    fn vue_script_only_keeps_script_blanks_markup_and_preserves_offsets() {
        let src = "<template>\n  <div>{{ 你好 }}</div>\n</template>\n\n<script>\nexport default { greet() { return 1 } }\n</script>\n\n<style>\n.a { color: red }\n</style>\n";
        let out = super::vue_script_only(src);
        // Byte length and every newline offset are preserved 1:1.
        assert_eq!(out.len(), src.len(), "byte length must be preserved");
        let nl = |s: &str| -> Vec<usize> {
            s.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i)
                .collect()
        };
        assert_eq!(nl(&out), nl(src), "newline offsets must be preserved");
        // Script body survives; template/style markup is gone.
        assert!(out.contains("export default { greet() { return 1 } }"));
        assert!(!out.contains("<div>"));
        assert!(!out.contains("color: red"));
        assert!(!out.contains("你好"));
        // The retained script sits at its original byte offset.
        let at = src.find("export default").unwrap();
        assert_eq!(&out[at..at + "export default".len()], "export default");
        // The blanked, valid-UTF-8 result parses as JS with the TSX grammar and
        // the object's shorthand method is reachable via object/pair descent.
        let scan = extract(&crate::typescript_treesitter::TSX_SPEC, &out);
        assert!(
            scan.symbols.iter().any(|s| s.name == "greet"),
            "the script's object shorthand method should be reachable: {:?}",
            scan.symbols
        );
    }

    #[test]
    fn blank_cpp_export_macros_preserves_offsets_and_is_narrow() {
        // Hits: macro between keyword and name, with `{` and with a base clause.
        let src = "class UTILS_PUBLIC Foo {};\nstruct A_B2 Bar : Base {};\n";
        let out = super::blank_cpp_export_macros(src).expect("a macro must be blanked");
        assert_eq!(out.len(), src.len(), "byte length must be preserved");
        assert_eq!(
            out.bytes().filter(|&b| b == b'\n').count(),
            src.bytes().filter(|&b| b == b'\n').count(),
            "newline count preserved"
        );
        assert!(
            out.contains("class              Foo {};"),
            "Foo macro blanked: {out:?}"
        );
        assert!(
            out.contains("struct      Bar : Base {};"),
            "Bar macro blanked: {out:?}"
        );

        // Misses (return None — never reallocate): plain class, single token,
        // `enum class` with an all-caps name, a macro'd forward declaration, and
        // an identifier that is not all-caps.
        for s in [
            "class Foo {};",
            "enum class FOO_T { A };",
            "class FOO_API Bar;",       // forward decl → no `{`/`:`
            "class Mixed_Case Bar {};", // id1 not all-caps
            "struct Plain { int x; };",
            "int subclass_count = 0;", // `class` only as a substring
        ] {
            assert!(
                super::blank_cpp_export_macros(s).is_none(),
                "must not rewrite: {s:?}"
            );
        }
    }

    #[test]
    fn test_of_reclassifies_declarations_and_keeps_normal_symbols() {
        let spec = py_cap_spec();
        let src = "\
class Service:
    def handle(self):
        pass

def test_top():
    pass

class TestThing:
    def test_one(self):
        pass
    def helper(self):
        pass
";
        let scan = extract(&spec, src);
        // Structural symbols: Service + Service.handle, plus the non-test
        // helper inside the test group (pytest collects only test_*).
        let sym_names: Vec<&str> = scan
            .symbols
            .iter()
            .map(|s| s.qualified_name.as_str())
            .collect();
        assert!(sym_names.contains(&"Service"), "{sym_names:?}");
        assert!(sym_names.contains(&"Service.handle"), "{sym_names:?}");
        assert!(
            !sym_names.contains(&"TestThing"),
            "Test* class must not be a structural symbol: {sym_names:?}"
        );
        assert!(
            !sym_names.contains(&"test_top"),
            "test_* function must not be a structural symbol: {sym_names:?}"
        );

        // Test nodes: function case, group, and the method case under it.
        let cases: Vec<&str> = scan
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Case)
            .map(|t| t.qualified_name.as_str())
            .collect();
        let groups: Vec<&str> = scan
            .tests
            .iter()
            .filter(|t| t.kind == TestKind::Group)
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert!(cases.contains(&"test_top"), "{cases:?}");
        assert!(groups.contains(&"TestThing"), "{groups:?}");
        assert!(cases.contains(&"TestThing.test_one"), "{cases:?}");
        let method_case = scan
            .tests
            .iter()
            .find(|t| t.qualified_name == "TestThing.test_one")
            .unwrap();
        assert_eq!(
            method_case.parent_qualified_name.as_deref(),
            Some("TestThing")
        );
    }

    #[test]
    fn metadata_of_attaches_json_to_symbols() {
        let spec = py_cap_spec();
        let scan = extract(&spec, "class Service:\n    pass\n");
        let service = scan
            .symbols
            .iter()
            .find(|s| s.qualified_name == "Service")
            .expect("Service present");
        assert_eq!(service.metadata.as_deref(), Some(r#"{"tag":"py"}"#));
    }

    // --- A TypeScript spec wired with call-based test detection. ---

    fn ts_call_test<'a>(node: tree_sitter::Node<'a>, src: &[u8]) -> Option<CallTestHit<'a>> {
        if node.kind() != "call_expression" {
            return None;
        }
        let func = node.child_by_field_name("function")?;
        let kind = match node_text(func, src)? {
            "describe" => TestKind::Group,
            "it" | "test" => TestKind::Case,
            _ => return None,
        };
        let args = node.child_by_field_name("arguments")?;
        let mut cursor = args.walk();
        let mut name = String::new();
        let mut body = None;
        for arg in args.named_children(&mut cursor) {
            match arg.kind() {
                "string" if name.is_empty() => {
                    if let Some(t) = node_text(arg, src) {
                        name = strip_quotes(t);
                    }
                }
                "arrow_function" | "function_expression" | "function" => {
                    body = arg.child_by_field_name("body");
                }
                _ => {}
            }
        }
        Some(CallTestHit { kind, name, body })
    }

    fn ts_cap_spec() -> LangSpec {
        LangSpec {
            call_test_of: ts_call_test,
            ..crate::typescript_treesitter::TYPESCRIPT_SPEC
        }
    }

    #[test]
    fn call_test_of_recovers_nested_describe_it_suites() {
        let spec = ts_cap_spec();
        let src = "\
describe('outer', () => {
  it('a', () => {});
  describe('inner', () => {
    it('b', () => {});
  });
});
";
        let scan = extract(&spec, src);
        let by_kind = |k: TestKind| -> Vec<String> {
            scan.tests
                .iter()
                .filter(|t| t.kind == k)
                .map(|t| t.qualified_name.clone())
                .collect()
        };
        let groups = by_kind(TestKind::Group);
        let cases = by_kind(TestKind::Case);
        assert!(groups.contains(&"outer".to_string()), "{groups:?}");
        assert!(groups.contains(&"outer.inner".to_string()), "{groups:?}");
        assert!(cases.contains(&"outer.a".to_string()), "{cases:?}");
        assert!(
            cases.contains(&"outer.inner.b".to_string()),
            "nested it() should qualify under its describe: {cases:?}"
        );
    }

    // Totality guard: specs wired with the new test / metadata / call hooks
    // must stay panic-free, deterministic, and emit well-formed tests on any
    // input (mirrors the per-language property tests for symbols).
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_hooks_are_total_and_deterministic(s in ".*") {
            let py = py_cap_spec();
            let ts = ts_cap_spec();
            prop_assert_eq!(extract(&py, &s), extract(&py, &s));
            prop_assert_eq!(extract(&ts, &s), extract(&ts, &s));
            for scan in [extract(&py, &s), extract(&ts, &s)] {
                for t in &scan.tests {
                    prop_assert!(!t.name.is_empty());
                    prop_assert!(!t.qualified_name.is_empty());
                    prop_assert!(t.end_line >= t.start_line);
                }
            }
        }
    }

    // --- End-to-end through the store: resolve_import + tests persist. ---

    #[test]
    fn index_repo_resolves_internal_imports_drops_external_and_persists_tests() {
        use groundgraph_core::artifact_id::file_id;
        use groundgraph_core::EdgeKind;
        use groundgraph_store::Store;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(root.join("pkg/util.py"), "def helper():\n    return 1\n").unwrap();
        std::fs::write(
            root.join("pkg/app.py"),
            "import os\nimport pkg.util\n\nclass Service:\n    pass\n\ndef test_app():\n    pass\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let spec = py_cap_spec();
        let result = index_repo_with_spec(
            &mut store,
            &spec,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec![],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        )
        .unwrap();

        assert_eq!(result.files, 2, "two python files discovered");
        assert_eq!(
            result.imports, 1,
            "only the internal pkg.util import resolves; os is dropped"
        );
        assert!(
            result.tests >= 1,
            "test_app should be collected: {result:?}"
        );

        // The resolved import edge connects file → file.
        let imports = store.list_edges_by_kind(EdgeKind::Imports).unwrap();
        let app = file_id("pkg/app.py");
        let util = file_id("pkg/util.py");
        assert!(
            imports.iter().any(|e| e.from_id == app && e.to_id == util),
            "expected pkg/app.py -> pkg/util.py import edge"
        );
        assert!(
            imports.iter().all(|e| e.to_id != file_id("os")),
            "external import os must not create a dangling node"
        );

        // The test case landed as a TestCase node; the class kept metadata.
        let nodes = store.list_all_nodes().unwrap();
        assert!(
            nodes
                .iter()
                .any(|n| n.kind == NodeKind::TestCase && n.name.as_deref() == Some("test_app")),
            "test_app should persist as a TestCase node"
        );
        let service = nodes
            .iter()
            .find(|n| n.kind == NodeKind::PythonClass && n.name.as_deref() == Some("Service"))
            .expect("Service node present");
        assert_eq!(service.metadata_json.as_deref(), Some(r#"{"tag":"py"}"#));
    }

    #[test]
    fn discovery_prunes_hidden_build_and_cache_dirs() {
        use groundgraph_store::Store;

        // A real iOS repo (invis) hid 2368 SwiftPM/DerivedData sources under a
        // `.derivedData-codex/` dir; the walker descended into it and indexed
        // ~20x the first-party code. Hidden directories (`.`-prefixed) are
        // tooling/build/cache output, never first-party source — pruned the same
        // way ripgrep/`ignore` do by default.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/app.py"), "def real():\n    return 1\n").unwrap();
        for hidden in [
            ".derivedData-codex/pkg",
            ".build/checkouts/dep",
            ".venv/lib",
        ] {
            std::fs::create_dir_all(root.join(hidden)).unwrap();
            std::fs::write(
                root.join(hidden).join("vendored.py"),
                "def vendored():\n    return 2\n",
            )
            .unwrap();
        }

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let spec = py_cap_spec();
        let result = index_repo_with_spec(
            &mut store,
            &spec,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec![],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        )
        .unwrap();

        assert_eq!(
            result.files, 1,
            "only src/app.py discovered; hidden build/cache dirs pruned: {result:?}"
        );
        let nodes = store.list_all_nodes().unwrap();
        assert!(
            nodes.iter().all(|n| n.name.as_deref() != Some("vendored")),
            "no symbol may come from a hidden dir"
        );
    }

    #[test]
    fn discovery_prunes_embedded_git_repositories() {
        use groundgraph_store::Store;

        // A real repo (nest) vendored two full upstream repos under
        // `references/<dep>/` — each keeping its own `.git/` directory. Git does
        // not track files inside an embedded repo, so they are not this repo's
        // source at all, yet the walker descended in and indexed ~1000 foreign
        // symbols as first-party. A subdirectory that *is itself* a git repo
        // (has a `.git/` dir) is a different project → prune its whole subtree.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The root being indexed is itself a repo — must stay indexable.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/app.py"), "def real():\n    return 1\n").unwrap();
        // An embedded upstream repo under references/dep/.
        std::fs::create_dir_all(root.join("references/dep/.git")).unwrap();
        std::fs::write(
            root.join("references/dep/vendored.py"),
            "def vendored():\n    return 2\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let spec = py_cap_spec();
        let result = index_repo_with_spec(
            &mut store,
            &spec,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec![],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        )
        .unwrap();

        assert_eq!(
            result.files, 1,
            "only src/app.py discovered; embedded git repo pruned: {result:?}"
        );
        let nodes = store.list_all_nodes().unwrap();
        assert!(
            nodes.iter().all(|n| n.name.as_deref() != Some("vendored")),
            "no symbol may come from an embedded git repo"
        );
    }

    // --- Java wildcard package imports (`import pkg.*;`) -------------------
    // MyBatis / Spring code wildcard-imports whole packages and calls the
    // imported symbols by bare name (`baseMapper.selectX()` collects `selectX`).
    // Dropping `pkg.*` left those calls unresolved, so an endpoint→table trace
    // dead-ended at the service impl. A wildcard now feeds every file under that
    // package into the file's name-resolution scope (resolution only — no
    // file→file import edge, so the file graph/import stats are unchanged).
    #[test]
    fn java_wildcard_package_import_resolves_called_symbol() {
        use groundgraph_core::EdgeKind;
        use groundgraph_store::Store;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("com/x/mapper")).unwrap();
        std::fs::create_dir_all(root.join("com/x/service")).unwrap();
        // Mapper interface lives in com.x.mapper; only wildcard-imported below.
        std::fs::write(
            root.join("com/x/mapper/CraftConflictMapper.java"),
            "package com.x.mapper;\npublic interface CraftConflictMapper {\n    int selectStyleConflicted(int id);\n}\n",
        )
        .unwrap();
        // Service impl wildcard-imports the mapper package and calls the method
        // by bare name (as `baseMapper.selectStyleConflicted(id)` would collect).
        std::fs::write(
            root.join("com/x/service/CraftConflictServiceImpl.java"),
            "package com.x.service;\nimport com.x.mapper.*;\npublic class CraftConflictServiceImpl {\n    public int selectStyleConflictById(int id) {\n        return selectStyleConflicted(id);\n    }\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        index_repo_with_spec(
            &mut store,
            &crate::java_treesitter::JAVA_SPEC,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec![],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        )
        .unwrap();

        let nodes = store.list_all_nodes().unwrap();
        let find = |name: &str| {
            nodes
                .iter()
                .find(|n| n.kind == NodeKind::JavaMethod && n.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("missing JavaMethod {name}: {nodes:?}"))
                .id
                .clone()
        };
        let impl_m = find("selectStyleConflictById");
        let mapper_m = find("selectStyleConflicted");
        let calls = store.list_edges_by_kind(EdgeKind::Calls).unwrap();
        assert!(
            calls
                .iter()
                .any(|e| e.from_id == impl_m && e.to_id == mapper_m),
            "wildcard `import com.x.mapper.*` must let the bare call \
             `selectStyleConflicted` resolve to the mapper method: {calls:?}"
        );
    }

    #[test]
    fn java_explicit_import_self_named_delegation_resolves_through_full_pass() {
        // Reproduces the real platform miss through the WHOLE pass (resolve_import
        // + import_targets + heuristic resolution), not a synthetic resolver call.
        // `StyleInfoController.selectMeasuresInfo` does
        // `return styleInfoService.selectMeasuresInfo(id)` — the called bare name
        // equals the caller's own method name AND lives behind an *explicit*
        // single-type import. The controller→interface Calls edge must exist so
        // `trace <route>` doesn't dead-end at the controller.
        use groundgraph_core::EdgeKind;
        use groundgraph_store::Store;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("com/kutesmart/cloud/style/controller")).unwrap();
        std::fs::create_dir_all(root.join("com/kutesmart/cloud/style/service")).unwrap();
        std::fs::write(
            root.join("com/kutesmart/cloud/style/service/IStyleInfoService.java"),
            "package com.kutesmart.cloud.style.service;\npublic interface IStyleInfoService {\n    java.util.List<Object> selectMeasuresInfo(Integer id);\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("com/kutesmart/cloud/style/controller/StyleInfoController.java"),
            "package com.kutesmart.cloud.style.controller;\nimport com.kutesmart.cloud.style.service.IStyleInfoService;\npublic class StyleInfoController {\n    private IStyleInfoService styleInfoService;\n    public java.util.List<Object> selectMeasuresInfo(Integer id) {\n        return styleInfoService.selectMeasuresInfo(id);\n    }\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        index_repo_with_spec(
            &mut store,
            &crate::java_treesitter::JAVA_SPEC,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec![],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        )
        .unwrap();

        let nodes = store.list_all_nodes().unwrap();
        let find = |suffix: &str| {
            nodes
                .iter()
                .find(|n| n.kind == NodeKind::JavaMethod && n.id.as_str().ends_with(suffix))
                .unwrap_or_else(|| panic!("missing JavaMethod {suffix}: {nodes:?}"))
                .id
                .clone()
        };
        let ctrl = find("StyleInfoController.selectMeasuresInfo");
        let svc = find("IStyleInfoService.selectMeasuresInfo");
        let calls = store.list_edges_by_kind(EdgeKind::Calls).unwrap();
        assert!(
            calls.iter().any(|e| e.from_id == ctrl && e.to_id == svc),
            "self-named delegation behind an explicit import must resolve \
             controller->interface: {calls:?}"
        );
    }

    // --- Go cross-package calls into non-representative package files -----
    // A Go `import "mod/pkg"` resolves to a single *representative* file of
    // the package directory, but the package's symbols are spread across all
    // its files. A bare cross-package call (`s.repo.GetOrCreateCode()` collects
    // `GetOrCreateCode`) must reach a method defined in ANY file of that
    // package — not only the representative. The whole package dir is fed into
    // the file's name-resolution scope (resolution only — the single
    // representative ImportEdge is unchanged, so file graph/import stats stay
    // byte-identical). Reproduces the Shift Go backend miss where every
    // handler→service→repo call dead-ended at the package boundary.
    #[test]
    fn go_cross_package_call_resolves_into_non_representative_file() {
        use groundgraph_core::EdgeKind;
        use groundgraph_store::Store;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        std::fs::create_dir_all(root.join("service")).unwrap();
        // `repo/aaa.go` sorts first, so it is the package representative; the
        // called method lives in `repo/referral.go`, which is NOT the rep.
        std::fs::write(
            root.join("repo/aaa.go"),
            "package repo\ntype Other struct{}\nfunc (o *Other) Misc() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("repo/referral.go"),
            "package repo\ntype ReferralRepo struct{}\nfunc (r *ReferralRepo) GetOrCreateCode() string { return \"\" }\n",
        )
        .unwrap();
        // Service imports the repo package and calls the method through a field
        // selector, exactly as the Shift backend does.
        std::fs::write(
            root.join("service/service.go"),
            "package service\nimport \"mymod/repo\"\ntype Service struct{ repo *repo.ReferralRepo }\nfunc (s *Service) Overview() string {\n\treturn s.repo.GetOrCreateCode()\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        index_repo_with_spec(
            &mut store,
            &crate::go_treesitter::GO_SPEC,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec![],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        )
        .unwrap();

        let nodes = store.list_all_nodes().unwrap();
        let find = |name: &str| {
            nodes
                .iter()
                .find(|n| n.kind == NodeKind::GoMethod && n.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("missing GoMethod {name}: {nodes:?}"))
                .id
                .clone()
        };
        let caller = find("Overview");
        let callee = find("GetOrCreateCode");
        let calls = store.list_edges_by_kind(EdgeKind::Calls).unwrap();
        assert!(
            calls
                .iter()
                .any(|e| e.from_id == caller && e.to_id == callee),
            "cross-package call `s.repo.GetOrCreateCode()` must resolve to the \
             method defined in the non-representative package file: {calls:?}"
        );
    }

    // --- Delegation across packages when names collide --------------------
    // A handler that delegates to a same-named service method
    // (`func (h *Handler) Handle() { return h.svc.Handle() }`) collects the bare
    // call `Handle`, which also matches the caller's OWN simple name. Same-file
    // resolution must not let that self-match shadow the imported cross-package
    // target: the self edge is dropped anyway, so the import-target fallback has
    // to still run. Reproduces the platform-go port miss where every
    // handler→service delegation (deliberately mirroring the Java method names)
    // produced no call edge.
    #[test]
    fn go_self_named_call_resolves_to_imported_method_not_dropped() {
        use groundgraph_core::EdgeKind;
        use groundgraph_store::Store;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("svc")).unwrap();
        std::fs::create_dir_all(root.join("handler")).unwrap();
        std::fs::write(
            root.join("svc/svc.go"),
            "package svc\ntype Selector struct{}\nfunc (s *Selector) Handle() string { return \"\" }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("handler/handler.go"),
            "package handler\nimport \"mymod/svc\"\ntype Handler struct{ sel *svc.Selector }\nfunc (h *Handler) Handle() string {\n\treturn h.sel.Handle()\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        index_repo_with_spec(
            &mut store,
            &crate::go_treesitter::GO_SPEC,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec![],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        )
        .unwrap();

        let nodes = store.list_all_nodes().unwrap();
        let by_id = |sub: &str| {
            nodes
                .iter()
                .find(|n| n.id.as_str().ends_with(sub))
                .unwrap_or_else(|| panic!("missing {sub}: {nodes:?}"))
                .id
                .clone()
        };
        let caller = by_id("handler/handler.go::Handler.Handle");
        let callee = by_id("svc/svc.go::Selector.Handle");
        let calls = store.list_edges_by_kind(EdgeKind::Calls).unwrap();
        assert!(
            calls
                .iter()
                .any(|e| e.from_id == caller && e.to_id == callee),
            "delegation `h.sel.Handle()` must resolve to the imported service \
             method even though the call name collides with the caller's own \
             name: {calls:?}"
        );
    }

    // --- Module-scoped (flat-namespace) name resolution -------------------
    // Languages whose `import`s name a *module*, not a file (Swift), have no
    // file→file import edges, so same-file + imported-file resolution leaves
    // every cross-file call unresolved and the file graph degenerates into one
    // blob. Opting into `module_scoped_resolution` lets a bare name resolve
    // against the *whole indexed module* — but only when the name maps to a
    // single definition file, keeping ubiquitous method names (viewDidLoad…)
    // from linking everything together.

    use crate::rust_treesitter::RUST_SPEC;
    use crate::swift_treesitter::SWIFT_SPEC;
    use std::collections::HashMap;

    fn pending(from: &str, to: &str) -> Vec<(String, ScannedRef)> {
        vec![(
            "ui/builder.swift".to_string(),
            ScannedRef {
                from_qualified: Arc::from(from),
                to_name: to.to_string(),
                kind: RefKind::Call,
            },
        )]
    }

    #[test]
    fn module_scoped_resolution_links_unique_name_across_files_without_imports() {
        // `OrderViewModel` is constructed in ui/builder.swift but defined once,
        // in model/order.swift, with no import edge between them.
        let symbols = vec![
            ("model/order.swift", "OrderViewModel", "OrderViewModel"),
            ("ui/builder.swift", "build", "Builder.build"),
        ];
        let edges = resolve_heuristic_refs(
            &SWIFT_SPEC,
            &symbols,
            &HashMap::new(),
            &pending("Builder.build", "OrderViewModel"),
        );
        assert!(
            edges.iter().any(|e| e
                .from_symbol_id
                .to_string()
                .ends_with("ui/builder.swift::Builder.build")
                && e.to_symbol_id
                    .to_string()
                    .ends_with("model/order.swift::OrderViewModel")),
            "a uniquely-named type must resolve module-wide for Swift: {edges:?}"
        );
    }

    #[test]
    fn module_scoped_resolution_skips_ambiguous_names() {
        // `viewDidLoad` is defined in two files: linking it module-wide would
        // glue unrelated screens together, so it must stay unresolved.
        let symbols = vec![
            ("a/screen.swift", "viewDidLoad", "AScreen.viewDidLoad"),
            ("b/screen.swift", "viewDidLoad", "BScreen.viewDidLoad"),
            ("ui/builder.swift", "build", "Builder.build"),
        ];
        let edges = resolve_heuristic_refs(
            &SWIFT_SPEC,
            &symbols,
            &HashMap::new(),
            &pending("Builder.build", "viewDidLoad"),
        );
        assert!(
            !edges.iter().any(|e| e
                .from_symbol_id
                .to_string()
                .ends_with("ui/builder.swift::Builder.build")),
            "an ambiguous name (2 defs) must not link module-wide: {edges:?}"
        );
    }

    #[test]
    fn java_self_named_delegation_resolves_to_imported_interface() {
        // Real platform bug: `StyleInfoController.selectMeasuresInfo` does
        // `return styleInfoService.selectMeasuresInfo(...)`. The call's bare
        // name (`selectMeasuresInfo`) collides with the caller's OWN method
        // name, so the same-file lookup finds only the self (skipped). The
        // import-target fallback to the service interface must still fire,
        // otherwise `trace <route>` dead-ends at the controller.
        let symbols = vec![
            (
                "c/StyleInfoController.java",
                "selectMeasuresInfo",
                "StyleInfoController.selectMeasuresInfo",
            ),
            (
                "s/IStyleInfoService.java",
                "selectMeasuresInfo",
                "IStyleInfoService.selectMeasuresInfo",
            ),
        ];
        let mut imports = HashMap::new();
        imports.insert(
            "c/StyleInfoController.java".to_string(),
            vec!["s/IStyleInfoService.java".to_string()],
        );
        let pending = vec![(
            "c/StyleInfoController.java".to_string(),
            ScannedRef {
                from_qualified: Arc::from("StyleInfoController.selectMeasuresInfo"),
                to_name: "selectMeasuresInfo".to_string(),
                kind: RefKind::Call,
            },
        )];
        let edges = resolve_heuristic_refs(
            &crate::java_treesitter::JAVA_SPEC,
            &symbols,
            &imports,
            &pending,
        );
        assert!(
            edges.iter().any(|e| e
                .to_symbol_id
                .to_string()
                .ends_with("s/IStyleInfoService.java::IStyleInfoService.selectMeasuresInfo")),
            "self-named delegation must resolve to the imported interface: {edges:?}"
        );
    }

    #[test]
    fn module_scoped_resolution_only_links_type_like_names() {
        // A *lowercase* unique name is a method/function (`append`,
        // `pushViewController`…) that routinely collides with stdlib/UIKit;
        // resolving it module-wide glues unrelated files together. Only
        // PascalCase type/constructor names carry feature coupling.
        let symbols = vec![
            ("util/ext.swift", "append", "Array.append"),
            ("ui/builder.swift", "build", "Builder.build"),
        ];
        let edges = resolve_heuristic_refs(
            &SWIFT_SPEC,
            &symbols,
            &HashMap::new(),
            &pending("Builder.build", "append"),
        );
        assert!(
            edges.is_empty(),
            "a lowercase method name must not resolve module-wide: {edges:?}"
        );
    }

    #[test]
    fn module_scoped_resolution_drops_high_fanin_hubs() {
        // A uniquely-defined PascalCase type referenced by *many* files is
        // cross-cutting infrastructure (a base class / Theme / Router), not a
        // feature boundary; linking every file to it collapses the graph.
        let mut symbols = vec![("core/theme.swift", "Theme", "Theme")];
        let mut pending: Vec<(String, ScannedRef)> = Vec::new();
        for i in 0..(MODULE_HUB_FANIN_CAP + 5) {
            let f = format!("f{i}.swift");
            // leak each caller file into the symbol universe too
            let name: &'static str = Box::leak(format!("use{i}").into_boxed_str());
            let path: &'static str = Box::leak(f.clone().into_boxed_str());
            symbols.push((path, name, name));
            pending.push((
                f,
                ScannedRef {
                    from_qualified: Arc::from(name),
                    to_name: "Theme".to_string(),
                    kind: RefKind::Call,
                },
            ));
        }
        let edges = resolve_heuristic_refs(&SWIFT_SPEC, &symbols, &HashMap::new(), &pending);
        assert!(
            !edges.iter().any(|e| e
                .to_symbol_id
                .to_string()
                .ends_with("core/theme.swift::Theme")),
            "a high fan-in hub type must be dropped: {} edges",
            edges.len()
        );
    }

    #[test]
    fn non_module_scoped_language_keeps_import_discipline() {
        // Rust does NOT opt in: a unique name defined elsewhere must still need
        // a use/import, so no cross-file edge appears from name alone.
        let symbols = vec![
            ("model/order.rs", "OrderViewModel", "OrderViewModel"),
            ("ui/builder.rs", "build", "Builder::build"),
        ];
        let edges = resolve_heuristic_refs(
            &RUST_SPEC,
            &symbols,
            &HashMap::new(),
            &[(
                "ui/builder.rs".to_string(),
                ScannedRef {
                    from_qualified: Arc::from("Builder::build"),
                    to_name: "OrderViewModel".to_string(),
                    kind: RefKind::Call,
                },
            )],
        );
        assert!(
            !edges.iter().any(|e| e
                .to_symbol_id
                .to_string()
                .ends_with("model/order.rs::OrderViewModel")),
            "Rust must not resolve a name across files without an import: {edges:?}"
        );
    }
}
