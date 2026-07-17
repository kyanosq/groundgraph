//! Typed errors for the engine's public surface (#166).
//!
//! Before this module every public workflow returned `anyhow::Result<T>`.
//! `anyhow::Error` is type-erased, so callers (the CLI, the MCP server) could
//! only `format!("{err}")` a flat string — they could not tell *"the repo has
//! no `.groundgraph.yaml`, run `init`"* (a user error) from *"the SQLite db is
//! corrupt, rebuild it"* (an operator error) from *"the indexer hit an
//! invariant violation"* (a bug). The MCP `tools/call` path folded all of
//! them into a single `ToolCallResult::err(message)`, dropping the
//! `INVALID_PARAMS` vs `INTERNAL_ERROR` split.
//!
//! [`EngineError`] restores that classification. It is a `thiserror` enum with
//! one variant per *source* of failure. The coarse operational class — what
//! the #233 exit-code contract and the MCP error-code split consume — is
//! exposed via [`EngineError::kind`] / [`ErrorKind`].
//!
//! ## Migration shape
//!
//! Private helpers keep returning `anyhow::Result`; they bubble through the
//! public boundary as [`EngineError::Internal`]. The two `#[from]` impls make
//! a public entry-point body compile almost unchanged: a `?` on a
//! `StoreResult` routes to [`EngineError::Store`], a `?` on an `anyhow::Result`
//! routes to [`EngineError::Internal`] (its `Display` keeps the full
//! `with_context` chain, so human-readable messages do not regress), and a `?`
//! on an `EngineResult` is the identity. Only the few failure sites that must
//! be *programmatically* distinguishable (no workspace, missing artifact, bad
//! config) opt into a specific variant.

use std::path::PathBuf;

use groundgraph_store::StoreError;
use thiserror::Error;

/// Convenience alias used by every public engine workflow.
pub type EngineResult<T> = Result<T, EngineError>;

/// Why an engine workflow failed, grouped by source so callers can react.
///
/// The variants are deliberately coarse about *what* went wrong and let the
/// inner error carry the detail (`#[error(transparent)]` for the two wrappers,
/// explicit `path` / `what` fields where the location itself is the message).
/// Variants:
///
/// - [`Store`](EngineError::Store) — SQLite layer failure; delegates to
///   [`StoreError`], whose own variants already carry the path and the SQLite
///   result code (busy / corrupt / read-only / disk-full / …).
/// - [`Io`](EngineError::Io) — filesystem I/O *outside* the SQLite layer
///   (reading source/doc bytes, atomic writes).
/// - [`NoWorkspace`](EngineError::NoWorkspace) — no `.groundgraph.yaml`; the
///   user must `groundgraph init` first.
/// - [`Config`](EngineError::Config) — the config file is present but invalid
///   (YAML parse failure, forbidden `storage.path` escape, …).
/// - [`NotFound`](EngineError::NotFound) — a requested symbol / requirement /
///   file is absent from the indexed graph. The db is fine; the thing asked
///   for simply is not there.
/// - [`Subprocess`](EngineError::Subprocess) — an external tool the engine
///   shells out to (`git`, `sourcekit-lsp`, `scip-*`, the Dart analyzer
///   sidecar) failed to spawn or exited non-zero.
/// - [`Parse`](EngineError::Parse) — structured bytes already on disk could
///   not be parsed (SCIP protobuf, YAML, JSON, a tree-sitter capture).
/// - [`InvalidInput`](EngineError::InvalidInput) — a caller-supplied argument
///   is bad (unknown git ref, out-of-range value).
/// - [`Internal`](EngineError::Internal) — catch-all for anything still
///   flowing through `anyhow` (indexer invariants, not-yet-migrated paths).
#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error("{context} {path}: {source}")]
    Io {
        context: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no GroundGraph workspace at {repo_root:?}: run `groundgraph init` first")]
    NoWorkspace { repo_root: PathBuf },

    /// `message` carries the full human-readable detail (callers fold the path
    /// in so the rendered string is self-contained); `path` is the structured
    /// location for programmatic matching.
    #[error("{message}")]
    Config {
        message: String,
        path: Option<PathBuf>,
    },

    /// `what` carries the full human-readable detail so the rendered string
    /// does not regress versus the pre-#166 `anyhow` message.
    #[error("{what}")]
    NotFound { what: String },

    #[error("subprocess `{tool}` failed: {message}")]
    Subprocess { tool: String, message: String },

    #[error("parsing {what} failed: {message}")]
    Parse { what: &'static str, message: String },

    #[error("{0}")]
    InvalidInput(String),

    /// Catch-all for failures that do not yet map to a more specific variant.
    /// `#[error(transparent)]` keeps the inner `anyhow` chain's `Display`
    /// intact, so `format!("{err:#}")` at the CLI/MCP boundary loses no
    /// message text relative to the pre-#166 behaviour.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl EngineError {
    /// Coarse operational class of this failure.
    ///
    /// This is the seam the #233 exit-code contract and the MCP
    /// `INVALID_PARAMS` / `INTERNAL_ERROR` split consume: it collapses the
    /// fine-grained variants into the handful of reactions a caller has
    /// (fix the invocation / fix the environment / report a bug).
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::NoWorkspace { .. } | Self::Config { .. } | Self::InvalidInput(_) => {
                ErrorKind::UserInput
            }
            Self::NotFound { .. } => ErrorKind::NotFound,
            Self::Store(_) | Self::Io { .. } | Self::Subprocess { .. } | Self::Parse { .. } => {
                ErrorKind::Operational
            }
            Self::Internal(_) => ErrorKind::Internal,
        }
    }

    /// Whether the caller could plausibly make progress by retrying the same
    /// call without changing anything. Mirrors [`StoreError::is_retryable`] at
    /// the engine layer: a contended SQLite lock may clear, everything else
    /// (including every user/internal class) needs action first.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Store(store) => store.is_retryable(),
            _ => false,
        }
    }
}

