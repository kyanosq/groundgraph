//! P22 — unified tree-sitter breadth backend, end to end.
//!
//! Drives the *real* engine pass (`index_repository`) over a temp repo
//! containing six languages and asserts the in-process tree-sitter
//! driver produced a structural graph for every one of them through a
//! single `treesitter:` config switch. This is the integration proof
//! that the generic framework is wired through config → engine → store.

use std::fs;
use std::path::Path;

use specslice_core::NodeKind;
use specslice_engine::index::index_repository;
use specslice_engine::{IndexOptions, IndexResult};
use specslice_store::Store;

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Write a minimal `.specslice.yaml` enabling only the unified
/// tree-sitter backend, plus the storage dir the engine writes into.
fn enable_treesitter(root: &Path, languages_csv: &str) {
    fs::create_dir_all(root.join(".specslice")).unwrap();
    write(
        root,
        ".specslice.yaml",
        &format!("treesitter:\n  enabled: true\n  languages: [{languages_csv}]\n  paths: [src]\n"),
    );
}

fn index(root: &Path) -> IndexResult {
    index_repository(IndexOptions::all(root)).expect("index must succeed")
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
    let mut store = Store::open(root.join(".specslice/graph.db")).unwrap();
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
    fs::create_dir_all(root.join(".specslice")).unwrap();
    write(
        root,
        ".specslice.yaml",
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

    let mut store = Store::open(root.join(".specslice/graph.db")).unwrap();
    store.migrate().unwrap();
    let nodes = store.list_all_nodes().unwrap();
    let names: BTreeSet<&str> = nodes.iter().filter_map(|n| n.name.as_deref()).collect();
    assert!(names.contains("getOrders"), "JS api function missing: {names:?}");
    assert!(
        names.contains("load"),
        "Vue Options-API method missing: {names:?}"
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
