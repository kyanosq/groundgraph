//! P11/P23.4 — Go language adapter.
//!
//! Since the P23 收敛, the in-process tree-sitter driver
//! ([`crate::go_treesitter`]) is the **sole structural source of truth**
//! for Go: structs / interfaces / functions / methods, `go test` cases
//! (`TestXxx` / `BenchmarkXxx` / `FuzzXxx` / `ExampleXxx`), and import
//! paths resolved to a representative file of the target package. Output is
//! tagged `indexer = go_treesitter`.
//!
//! `gopls` is an **optional Tier-3 enrichment**: when discovered it
//! contributes only the semantic `Calls` / `References` edges, overlaid onto
//! the existing tree-sitter symbol ids (the two id schemes are identical by
//! construction). LSP edges are tagged `indexer = go_lsp`. When `gopls` is
//! unavailable the structural graph is already complete; there is no longer
//! a second structural pass.

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

pub const GO_INDEXER_NAME: &str = "go_lsp";
pub const GO_LANGUAGE_ID: &str = "go";
pub const GO_LSP_COMMAND_ENV: &str = "SPECSLICE_GO_LSP_BIN";

#[derive(Debug, Clone, Default)]
pub struct GoIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    pub lsp_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct GoIndexResult {
    pub files: usize,
    pub symbols: usize,
    #[serde(default)]
    pub tests: usize,
    #[serde(default)]
    pub imports: usize,
    /// Number of `Calls` / `References` edges contributed by the optional
    /// Tier-3 LSP enrichment pass (0 when no `gopls` was available).
    #[serde(default)]
    pub references: usize,
    /// `go_treesitter` when the structural pass produced anything, empty
    /// when no Go files were found.
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

/// Top-level entrypoint. The tree-sitter driver produces the entire
/// structural graph (symbols + `go test` cases + resolved imports); an
/// optional `gopls` pass then overlays `Calls` / `References`.
pub fn index_go(store: &mut Store, options: &GoIndexOptions) -> Result<GoIndexResult> {
    let spec = &crate::go_treesitter::GO_SPEC;
    let ts_name = treesitter::indexer_name(spec);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous Go tree-sitter outputs")?;
    store
        .clear_indexer_outputs(GO_INDEXER_NAME)
        .context("clearing previous Go LSP outputs")?;

    let ts = treesitter::index_repo_with_spec(
        store,
        spec,
        &TsIndexOptions {
            repo_root: options.repo_root.clone(),
            code_roots: options.code_roots.clone(),
            exclude_globs: options.exclude_globs.clone(),
            resolution_paths: Vec::new(),
        },
    )
    .context("indexing Go structure via tree-sitter")?;

    // Id set of structural nodes so the optional LSP pass attaches edges
    // without dangling targets.
    let mut known_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in store.list_all_nodes().context("listing nodes")? {
        if node.indexer.as_deref() == Some(ts_name.as_str()) {
            known_ids.insert(node.id.to_string());
        }
    }

    // Tier 3 (optional): gopls `Calls` / `References` enrichment overlaid
    // onto the tree-sitter symbol ids (identical id scheme). We reuse the
    // shared LSP layer so its skip reasons (PATH hints, warmup notes) carry
    // through unchanged.
    let lsp_options = LspIndexOptions {
        repo_root: options.repo_root.clone(),
        code_roots: options.code_roots.clone(),
        exclude_globs: options.exclude_globs.clone(),
        lsp_command: options.lsp_command.clone(),
    };
    let mut references = 0usize;
    let skip_reason = match run_profile(&go_profile(), &lsp_options)? {
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
                    language: GO_LANGUAGE_ID.into(),
                    references: refs,
                    ..Default::default()
                };
                ingest_language_batch_minimal(store, &refs_batch, GO_INDEXER_NAME)
                    .context("ingesting Go LSP reference edges")?;
            }
            stats.skip_reason
        }
        LspIndexOutcome::Skipped { reason, .. } => reason,
    };

    Ok(GoIndexResult {
        files: ts.files,
        symbols: ts.symbols,
        tests: ts.tests,
        imports: ts.imports,
        references,
        resolver_used: ts.resolver_used,
        sidecar_skip_reason: skip_reason,
    })
}

