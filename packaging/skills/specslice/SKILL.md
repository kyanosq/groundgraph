---
name: specslice
description: "Use when analyzing a repository with SpecSlice: initialize or index its non-invasive graph, generate browser-viewable graph exports, create AI business-logic candidates from code/doc/test facts, review candidate confidence, or produce impact/context reports without adding annotations to the target codebase."
metadata:
  short-description: Non-invasive repo graph and business-logic analysis
---

# SpecSlice

SpecSlice is a non-invasive context layer for AI coding. It reads repository facts into an external graph, lets AI propose business-logic descriptions and links, then keeps human confirmation separate from code/doc/test files.

## Core Rules

- Do not ask users to add SpecSlice annotations to production code, tests, or business docs.
- Treat deterministic graph facts as evidence, not business truth.
- Treat AI business logic as `candidate` until a human accepts it.
- Confirmed relationships live only in SpecSlice-owned files such as `.specslice/links.yaml`, `.specslice/requirements.yaml`, and `.specslice/candidates/`.
- Prefer Chinese natural-language summaries for Chinese users; keep raw artifact ids as evidence.
- Always report real command outputs and whether a sidecar/indexer was skipped.

## Command Resolution

Use `specslice` if it is on PATH. If not, find the tool:

```bash
command -v specslice
```

For packaged installs, the expected command is:

```bash
/usr/local/specslice/bin/specslice
```

When working inside the SpecSlice source checkout, use:

```bash
cargo run --quiet -- <subcommand>
```

## Standard Workflow

1. Initialize the target repository:

```bash
specslice --repo-root /path/to/repo init
```

2. Index facts:

```bash
specslice --repo-root /path/to/repo index
```

If Dart analyzer sidecar is available, confirm whether output says `resolver=dart_analyzer`; otherwise report the fallback reason. The
analyzer sidecar is Dart's **authoritative** precision source — it emits
Dart-domain semantic edges (Riverpod / Hive / navigation / IAP) that
generic SCIP cannot. `scip_dart` is verified working and auto-invoked
**only when the sidecar is disabled** (`enrichment.analyzer=false`),
where it upgrades the `dart_lightweight` heuristic `Calls`/`References`
to SCIP precision (ADR-0001 §8.8 (f)).

3. Check graph consistency:

```bash
specslice --repo-root /path/to/repo check
specslice --repo-root /path/to/repo logic --only-risks
```

4. Export a browser-viewable graph:

```bash
specslice --repo-root /path/to/repo graph --format html --view code
specslice --repo-root /path/to/repo graph --format html --view business
specslice --repo-root /path/to/repo search "purchase pro" --format html
```

Default HTML output is `.specslice/export/graph.html` unless `--out` is passed.
Search HTML output defaults to `.specslice/export/search-<query>.html` unless
`--output` is passed. Prefer search HTML for large repositories because it opens
on a ranked result list plus a small focus graph instead of a full graph dump.

5. Export local Mermaid diagrams (P14/P15) — small enough for PR
   descriptions and design docs, edges come from the real graph
   facts (impact uses `ImpactReport.impact_edges`, search uses
   the subgraph, candidate uses manifest evidence):

```bash
specslice --repo-root /path/to/repo search "login" --format mermaid \
    --depth 1 --output /tmp/login.mmd
specslice --repo-root /path/to/repo impact --base origin/main \
    --format mermaid --output /tmp/pr-impact.mmd
specslice --repo-root /path/to/repo candidate show <candidate-id> \
    --format mermaid > /tmp/candidate.mmd
```

Do not try to render the whole repo with `graph --format mermaid` for
sizeable projects — the local exporters above are the right tool.

## MCP Server (P11 / P15)

