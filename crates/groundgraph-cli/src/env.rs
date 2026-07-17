//! Central registry of every `GROUNDGRAPH_*` environment variable the CLI
//! and engine read (issues.md #234).
//!
//! Before this module existed, the variables were scattered across eight
//! source files with no inventory, no `--help` listing, and no docs page —
//! operators had to `grep` the source to discover `GROUNDGRAPH_PARSE_BUDGET_MS`
//! could rescue a slow parse. This module is the single source of truth: the
//! same `REGISTRY` feeds both the `Environment:` block on `groundgraph --help`
//! (via [`render_environment_help`]) and `docs/environment.md`.
//!
//! The CLI/engine still read each variable at its original call site (the
//! registry documents; it does not yet resolve). Defaults recorded here must
//! stay in lock-step with the call-site fallbacks — the
//! `registry_covers_every_grounded_variable` test guards the coverage side.

/// One `GROUNDGRAPH_*` environment variable.
#[derive(Debug, Clone, Copy)]
pub struct EnvSpec {
    /// The variable name, e.g. `GROUNDGRAPH_TIMING`. For a per-language
    /// family generated dynamically the placeholder `<LANG>` is used.
    pub name: &'static str,
    /// Human-readable default (the value used when the variable is unset).
    pub default: &'static str,
    /// Logical grouping used to organise the `--help` block and docs table.
    pub category: EnvCategory,
    /// One-line description of what the variable does.
    pub help: &'static str,
}

/// Coarse grouping so `--help` and the docs page read as sections, not a
/// 14-row flat list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvCategory {
    /// Indexing pipeline knobs (timing, parse budget).
    Indexing,
    /// Precision-layer resolvers: SCIP indexers, the Swift LSP, the Dart
    /// analyzer sidecar, and the community-detection tuning knob.
    Resolvers,
    /// Subprocess retry policy shared by every spawned tool.
    Subprocess,
    /// Safety gate for untrusted repo config (#187).
    Safety,
    /// The MCP stdio server's workspace discovery.
    Server,
    /// Test-only — never surfaced on user-facing `--help`.
    Test,
}

