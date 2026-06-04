//! P11/P23.5 — Swift language adapter.
//!
//! Since the P23 收敛, the in-process tree-sitter driver
//! ([`crate::swift_treesitter`]) is the **sole structural source of truth**
//! for Swift: classes / structs / enums / actors / protocols, their
//! methods / initializers / deinitializers, free functions, XCTest &
//! swift-testing cases, and `import` declarations. Output is tagged
//! `indexer = swift_treesitter`.
//!
//! `sourcekit-lsp` (Xcode-bundled on macOS, `swift-lsp` on Linux) is an
//! **optional Tier-3 enrichment**: when discovered it contributes only the
//! semantic `Calls` / `References` edges, overlaid onto the existing
//! tree-sitter symbol ids — the LSP id scheme `swift::<file>::<qualified>`
//! is identical to the tree-sitter one by construction (see
//! [`swift_qualify`]). LSP edges are tagged `indexer = swift_lsp`. When no
//! `sourcekit-lsp` is available the structural graph is already complete.

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
    #[serde(default)]
    pub tests: usize,
    #[serde(default)]
    pub imports: usize,
    /// Number of `Calls` / `References` edges contributed by the optional
    /// Tier-3 LSP enrichment pass (0 when no `sourcekit-lsp` was available).
    #[serde(default)]
    pub references: usize,
    /// `swift_treesitter` when the structural pass produced anything, empty
    /// when no Swift files were found.
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

/// Index a repository's Swift sources. The tree-sitter driver produces the
/// entire structural graph; an optional `sourcekit-lsp` pass then overlays
/// `Calls` / `References`. Returns gracefully (empty `resolver_used`) when
/// no `*.swift` files are present.
pub fn index_swift(store: &mut Store, options: &SwiftIndexOptions) -> Result<SwiftIndexResult> {
    let spec = &crate::swift_treesitter::SWIFT_SPEC;
    let ts_name = treesitter::indexer_name(spec);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous Swift tree-sitter outputs")?;
    store
        .clear_indexer_outputs(SWIFT_INDEXER_NAME)
        .context("clearing previous Swift LSP outputs")?;

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
    .context("indexing Swift structure via tree-sitter")?;

    // Id set of structural nodes so the optional LSP pass attaches edges
    // without dangling targets.
    let mut known_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in store.list_all_nodes().context("listing nodes")? {
        if node.indexer.as_deref() == Some(ts_name.as_str()) {
            known_ids.insert(node.id.to_string());
        }
    }

    // Tier 3 (optional): sourcekit-lsp `Calls` / `References` enrichment
    // overlaid onto the tree-sitter symbol ids (identical id scheme). The
    // shared LSP layer's skip reasons (PATH hints, warmup notes) carry
    // through unchanged.
    let lsp_options = LspIndexOptions {
        repo_root: options.repo_root.clone(),
        code_roots: options.code_roots.clone(),
        exclude_globs: options.exclude_globs.clone(),
        lsp_command: options.lsp_command.clone(),
    };
    let mut references = 0usize;
    let skip_reason = match run_profile(&swift_profile(), &lsp_options)? {
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
                    language: SWIFT_LANGUAGE_ID.into(),
                    references: refs,
                    ..Default::default()
                };
                ingest_language_batch_minimal(store, &refs_batch, SWIFT_INDEXER_NAME)
                    .context("ingesting Swift LSP reference edges")?;
            }
            stats.skip_reason
        }
        LspIndexOutcome::Skipped { reason, .. } => reason,
    };

    Ok(SwiftIndexResult {
        files: ts.files,
        symbols: ts.symbols,
        tests: ts.tests,
        imports: ts.imports,
        references,
        resolver_used: ts.resolver_used,
        sidecar_skip_reason: skip_reason,
    })
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