For AI agents, prefer the JSON-RPC MCP server over scraping CLI text.
It exposes `search_graph`, `get_subgraph`, `explain_symbol`,
`impact`, `dead_code`, `context_pack`, and `check_drift`. Each returns
structured JSON matching the CLI's `--json` schema. Candidate context
is exposed through `context_pack` / `explain_symbol`, not separate
candidate-prefixed MCP tools. `check_drift` runs the consistency
checks (broken links, orphan requirements, `doc_stale_code_ref`
doc→code drift, requirement implementation hints) and returns the
findings list.

```bash
specslice-mcp --repo-root /path/to/repo
```

`get_subgraph` accepts a `resolvers: [...]` filter
(`"scip"`, `"go_treesitter"`, `"swift_treesitter"`, `"dart_analyzer"`,
…) so an agent can restrict expansion to a single provenance / adapter
when needed.

## Python via tree-sitter + 启发式（精度交给 SCIP）(P16)

`python.enabled: true` activates the Python adapter. **LSP 已退役**
（见 ADR-0001 §8.8）。结构与基线精度都来自进程内 tree-sitter 驱动：

1. **结构 + 启发式（始终运行）.** tree-sitter 驱动产出节点
   （`python_module` / `python_class` / `python_method` /
   `python_function`）、`Imports`（跨 repo 文件解析；无法解析的
   stdlib / 第三方导入静默丢弃）、pytest `TestCase` / `TestGroup`
   （`def test_*` 与 `class Test*`），以及**启发式 `Calls` /
   `References`**（按调用标识符就近匹配）。所有行标
   `indexer = python_treesitter`，resolver 即 `python_treesitter`。
2. **SCIP 精度叠加（有 indexer 时）.** 若 `scip-python` 在 PATH，
   `specslice index` 会自动调用它产出 `.specslice/scip/python.scip`，
   overlay 在覆盖文件上用高可信 SCIP 边**取代**同名启发式边
   （§8.8「权威 + 补空」）。**上游限制**：实测 `scip-python`（0.6.6）
   对样例仓库产出空索引（0 documents），因此 Python 目前**多由启发式
   独撑**；待上游修复后自动恢复 SCIP 权威，无需改配置。

`specslice index` reports Python files, pytest tests, and the
P17 framework entrypoint count:

```
Python index:
  Python files: 162
  Symbols: 1216
  TestCases: 272
  Imports: 662
  Framework entrypoints: 45
  References (heuristic): 540
  Resolver: python_treesitter
```

Python 启发式 `Calls` should be treated as a *line, not a fact* —
unless that file is covered by a SCIP overlay (then it is authoritative,
tagged `indexer = scip`). The agent should cross-check with the
surrounding structural output and the P17 framework facts before
claiming a function is unused or unreachable.

## Framework facts (P17)

Decorator-based entry points are detected during the tree-sitter
structural pass and recorded as `metadata_json` on the wrapped symbol.
The detection is purely structural (no LSP required) and covers the
most common Python application frameworks:

- **FastAPI / Starlette / APIRouter** routes — `@router.get(...)`,
  `@app.post(...)`, `@app.websocket(...)`, `@app.api_route(...)`,
  including HTTP verb and path extraction.
- **Flask / Blueprint** routes — `@app.route(...)`,
  `@bp.route("/login", methods=["POST"])`.
- **Django** view decorators — `@login_required`,
  `@require_http_methods(...)`, `@api_view`, …
- **Celery / RQ / Dramatiq** background tasks — `@shared_task`,
  `@app.task(queue="emails")`, `@job(queue="high")`, `@dramatiq.actor`.
- **Click / Typer** CLI commands — `@click.command`, `@click.group`,
  `@app.command(...)`, `@cli.callback`.
- **FastAPI lifecycle events** — `@app.on_event("startup")`.
- **FastAPI exception handlers / middleware** —
  `@app.exception_handler(Exception)`, `@app.middleware("http")`.
- **SQLAlchemy event listeners** — `@event.listens_for(...)`.
- **Pydantic validators** — `@validator(...)`, `@field_validator(...)`,
  `@model_validator(...)`.
