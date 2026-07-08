---
name: groundgraph
description: "Use when analyzing a repository with GroundGraph: index its non-invasive code/doc graph, search code via the graph (a grep replacement), extract behavior facts / purity / constants / data contracts, drive a code rewrite or port (e.g. Java→Go) with port-coverage and business-graph equivalence, index DB table schemas as evidence, generate browser-viewable graph exports, propose & review AI business-logic candidates, and produce impact/context/dead-code reports — all without adding annotations to the target codebase."
metadata:
  short-description: Non-invasive repo graph, code search, porting & business-logic analysis
---

# GroundGraph

GroundGraph is a non-invasive context layer for AI coding. It reads repository facts into an external graph (`.groundgraph/graph.db`), lets AI propose business-logic descriptions and links, and keeps human confirmation separate from code/doc/test files. It also powers **code search without grep**, **behavior-fact extraction**, and a **port/rewrite ledger** (track how completely one implementation reproduces another, e.g. Java→Go).

## Core Rules

- Do not ask users to add GroundGraph annotations to production code, tests, or business docs.
- Treat deterministic graph facts as evidence, not business truth.
- Treat AI business logic as `candidate` until a human accepts it.
- Confirmed relationships live only in GroundGraph-owned files such as `.groundgraph/links.yaml`, `.groundgraph/requirements.yaml`, and `.groundgraph/candidates/`.
- Prefer code-graph navigation (`search`, `facts`, `feature-pack`) over `grep`/`rg` when locating or understanding code.
- Prefer Chinese natural-language summaries for Chinese users; keep raw artifact ids as evidence.
- Always report real command outputs and whether a sidecar/indexer was skipped.

## Command Resolution

Use `groundgraph` if it is on PATH:

```bash
command -v groundgraph
```

The repository ships a source-of-truth build; the canonical install lands in `~/.cargo/bin/groundgraph` via:

```bash
cargo install --path crates/groundgraph-cli --force   # from the GroundGraph checkout
```

When working inside the GroundGraph source checkout you can also run:

```bash
cargo run --quiet -p groundgraph-cli --bin groundgraph -- <subcommand>
# or the release binary directly:
./target/release/groundgraph <subcommand>
```

`--repo-root` may be passed globally (before the subcommand) or per-subcommand (after it); both work:

```bash
groundgraph --repo-root /path/to/repo index
groundgraph index --repo-root /path/to/repo
```

## Standard Workflow

1. Initialize the target repository:

```bash
groundgraph --repo-root /path/to/repo init
```

`init` auto-detects **every** first-party language in the repo and writes one entry per language under `languages:` (zero-dep tree-sitter backend, `enrichment.lsp: false`). This is monorepo-aware: a repo with `backend/` (Go) + `apps/foo` (Flutter/Dart) + `apps/bar` (Swift) + a TS admin web gets `dart` + `go` + `swift` + `typescript` all enabled, each scoped to its source roots. Flutter/RN/desktop platform-embedding dirs (`android/`, `ios/`, `macos/`, `windows/`, `linux/`) are ignored during detection so generated glue (`GeneratedPluginRegistrant.java`, `AppDelegate.swift`) and nested `build.gradle` never elect a phantom language. **After `init`, sanity-check the generated `languages:` list matches what you expect**; add/trim entries by hand for unusual layouts.

2. Index facts (code + docs). Also index DB schema as first-class evidence:

```bash
groundgraph --repo-root /path/to/repo index          # also folds in schema (tables + mapper SQL)
groundgraph --repo-root /path/to/repo schema-index    # CREATE TABLE + @TableName/@Table -> DbTable; MyBatis <select|insert|update|delete> -> SqlMapperStmt
```

`index` reports counts (files / symbols / edges / tables / mapper statements). If a Dart analyzer sidecar is available, confirm whether output says `resolver=dart_analyzer`; otherwise report the fallback reason.

For Java/MyBatis repos, `schema-index` also indexes mapper XML statements as `SqlMapperStmt` nodes (name = statement id = the mapper-interface method; raw SQL kept in metadata). This makes the query SQL searchable via the graph, so a port can read query semantics with `search <methodName>` instead of grepping XML. These nodes are deliberately excluded from `graph-equiv` (a target without MyBatis should not be penalised).

3. Check graph consistency:

```bash
groundgraph --repo-root /path/to/repo check
groundgraph --repo-root /path/to/repo logic --only-risks
```

## Code Search (grep replacement)

Use `search` instead of `grep`/`rg` to locate symbols, methods, and references through the graph:

```bash
groundgraph --repo-root /path/to/repo search "selectCraftTree"
groundgraph --repo-root /path/to/repo search "purchase pro" --format html
groundgraph --repo-root /path/to/repo search --code "<symbol-or-call>" --json
```

Search HTML defaults to `.groundgraph/export/search-<query>.html` unless `--output` is passed. Prefer search HTML for large repositories because it opens on a ranked result list plus a small focus graph instead of a full graph dump.

