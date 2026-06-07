//! P20/P23.2 — TypeScript language adapter (structure + heuristic).
//!
//! The in-process tree-sitter driver ([`crate::typescript_treesitter`]) is the
//! **sole source of truth** for `.ts` / `.mts` / `.cts` ([`TYPESCRIPT_SPEC`])
//! and `.tsx` / `.js` / `.jsx` / `.vue` ([`TSX_SPEC`]). It owns classes /
//! functions / methods, jest / vitest tests, ESM imports resolved to
//! repo-relative file ids (including cross-extension `.ts` ↔ `.tsx`), and the
//! medium-confidence heuristic `Calls` / `References` edges resolved across the
//! merged dialect symbol set. Output is tagged `indexer = typescript_treesitter`.
//!
//! Precise cross-symbol resolution is supplied out-of-band by the SCIP overlay
//! (`scip-typescript`; ADR-0001 R1/R2), which the engine ingests after this
//! pass and which authoritatively supersedes the heuristic edges on the files
//! it covers. The former in-process `typescript-language-server` Tier-3 sidecar
//! was retired in favour of SCIP — only Swift keeps an LSP.
//!
//! TypeScript keeps a *dedicated* adapter (not the generic single-spec driver)
//! because it must cover **both** dialects in one pass: the generic driver runs
//! a single grammar and would silently miss the entire JSX/JS/Vue dialect.

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_core::language_batch::LanguageIndexBatch;
use specslice_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;
use crate::treesitter::{self, TsIndexOptions};
use crate::typescript_treesitter::{TSX_SPEC, TYPESCRIPT_SPEC};

pub const TYPESCRIPT_LANGUAGE_ID: &str = "typescript";

/// Legacy `indexer` tag for the retired TypeScript LSP overlay. Cleared on
/// every run so upgrading an existing store drops any stale `typescript_lsp`
/// rows it still holds.
const LEGACY_TYPESCRIPT_LSP_INDEXER: &str = "typescript_lsp";

#[derive(Debug, Clone, Default)]
pub struct TypescriptIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TypescriptIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub imports: usize,
    /// Number of medium-confidence `Calls` / `References` edges produced by the
    /// in-process tree-sitter heuristic resolver, resolved across the merged
    /// `.ts` + `.tsx` symbol set. SCIP supersedes these on the files it covers.
    #[serde(default)]
    pub heuristic_references: usize,
    /// `typescript_treesitter` when the structural pass produced anything,
    /// empty when no TypeScript files were found.
    pub resolver_used: String,
}