- **Dataclasses** — `@dataclass`, `@attrs.define`, `@attrs.frozen`
  (recorded as metadata only — *not* treated as entry points).

The classifier intentionally rejects look-alike calls (e.g.
`httpx.get("/items")`, `os.get(...)`) by requiring the decorator's
object name to look like a router (`app / router / api / blueprint /
*Router / *_router / *_app`).

For every framework-decorated symbol, two surfaces light up:

1. **`specslice dead-code`** treats the symbol as an entry point, so
   route handlers, Celery tasks, Click commands, Pydantic validators
   and ASGI exception handlers/middleware are no longer flagged as
   "possibly dead" just because the framework — not any in-repo
   caller — invokes them.
2. **`specslice search` / MCP `search_graph`** returns
   `framework_role: "fastapi_route" | "background_task" |
   "pydantic_validator" | "asgi_infrastructure" | …` on every match.
   Agents and humans can spot framework entry points at a glance
   without re-parsing the underlying `metadata_json`.

## Swift via LSP (P11–P15) — 唯一保留 LSP 的语言

Swift 缺乏成熟 SCIP indexer，是**唯一**保留实时 LSP 的语言。
当 `swift.enabled: true` 时，indexer 用 tree-sitter 产出结构
（`swift_class`、`swift_struct`、`swift_protocol`、`swift_method`、
`swift_function`、`swift_initializer`、`swift_enum`），再用
`sourcekit-lsp` overlay 叠加：

- `EdgeKind::Calls`（`callHierarchy/outgoingCalls`）。边证据
  （`source_file` / 调用行）指向调用方的 `fromRanges`（真实调用点，
  而非被调用者声明处）；
- `EdgeKind::References`（`textDocument/references`）。

LSP 缺失时静默跳过并把原因写入 `result.sidecar_skip_reason`。
CLI 这一段仍显示 `References (LSP)` / `LSP skipped`（仅 Swift）。

Swift 是 LSP 退役后**唯一**仍受 `enrichment.lsp` 影响的语言：开关与
`sourcekit-lsp` 路径都配在根 `.specslice.yaml`（`enrichment.lsp: true` +
可选 `swift.lsp_command` / `SPECSLICE_SWIFT_LSP_COMMAND`）。Go 等其余语言
的精度则统一交给根 `.specslice.yaml` 里对应 `*.enabled` 的 SCIP overlay，
不再有任何 LSP 开关。

## Go via tree-sitter + 启发式（精度交给 SCIP）

Go 的 **LSP（gopls）已退役**（ADR-0001 §8.8）。`go.enabled: true` 时
indexer 用 tree-sitter 产出结构（`go_struct`、`go_interface`、
`go_method`、`go_function`）+ `Imports` + 启发式 `Calls` /
`References`（`indexer = go_treesitter`）。若 `scip-go` 在 PATH，
`specslice index` 自动调用产出 `.specslice/scip/go.scip`，overlay 在覆盖
文件上以高可信 SCIP 边取代启发式边（已实测 Go✓）。CLI 显示
`References (heuristic)`，无 `References (LSP)` / `LSP skipped`。

## TypeScript / Java via tree-sitter + 启发式（精度交给 SCIP）(P20)

`typescript.enabled: true` and `java.enabled: true` activate the
TypeScript and Java adapters. **两者的 LSP（typescript-language-server /
jdtls）均已退役**（ADR-0001 §8.8）。结构与基线精度都来自 tree-sitter：

- **结构 + 启发式（始终运行）**：tree-sitter 驱动产出结构节点 +
  `Imports` + 测试用例 + 启发式 `Calls` / `References`。TypeScript
  同时覆盖 `.ts`/`.tsx`/`.js`/`.jsx`/`.vue` 双方言；行标
  `indexer = typescript_treesitter` / `java_treesitter`。