## Endpoint → Whole Graph (`trace`, P27)

`search` returns a 1-hop union and `graph --view focus` returns focus+descendants+1-hop — both are intentionally **shallow**. To get the **entire downstream chain behind an interface/endpoint** (controller → service → impl → mapper → SQL → table, plus external Feign boundaries), use `trace`:

```bash
groundgraph --repo-root /path/to/repo trace "selectCraftTree" --depth 14
groundgraph --repo-root /path/to/repo trace "selectCraftTree" --json   # for agents
```

`trace` does a bounded forward transitive closure along `calls / declares_implementation / references / persists_to / reads_provider / navigates_to / …`, groups nodes by layer (controller/service/service_impl/mapper/sql/table/other) and reports the distinct **tables** the endpoint ultimately touches. It is the main command for porting/impact analysis ("what does this endpoint really pull in?").

For this to reach the data layer, `schema-index` stitches four derived edge families into the graph (counts printed by `index`/`schema-index`):
- `mapper-interface method --references--> SqlMapperStmt` (MyBatis statement),
- `SqlMapperStmt --persists_to--> DbTable` (table names parsed from the SQL),
- `interface method --declares_implementation--> impl method` — both Java/Spring conventions: the dominant `<Name>Service` ↔ `<Name>ServiceImpl` **and** the legacy `I<Name>` ↔ `<Name>Impl`. So traversal descends through interface dispatch instead of dead-ending at the declaration (a real Spring repo links thousands of these, not just the rare `I`-prefixed ones).
- `callable --persists_to--> DbTable` for **inline SQL** — any non-Java method/function (Go/Dart/TS/Python/Rust/…) whose body embeds SQL string literals referencing a known table. This is what lets `trace` reach tables in repos that keep SQL in code instead of MyBatis XML (e.g. a Go `repo.go` with `` `SELECT … FROM craft` ``). A table edge is only emitted when the parsed name matches an existing `DbTable`, so it cannot invent tables.

## Port / Rewrite Workflow (P24–P26)

This is the suite for reproducing one implementation in another language/stack (e.g. Java→Go) with measurable fidelity. Index BOTH repos first (`index` + `schema-index` in each), then compare their `graph.db` files.

1. **Understand a feature before rewriting** — pull everything an agent needs in one self-contained pack (symbols, behavior facts, internal/external edges, constants, data contract, test suggestions):

```bash
groundgraph --repo-root /path/to/source feature-pack --path <dir/prefix> --text
groundgraph --repo-root /path/to/source feature-pack --requirement REQ-XYZ        # JSON for agent consumption
```

2. **Behavior facts / purity / constants / contract** — recover what the graph alone does not show (branches, comparisons, null checks, throws, awaits; pure vs impure; magic values; DB schema + serialization keys):

```bash
groundgraph --repo-root /path/to/source facts <symbol-or-path>
groundgraph --repo-root /path/to/source purity
groundgraph --repo-root /path/to/source constants            # every literal + all occurrences
groundgraph --repo-root /path/to/source contract             # CREATE TABLE schema + obj['key'] ?? default
groundgraph --repo-root /path/to/source suggest-tests <symbol>
```

3. **Port-coverage ledger** — which source symbols are ported / missing / target-only. Use `--ignore-case` for Java→Go (PascalCase) and a `--port-map` YAML to count renamed symbols:

```bash
groundgraph port-coverage \
  --source-db /path/to/java/.groundgraph/graph.db \
  --target-db /path/to/go/.groundgraph/graph.db \
  --ignore-case --port-map /path/to/go/docs/port-map.yaml --json
```

