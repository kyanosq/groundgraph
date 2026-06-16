# SpecSlice macOS Universal Package

This package contains the closed-source SpecSlice Rust CLI binary for macOS
Apple Silicon and Intel, plus the Dart analyzer sidecar source that enables
higher-precision Flutter/Dart indexing.

## Contents

```text
bin/specslice
libexec/specslice
tool/specslice_dart_analyzer/
skills/specslice/
README.md
README-AI-SKILL.md
BUILD-INFO.txt
```

`bin/specslice` is the user-facing wrapper. `libexec/specslice` is the
universal binary. The Dart analyzer sidecar is intentionally included as
source under `tool/specslice_dart_analyzer/`.

## Install

```bash
tar -xzf specslice-0.2.0-macos-universal.tar.gz
sudo cp -R specslice-0.2.0-macos-universal /usr/local/specslice
sudo ln -sf /usr/local/specslice/bin/specslice /usr/local/bin/specslice
specslice --help
```

If you do not want to use `/usr/local`, put the extracted directory anywhere
and add its `bin` directory to PATH.

## Supported languages (0.2.0)

SpecSlice indexes in two tiers. **Breadth** is always available (the
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
| Dart | Bundled **`specslice_dart_analyzer`** sidecar (resolved AST, domain-aware) | Dart |
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

Point SpecSlice at a specific indexer binary with `SPECSLICE_SCIP_<LANG>_BIN`
(e.g. `SPECSLICE_SCIP_RUST_BIN`). A missing or failing indexer is a clear,
non-fatal "structure-only" note in the `index` output — never an error.

### Enable an opt-in adapter

`specslice init` writes every non-Dart adapter to `.specslice.yaml` with
`enabled: false` so a fresh workspace never pulls in unrelated languages.
To turn one on, edit the matching block (or the unified `languages:`
selector) and re-index. Example for a TypeScript project:

```yaml
typescript:
  enabled: true
  paths: [src, tests]   # roots where SpecSlice should look
  exclude: []
```

```bash
specslice --repo-root /path/to/repo index
# look for the `TypeScript index:` block in the output
```

Same shape for `swift`, `go`, `python`, `java` and the tree-sitter breadth
languages.

For Swift precision, install `sourcekit-lsp` through Xcode / Swift toolchains
and either keep auto-discovery or set the trusted operator environment variable
`SPECSLICE_SWIFT_LSP_BIN`. A repo-provided `swift.lsp_command` is ignored unless
`SPECSLICE_TRUST_CONFIG_COMMANDS=1` is set for that workspace.

## Dart analyzer sidecar

For Flutter/Dart repositories, install a Dart or Flutter SDK on the machine:

```bash
dart --version
# or
flutter --version
```

When Dart is available, SpecSlice automatically probes the bundled sidecar at:

```text
tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart
```

If Dart is unavailable, SpecSlice falls back to the lightweight scanner and
prints a skip reason. The fallback is usable, but less precise.

## Basic Usage

```bash
specslice --repo-root /path/to/repo init
specslice --repo-root /path/to/repo index
specslice --repo-root /path/to/repo check
specslice --repo-root /path/to/repo logic --only-risks
specslice --repo-root /path/to/repo graph --format html --view code
specslice --repo-root /path/to/repo graph --format html --view business
specslice --repo-root /path/to/repo search "purchase pro" --format html
specslice --repo-root /path/to/repo dead-code --json --min-confidence medium
```

HTML graph output defaults to:

```text
/path/to/repo/.specslice/export/graph.html
```

Search HTML output defaults to:

```text
/path/to/repo/.specslice/export/search-<query>.html
```

`dead-code` emits possible dead-code candidates with confidence and reasons.
It does not delete files and should not be treated as proof that code is
removable. Use `--include-tests` only when you want orphan test facts included;
test helper functions under `test/` are filtered from production findings.

## AI Candidate Flow

```bash
specslice --repo-root /path/to/repo connect propose --pretty --out /tmp/specslice-evidence.json
specslice --repo-root /path/to/repo candidate list
specslice --repo-root /path/to/repo candidate review <candidate-id> --accept --note "用户确认"
```

AI candidates are not confirmed business logic until a human reviews them.
SpecSlice does not require annotations in production code, tests, or docs.

## Uninstall

```bash
sudo rm -f /usr/local/bin/specslice
sudo rm -rf /usr/local/specslice
```