- **SCIP 精度叠加（有 indexer 时）**：`scip-typescript`（已实测 TS✓）/
  `scip-java` 在 PATH 时由 `specslice index` 自动调用，overlay 在覆盖
  文件上以高可信 SCIP 边取代启发式边。CLI 显示 `References (heuristic)`，
  无 `References (LSP)` / `LSP skipped`。

Node kinds:

- TypeScript: `typescript_module`, `typescript_class`,
  `typescript_interface`, `typescript_enum`, `typescript_function`,
  `typescript_method`.
- Java: `java_package`, `java_class`, `java_interface`, `java_enum`,
  `java_method`, `java_constructor`. `enum` declarations get their own
  `java_enum` kind (so graph filters can distinguish enum cases from
  plain classes); `record` declarations currently collapse to
  `java_class`. Class qualification follows the file's `package`
  declaration (`java::com.example.Greeter`).

Test recovery:

- TypeScript: vitest / jest `describe(...)` / `it(...)` / `test(...)`
  calls become `TestGroup` / `TestCase`.
- Java: methods annotated with `@Test`, `@ParameterizedTest`,
  `@RepeatedTest`, `@TestFactory`, `@TestTemplate`, or `@Theory`
  become `TestCase`.

TypeScript / Java 启发式 `Calls` should be treated the same way as
Python: a *line, not a fact* — unless the file is covered by a SCIP
overlay (then authoritative, `indexer = scip`). Always check
`result.resolver_used` (`*_treesitter`) and whether the `SCIP overlay`
section appeared in `specslice index` output before claiming
SCIP-quality precision is present.

## Unified LSP probe（仅 Swift）

LSP 退役后，唯一仍走 LSP 的 Swift 仍通过统一探针确认
「这个 LSP 二进制在本机真的能跑吗？」：`specslice_engine::lsp_probe::
probe_lsp_command(command, args, timeout)`。探针启动二进制、给
`DEFAULT_TIMEOUT`（1500ms）让其从 `--help` 退出 0、抽取 stderr
（4 KiB 上限），出现 broken-stub 标记即拒绝：`bad interpreter`、
`no such file or directory`、`cannot execute`、`command not found`、
`SOURCEKITD FATAL ERROR`、`could not load` 等。这能在 indexer 真正开 stdio
会话前拦截「`sourcekit-lsp` 初始化即崩 IndexStoreDB」之类故障。

`swift_lsp_available` 链 `binary_on_path` →
`probe_lsp_command(…).is_runnable()`；opt-in 的 Swift LSP smoke 测试
（`tests/lsp_indexers.rs`，`#[ignore]`）把 `index_swift(…) → Err` 转为
带 `eprintln!` 的 soft-skip，使 `cargo test --include-ignored` 不因本机
LSP 状态变红。go/python/ts/java 的 `*_lsp_available` / `*_LSP_COMMAND_ENV`
已随 LSP 退役删除。

## Cross-language consistency (P20)

Every consumer (`questions`, `dead-code`, `slice`, `feature_map`,
`search`, MCP `parse_node_kind`, store decoding) routes node-kind
predicates through `specslice_core::language_traits`. New node
kinds must update `language_of`, `family_of`,
`default_dead_code_reason`, `search_aliases`, and the `ALL_KINDS`
matrix in `language_traits::tests` — the compiler + matrix tests
will refuse to ship a kind that any predicate forgot.

## Dead-Code Candidate Workflow

Use `dead-code` only as a candidate report. It is not an automatic deletion
tool and it must not be presented as proof that a symbol is removable.

```bash
specslice --repo-root /path/to/repo dead-code
specslice --repo-root /path/to/repo dead-code --json --min-confidence low
specslice --repo-root /path/to/repo dead-code --json --min-confidence high
specslice --repo-root /path/to/repo dead-code --include-tests
```

Interpretation:

- `high`: private, unreachable, no inbound usage edges; still needs manual review.
- `medium`: public, lifecycle-like, constructor/class, or otherwise externally reachable.
- `low`: weak evidence such as a dead island or orphan test; use for triage, not deletion.
- `--include-tests` reports orphan `TestCase` / `TestGroup` nodes with no
  verification target. Test-file helper functions such as `test/**#main`,
  `expect`, or matcher helpers are intentionally filtered from the report.

Before recommending deletion, inspect the symbol with:

```bash
specslice --repo-root /path/to/repo search --code "<symbol-or-call>" --json
specslice --repo-root /path/to/repo graph --format html --view focus --focus "<artifact-id>"
```

If the report looks noisy, adjust only SpecSlice-owned config:

```yaml
dead_code:
  entrypoints:
    - lib/main.dart
  ignore:
    - "**/*.g.dart"
    - "**/*.freezed.dart"
    - "**/generated/**"
    - "**/l10n/app_localizations*.dart"
  public_api_roots:
    - lib/public/**
```

Never ask the user to add `@used`, `@business`, comments, or other annotations
to production code, tests, or docs just to satisfy dead-code analysis.

## AI Candidate Workflow

Use this flow when the repo has code/tests but business logic is not yet confirmed.

1. Produce an evidence pack:

```bash
specslice --repo-root /path/to/repo connect propose --pretty --out /tmp/specslice-evidence.json
```

2. As the AI agent, read the evidence pack and produce Chinese business-logic candidates with:

- a concise natural-language description
- evidence files/symbols/tests
- confidence and rationale
- risks and open questions
- no claims of confirmation

3. Save candidate output to `.specslice/candidates/business_logic.yaml` only when the user asks you to write it. Do not edit target source files.

4. Present candidates to the user in natural language first. Ask for one of:

- confirm / accept
- reject
- needs changes
- pending / needs answer

5. Apply review decisions with:

```bash
specslice --repo-root /path/to/repo candidate review <candidate-id> --accept --note "用户确认"
specslice --repo-root /path/to/repo candidate review <candidate-id> --reject --note "用户拒绝"
specslice --repo-root /path/to/repo candidate review <candidate-id> --needs-changes --note "需要补测试"
```

After accepting candidates, rerun:

```bash
specslice --repo-root /path/to/repo index
specslice --repo-root /path/to/repo logic
specslice --repo-root /path/to/repo graph --format html --view business
```

## Common Analysis Commands

For a confirmed requirement or business id:

```bash
specslice --repo-root /path/to/repo slice REQ-EXAMPLE-001
specslice --repo-root /path/to/repo context REQ-EXAMPLE-001 --json
```

For PR impact:

```bash
specslice --repo-root /path/to/repo impact --base main
```

For machine-readable graph output:

```bash
specslice --repo-root /path/to/repo graph --format json --view business --pretty
specslice --repo-root /path/to/repo graph --format json --view business --include-candidates=false
```

## Similar code report (P18 — structural + near-duplicate)

`specslice similar` ships two tiers:

- **tier 1 (`exact_ast`)** — Python / Dart function / method bodies
  whose normalized token streams (identifiers / literals / comments
  stripped) collide on the same 64-bit FNV-1a fingerprint.
- **tier 2 (`near_token`, SimHash)** — pairs whose SimHash over
  k-shingles has Hamming distance below the threshold derived from
  `--min-score` (default 0.85). Catches "copy + rename a few fields,
  add or remove a couple of statements".

It is always a **candidate** report — never auto-merges, never
auto-deletes.

```bash
specslice --repo-root /path/to/repo similar
specslice --repo-root /path/to/repo similar --mode exact
specslice --repo-root /path/to/repo similar --mode near --min-score 0.8
specslice --repo-root /path/to/repo similar --node python::backend/app/foo.py::Foo.bar
specslice --repo-root /path/to/repo similar --format json
```

Output schema (`schema_version: 1`, backward compatible):

