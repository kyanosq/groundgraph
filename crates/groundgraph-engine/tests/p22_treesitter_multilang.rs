//! P22 — unified tree-sitter breadth backend, end to end.
//!
//! Drives the *real* engine pass (`index_repository`) over a temp repo
//! containing six languages and asserts the in-process tree-sitter
//! driver produced a structural graph for every one of them through a
//! single `treesitter:` config switch. This is the integration proof
//! that the generic framework is wired through config → engine → store.

use std::fs;
use std::path::Path;

use groundgraph_core::{EdgeKind, NodeKind};
use groundgraph_engine::config::DeadCodeConfig;
use groundgraph_engine::dead_code::{
    analyze_dead_code_with_store, DeadCodeConfidence, DeadCodeOptions,
};
use groundgraph_engine::index::index_repository;
use groundgraph_engine::{IndexOptions, IndexResult};
use groundgraph_store::Store;

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Write a minimal `.groundgraph.yaml` enabling only the unified
/// tree-sitter backend, plus the storage dir the engine writes into.
fn enable_treesitter(root: &Path, languages_csv: &str) {
    fs::create_dir_all(root.join(".groundgraph")).unwrap();
    write(
        root,
        ".groundgraph.yaml",
        &format!("treesitter:\n  enabled: true\n  languages: [{languages_csv}]\n  paths: [src]\n"),
    );
}

fn index(root: &Path) -> IndexResult {
    index_repository(IndexOptions::all(root)).expect("index must succeed")
}

/// Regression (rust-analyzer): `crates/parser/test_data/**` holds ~470
/// fixture `.rs` files (many intentionally malformed). The Go-convention
/// `testdata/` was already skipped, but the underscored variant slipped
/// through and inflated the parser module to 539 "files" in `propose`.
#[test]
fn fixture_dirs_are_never_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "src/lib.rs", "pub fn real() {}\n");
    write(
        root,
        "src/test_data/fixture_a.rs",
        "pub fn phantom_a() {}\n",
    );
    write(root, "src/testdata/fixture_b.rs", "pub fn phantom_b() {}\n");
    enable_treesitter(root, "rust");
    index(root);

    let store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    let nodes = store.list_all_nodes().unwrap();
    assert!(
        nodes.iter().any(|n| n.name.as_deref() == Some("real")),
        "production symbol must be indexed"
    );
    for phantom in ["phantom_a", "phantom_b"] {
        assert!(
            !nodes.iter().any(|n| n.name.as_deref() == Some(phantom)),
            "fixture symbol {phantom} must not be indexed"
        );
    }
}

#[test]
fn unified_pass_indexes_all_six_languages() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "src/lib.rs",
        "pub struct Widget { pub name: String }\n\
         impl Widget { pub fn render(&self) -> String { self.name.clone() } }\n\
         pub fn build() -> Widget { Widget { name: String::new() } }\n",
    );
    write(
        root,
        "src/app.ts",
        "export class App { run(): void {} }\nexport function boot(): void {}\n",
    );
    write(
        root,
        "src/mod.py",
        "class Service:\n    def handle(self):\n        pass\n\ndef main():\n    pass\n",
    );
    write(
        root,
        "src/main.go",
        "package main\ntype Server struct{}\nfunc (s *Server) Serve() {}\nfunc New() *Server { return &Server{} }\n",
    );
    write(
        root,
        "src/core.c",
        "struct Buf { int len; };\nint init(void) { return 0; }\n",
    );
    write(
        root,
        "src/engine.cpp",
        "namespace eng {\n  class Core { public: void tick() {} };\n  int run() { return 0; }\n}\n",
    );

    enable_treesitter(root, "rust, typescript, python, go, c, cpp");
    let result = index(root);

    // Every configured language produced output.
    let langs: Vec<&str> = result
        .treesitter
        .iter()
        .map(|r| r.language.as_str())
        .collect();
    for want in ["rust", "typescript", "python", "go", "c", "cpp"] {
        let entry = result
            .treesitter
            .iter()
            .find(|r| r.language == want)
            .unwrap_or_else(|| panic!("missing {want} in treesitter results: {langs:?}"));
        assert!(
            entry.files >= 1 && entry.symbols >= 2,
            "{want} produced too little: {entry:?}"
        );
        assert_eq!(entry.resolver_used, format!("{want}_treesitter"));
    }

    // Inspect the persisted graph: each language's flagship kinds landed.
    let mut store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    store.migrate().unwrap();
    let nodes = store.list_all_nodes().unwrap();
    let has = |kind: NodeKind, name: &str| {
        nodes
            .iter()
            .any(|n| n.kind == kind && n.name.as_deref() == Some(name))
    };

    assert!(has(NodeKind::RustStruct, "Widget"), "rust struct");
    assert!(has(NodeKind::RustMethod, "render"), "rust method");
    assert!(has(NodeKind::TypescriptClass, "App"), "ts class");
    assert!(has(NodeKind::TypescriptFunction, "boot"), "ts function");
    assert!(has(NodeKind::PythonClass, "Service"), "py class");
    assert!(has(NodeKind::PythonMethod, "handle"), "py method");
    assert!(has(NodeKind::GoStruct, "Server"), "go struct");
    assert!(has(NodeKind::GoMethod, "Serve"), "go method");
    assert!(has(NodeKind::CStruct, "Buf"), "c struct");
    assert!(has(NodeKind::CFunction, "init"), "c function");
    assert!(has(NodeKind::CppNamespace, "eng"), "cpp namespace");
    assert!(has(NodeKind::CppClass, "Core"), "cpp class");
    assert!(has(NodeKind::CppMethod, "tick"), "cpp method");
}

