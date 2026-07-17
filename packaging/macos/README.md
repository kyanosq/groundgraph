# GroundGraph macOS Universal Package

This package contains the closed-source GroundGraph Rust CLI binary for macOS
Apple Silicon and Intel, plus the Dart analyzer sidecar source that enables
higher-precision Flutter/Dart indexing.

## Contents

```text
bin/groundgraph
bin/groundgraph-mcp
libexec/groundgraph
libexec/groundgraph-mcp
tool/groundgraph_dart_analyzer/
skills/groundgraph/
README.md
README-AI-SKILL.md
BUILD-INFO.txt
```

`bin/groundgraph` and `bin/groundgraph-mcp` are the user-facing wrappers;
`libexec/groundgraph` and `libexec/groundgraph-mcp` are the universal binaries
(arm64 + x86_64). The Dart analyzer sidecar is intentionally included as source
under `tool/groundgraph_dart_analyzer/`. To expose the graph to AI agents over
MCP, point a stdio MCP client at `bin/groundgraph-mcp` — no separate install
needed.

## Install

```bash
tar -xzf groundgraph-<VERSION>-macos-universal.tar.gz
sudo cp -R groundgraph-<VERSION>-macos-universal /usr/local/groundgraph
sudo ln -sf /usr/local/groundgraph/bin/groundgraph /usr/local/bin/groundgraph
sudo ln -sf /usr/local/groundgraph/bin/groundgraph-mcp /usr/local/bin/groundgraph-mcp   # optional, for AI agents
groundgraph --help
```

Replace `<VERSION>` with the package version (e.g. `0.3.0`).

If you do not want to use `/usr/local`, put the extracted directory anywhere
and add its `bin` directory to PATH.

## Supported languages (0.3.0)

GroundGraph indexes in two tiers. **Breadth** is always available (the
tree-sitter grammars are linked into the binary — no external tool, no
network). **Precision** call/reference edges come from an optional, offline
**SCIP overlay** that `index` auto-invokes when the indexer is installed.

> The standalone LSP tier for Go / Python / TypeScript / Java was **retired**
> (ADR-0001 §8.8); precision for those languages now comes from SCIP, not LSP.
> Swift still supports an optional `sourcekit-lsp` overlay, so `swift.lsp_command`
> remains meaningful when explicitly trusted by the operator.

| Tier | Mechanism | Languages |
|------|-----------|-----------|
| Breadth (default) | In-process **tree-sitter** | Rust, TypeScript, Python, Go, Java, C, C++, Swift, C#, Ruby, PHP, Kotlin |
| Dart | Bundled **`groundgraph_dart_analyzer`** sidecar (resolved AST, domain-aware) | Dart |
| Docs | Markdown / RST / AsciiDoc / requirements / ADR | `.md`, `.mdx`, `.rst`, `.adoc` |

### Optional precision overlay (SCIP)

For precise `Calls` / `References` edges, install a SCIP indexer for the
language; `index` runs it automatically and ingests the result. Without it
you still get the full structural graph plus heuristic call/reference edges.

| Language | Indexer | Install |
|----------|---------|---------|
| Rust | `rust-analyzer scip` | `rustup component add rust-analyzer` |
| Go | `scip-go` | `go install github.com/sourcegraph/scip-go/cmd/scip-go@latest` |
| TypeScript | `scip-typescript` | `npm i -g @sourcegraph/scip-typescript` |
| Python | `scip-python` | `npm i -g @sourcegraph/scip-python` |

Point GroundGraph at a specific indexer binary with `GROUNDGRAPH_SCIP_<LANG>_BIN`
(e.g. `GROUNDGRAPH_SCIP_RUST_BIN`).
A missing or failing indexer is a clear,
non-fatal "structure-only" note in the `index` output — never an error.

### Enable an opt-in adapter

`groundgraph init` writes every non-Dart adapter to `.groundgraph.yaml` with
`enabled: false` so a fresh workspace never pulls in unrelated languages.
To turn one on, edit the matching block (or the unified `languages:`
selector) and re-index. Example for a TypeScript project:

```yaml
typescript:
  enabled: true
  paths: [src, tests]   # roots where GroundGraph should look
  exclude: []
```

```bash
groundgraph --repo-root /path/to/repo index
# look for the `TypeScript index:` block in the output
```

Same shape for `swift`, `go`, `python`, `java` and the tree-sitter breadth
languages.

For Swift precision, install `sourcekit-lsp` through Xcode / Swift toolchains
and either keep auto-discovery or set the trusted operator environment variable
`GROUNDGRAPH_SWIFT_LSP_BIN`.
A repo-provided `swift.lsp_command` is ignored unless
`GROUNDGRAPH_TRUST_CONFIG_COMMANDS=1` is set for that workspace.

## Dart analyzer sidecar

For Flutter/Dart repositories, install a Dart or Flutter SDK on the machine:

```bash
dart --version
# or
flutter --version
```

When Dart is available, GroundGraph automatically probes the bundled sidecar at:

```text
tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart
```

If Dart is unavailable, GroundGraph falls back to the lightweight scanner and
prints a skip reason. The fallback is usable, but less precise.

## Basic Usage

```bash
groundgraph --repo-root /path/to/repo init
groundgraph --repo-root /path/to/repo index
groundgraph --repo-root /path/to/repo check
groundgraph --repo-root /path/to/repo logic --only-risks
groundgraph --repo-root /path/to/repo graph --format html --view code
groundgraph --repo-root /path/to/repo graph --format html --view business
groundgraph --repo-root /path/to/repo search "purchase pro" --format html
groundgraph --repo-root /path/to/repo dead-code --json --min-confidence medium
```

HTML graph output defaults to:

```text
/path/to/repo/.groundgraph/export/graph.html
```

Search HTML output defaults to:

```text
/path/to/repo/.groundgraph/export/search-<query>.html
```

`dead-code` emits possible dead-code candidates with confidence and reasons.
It does not delete files and should not be treated as proof that code is
removable. Use `--include-tests` only when you want orphan test facts included;
test helper functions under `test/` are filtered from production findings.

## AI Candidate Flow

```bash
groundgraph --repo-root /path/to/repo connect propose --pretty --out /tmp/groundgraph-evidence.json
groundgraph --repo-root /path/to/repo candidate list
groundgraph --repo-root /path/to/repo candidate review <candidate-id> --accept --note "用户确认"
```

AI candidates are not confirmed business logic until a human reviews them.
GroundGraph does not require annotations in production code, tests, or docs.

## Uninstall

```bash
sudo rm -f /usr/local/bin/groundgraph /usr/local/bin/groundgraph-mcp
sudo rm -rf /usr/local/groundgraph
```