/// Operational class of an [`EngineError`], for exit-code / MCP-code routing.
///
/// Kept intentionally small — callers do not branch on the enum variant, they
/// branch on *what to do next*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The caller's invocation or config is wrong (no workspace, bad config,
    /// invalid argument). Fix the call, not the environment.
    UserInput,
    /// The requested symbol / requirement / file is absent from the graph.
    /// The store is healthy; the query target simply is not indexed.
    NotFound,
    /// An environment problem — store / I/O / subprocess / parse failure.
    /// Often operator-fixable (rebuild the index, install the tool, free
    /// space); sometimes retryable (see [`EngineError::is_retryable`]).
    Operational,
    /// An unexpected failure — an invariant violation or a not-yet-classified
    /// `anyhow` path. Treat as a bug.
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_classifies_user_vs_notfound_vs_operational_vs_internal() {
        assert_eq!(
            EngineError::NoWorkspace {
                repo_root: PathBuf::from("/r")
            }
            .kind(),
            ErrorKind::UserInput
        );
        assert_eq!(
            EngineError::Config {
                message: "bad".into(),
                path: None
            }
            .kind(),
            ErrorKind::UserInput
        );
        assert_eq!(
            EngineError::InvalidInput("nope".into()).kind(),
            ErrorKind::UserInput
        );
        assert_eq!(
            EngineError::NotFound {
                what: "symbol x".into()
            }
            .kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            EngineError::Internal(anyhow::anyhow!("boom")).kind(),
            ErrorKind::Internal
        );
    }

    #[test]
    fn store_variant_delegates_retryability() {
        // A contended lock is the one retryable store failure (#215).
        // SQLITE_BUSY = 5. `is_retryable` keys off the variant name, so the
        // exact code only needs to round-trip through rusqlite's constructors.
        let busy = StoreError::Busy(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(5),
            None,
        ));
        let err = EngineError::Store(busy);
        assert!(err.is_retryable());
        assert_eq!(err.kind(), ErrorKind::Operational);
    }

    #[test]
    fn anyhow_error_routes_to_internal_and_keeps_its_message() {
        // The pre-#166 surface was `anyhow::Result`; that chain's `Display`
        // must survive the `Internal(#[from] anyhow::Error)` wrap so CLI/MCP
        // output does not regress.
        let inner = anyhow::Error::msg("indexing Dart sources")
            .context("opening SQLite database at /r/.groundgraph/graph.db");
        let err: EngineError = inner.into();
        assert!(matches!(err, EngineError::Internal(_)));
        let rendered = format!("{err:#}");
        assert!(rendered.contains("indexing Dart sources"), "{rendered}");
        assert!(rendered.contains("graph.db"), "{rendered}");
    }
}