/// P26 — wave-3 breadth languages (C# / Ruby / PHP / Kotlin) flow through the
/// same unified pass: config selects them, the generic driver indexes them,
/// and their flagship kinds + tests land in the store.
#[test]
fn unified_pass_indexes_wave3_languages() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "src/Greeter.cs",
        "namespace App;\npublic class Greeter\n{\n    public string Greet(string name) => \"hi\";\n}\npublic class GreeterTests\n{\n    [Fact]\n    public void Greets() { }\n}\n",
    );
    write(
        root,
        "src/billing.rb",
        "module Billing\n  class Invoice\n    def charge!(gw)\n      gw.charge(1)\n    end\n  end\nend\n",
    );
    write(
        root,
        "src/Greeter.php",
        "<?php\nclass Greeter {\n    public function greet(string $name): string { return \"hi\"; }\n}\nfunction top_level(): void {}\n",
    );
    write(
        root,
        "src/Greeter.kt",
        "package app\n\nclass Greeter {\n    fun greet(name: String): String = \"hi\"\n}\n\nobject Registry { fun touch() {} }\n",
    );

    enable_treesitter(root, "csharp, ruby, php, kotlin");
    let result = index(root);

    let langs: Vec<&str> = result
        .treesitter
        .iter()
        .map(|r| r.language.as_str())
        .collect();
    for want in ["csharp", "ruby", "php", "kotlin"] {
        let entry = result
            .treesitter
            .iter()
            .find(|r| r.language == want)
            .unwrap_or_else(|| panic!("missing {want} in treesitter results: {langs:?}"));
        assert!(
            entry.files >= 1 && entry.symbols >= 2,
            "{want} produced too little: {entry:?}"
        );
        assert_eq!(entry.resolver_used, format!("{want}_treesitter"));
    }

    let mut store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    store.migrate().unwrap();
    let nodes = store.list_all_nodes().unwrap();
    let has = |kind: NodeKind, name: &str| {
        nodes
            .iter()
            .any(|n| n.kind == kind && n.name.as_deref() == Some(name))
    };

    assert!(has(NodeKind::CSharpClass, "Greeter"), "cs class");
    assert!(has(NodeKind::CSharpMethod, "Greet"), "cs method");
    assert!(
        has(NodeKind::TestCase, "Greets"),
        "cs [Fact] becomes a test case"
    );
    assert!(has(NodeKind::RubyModule, "Billing"), "rb module");
    assert!(has(NodeKind::RubyClass, "Invoice"), "rb class");
    assert!(has(NodeKind::RubyMethod, "charge!"), "rb method");
    assert!(has(NodeKind::PhpClass, "Greeter"), "php class");
    assert!(has(NodeKind::PhpMethod, "greet"), "php method");
    assert!(has(NodeKind::PhpFunction, "top_level"), "php function");
    assert!(has(NodeKind::KotlinClass, "Greeter"), "kt class");
    assert!(has(NodeKind::KotlinMethod, "greet"), "kt method");
    assert!(has(NodeKind::KotlinObject, "Registry"), "kt object");
}

