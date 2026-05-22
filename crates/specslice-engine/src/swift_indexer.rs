//! Swift language adapter (P11) — drives `sourcekit-lsp` over LSP.
//!
//! `sourcekit-lsp` ships with every Swift toolchain (Xcode-bundled on
//! macOS, `swift-lsp` on Linux). When it is on `PATH` the engine asks
//! it for `textDocument/documentSymbol` on every `*.swift` file under
//! the configured `code_paths`. The symbol tree is mapped onto the
//! SpecSlice `NodeKind` variants added in P11:
//!
//! - `Class` → [`NodeKind::SwiftClass`]
//! - `Struct` → [`NodeKind::SwiftStruct`]
//! - `Enum` → [`NodeKind::SwiftEnum`]
//! - `Interface` (Swift `protocol`) → [`NodeKind::SwiftProtocol`]
//! - `Method` → [`NodeKind::SwiftMethod`]
//! - `Function` → [`NodeKind::SwiftFunction`]
//! - `Constructor` → [`NodeKind::SwiftInitializer`]
//!
//! Everything else (properties, fields, enum members, variables) is
//! dropped to keep the graph focused on symbols that can plausibly own
//! a `calls` / `references` edge. Properties / cases are still
//! reachable via their parent type once we wire call hierarchy.

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

pub const SWIFT_INDEXER_NAME: &str = "swift_lsp";
pub const SWIFT_LANGUAGE_ID: &str = "swift";
pub const SWIFT_LSP_COMMAND_ENV: &str = "SPECSLICE_SWIFT_LSP_BIN";

#[derive(Debug, Clone, Default)]
pub struct SwiftIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    pub lsp_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SwiftIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

/// Index a repository's Swift sources. Returns gracefully (with a
/// skip reason) when `sourcekit-lsp` is not on PATH or no `*.swift`
/// files are present — same UX as the Dart sidecar.
pub fn index_swift(store: &mut Store, options: &SwiftIndexOptions) -> Result<SwiftIndexResult> {
    let profile = swift_profile();
    let lsp_options = LspIndexOptions {
        repo_root: options.repo_root.clone(),
        code_roots: options.code_roots.clone(),
        exclude_globs: options.exclude_globs.clone(),
        lsp_command: options.lsp_command.clone(),
    };
    match run_profile(&profile, &lsp_options)? {
        LspIndexOutcome::Skipped { reason, .. } => Ok(SwiftIndexResult {
            sidecar_skip_reason: reason,
            resolver_used: String::new(),
            ..Default::default()
        }),
        LspIndexOutcome::Indexed(boxed) => {
            let crate::lsp_indexer::LspIndexedBatch { batch, stats } = *boxed;
            ingest_language_batch_minimal(store, &batch, SWIFT_INDEXER_NAME)?;
            Ok(SwiftIndexResult {
                files: stats.files,
                symbols: stats.symbols,
                resolver_used: SWIFT_INDEXER_NAME.into(),
                sidecar_skip_reason: stats.skip_reason,
            })
        }
    }
}

/// Lower-level entry: build the [`LanguageIndexBatch`] without
/// touching the store. Useful for unit tests and for the MCP
/// `index_dry_run` tool we expect to add in the next iteration.
pub fn build_swift_batch(options: &SwiftIndexOptions) -> Result<Option<LanguageIndexBatch>> {
    let lsp_options = LspIndexOptions {
        repo_root: options.repo_root.clone(),
        code_roots: options.code_roots.clone(),
        exclude_globs: options.exclude_globs.clone(),
        lsp_command: options.lsp_command.clone(),
    };
    let profile = swift_profile();
    match run_profile(&profile, &lsp_options)? {
        LspIndexOutcome::Indexed(boxed) => Ok(Some(boxed.batch)),
        LspIndexOutcome::Skipped { .. } => Ok(None),
    }
}