/// Top-level entrypoint. The tree-sitter driver runs once per dialect
/// (`.ts/.mts/.cts` then `.tsx`) to produce the entire structural graph
/// (symbols + tests + resolved imports) and the heuristic Calls/References
/// across the merged symbol set.
pub fn index_typescript(
    store: &mut Store,
    options: &TypescriptIndexOptions,
) -> Result<TypescriptIndexResult> {
    let ts_name = treesitter::indexer_name(&TYPESCRIPT_SPEC);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous TypeScript tree-sitter outputs")?;
    store
        .clear_indexer_outputs(LEGACY_TYPESCRIPT_LSP_INDEXER)
        .context("clearing retired TypeScript LSP outputs")?;

    // Resolution universe spans every dialect (TS + JS) so a `.ts` importing a
    // `.tsx` component, or a `.js` importing a `.ts` module (and vice-versa),
    // still resolves to a real file id.
    let resolution_paths = treesitter::discover_relative_paths(
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
        &["ts", "mts", "cts", "tsx", "js", "jsx", "mjs", "cjs", "vue"],
        TYPESCRIPT_SPEC.skip_dirs,
    )
    .context("discovering TypeScript files")?;

    let mut files = 0usize;
    let mut symbols = 0usize;
    let mut tests = 0usize;
    let mut imports = 0usize;
    // Defer heuristic resolution: `.ts` and `.tsx` need separate grammars
    // (the `<T>` generic / JSX ambiguity) but share one symbol namespace, so a
    // call in `app.js` to a `helper` declared in `util.ts` only resolves when
    // both passes' symbols are merged before resolving.
    let mut merged = treesitter::RefResolutionInputs::default();
    for spec in [&TYPESCRIPT_SPEC, &TSX_SPEC] {
        let (r, inputs) = treesitter::index_repo_with_spec_collect(
            store,
            spec,
            &TsIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: options.code_roots.clone(),
                exclude_globs: options.exclude_globs.clone(),
                resolution_paths: resolution_paths.clone(),
            },
        )
        .context("indexing TypeScript structure via tree-sitter")?;
        files += r.files;
        symbols += r.symbols;
        tests += r.tests;
        imports += r.imports;
        merged.symbols.extend(inputs.symbols);
        for (file, targets) in inputs.import_targets {
            merged
                .import_targets
                .entry(file)
                .or_default()
                .extend(targets);
        }
        merged.pending.extend(inputs.pending);
    }
    let resolver_used = if files > 0 {
        ts_name.clone()
    } else {
        String::new()
    };

    // Heuristic Calls / References across the merged `.ts` + `.tsx` symbol set
    // (medium confidence; indexer name is `typescript_treesitter`).
    let mut heuristic_references = 0usize;
    if !merged.pending.is_empty() {
        let view: Vec<(&str, &str, &str)> = merged
            .symbols
            .iter()
            .map(|(p, n, q)| (p.as_str(), n.as_str(), q.as_str()))
            .collect();
        let edges = treesitter::resolve_heuristic_refs(
            &TYPESCRIPT_SPEC,
            &view,
            &merged.import_targets,
            &merged.pending,
        );
        heuristic_references = edges.len();
        if !edges.is_empty() {
            let refs_batch = LanguageIndexBatch {
                language: TYPESCRIPT_LANGUAGE_ID.into(),
                references: edges,
                ..Default::default()
            };
            ingest_language_batch_minimal(store, &refs_batch, &ts_name)
                .context("ingesting TypeScript heuristic reference edges")?;
        }
    }

    Ok(TypescriptIndexResult {
        files,
        symbols,
        tests,
        imports,
        heuristic_references,
        resolver_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::NodeKind;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_fixture(root: &Path) {
        for (rel, body) in [
            (
                "src/greeter.ts",
                "export class Greeter {\n  greet(name: string): string {\n    return `hi ${name}`;\n  }\n}\n",
            ),
            (
                "src/index.ts",
                "import { Greeter } from \"./greeter\";\nexport function makeGreeter() { return new Greeter(); }\n",
            ),
            (
                "tests/greeter.test.ts",
                "import { describe, it, expect } from \"vitest\";\nimport { Greeter } from \"../src/greeter\";\n\ndescribe(\"greeter\", () => {\n  it(\"greets\", () => {\n    expect(new Greeter().greet(\"Ada\")).toBe(\"hi Ada\");\n  });\n});\n",
            ),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
    }

    fn open_temp_store(root: &Path) -> (Store, PathBuf) {
        let db = root.join("graph.db");
        let mut store = Store::open(&db).unwrap();
        store.migrate().unwrap();
        (store, db)
    }

    #[test]
    fn treesitter_pass_runs_against_typescript_hello_fixture_without_lsp() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let (mut store, _db) = open_temp_store(tmp.path());

        let opts = TypescriptIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src"), PathBuf::from("tests")],
            exclude_globs: Vec::new(),
        };

        let result = index_typescript(&mut store, &opts).expect("typescript indexer ran");
        assert_eq!(
            result.resolver_used,
            treesitter::indexer_name(&TYPESCRIPT_SPEC),
            "structure comes from the tree-sitter driver: {result:?}"
        );
        assert!(result.files >= 3, "{result:?}");
        assert!(result.tests >= 1, "{result:?}");
        assert!(result.imports >= 2, "{result:?}");

        let nodes = store.list_all_nodes().unwrap();
        let kinds: std::collections::BTreeSet<&str> =
            nodes.iter().map(|n| n.kind.as_str()).collect();
        for required in [
            "typescript_class",
            "typescript_method",
            "typescript_function",
            "test_case",
            "test_group",
        ] {
            assert!(
                kinds.contains(required),
                "expected `{required}` in {:?}",
                kinds
            );
        }
    }

    /// `.tsx` components index through the dedicated TSX dialect, and a
    /// `.ts` file importing a `.tsx` resolves across the extension boundary.
    #[test]
    fn tsx_components_and_cross_extension_imports_resolve() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        for (rel, body) in [
            (
                "src/Button.tsx",
                "import React from \"react\";\nexport function Button() { return <button/>; }\n",
            ),
            (
                "src/index.ts",
                "import { Button } from \"./Button\";\nexport function mount() { return Button; }\n",
            ),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let (mut store, _db) = open_temp_store(root);
        let opts = TypescriptIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: Vec::new(),
        };
        let result = index_typescript(&mut store, &opts).expect("typescript indexer ran");
        assert!(result.files >= 2, "tsx + ts counted: {result:?}");

        let edges = store.list_all_edges().unwrap();
        let cross = edges.iter().any(|e| {
            e.kind == specslice_core::EdgeKind::Imports
                && e.from_id.as_str() == "file::src/index.ts"
                && e.to_id.as_str() == "file::src/Button.tsx"
        });
        assert!(
            cross,
            "`.ts` → `.tsx` import should resolve across extensions"
        );
    }

    /// Plain JavaScript (`.js` / `.jsx` / `.mjs` / `.cjs`) indexes through the
    /// same TypeScript driver, and a `.js` importing a `.ts` module resolves
    /// across the extension boundary (P23 JS coverage).
    #[test]
    fn javascript_files_index_and_resolve_imports_to_typescript() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        for (rel, body) in [
            (
                "src/util.ts",
                "export function helper(): number { return 1; }\n",
            ),
            (
                "src/app.js",
                "import { helper } from \"./util\";\nexport function run() { return helper(); }\n",
            ),
            (
                "src/Widget.jsx",
                "export function Widget() { return <div/>; }\n",
            ),
            ("src/server.mjs", "export function boot() { return 0; }\n"),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let (mut store, _db) = open_temp_store(root);
        let opts = TypescriptIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: Vec::new(),
        };
        let result = index_typescript(&mut store, &opts).expect("typescript indexer ran");
        assert!(
            result.files >= 4,
            "ts + js + jsx + mjs must all be indexed: {result:?}"
        );

        let nodes = store.list_all_nodes().unwrap();
        let fn_names: std::collections::BTreeSet<&str> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::TypescriptFunction)
            .filter_map(|n| n.name.as_deref())
            .collect();
        for required in ["run", "Widget", "boot"] {
            assert!(
                fn_names.contains(required),
                "expected JS function `{required}` indexed, got {fn_names:?}"
            );
        }

        // `.js` → `.ts` import resolves across the extension boundary.
        let edges = store.list_all_edges().unwrap();
        let cross = edges.iter().any(|e| {
            e.kind == specslice_core::EdgeKind::Imports
                && e.from_id.as_str() == "file::src/app.js"
                && e.to_id.as_str() == "file::src/util.ts"
        });
        assert!(cross, "`.js` → `.ts` import should resolve, got {edges:?}");
    }

    /// P23 R1/R2 for TypeScript/JavaScript: the heuristic resolver links a
    /// bare call to an imported function across the `.js` → `.ts` boundary and
    /// records the edge at medium confidence (never the precision tier).
    #[test]
    fn emits_heuristic_call_edges_across_ts_js() {
        use crate::EdgeConfidence;
        use specslice_core::EdgeKind;
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        for (rel, body) in [
            (
                "src/util.ts",
                "export function helper(): number { return 1; }\n",
            ),
            (
                "src/app.js",
                "import { helper } from \"./util\";\nexport function run() { return helper(); }\n",
            ),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let (mut store, _db) = open_temp_store(root);
        let opts = TypescriptIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: Vec::new(),
        };
        let result = index_typescript(&mut store, &opts).expect("typescript indexer ran");
        assert!(
            result.heuristic_references >= 1,
            "heuristic resolver should link run() -> helper(): {result:?}"
        );

        let calls = store.list_edges_by_kind(EdgeKind::Calls).unwrap();
        let run_to_helper = calls.iter().find(|e| {
            e.from_id.as_str() == "typescript::src/app.js::run"
                && e.to_id.as_str() == "typescript::src/util.ts::helper"
        });
        let edge = run_to_helper.unwrap_or_else(|| {
            panic!("expected run() → helper() heuristic call edge, got {calls:?}")
        });
        assert_eq!(
            crate::edge_confidence::confidence_for_edge(edge),
            EdgeConfidence::Medium,
            "heuristic tree-sitter call edges must stay at medium confidence"
        );
    }

    /// Vue Single-File Components (`.vue`) index through the same TypeScript
    /// driver: the `<script>` block is parsed (template/style stripped), the
    /// file node + ESM imports are recovered (resolving `.vue` targets across
    /// the extension boundary), and Options-API component methods nested in
    /// `export default { methods: { … } }` are captured as symbols.
    #[test]
    fn vue_sfc_script_block_indexes_imports_and_options_methods() {
        use specslice_core::EdgeKind;
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        for (rel, body) in [
            (
                "src/api/order.js",
                "import request from '@/utils/request'\nexport function getOrderList(params) {\n  return request({ url: '/order/list', method: 'get', params })\n}\n",
            ),
            (
                "src/components/Toolbar.vue",
                "<template>\n  <div class=\"bar\">{{ 标题 }}</div>\n</template>\n\n<script>\nexport default {\n  name: 'Toolbar',\n}\n</script>\n",
            ),
            (
                "src/views/OrderList.vue",
                "<template>\n  <div class=\"order\">\n    <Toolbar/>\n    <span>{{ title }}</span>\n  </div>\n</template>\n\n<script>\nimport Toolbar from '../components/Toolbar.vue'\nimport { getOrderList } from '../api/order'\n\nexport default {\n  name: 'OrderList',\n  components: { Toolbar },\n  data() {\n    return { title: '订单列表', list: [] }\n  },\n  methods: {\n    async fetchList() {\n      this.list = await getOrderList({})\n    },\n    handleDelete(id) {\n      this.list = this.list.filter((x) => x.id !== id)\n    },\n  },\n}\n</script>\n\n<style scoped>\n.order { color: red; }\n</style>\n",
            ),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let (mut store, _db) = open_temp_store(root);
        let opts = TypescriptIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: Vec::new(),
        };
        let result = index_typescript(&mut store, &opts).expect("typescript indexer ran");
        assert!(
            result.files >= 3,
            "two .vue + one .js must all be indexed: {result:?}"
        );

        let nodes = store.list_all_nodes().unwrap();
        let method_names: std::collections::BTreeSet<&str> = nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.kind,
                    NodeKind::TypescriptMethod | NodeKind::TypescriptFunction
                )
            })
            .filter_map(|n| n.name.as_deref())
            .collect();
        for required in ["fetchList", "handleDelete", "getOrderList"] {
            assert!(
                method_names.contains(required),
                "expected Vue/JS symbol `{required}` indexed, got {method_names:?}"
            );
        }

        let edges = store.list_all_edges().unwrap();
        // `.vue` → `.js` import resolves across the extension boundary.
        let vue_to_js = edges.iter().any(|e| {
            e.kind == EdgeKind::Imports
                && e.from_id.as_str() == "file::src/views/OrderList.vue"
                && e.to_id.as_str() == "file::src/api/order.js"
        });
        assert!(vue_to_js, "`.vue` → `.js` import should resolve: {edges:?}");
        // `.vue` → `.vue` import resolves (explicit `.vue` specifier).
        let vue_to_vue = edges.iter().any(|e| {
            e.kind == EdgeKind::Imports
                && e.from_id.as_str() == "file::src/views/OrderList.vue"
                && e.to_id.as_str() == "file::src/components/Toolbar.vue"
        });
        assert!(
            vue_to_vue,
            "`.vue` → `.vue` import should resolve: {edges:?}"
        );
    }

    /// Re-indexing the same repo twice must be a graph-level no-op (P23.2
    /// idempotency contract).
    #[test]
    fn reindexing_is_idempotent() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let (mut store, _db) = open_temp_store(tmp.path());
        let opts = TypescriptIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src"), PathBuf::from("tests")],
            exclude_globs: Vec::new(),
        };
        let first = index_typescript(&mut store, &opts).expect("first index ok");
        let nodes_1 = store.list_all_nodes().unwrap().len();
        let edges_1 = store.list_all_edges().unwrap().len();
        let second = index_typescript(&mut store, &opts).expect("second index ok");
        let nodes_2 = store.list_all_nodes().unwrap().len();
        let edges_2 = store.list_all_edges().unwrap().len();
        assert_eq!(first, second, "result counts stable across re-index");
        assert_eq!(nodes_1, nodes_2, "node count stable across re-index");
        assert_eq!(edges_1, edges_2, "edge count stable across re-index");
    }
}