/// Regression: a project selected through the unified `languages:` list with
/// `enrichment.lsp = false` (exactly what `init` writes for a JS/Vue repo) must
/// still index the full `.tsx`/`.js`/`.jsx`/`.vue` dialect — not just `.ts`.
/// Before the fix, `normalized()` routed TypeScript to the generic single-spec
/// driver (which owns only `.ts`/`.mts`/`.cts`), so a `.js` + `.vue` repo
/// indexed zero files.
#[test]
fn unified_languages_typescript_indexes_js_and_vue_without_lsp() {
    use std::collections::BTreeSet;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/api/order.js",
        "import request from '@/utils/request'\nexport function getOrders() {\n  return request({ url: '/order/list' })\n}\n",
    );
    write(
        root,
        "src/views/List.vue",
        "<template>\n  <div>{{ 列表 }}</div>\n</template>\n\n<script>\nimport { getOrders } from '../api/order'\nexport default {\n  name: 'List',\n  methods: {\n    async load() {\n      this.rows = await getOrders()\n    },\n  },\n}\n</script>\n\n<style>\n.a{}\n</style>\n",
    );
    // Mirror `init`'s output for a JS/Vue project: unified selector + lsp off.
    fs::create_dir_all(root.join(".groundgraph")).unwrap();
    write(
        root,
        ".groundgraph.yaml",
        "languages:\n- id: typescript\n  paths: [src]\n  exclude: ['**/node_modules/**']\nenrichment:\n  lsp: false\n  analyzer: true\n",
    );

    let result = index(root);
    let ts = result
        .typescript
        .expect("TypeScript must run through its dual-dialect adapter, not the generic driver");
    assert!(
        ts.files >= 2,
        "both the .js module and the .vue SFC must be indexed: {ts:?}"
    );

    let mut store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    store.migrate().unwrap();
    let nodes = store.list_all_nodes().unwrap();
    let names: BTreeSet<&str> = nodes.iter().filter_map(|n| n.name.as_deref()).collect();
    assert!(
        names.contains("getOrders"),
        "JS api function missing: {names:?}"
    );
    assert!(
        names.contains("load"),
        "Vue Options-API method missing: {names:?}"
    );
}

/// Regression: `.h` is shared between C and C++. A header carrying C++
/// constructs (`namespace` / `class` / `::`) must be parsed by the C++ grammar,
/// not silently handed to the C parser (which drops every C++ declaration).
/// A plain C header (no C++ signals) must still route to C. Before the fix,
/// `.h` was owned exclusively by `C_SPEC`, so header-only C++ libraries indexed
/// zero classes/methods.
#[test]
fn cpp_header_h_is_routed_to_cpp_not_c() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // A C++ header (very common to use `.h`, not `.hpp`).
    write(
        root,
        "src/widget.h",
        "namespace ui {\n  class Widget {\n  public:\n    int area() const { return 0; }\n  };\n}\n",
    );
    // A genuine C header must stay with the C parser.
    write(root, "src/legacy.h", "struct CBuf { int len; };\n");

    enable_treesitter(root, "c, cpp");
    let _ = index(root);

    let mut store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    store.migrate().unwrap();
    let nodes = store.list_all_nodes().unwrap();
    let has = |kind: NodeKind, name: &str| {
        nodes
            .iter()
            .any(|n| n.kind == kind && n.name.as_deref() == Some(name))
    };

    // C++ header → C++ symbols recovered.
    assert!(
        has(NodeKind::CppNamespace, "ui"),
        "C++ namespace in .h must be indexed by the C++ parser"
    );
    assert!(
        has(NodeKind::CppClass, "Widget"),
        "C++ class in .h must be indexed by the C++ parser"
    );
    assert!(
        has(NodeKind::CppMethod, "area"),
        "C++ method in .h must be indexed by the C++ parser"
    );
    // Plain C header → still owned by C.
    assert!(
        has(NodeKind::CStruct, "CBuf"),
        "a plain C .h header must still route to the C parser"
    );
}

