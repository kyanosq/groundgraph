//! P20/P23.2 — TypeScript language adapter.
//!
//! Since the P23 收敛, the in-process tree-sitter driver
//! ([`crate::typescript_treesitter`]) is the **sole structural source of
//! truth** for `.ts` / `.mts` / `.cts` ([`TYPESCRIPT_SPEC`]) and `.tsx`
//! ([`TSX_SPEC`]). It owns classes / functions / methods, jest / vitest
//! tests, and ESM imports resolved to repo-relative file ids (including
//! cross-extension `.ts` ↔ `.tsx`). Output is tagged
//! `indexer = typescript_treesitter`.
//!
//! `typescript-language-server` is an **optional Tier-3 enrichment**: when
//! discovered it contributes only the semantic `Calls` / `References`
//! edges, overlaid onto the existing tree-sitter symbol ids (the two id
//! schemes are identical by construction). LSP edges are tagged
//! `indexer = typescript_lsp`. When no LSP is present the structural graph
//! is already complete; there is no longer any second structural pass.

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_core::language_batch::LanguageIndexBatch;
use specslice_core::NodeKind;
use specslice_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;
use crate::lsp_client::LspSymbolKind;
use crate::lsp_indexer::{
    binary_on_path, run_profile, LspIndexOptions, LspIndexOutcome, LspProfile,
};
use crate::treesitter::{self, TsIndexOptions};
use crate::typescript_treesitter::{TSX_SPEC, TYPESCRIPT_SPEC};

pub const TYPESCRIPT_INDEXER_NAME: &str = "typescript_lsp";
pub const TYPESCRIPT_LANGUAGE_ID: &str = "typescript";
pub const TYPESCRIPT_LSP_COMMAND_ENV: &str = "SPECSLICE_TYPESCRIPT_LSP_BIN";

#[derive(Debug, Clone, Default)]
pub struct TypescriptIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    pub lsp_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TypescriptIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub imports: usize,
    /// Number of `Calls` / `References` edges contributed by the optional
    /// Tier-3 LSP enrichment pass (0 when no LSP was available).
    #[serde(default)]
    pub references: usize,
    /// Number of medium-confidence `Calls` / `References` edges produced by the
    /// in-process tree-sitter heuristic resolver (P23 R1/R2), resolved across
    /// the merged `.ts` + `.tsx` symbol set. Independent of the LSP overlay.
    #[serde(default)]
    pub heuristic_references: usize,
    /// `typescript_treesitter` when the structural pass produced anything,
    /// empty when no TypeScript files were found.
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

/// Top-level entrypoint. The tree-sitter driver runs once per dialect
/// (`.ts/.mts/.cts` then `.tsx`) to produce the entire structural graph
/// (symbols + tests + resolved imports); an optional LSP pass then overlays
/// `Calls` / `References`.
pub fn index_typescript(
    store: &mut Store,
    options: &TypescriptIndexOptions,
) -> Result<TypescriptIndexResult> {
    let ts_name = treesitter::indexer_name(&TYPESCRIPT_SPEC);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous TypeScript tree-sitter outputs")?;
    store
        .clear_indexer_outputs(TYPESCRIPT_INDEXER_NAME)
        .context("clearing previous TypeScript LSP outputs")?;

    // Resolution universe spans every dialect (TS + JS) so a `.ts` importing a
    // `.tsx` component, or a `.js` importing a `.ts` module (and vice-versa),
    // still resolves to a real file id.
    let resolution_paths = treesitter::discover_relative_paths(
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
        &["ts", "mts", "cts", "tsx", "js", "jsx", "mjs", "cjs"],
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

    // Id set of structural nodes so the optional LSP pass attaches edges
    // without dangling targets.
    let mut known_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in store.list_all_nodes().context("listing nodes")? {
        if node.indexer.as_deref() == Some(ts_name.as_str()) {
            known_ids.insert(node.id.to_string());
        }
    }

    // Tier 3 (optional): LSP `Calls` / `References` enrichment overlaid onto
    // the tree-sitter symbol ids (identical id scheme).
    let probe = ProbeOutcome::from_options(options);
    let mut references = 0usize;
    let skip_reason = match probe.command.clone() {
        Some(cmd) => {
            let profile = typescript_profile();
            let lsp_options = LspIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: options.code_roots.clone(),
                exclude_globs: options.exclude_globs.clone(),
                lsp_command: Some(cmd),
            };
            match run_profile(&profile, &lsp_options)? {
                LspIndexOutcome::Indexed(boxed) => {
                    let crate::lsp_indexer::LspIndexedBatch { batch, stats } = *boxed;
                    let refs: Vec<_> = batch
                        .references
                        .into_iter()
                        .filter(|r| {
                            known_ids.contains(r.from_symbol_id.as_str())
                                && known_ids.contains(r.to_symbol_id.as_str())
                        })
                        .collect();
                    references = refs.len();
                    if !refs.is_empty() {
                        let refs_batch = LanguageIndexBatch {
                            language: TYPESCRIPT_LANGUAGE_ID.into(),
                            references: refs,
                            ..Default::default()
                        };
                        ingest_language_batch_minimal(store, &refs_batch, TYPESCRIPT_INDEXER_NAME)
                            .context("ingesting TypeScript LSP reference edges")?;
                    }
                    stats.skip_reason
                }
                LspIndexOutcome::Skipped { reason, .. } => reason,
            }
        }
        None => probe.skip_reason,
    };

    Ok(TypescriptIndexResult {
        files,
        symbols,
        tests,
        imports,
        references,
        heuristic_references,
        resolver_used,
        sidecar_skip_reason: skip_reason,
    })
}