/// True when `gopls` (or the override binary) actually runs on this
/// host. We require the smoke launch to succeed, not just the binary
/// to exist on PATH — the historical "exists on PATH" gate let
/// half-installed Go toolchains slip through and crash the indexer
/// mid-session.
pub fn go_lsp_available(options: &GoIndexOptions) -> bool {
    let command = options
        .lsp_command
        .clone()
        .unwrap_or_else(|| "gopls".into());
    if !binary_on_path(&command) {
        return false;
    }
    crate::lsp_probe::probe_lsp_command(
        &command,
        crate::lsp_probe::DEFAULT_SMOKE_ARGS,
        crate::lsp_probe::DEFAULT_TIMEOUT,
    )
    .is_runnable()
}

fn go_profile() -> LspProfile {
    LspProfile {
        language: "go",
        language_id: GO_LANGUAGE_ID,
        file_extensions: &["go"],
        // Vendored deps + IDE caches.
        skip_dirs: &["vendor", ".git", ".idea", "node_modules"],
        skip_suffixes: &[],
        default_command: "gopls",
        // gopls accepts `serve` as the explicit subcommand, but plain
        // `gopls` defaults to the same behaviour. We keep the args
        // empty to maximise compatibility across versions.
        default_args: &[],
        command_env_var: GO_LSP_COMMAND_ENV,
        map_kind: go_map_kind,
        qualify: go_qualify,
    }
}

fn go_map_kind(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
    match kind {
        // `gopls` reports Go `struct` types as either `Class` (older
        // versions) or `Struct` (current versions). Treat both as Go
        // structs.
        LspSymbolKind::Struct | LspSymbolKind::Class => Some(NodeKind::GoStruct),
        LspSymbolKind::Interface => Some(NodeKind::GoInterface),
        LspSymbolKind::Method => Some(NodeKind::GoMethod),
        LspSymbolKind::Function => Some(NodeKind::GoFunction),
        // Constructors do not exist in Go; the spec value 9 is unused
        // here.
        _ => None,
    }
}

fn go_qualify(file_rel: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}.{name}"),
        None => format!("{file_rel}::{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_map_kind_collapses_class_and_struct() {
        assert_eq!(
            go_map_kind(LspSymbolKind::Struct, None),
            Some(NodeKind::GoStruct)
        );
        assert_eq!(
            go_map_kind(LspSymbolKind::Class, None),
            Some(NodeKind::GoStruct)
        );
        assert_eq!(
            go_map_kind(LspSymbolKind::Interface, None),
            Some(NodeKind::GoInterface)
        );
        assert_eq!(
            go_map_kind(LspSymbolKind::Method, None),
            Some(NodeKind::GoMethod)
        );
        assert_eq!(
            go_map_kind(LspSymbolKind::Function, None),
            Some(NodeKind::GoFunction)
        );
        assert_eq!(go_map_kind(LspSymbolKind::Variable, None), None);
        assert_eq!(go_map_kind(LspSymbolKind::Constant, None), None);
    }

    #[test]
    fn go_qualified_names_use_dot_separator() {
        assert_eq!(
            go_qualify("internal/api/users.go", None, "Server"),
            "internal/api/users.go::Server"
        );
        assert_eq!(
            go_qualify(
                "internal/api/users.go",
                Some("internal/api/users.go::Server"),
                "Handle"
            ),
            "internal/api/users.go::Server.Handle"
        );
    }

    #[test]
    fn go_lsp_available_returns_false_for_bogus_override() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = GoIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: Vec::new(),
            exclude_globs: Vec::new(),
            lsp_command: Some("specslice_nonexistent_gopls_999".into()),
        };
        assert!(!go_lsp_available(&opts));
    }
}