/// Regression: a `.h` guarded by `extern "C" { … }` (the universal dual-use
/// header idiom) must route to the C++ parser — tree-sitter's C grammar cannot
/// parse the linkage block and drops every declaration inside it, while the C++
/// grammar handles both the block and the C declarations. The anonymous typedef
/// record inside must also surface (shared C-family handling).
#[test]
fn extern_c_header_routes_to_cpp_and_recovers_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/api.h",
        "#ifdef __cplusplus\nextern \"C\" {\n#endif\n\
         typedef struct { int x; } Handle;\n\
         int api_open(const char* p);\n\
         int api_run(Handle* h) { return 0; }\n\
         #ifdef __cplusplus\n}\n#endif\n",
    );
    enable_treesitter(root, "c, cpp");
    let _ = index(root);

    let mut store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    store.migrate().unwrap();
    let nodes = store.list_all_nodes().unwrap();
    let has = |kind: NodeKind, name: &str| {
        nodes
            .iter()
            .any(|n| n.kind == kind && n.name.as_deref() == Some(name))
    };
    assert!(
        has(NodeKind::CppStruct, "Handle"),
        "anonymous typedef struct inside extern \"C\" must be recovered (via C++)"
    );
    assert!(
        has(NodeKind::CppFunction, "api_run"),
        "an inline function inside extern \"C\" must be recovered (via C++)"
    );
}

#[test]
fn unknown_languages_are_skipped_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "src/lib.rs", "pub fn only() {}\n");

    enable_treesitter(root, "rust, cobol, brainfuck");
    let result = index(root);
    assert_eq!(
        result.treesitter.len(),
        1,
        "only rust should produce output"
    );
    assert_eq!(result.treesitter[0].language, "rust");
}

/// issues.md #123 — a `macro_rules!` definition must (a) become a RustMacro
/// node and (b) receive an inbound `Calls` edge from every `foo!()` site, so a
/// macro that is in use is reachable and never a dead-code false positive. A
/// macro that is genuinely never invoked stays dead — the correctness floor is
/// unchanged, the false positive the issue warns about is gone.
#[test]
fn rust_macro_in_use_is_not_a_dead_code_false_positive() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // `used_macro` sits in the reachable crate root and is invoked by
    // `helper`, which `main` calls — so the macro is reachable and must not
    // be flagged. `dead_macro` lives in a separate file that no reachable
    // code references, so it stays unreachable and MUST still be flagged —
    // dead-code sensitivity is unchanged. (A same-file dead macro would be
    // masked by file-level reachability up-propagation, so the control case
    // has to be cross-file.)
    write(
        root,
        "src/lib.rs",
        "macro_rules! used_macro { () => { } }\n\
         fn helper() { used_macro!(); }\n\
         fn main() { helper(); }\n",
    );
    write(
        root,
        "src/orphan.rs",
        "macro_rules! dead_macro { () => { } }\n",
    );
    enable_treesitter(root, "rust");
    index(root);

    let mut store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    store.migrate().unwrap();
    let nodes = store.list_all_nodes().unwrap();
    let edges = store.list_all_edges().unwrap();

    // (a) The macro definition is a node.
    let used = nodes
        .iter()
        .find(|n| n.kind == NodeKind::RustMacro && n.name.as_deref() == Some("used_macro"))
        .expect("used_macro RustMacro node must exist");
    // (b) The call site resolves to it with a Calls edge — the property the
    // issue warns is missing when only the node (not the invocation edge) is
    // emitted, which strands every macro at zero inbound.
    assert!(
        edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.to_id == used.id),
        "used_macro!() must link to the RustMacro node with a Calls edge"
    );

    // (c) Dead-code analysis: `main` is an implicit entry, so
    // main → helper → used_macro keeps used_macro reachable; dead_macro is
    // never invoked and must still be flagged.
    let report = analyze_dead_code_with_store(
        &store,
        DeadCodeOptions {
            repo_root: root.to_path_buf(),
            min_confidence: DeadCodeConfidence::Medium,
            include_tests: false,
        },
        &DeadCodeConfig::default(),
    )
    .expect("dead-code analysis");
    let dead_names: Vec<&str> = report
        .candidates
        .iter()
        .filter_map(|c| {
            nodes
                .iter()
                .find(|n| n.id.to_string() == c.id)
                .and_then(|n| n.name.as_deref())
        })
        .collect();
    assert!(
        !dead_names.contains(&"used_macro"),
        "an invoked macro must not be flagged dead: {dead_names:?}"
    );
    assert!(
        dead_names.contains(&"dead_macro"),
        "a never-invoked macro must still be flagged dead: {dead_names:?}"
    );
}