/// True when an optional TypeScript LSP enrichment server is discoverable.
/// Structural indexing no longer depends on it — this only gates the Tier-3
/// `Calls` / `References` overlay.
pub fn typescript_lsp_available(options: &TypescriptIndexOptions) -> bool {
    ProbeOutcome::from_options(options).command.is_some()
}

/// Helper for the TypeScript probe: a binary is "available" only when
/// it both resolves on PATH (or as an absolute path) AND survives the
/// shared `lsp_probe` smoke launch. This catches `tsserver` wrappers
/// whose `node` shebang points at a deleted nvm install.
fn typescript_binary_runnable(cmd: &str) -> bool {
    if !binary_on_path(cmd) {
        return false;
    }
    crate::lsp_probe::probe_lsp_command(
        cmd,
        crate::lsp_probe::DEFAULT_SMOKE_ARGS,
        crate::lsp_probe::DEFAULT_TIMEOUT,
    )
    .is_runnable()
}

fn typescript_profile() -> LspProfile {
    LspProfile {
        language: TYPESCRIPT_LANGUAGE_ID,
        language_id: TYPESCRIPT_LANGUAGE_ID,
        file_extensions: &["ts", "tsx", "mts", "cts"],
        skip_dirs: &[
            "node_modules",
            ".next",
            ".nuxt",
            "dist",
            "build",
            ".turbo",
            ".cache",
            "coverage",
            ".git",
        ],
        skip_suffixes: &[".d.ts"],
        default_command: "typescript-language-server",
        default_args: &["--stdio"],
        command_env_var: TYPESCRIPT_LSP_COMMAND_ENV,
        map_kind: typescript_map_kind,
        qualify: typescript_qualify,
    }
}

fn typescript_map_kind(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
    match kind {
        LspSymbolKind::Module | LspSymbolKind::Namespace => Some(NodeKind::TypescriptModule),
        LspSymbolKind::Class => Some(NodeKind::TypescriptClass),
        LspSymbolKind::Interface => Some(NodeKind::TypescriptInterface),
        LspSymbolKind::Enum => Some(NodeKind::TypescriptEnum),
        LspSymbolKind::Method | LspSymbolKind::Constructor => Some(NodeKind::TypescriptMethod),
        LspSymbolKind::Function => Some(NodeKind::TypescriptFunction),
        _ => None,
    }
}

fn typescript_qualify(file_rel: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}.{name}"),
        None => format!("{file_rel}::{name}"),
    }
}

#[derive(Debug, Default)]
struct ProbeOutcome {
    command: Option<String>,
    skip_reason: String,
}

impl ProbeOutcome {
    fn from_options(options: &TypescriptIndexOptions) -> Self {
        if let Ok(env_cmd) = std::env::var(TYPESCRIPT_LSP_COMMAND_ENV) {
            if typescript_binary_runnable(&env_cmd) {
                return Self {
                    command: Some(env_cmd),
                    skip_reason: String::new(),
                };
            }
            return Self {
                command: None,
                skip_reason: format!(
                    "{TYPESCRIPT_LSP_COMMAND_ENV}=`{env_cmd}` smoke launch 未通过，已退化为 AST fallback"
                ),
            };
        }
        if let Some(cmd) = options.lsp_command.as_deref() {
            if typescript_binary_runnable(cmd) {
                return Self {
                    command: Some(cmd.to_string()),
                    skip_reason: String::new(),
                };
            }
            return Self {
                command: None,
                skip_reason: format!(
                    "`typescript.lsp_command = {cmd}` smoke launch 未通过，已退化为 AST fallback"
                ),
            };
        }
        // Project-local: `node_modules/.bin/typescript-language-server`.
        let local = options
            .repo_root
            .join("node_modules/.bin/typescript-language-server");
        if local.is_file() && typescript_binary_runnable(&local.to_string_lossy()) {
            return Self {
                command: Some(local.to_string_lossy().into_owned()),
                skip_reason: String::new(),
            };
        }
        if typescript_binary_runnable("typescript-language-server") {
            return Self {
                command: Some("typescript-language-server".into()),
                skip_reason: String::new(),
            };
        }
        Self {
            command: None,
            skip_reason:
                "未在 PATH / node_modules/.bin 找到 typescript-language-server，已退化为 AST fallback"
                    .into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            lsp_command: Some("specslice_nonexistent_ts_lsp_999".into()),
        };

        let result = index_typescript(&mut store, &opts).expect("typescript indexer ran");
        assert_eq!(
            result.resolver_used,
            treesitter::indexer_name(&TYPESCRIPT_SPEC),
            "structure now comes from the tree-sitter driver: {result:?}"
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
            lsp_command: Some("specslice_nonexistent_ts_lsp_999".into()),
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
            lsp_command: Some("specslice_nonexistent_ts_lsp_999".into()),
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
            lsp_command: Some("specslice_nonexistent_ts_lsp_999".into()),
        };
        index_typescript(&mut store, &opts).expect("typescript indexer ran");

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
            lsp_command: Some("specslice_nonexistent_ts_lsp_999".into()),
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
