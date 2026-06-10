<div align="center">

# SpecSlice

**A non-invasive *intent layer* for AI-assisted coding.**

SpecSlice builds an evidence-linked graph of your codebase — connecting requirements, docs, tests and code — so AI agents (and humans) get *grounded* context instead of guesses. It never touches your source: everything lives under `.specslice/`.

[![CI](https://github.com/specslice/specslice/actions/workflows/ci.yml/badge.svg)](https://github.com/specslice/specslice/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/rust-1.96-orange.svg)](rust-toolchain.toml)

**English** · [简体中文](README.zh-CN.md)

</div>

---

## What is SpecSlice?

Most "code intelligence" tools answer *"where is this symbol?"*. SpecSlice also answers *"what is this code **for**, and what proves it?"*

It indexes your repository into a SQLite graph of **nodes** (symbols, files, docs, requirements, tests, routes, DB tables…) and **edges** (calls, references, implements, verifies, persists…), where every edge carries **evidence**. On top of that graph it offers code search, impact analysis, dead-code detection, behavioral-fact extraction, and an AI **propose → human confirm** workflow for capturing business logic.

- **Non-invasive (zero write-back).** SpecSlice never edits, annotates, or commits to your code. All state is a rebuildable cache under `.specslice/`.
- **Evidence over assertion.** Edges are backed by concrete facts (a call site, a doc link, a test reference), each with a confidence level — not opaque heuristics.
- **AI proposes, humans confirm.** Business-logic candidates are generated from code/doc/test facts and only become authoritative after a human review step.
- **Tiered, multi-language.** A fast in-process tree-sitter backend covers breadth (Rust, TypeScript, Python, Go, Java, C, C++, Swift) plus a Dart analyzer sidecar; an *optional* SCIP/LSP overlay adds precise call/reference edges where you want them.

> SpecSlice is **not** a faster grep. It is the layer above retrieval: intent alignment, traceability, and doc/code drift. It self-hosts — SpecSlice indexes its own Rust source.

## Highlights

- 🔎 **`search`** — hybrid retrieval: structural scoring (ids/names/paths/evidence/adjacency) **plus a BM25 fulltext content layer** over code bodies, doc comments and markdown bodies — bilingual (CJK bigrams), with a grounding source snippet per hit. Concept queries like `byte boundary panic` or `错位竞争` hit even when no identifier contains those words.
- 📋 **`check`** — doc→code drift detection: stale doc references (`doc_stale_code_ref` — a doc mentions a path/symbol that no longer exists), orphan requirements with **graph-suggested implementations** (`requirement_implementation_hint`), broken declared links, missing linked tests.
- 🧭 **`trace`** — endpoint → full downstream chain (controller → service → impl → SQL → table).
- 💥 **`impact`** — which requirements, docs and tests a git diff affects.
- 🪦 **`dead-code`** — symbols unreachable from any entry point, with reasons and confidence (never auto-deletes).
- 🧪 **`facts` / `purity` / `constants` / `contract`** — deterministic behavioral facts for refactoring & porting.
- 🧠 **`propose` / `candidate` / `logic`** — AI business-logic evidence packs and a human review workflow.
- 🔁 **`port-coverage` / `graph-equiv`** — track and prove a rewrite/port against the source graph.
- 📊 **`dashboard`** — a single self-contained offline HTML panel aggregating overview, business modules, feature clusters, checks, dead code, open questions and purity. No server, no CDN — open it from `file://`.
- 🔌 **MCP server** — expose the graph to AI agents via the Model Context Protocol.

Battle-tested on large codebases across languages: Redis (C, ~200k lines) indexes in ~11s, the TypeScript compiler repo (20k+ files) in ~52s (parallel parsing + a per-file parse budget that survives fixture corpora with intentional syntax errors), Django (Python), gin (Go) and gson (Java/Maven) validated end-to-end. SCIP enrichment is incremental — unchanged sources reuse the previous `.scip` instead of re-running the type-checker — and search ranking demotes tests/tools/examples so issue-style queries hit production code first (validated against real Redis issues).

## Install

SpecSlice is a Rust workspace. Build from source (a `rust-toolchain.toml` pins the exact toolchain):

```bash
git clone https://github.com/specslice/specslice.git
cd specslice

# Install the CLI (`specslice`) and the MCP server (`specslice-mcp`)
cargo install --path crates/specslice-cli
cargo install --path crates/specslice-mcp   # optional, for AI agents

# …or just build the binaries into target/release/
cargo build --release
```

## Quickstart

```bash
cd /path/to/your/repo

specslice init                      # create .specslice.yaml + .specslice/graph.db
specslice index                     # index docs + code into the graph

specslice search "parse sql tables" # ranked, evidence-backed hits
specslice dead-code                 # unreachable symbols, with reasons
specslice trace UserController      # full downstream chain of an endpoint
specslice propose                   # AI business-logic evidence pack (+ prompt)
specslice dashboard                 # one-file offline HTML management panel
```

Everything written stays under `.specslice/`. Delete that directory to start clean — your source is never modified. Add `.specslice/` to your `.gitignore` if you don't want to commit the cache.

## Command reference

Run `specslice --help` (or `specslice <command> --help`) for the full, authoritative list. The most-used commands:

| Area | Command | What it does |
| --- | --- | --- |
| **Setup** | `init`, `index` | Create the workspace; index docs + code into the graph |
| **Navigate** | `search`, `trace`, `graph`, `context`, `slice` | Find code, follow chains, render the graph, build context packs |
| **Overview** | `dashboard`, `features`, `stats` | Offline HTML management panel; functional-area clusters; command ledger |
| **Change impact** | `impact`, `graph-diff`, `select-tests` | What a diff affects; compare graph snapshots; which tests to run |
| **Quality** | `dead-code`, `similar`, `check`, `questions` | Unreachable code, duplicate clusters, consistency checks, open questions |
| **Behavioral facts** | `facts`, `purity`, `constants`, `contract` | Branches/returns/nullability, purity census, literal catalogue, data contracts |
| **Business intent** | `propose`, `candidate`, `logic`, `business-doc`, `connect` | Generate/review business-logic candidates; render confirmed docs |
| **Porting** | `port-coverage`, `route-coverage`, `graph-equiv`, `feature-pack`, `schema-index` | Track a rewrite against the source graph and prove equivalence |

> Read-only commands never mutate your source. `dead-code`, `similar`, `select-tests` etc. **report** — they never delete or run anything on your behalf.

## Language support

| Tier | Mechanism | Languages |
| --- | --- | --- |
| Breadth (default) | In-process **tree-sitter** | Rust, TypeScript, Python, Go, Java, C, C++, Swift |
| Dart | Bundled **analyzer sidecar** (domain-aware: Riverpod / Hive / navigation / IAP) | Dart |
| Docs | Markdown / requirements / ADR | `.md`, `.mdx` |

Select languages in `.specslice.yaml` (the unified `languages:` selector) and re-run `specslice index`.

### Optional precision overlay (SCIP)

For precise `Calls`/`References` edges, SpecSlice will auto-invoke an installed SCIP indexer per language during `index` and ingest the result. This is **optional** — without it you still get the full structural graph.

| Language | Indexer | Install |
| --- | --- | --- |
| Rust | `rust-analyzer scip` | `rustup component add rust-analyzer` |
| Go | `scip-go` | `go install github.com/sourcegraph/scip-go/cmd/scip-go@latest` |
| TypeScript | `scip-typescript` | `npm i -g @sourcegraph/scip-typescript` |
| Python | `scip-python` | `npm i -g @sourcegraph/scip-python` |

A missing or failing indexer is a clear, non-fatal "structure-only" note — never an error. Point SpecSlice at a specific binary with `SPECSLICE_SCIP_<LANG>_BIN` (e.g. `SPECSLICE_SCIP_RUST_BIN`).

> **Note for Rust repos with a pinned toolchain:** the `rust-analyzer` rustup proxy resolves against your repo's `rust-toolchain.toml`. If that toolchain lacks the component, run `rustup component add rust-analyzer` (for that toolchain) or set `SPECSLICE_SCIP_RUST_BIN`.

## MCP integration

`specslice-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server that exposes the graph (search, subgraph, dead-code, …) to AI agents. Point your MCP-capable client at the binary:

```jsonc
{
  "mcpServers": {
    "specslice": {
      "command": "specslice-mcp",
      "args": ["--repo-root", "/path/to/your/repo"]
    }
  }
}
```

## Configuration

`specslice init` writes a `.specslice.yaml` you can edit. Key sections:

```yaml
storage:
  path: .specslice/graph.db   # the graph cache (rebuildable)
docs:
  paths: [docs, specs, adr]   # where to find docs/requirements
  include: ["**/*.md", "**/*.mdx"]
treesitter:
  languages: [rust]           # the unified language selector
enrichment:
  scip: true                  # auto-invoke SCIP indexers when present
  analyzer: true              # Dart analyzer sidecar (when Dart is configured)
```

## How it works

```
crates/
├── specslice-core      # graph domain model: nodes, edges, evidence, ids
├── specslice-store     # SQLite store + migrations (the .specslice/graph.db)
├── specslice-engine    # indexers, scanners, search, analyses (the brains)
├── specslice-lang-dart # Dart language support
├── specslice-cli       # the `specslice` binary
└── specslice-mcp       # the `specslice-mcp` MCP server
```

`index` runs structural passes (tree-sitter / Dart) first, then an optional SCIP overlay binds precise edges onto the symbols that already exist. Read commands open the store and query the graph — they idempotently ensure performance indexes on open, so queries stay fast even right after a binary upgrade.

## Development

```bash
cargo fmt --all                                   # format
cargo clippy --workspace --all-targets -- -D warnings   # lint (zero-warning policy)
cargo test --workspace                            # ~1000+ tests
```

- The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml); CI (`.github/workflows/ci.yml`) enforces fmt + clippy (`-D warnings`) + tests on every push.
- **Test-driven:** new behavior starts with a failing test, then the minimal code to pass it.
- Hand-rolled scanners are guarded by `proptest` totality tests (arbitrary UTF-8 → no panic, deterministic).
- Acceptance is judged by **real command output**, not prose.

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Please keep the zero-warning policy and write a failing test first.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