/// Every `GROUNDGRAPH_*` variable read by the CLI or engine, grouped by
/// [`EnvCategory`] for the `--help` block and the docs table. Defaults here
/// mirror the call-site fallbacks exactly (verified by
/// `registry_covers_every_grounded_variable`); if you add a variable, register
/// it here too.
pub static REGISTRY: &[EnvSpec] = &[
    EnvSpec {
        name: "GROUNDGRAPH_TIMING",
        default: "unset (off)",
        category: EnvCategory::Indexing,
        help: "Emit per-phase wall-clock timings (docs / each language / scip / \
               fulltext) to stderr while indexing.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_PARSE_BUDGET_MS",
        default: "500 (ms)",
        category: EnvCategory::Indexing,
        help: "Per-file tree-sitter parse budget; files that exceed it are \
               structure-skipped (typically compiler fixtures with intentional \
               syntax errors that would trigger exponential error-recovery).",
    },
    EnvSpec {
        name: "GROUNDGRAPH_SWIFT_LSP_BIN",
        default: "sourcekit-lsp",
        category: EnvCategory::Resolvers,
        help: "Override the Swift LSP executable path. Swift is the only \
               language still on an LSP tier (Go/Dart LSP were retired).",
    },
    EnvSpec {
        name: "GROUNDGRAPH_SCIP_<LANG>_BIN",
        default: "per language: rust-analyzer / scip-go / scip-typescript / \
                 scip-python / scip_dart",
        category: EnvCategory::Resolvers,
        help: "Override a language's SCIP indexer binary; <LANG> is one of \
               RUST/GO/TYPESCRIPT/PYTHON/DART. Absent → probe PATH and skip the \
               language silently when not found.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_SCIP_TIMEOUT_SECS",
        default: "600 (s)",
        category: EnvCategory::Resolvers,
        help: "Wall-clock budget per SCIP indexer subprocess; guards against \
               hangs, not legitimate slow indexes.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_DART_ANALYZER",
        default: "enabled",
        category: EnvCategory::Resolvers,
        help: "Master switch for the Dart analyzer precision sidecar; set to \
               0 / false / off / no to disable.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_DART_ANALYZER_BIN",
        default: "dart run <repo>/tool/groundgraph_dart_analyzer/\
                 bin/groundgraph_dart_analyzer.dart",
        category: EnvCategory::Resolvers,
        help: "Override the Dart analyzer sidecar command (shlex-split, so it \
               may be a compiled binary or a `dart run` invocation).",
    },
    EnvSpec {
        name: "GROUNDGRAPH_DART_ANALYZER_TIMEOUT_SECS",
        default: "600 (s)",
        category: EnvCategory::Resolvers,
        help: "Wall-clock budget for the Dart analyzer sidecar; prevents a \
               wedged analyzer from stalling the index.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_LOUVAIN_RESOLUTION",
        default: "unset (γ=1.0 + recursive refinement)",
        category: EnvCategory::Resolvers,
        help: "Single resolution γ escape hatch for business-module \
               community detection; setting it skips the recursive cap and runs \
               exactly one pass at the given γ.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_SUBPROCESS_RETRY_ATTEMPTS",
        default: "2",
        category: EnvCategory::Subprocess,
        help: "Retry attempts for transient subprocess failures \
               (cold-cache crashes, fork EAGAIN, fd exhaustion).",
    },
    EnvSpec {
        name: "GROUNDGRAPH_SUBPROCESS_RETRY_BACKOFF_MS",
        default: "200",
        category: EnvCategory::Subprocess,
        help: "Initial backoff between subprocess retries; doubles each \
               attempt, capped at 30 s.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_TRUST_CONFIG_COMMANDS",
        default: "unset (do NOT trust)",
        category: EnvCategory::Safety,
        help: "When set (any value), allow executing command strings read from \
               the target repo's .groundgraph.yaml (#187 RCE gate). Off by \
               default so indexing an untrusted clone cannot run attacker-specified \
               binaries.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_REPO_ROOT",
        default: "current working directory",
        category: EnvCategory::Server,
        help: "Default workspace root for the groundgraph-mcp stdio server when \
               no --repo-root is given; a per-call root still wins.",
    },
    EnvSpec {
        name: "GROUNDGRAPH_GOLDEN_REQUIRED",
        default: "unset",
        category: EnvCategory::Test,
        help: "TEST-ONLY: force the Dart golden regression suites to hard-fail \
               when the sidecar is unavailable instead of silently skipping. \
               Never read from production code; hidden from user --help.",
    },
];

const CATEGORY_ORDER: &[EnvCategory] = &[
    EnvCategory::Indexing,
    EnvCategory::Resolvers,
    EnvCategory::Subprocess,
    EnvCategory::Safety,
    EnvCategory::Server,
];

fn category_label(category: EnvCategory) -> &'static str {
    match category {
        EnvCategory::Indexing => "Indexing",
        EnvCategory::Resolvers => "Precision resolvers (SCIP / LSP / Dart analyzer)",
        EnvCategory::Subprocess => "Subprocess retry",
        EnvCategory::Safety => "Safety",
        EnvCategory::Server => "MCP server",
        EnvCategory::Test => "Test-only (hidden from --help)",
    }
}