```json
{
  "schema_version": 1,
  "stats": {
    "symbols_scanned": 944,
    "symbols_skipped": 50,
    "clusters_reported": 151,
    "exact_clusters": 96,
    "near_clusters": 55,
    "near_pairwise_skipped": false
  },
  "clusters": [
    {
      "fingerprint": "60f13e8878a10ce3",
      "duplicate_type": "exact_ast",
      "recommendation": "review",
      "normalized_token_count": 187,
      "members": [ { "id": "...", "kind": "python_method", "label": "...", "path": "...", "line_range": [229, 260] } ]
    },
    {
      "fingerprint": "1ae3c00f2d4be0a1",
      "duplicate_type": "near_token",
      "recommendation": "review",
      "normalized_token_count": 1240,
      "similarity_score": 0.859,
      "members": [ /* … */ ]
    }
  ]
}
```

Rules for agents consuming this report:

- Treat every cluster as a *review candidate*, not a fact.
- `exact_ast` clusters omit `similarity_score` (always 1.0 by
  construction). `near_token` clusters carry the worst-case lower
  bound across all pairwise comparisons inside the cluster.
- Before recommending a merge, run `specslice graph --focus <id>` for
  each member to verify both call sites really invoke the same
  semantics — structural identity does NOT imply behavioral identity
  in dynamic languages.
- Override pairs (e.g. `BaseRepository.list_blocks` vs
  `Repository.list_blocks`) often appear in clusters — that is
  expected; surface them as "intentional override" candidates rather
  than duplicates.
- If `stats.near_pairwise_skipped: true`, the graph had more
  uncovered symbols than `--max-pairwise` allows; near tier did
  NOT run. Re-scope with `code_roots` or raise the limit.

## Edge confidence (P19 base)

Every edge in `specslice graph --format json` now carries an
`evidence_quality: "high" | "medium" | "low"` field derived from
`(kind, source, certainty, status, indexer)`:

- **high** — `Contains`, `Imports`, `Documents` (Markdown),
  `DeclaresImplementation` / `DeclaresVerification`, and any
  `Calls` / `References` / `ReadsProvider` / `NavigatesTo` /
  `PersistsTo` / `SubscribesStream` resolved by **offline SCIP**
  (`scip` / `scip:<lang>` indexer — the primary precision source),
  the Swift LSP (`*_lsp`), or the Dart analyzer (`dart_analyzer`).
- **medium** — heuristic `Calls` / `References` from the tree-sitter
  driver (`*_treesitter` indexer; near-match by call identifier) or
  legacy `*_ast` rows; unknown combinations.
- **low** — AI-derived `DerivesFrom`, GitDiff edges, anything
  with `EdgeStatus::Deprecated`.

Agents should weight reasoning by this label. Never claim
"verified" for `low` edges without a follow-up step (running a
test, reading the source, asking the user).

## v0.3.0-A — `evidence_quality` now drives ranking + explanations

Starting with v0.3.0-A (branch-state, not yet released), the
`evidence_quality` signal is **plumbed** through dead-code reasons
and search ranking via a new `confidence_view` module. The schema
does **not** change; nothing the agent already consumes breaks.

- **dead-code**: candidates that survive BFS but whose inbound
  *usage* edges (Calls / References / ReadsProvider / PersistsTo /
  NavigatesTo / SubscribesStream / DeclaresVerification — i.e. NOT
  Contains / Imports / DerivesFrom) are *all* `low` tier get an
  extra reason line: `仅有 N 条 low-tier 入边（来自低置信
  indexer / AST fallback / lightweight resolver），证据较弱`. BFS
  reach set unchanged; this is purely an explanation upgrade.
