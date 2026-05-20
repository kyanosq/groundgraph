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

If Dart analyzer sidecar is available, confirm whether output says `resolver=dart_analyzer`; otherwise report the fallback reason.

3. Check graph consistency:

```bash
specslice --repo-root /path/to/repo check
specslice --repo-root /path/to/repo logic --only-risks
```

4. Export a browser-viewable graph:

```bash
specslice --repo-root /path/to/repo graph --format html --view code
specslice --repo-root /path/to/repo graph --format html --view business
```

Default HTML output is `.specslice/export/graph.html` unless `--out` is passed.

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

## Reporting

In the final answer, include:

- whether `specslice` was global or source-run
- commands run and key results
- graph/export file path if produced
- sidecar resolver status
- candidate confidence boundary: fact, candidate, or confirmed