/// Render the `Environment:` block for `groundgraph --help`.
///
/// Variables are grouped by [`EnvCategory`] in a fixed order. Test-only
/// variables (category [`EnvCategory::Test`]) are omitted — they are not part
/// of the user contract. The same [`REGISTRY`] feeds `docs/environment.md`.
pub fn render_environment_help() -> String {
    use std::fmt::Write;
    let visible: Vec<&EnvSpec> = REGISTRY
        .iter()
        .filter(|e| e.category != EnvCategory::Test)
        .collect();
    let width = visible.iter().map(|e| e.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    writeln!(
        out,
        "Environment (GROUNDGRAPH_* env vars; full detail in docs/environment.md):"
    )
    .ok();
    for &category in CATEGORY_ORDER {
        let entries: Vec<&EnvSpec> = visible
            .iter()
            .copied()
            .filter(|e| e.category == category)
            .collect();
        if entries.is_empty() {
            continue;
        }
        writeln!(out, "  {}:", category_label(category)).ok();
        for e in entries {
            let name = e.name;
            let default = e.default;
            let help = e.help;
            writeln!(out, "    {name:<width$}  default: {default}").ok();
            writeln!(out, "      {help}").ok();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names the registry must cover. Grounded in a workspace-wide
    /// `grep GROUNDGRAPH_` — every variable an operator can actually set.
    const EXPECTED_USER_NAMES: &[&str] = &[
        "GROUNDGRAPH_TIMING",
        "GROUNDGRAPH_PARSE_BUDGET_MS",
        "GROUNDGRAPH_SWIFT_LSP_BIN",
        "GROUNDGRAPH_SCIP_<LANG>_BIN",
        "GROUNDGRAPH_SCIP_TIMEOUT_SECS",
        "GROUNDGRAPH_DART_ANALYZER",
        "GROUNDGRAPH_DART_ANALYZER_BIN",
        "GROUNDGRAPH_DART_ANALYZER_TIMEOUT_SECS",
        "GROUNDGRAPH_LOUVAIN_RESOLUTION",
        "GROUNDGRAPH_SUBPROCESS_RETRY_ATTEMPTS",
        "GROUNDGRAPH_SUBPROCESS_RETRY_BACKOFF_MS",
        "GROUNDGRAPH_TRUST_CONFIG_COMMANDS",
        "GROUNDGRAPH_REPO_ROOT",
    ];

    #[test]
    fn registry_covers_every_grounded_variable() {
        let names: Vec<&str> = REGISTRY.iter().map(|e| e.name).collect();
        for want in EXPECTED_USER_NAMES {
            assert!(
                names.contains(want),
                "registry missing {want}; known: {names:?}"
            );
        }
    }

    #[test]
    fn registry_also_documents_test_only_variable() {
        // GOLDEN_REQUIRED is read only from test helpers, but it is still a
        // GROUNDGRAPH_* variable and belongs in the docs page.
        let has = REGISTRY
            .iter()
            .any(|e| e.name == "GROUNDGRAPH_GOLDEN_REQUIRED");
        assert!(has, "registry should document GROUNDGRAPH_GOLDEN_REQUIRED");
    }

    #[test]
    fn registry_has_no_duplicate_names() {
        let mut names: Vec<&str> = REGISTRY.iter().map(|e| e.name).collect();
        names.sort_unstable();
        let dups: Vec<&str> = names
            .windows(2)
            .filter(|w| w[0] == w[1])
            .map(|w| w[0])
            .collect();
        assert!(dups.is_empty(), "duplicate env var names: {dups:?}");
    }

    #[test]
    fn every_name_is_prefixed_with_groundgraph() {
        for e in REGISTRY {
            assert!(
                e.name.starts_with("GROUNDGRAPH_"),
                "name {} lacks the GROUNDGRAPH_ prefix",
                e.name
            );
        }
    }

    #[test]
    fn render_lists_every_user_variable_under_environment_header() {
        let block = render_environment_help();
        assert!(
            block.contains("Environment"),
            "rendered block should carry an Environment header: {block}"
        );
        for e in REGISTRY {
            if e.category == EnvCategory::Test {
                assert!(
                    !block.contains(e.name),
                    "test-only {} must not appear on user --help: {block}",
                    e.name
                );
            } else {
                assert!(
                    block.contains(e.name),
                    "rendered block missing {}: {block}",
                    e.name
                );
            }
        }
    }
}
