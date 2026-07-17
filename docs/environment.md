# Environment variables

GroundGraph reads a small set of `GROUNDGRAPH_*` environment variables. They
are also listed under the `Environment:` section of `groundgraph --help`,
which is generated from the same registry
(`crates/groundgraph-cli/src/env.rs`). When you add or change a variable,
update that registry and this page together.

`RUST_LOG` (the standard `tracing`/`tracing-subscriber` filter) is honoured
by the CLI and overrides `-v`/`-q`; it is not a `GROUNDGRAPH_*` variable and
is documented under `groundgraph --help`'s `-v` flag instead.

## Indexing

| Variable | Default | Description |
|---|---|---|
| `GROUNDGRAPH_TIMING` | unset (off) | Emit per-phase wall-clock timings (docs / each language / scip / fulltext) to stderr while indexing. Any non-empty value enables it. |
| `GROUNDGRAPH_PARSE_BUDGET_MS` | `500` (ms) | Per-file tree-sitter parse budget; files that exceed it are structure-skipped (typically compiler fixtures with intentional syntax errors that would trigger exponential error-recovery). |

## Precision resolvers (SCIP / LSP / Dart analyzer)

| Variable | Default | Description |
|---|---|---|
| `GROUNDGRAPH_SWIFT_LSP_BIN` | `sourcekit-lsp` | Override the Swift LSP executable path. Swift is the only language still on an LSP tier (Go/Dart LSP were retired in favour of tree-sitter + SCIP). |
| `GROUNDGRAPH_SCIP_<LANG>_BIN` | per language: `rust-analyzer` / `scip-go` / `scip-typescript` / `scip-python` / `scip_dart` | Override a language's SCIP indexer binary; `<LANG>` is one of `RUST`/`GO`/`TYPESCRIPT`/`PYTHON`/`DART`. Absent → probe `PATH` and skip the language silently when not found. |
| `GROUNDGRAPH_SCIP_TIMEOUT_SECS` | `600` (s) | Wall-clock budget per SCIP indexer subprocess; guards against hangs, not legitimate slow indexes. |
| `GROUNDGRAPH_DART_ANALYZER` | enabled | Master switch for the Dart analyzer precision sidecar; set to `0` / `false` / `off` / `no` to disable. |
| `GROUNDGRAPH_DART_ANALYZER_BIN` | `dart run <repo>/tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart` | Override the Dart analyzer sidecar command (shlex-split, so it may be a compiled binary or a `dart run` invocation). |
| `GROUNDGRAPH_DART_ANALYZER_TIMEOUT_SECS` | `600` (s) | Wall-clock budget for the Dart analyzer sidecar; prevents a wedged analyzer from stalling the index. |
| `GROUNDGRAPH_LOUVAIN_RESOLUTION` | unset (γ=1.0 + recursive refinement) | Single resolution γ escape hatch for business-module community detection; setting it skips the recursive cap and runs exactly one pass at the given γ. |

## Subprocess retry

Shared by every spawned tool (sourcekit-lsp, scip-*, dart analyzer). Used to
ride out cold-cache crashes, fork `EAGAIN`, and transient fd exhaustion.

| Variable | Default | Description |
|---|---|---|
| `GROUNDGRAPH_SUBPROCESS_RETRY_ATTEMPTS` | `2` | Retry attempts for transient subprocess failures (`NotFound` / `TimedOut` / exit 2 / 127 are not retried). |
| `GROUNDGRAPH_SUBPROCESS_RETRY_BACKOFF_MS` | `200` | Initial backoff between retries; doubles each attempt, capped at 30 s. |

## Safety

| Variable | Default | Description |
|---|---|---|
| `GROUNDGRAPH_TRUST_CONFIG_COMMANDS` | unset (do NOT trust) | When set (any value), allow executing command strings read from the target repo's `.groundgraph.yaml` (the `*_command` fields — issues.md #187 RCE gate). Off by default so indexing an untrusted clone cannot run attacker-specified binaries. Overrides an operator sets via `GROUNDGRAPH_SWIFT_LSP_BIN` / `GROUNDGRAPH_SCIP_*_BIN` are not affected — those do not come from repo config. |

## MCP server

| Variable | Default | Description |
|---|---|---|
| `GROUNDGRAPH_REPO_ROOT` | current working directory | Default workspace root for the `groundgraph-mcp` stdio server when no `--repo-root` is given; an explicit per-call root still wins. |

## Test-only

These are read only from test helpers, never from production code, and are
omitted from the user-facing `--help`. Documented here so the registry stays
the complete inventory.

| Variable | Default | Description |
|---|---|---|
| `GROUNDGRAPH_GOLDEN_REQUIRED` | unset | Force the Dart golden regression suites to hard-fail when the sidecar is unavailable instead of silently skipping. Set on CI so a host without `dart` cannot pass the golden net vacuously. |