/// True when `sourcekit-lsp` (or the override binary) is actually
/// runnable on this host — i.e. it can be launched, exits 0 from
/// `--help` within the smoke timeout, and does not emit a known
/// "broken stub" stderr marker (the most common real-world failure
/// being `SOURCEKITD FATAL ERROR: Service is invalid` when the
/// IndexStoreDB cache is stale or the toolchain mismatches).
///
/// This is intentionally stricter than the historical
/// `binary_on_path` check, which let unusable binaries slip through
/// and cascade into hard test failures during the v0.2.0 close-out.
pub fn swift_lsp_available(options: &SwiftIndexOptions) -> bool {
    let command = options
        .lsp_command
        .clone()
        .unwrap_or_else(|| "sourcekit-lsp".into());
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

fn swift_profile() -> LspProfile {
    LspProfile {
        language: "swift",
        language_id: SWIFT_LANGUAGE_ID,
        file_extensions: &["swift"],
        // `.build` is SwiftPM's output directory; `Pods` is CocoaPods'
        // cache; `.swiftpm` holds local-only tooling state.
        skip_dirs: &[".build", "Pods", ".swiftpm", "DerivedData"],
        skip_suffixes: &[],
        default_command: "sourcekit-lsp",
        default_args: &[],
        command_env_var: SWIFT_LSP_COMMAND_ENV,
        map_kind: swift_map_kind,
        qualify: swift_qualify,
    }
}

fn swift_map_kind(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
    match kind {
        LspSymbolKind::Class => Some(NodeKind::SwiftClass),
        LspSymbolKind::Struct => Some(NodeKind::SwiftStruct),
        LspSymbolKind::Enum => Some(NodeKind::SwiftEnum),
        // `sourcekit-lsp` reports Swift `protocol` declarations as
        // `Interface` per the LSP spec (mapping is documented in
        // `Sources/SourceKitLSP/Swift/SymbolKindExtensions.swift`).
        LspSymbolKind::Interface => Some(NodeKind::SwiftProtocol),
        LspSymbolKind::Constructor => Some(NodeKind::SwiftInitializer),
        LspSymbolKind::Method => Some(NodeKind::SwiftMethod),
        LspSymbolKind::Function => Some(NodeKind::SwiftFunction),
        _ => None,
    }
}

fn swift_qualify(file_rel: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}.{name}"),
        None => format!("{file_rel}::{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_documentsymbol_kinds_to_expected_nodekinds() {
        assert_eq!(
            swift_map_kind(LspSymbolKind::Class, None),
            Some(NodeKind::SwiftClass)
        );
        assert_eq!(
            swift_map_kind(LspSymbolKind::Struct, None),
            Some(NodeKind::SwiftStruct)
        );
        assert_eq!(
            swift_map_kind(LspSymbolKind::Enum, None),
            Some(NodeKind::SwiftEnum)
        );
        assert_eq!(
            swift_map_kind(LspSymbolKind::Interface, None),
            Some(NodeKind::SwiftProtocol)
        );
        assert_eq!(
            swift_map_kind(LspSymbolKind::Constructor, None),
            Some(NodeKind::SwiftInitializer)
        );
        assert_eq!(
            swift_map_kind(LspSymbolKind::Method, None),
            Some(NodeKind::SwiftMethod)
        );
        assert_eq!(
            swift_map_kind(LspSymbolKind::Function, None),
            Some(NodeKind::SwiftFunction)
        );
        assert_eq!(swift_map_kind(LspSymbolKind::Variable, None), None);
        assert_eq!(swift_map_kind(LspSymbolKind::Property, None), None);
    }

    #[test]
    fn qualified_names_use_dot_for_nested_swift_members() {
        assert_eq!(
            swift_qualify("Sources/App/Greeter.swift", None, "Greeter"),
            "Sources/App/Greeter.swift::Greeter"
        );
        assert_eq!(
            swift_qualify(
                "Sources/App/Greeter.swift",
                Some("Sources/App/Greeter.swift::Greeter"),
                "greet"
            ),
            "Sources/App/Greeter.swift::Greeter.greet"
        );
    }

    #[test]
    fn swift_lsp_available_respects_override_and_path() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = SwiftIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: Vec::new(),
            exclude_globs: Vec::new(),
            lsp_command: Some("specslice_nonexistent_swift_lsp_999".into()),
        };
        assert!(!swift_lsp_available(&opts));
    }
}