Scope the **source denominator** to one slice when porting a microservice out of a monolith — `--source-include`/`--source-exclude` are path globs applied to the **source side only** (the target's `extra` list is untouched), so coverage % reflects just that slice's progress instead of the whole monolith:

```bash
# measure only the craft microservice's port progress
groundgraph port-coverage --source-db java/.groundgraph/graph.db --target-db go/.groundgraph/graph.db \
  --ignore-case --source-include '**/rcmtm-cloud-craft/**' --source-exclude '**/generated/**' --json
```

(`--exclude` differs: it drops paths from *both* sides. Use `--source-include`/`--source-exclude` for denominator scoping, `--exclude` for noise that exists in both trees.)

`port-map.yaml` shape:

```yaml
aliases:
  selectCraftTree: SelectCraftTree
  getCraftRecommendsByCloth: RecommendsByCloth
```

4. **Business-graph equivalence** — quantify that a target slice equivalently replaces a source slice (node counts by kind/family, internal edges, name coverage, AND table/column coverage from `schema-index`). JSON is meant to be fed to an AI to walk sub-graphs and prove equivalence with numbers:

```bash
groundgraph graph-equiv \
  --source-db /path/to/java/.groundgraph/graph.db \
  --target-db /path/to/go/.groundgraph/graph.db \
  --source-scope "rcmtm-cloud-craft/**" --target-scope "internal/**" --json
```

Read `tables.matched_tables` / `missing_tables` / `per_table[].coverage` to report schema fidelity. Declare any non-modeled columns as explicit fidelity gaps rather than silently dropping them.

5. After porting a slice, re-index the target and re-run `port-coverage` / `graph-equiv` to show progress. This is a spiral: when the tool itself misreports something during a real port, fix GroundGraph, rebuild (`cargo install --path crates/groundgraph-cli --force`), re-index, and re-compare.

## Graph Export

```bash
groundgraph --repo-root /path/to/repo graph --format html --view code
groundgraph --repo-root /path/to/repo graph --format html --view business
groundgraph --repo-root /path/to/repo graph --format json --view business --pretty
groundgraph --repo-root /path/to/repo graph-diff <base.db> <head.db>   # snapshot diff (CI artefacts)
```

Default HTML output is `.groundgraph/export/graph.html` unless `--out` is passed.

## MCP Integration (stdio)

For AI agents, the graph is also exposed over the [Model Context Protocol](https://modelcontextprotocol.io) by a separate binary, `groundgraph-mcp`, which speaks **MCP over stdio** (the standard local-server transport — not SSE/HTTP). It reads its workspace from the repo's `.groundgraph.yaml` and never writes to source files. Launch it directly:

```bash
groundgraph-mcp --repo-root /path/to/repo
```

Or point any stdio-capable MCP client (Cursor, Claude Desktop, …) at that command:

```jsonc
{
  "mcpServers": {
    "groundgraph": {
      "command": "groundgraph-mcp",
      "args": ["--repo-root", "/path/to/repo"]
    }
  }
}
```

It advertises these tools, each with a JSON-Schema `inputSchema`: `search_graph`, `get_subgraph`, `explain_symbol`, `impact`, `dead_code`, `context_pack`, `check_drift`. Business-logic candidates are exposed through `context_pack` / `explain_symbol` (there are no separate candidate tools), matching the CLI's evidence-then-confirm model.

## Dead-Code Candidate Workflow

Use `dead-code` only as a candidate report. It is not an automatic deletion tool and must not be presented as proof that a symbol is removable.

```bash
groundgraph --repo-root /path/to/repo dead-code
groundgraph --repo-root /path/to/repo dead-code --json --min-confidence high
groundgraph --repo-root /path/to/repo dead-code --include-tests
```

Interpretation:

- `high`: private, unreachable, no inbound usage edges; still needs manual review.
- `medium`: public, lifecycle-like, constructor/class, or otherwise externally reachable.
- `low`: weak evidence such as a dead island or orphan test; use for triage, not deletion.

Tune only GroundGraph-owned config (`dead_code.entrypoints` / `ignore` / `public_api_roots` in `.groundgraph.yaml`). Never ask the user to add `@used`/`@business`/comments to satisfy analysis.

## AI Candidate Workflow

1. Produce an evidence pack:

```bash
groundgraph --repo-root /path/to/repo propose --pretty --out /tmp/groundgraph-evidence.json
# (alias: `connect propose` in older builds)
```

2. As the AI agent, read the pack and produce Chinese business-logic candidates: concise description, evidence files/symbols/tests, confidence + rationale, risks/open questions, no claims of confirmation.

3. Save to `.groundgraph/candidates/business_logic.yaml` only when the user asks. Do not edit target source files.

4. Present candidates in natural language; ask the user to confirm / reject / needs-changes / pending.

5. Apply decisions, then re-index:

```bash
groundgraph --repo-root /path/to/repo candidate review <id> --accept --note "用户确认"
groundgraph --repo-root /path/to/repo index
groundgraph --repo-root /path/to/repo logic
groundgraph --repo-root /path/to/repo graph --format html --view business
```

## Common Analysis

```bash
groundgraph --repo-root /path/to/repo slice REQ-EXAMPLE-001
groundgraph --repo-root /path/to/repo context REQ-EXAMPLE-001 --json
groundgraph --repo-root /path/to/repo impact --base main
groundgraph --repo-root /path/to/repo select-tests --base main
groundgraph --repo-root /path/to/repo similar
groundgraph --repo-root /path/to/repo features
groundgraph --repo-root /path/to/repo questions
groundgraph --repo-root /path/to/repo business-doc            # render confirmed candidates + evidence
```

## Command Statistics

Every command run auto-appends a record to `.groundgraph/stats.jsonl` (invocations, total/avg/max duration, failures, per-command metrics such as nodes/columns/coverage). Summarize with:

```bash
groundgraph --repo-root /path/to/repo stats
groundgraph --repo-root /path/to/repo stats --json
groundgraph --repo-root /path/to/repo stats --reset
```

## Reporting

In the final answer, include:

- whether `groundgraph` was global (PATH) or source-run, and its version
- commands run and key results (real outputs)
- graph/export file path if produced
- sidecar resolver status
- for ports: port-coverage %, graph-equiv node/edge/table numbers, declared fidelity gaps
- candidate confidence boundary: fact, candidate, or confirmed
