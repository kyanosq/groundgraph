//! Go language adapter (P11) — drives `gopls` over LSP.
//!
//! `gopls` is the official language server bundled with the Go
//! toolchain. The mapping is intentionally narrow: we surface
//! top-level structs, interfaces, free functions, and methods. Type
//! aliases and constants are skipped because their fan-in to other
//! symbols is structural rather than behavioural and they bloat the
//! graph without helping search / dead-code analysis.
//!
//! `gopls` requires the workspace to contain at least one `go.mod`
//! file before it will respond to documentSymbol; the engine still
//! attempts the run and reports an empty result rather than failing,
//! so operators get an actionable skip reason.

use std::path::PathBuf;

use anyhow::Result;
use specslice_core::language_batch::LanguageIndexBatch;
use specslice_core::NodeKind;
use specslice_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;
use crate::lsp_client::LspSymbolKind;
use crate::lsp_indexer::{
    binary_on_path, run_profile, LspIndexOptions, LspIndexOutcome, LspProfile,
};

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
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

pub fn index_go(store: &mut Store, options: &GoIndexOptions) -> Result<GoIndexResult> {
    let profile = go_profile();
    let lsp_options = LspIndexOptions {
        repo_root: options.repo_root.clone(),
        code_roots: options.code_roots.clone(),
        exclude_globs: options.exclude_globs.clone(),
        lsp_command: options.lsp_command.clone(),
    };
    match run_profile(&profile, &lsp_options)? {
        LspIndexOutcome::Skipped { reason, .. } => Ok(GoIndexResult {
            sidecar_skip_reason: reason,
            resolver_used: String::new(),
            ..Default::default()
        }),
        LspIndexOutcome::Indexed(boxed) => {
            let crate::lsp_indexer::LspIndexedBatch { batch, stats } = *boxed;
            ingest_language_batch_minimal(store, &batch, GO_INDEXER_NAME)?;
            Ok(GoIndexResult {
                files: stats.files,
                symbols: stats.symbols,
                resolver_used: GO_INDEXER_NAME.into(),
                sidecar_skip_reason: stats.skip_reason,
            })
        }
    }
}

pub fn build_go_batch(options: &GoIndexOptions) -> Result<Option<LanguageIndexBatch>> {
    let lsp_options = LspIndexOptions {
        repo_root: options.repo_root.clone(),
        code_roots: options.code_roots.clone(),
        exclude_globs: options.exclude_globs.clone(),
        lsp_command: options.lsp_command.clone(),
    };
    match run_profile(&go_profile(), &lsp_options)? {
        LspIndexOutcome::Indexed(boxed) => Ok(Some(boxed.batch)),
        LspIndexOutcome::Skipped { .. } => Ok(None),
    }
}

pub fn go_lsp_available(options: &GoIndexOptions) -> bool {
    let command = options
        .lsp_command
        .clone()
        .unwrap_or_else(|| "gopls".into());
    binary_on_path(&command)
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