- **search**: two new scoring passes run before sort:
  - **Pass A — evidence boost** (+30): hits whose `outbound`
    usage-edge summary has `≥ 1` high-tier edge get `+30 score` and
    a reason `出边 evidence_quality=high (N 条)，符号有强证据支撑`.
    Empirically, pixcraft-app `--kind dart_method` "build" lifts
    75/100 hits from 100 → 130.
  - **Pass B — neighbor boost** (capped +20): hits whose 1-hop
    neighbors (cap=8) include other hits get `+20 score` (at most
    once per hit) and a reason `邻接其他命中（A、B [等]）`. Designed
    as a tie-breaker, not a primary signal. vub/"service" lifts
    30/30 neighbor-cluster hits.
- **structured `warnings`**: both `DeadCodeReport` and
  `SearchResult` now include `warnings: Vec<String>` with
  `skip_serializing_if = "Vec::is_empty"`. Old MCP / JSON
  consumers see zero shape change when nothing fails; new
  consumers can surface engine-side advisories (e.g. sqlite probe
  failures) without scraping stderr. CLI human output renders a
  `== Warnings ==` block when present, otherwise stays silent.

## Test selection (P19)

`specslice select-tests --base main [--head HEAD] [--include-deps]`
emits a confidence-tagged list of tests to run for a given diff:

```bash
specslice select-tests --base main
specslice select-tests --base main --include-deps --max-depth 2
specslice select-tests --base main --format json
```

Each `tests[]` entry carries:

- `reasons`: ordered list, one of
  `test_file_directly_changed` (high),
  `references_changed_symbol` (high),
  `imports_changed_module` (medium),
  `transitive_caller_of_changed_symbol` (medium, only with `--include-deps`).
- `confidence`: `high` / `medium` / `low` — the strongest tier among the reasons.

Rules:

- Never claim "the test suite passes" from this report alone — it
  decides *which tests to run*, not *whether they pass*.
- An empty `tests` list does NOT mean "no risk"; pair with
  `specslice impact --base main` to verify business surfaces aren't affected.

## Feature map (P19)

`specslice features` clusters File / Module / Class nodes into
"functional areas" by walking Contains / Imports / Calls /
References edges from framework-anchored seeds. Output is a
heuristic — improve it by installing the language's SCIP indexer
(`scip-go` / `scip-typescript` / `rust-analyzer` / …) so the
`Calls` / `References` edges become SCIP-authoritative.

```bash
specslice features
specslice features --max-clusters 10 --min-cluster-size 5
specslice features --format json
```

Each cluster carries `name`, `seed_path`, `seed_score`, `roles`
(framework families detected on the seed), and a top-N
`representative_symbols` list ordered by distance from the seed.

## Graph diff (P19)

`specslice graph-diff --base-db <path> --head-db <path>` compares
two `.specslice/graph.db` snapshots and reports `nodes_added /
nodes_removed / nodes_kind_changed / edges_added / edges_removed /
edges_status_changed`. The MVP expects the caller to have already
indexed both commits — historic auto-reindex is a later iteration.

## Clarifying questions (P19)

`specslice questions` surfaces unresolved facts the AI / human
should confirm before acting on the graph. Four categories:

- `orphan_symbol` (info) — no incoming Calls/References/Imports and
  no framework role.
- `pending_candidate` (warn) — AI business candidate not yet
  accepted into the confirmed graph.
- `test_without_references` (info) — TestCase / TestGroup with no
  Calls/References to any indexed symbol.
- `dangling_import` (info) — test imports a module SpecSlice
  doesn't have a node for.

```bash
specslice questions
specslice questions --max-per-category 5 --format json
```

Each question is written as a natural-language prompt ready to
hand to a chat agent verbatim; `artifact_id` and `path` give the
agent the next file / id to read.
- Tier 2 (near-duplicate via SimHash) and tier 3 (behavioral
  duplicate via shared graph neighborhood) are not yet implemented.
  Do not claim they exist.

## Reporting

In the final answer, include:

- whether `specslice` was global or source-run
- commands run and key results
- graph/export file path if produced
- sidecar resolver status
- candidate confidence boundary: fact, candidate, or confirmed
