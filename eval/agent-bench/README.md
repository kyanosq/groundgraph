# Agent code-lookup benchmark: grep vs specslice

Measures whether giving a coding agent the **specslice** code graph changes its
behaviour and accuracy on code-navigation tasks, compared with the default
**grep/ripgrep** approach — and **records every tool call** so the comparison is
auditable.

## How it works

- **Driver:** `cursor-agent -p --output-format stream-json` runs the same agent
  headless and emits a structured event stream that includes every tool call
  (name + args + result + timestamps), the final answer, wall-clock and token
  usage. No API key needed (reuses your `cursor-agent` login); no hooks needed
  (the stream-json log already contains the full tool trace).
- **Two arms, one variable:** both arms get the *same* model, workspace and task
  text. They differ only by a one-paragraph constraint (see `tasks.json`):
  - `grep` — "use only ripgrep/grep + file reads; do not use specslice"
  - `specslice` — "you have the specslice CLI (search/trace/impact/...); prefer it"
  The recorded tool calls let `score.py` verify each arm stayed in its lane
  (`purity` column: a `grep` run that touched specslice is flagged `LEAK`).
- **Independent ground truth:** answer keys in `tasks.json` are hand-verified
  from source + ripgrep, **never derived from specslice's own graph** — otherwise
  the benchmark would be circular. For scaling, rust-analyzer (call hierarchy) is
  the intended automated oracle.

## Task buckets

| id | bucket | why |
|---|---|---|
| `t1_def_kill_tree` | definitional | grep-fair baseline (both should ace it) |
| `t2_callcount_kill_tree` | call-site precision | bare grep over-counts (def + 4 doc-comment mentions); true call count is 1 → rewards graph precision |
| `t3_callers_detach` | relational callers | "which files call X" across the repo; set-graded P/R/F1 |
| `t4_trace_kill_and_reap` | transitive call closure | downstream closure of `kill_and_reap` ({kill_tree, reap_within}); a deeper closure forces multi-hop reasoning |

## First findings (composer-2.5, N≤3 seeds — anecdote, not proof)

- **t2 is the decisive win.** The `grep` arm could not converge on the true
  call count (1) within a 600 s budget — bare text search drowns in the
  definition + 4 doc-comment mentions of `kill_tree`. The `specslice` arm
  answered in ~44 s. This "precision against textual noise" is where the graph
  pays off.
- **t1 / t3 / t4 tie.** Definitional lookup, cross-file caller listing, and a
  *shallow* (1-hop, 2-node) call closure are all grep-tractable; `specslice`
  matched accuracy and was sometimes faster/cheaper (t3) but not always (t4, where
  its trace exploration used *more* tool calls). The benchmark thus **falsifies
  the over-strong claim that "trace is something grep can't do"** at trivial
  depth.
- **Implication for the next iteration:** to demonstrate the structural
  advantage convincingly, add tasks grep genuinely cannot do at any reasonable
  cost — deep/wide call closures, whole-graph reachability (`dead-code`), or
  semantic facts (`purity`/`facts`). For `dead-code`, use the Rust compiler's
  `never used` lint as the independent oracle (not specslice's own output).

## Setup (pinned, isolated workspace)

The agent runs against a detached **git worktree** pinned to a commit, indexed
once, so runs are reproducible and the real working tree is never touched:

```bash
SS=/Users/qjs/Code/Projects/specslice
git -C "$SS" worktree add --detach /Users/qjs/Code/Projects/specslice-bench/wt HEAD
( cd /Users/qjs/Code/Projects/specslice-bench/wt && specslice index )   # builds .specslice/graph.db
```

`specslice` must be on `PATH` (`cargo install --path crates/specslice-cli`).

## Run

```bash
cd eval/agent-bench
python3 score.py --selftest          # validate parser/grader (no model calls)
python3 run.py                        # all tasks x {grep,specslice} x 1 seed
python3 run.py --only t2_callcount_kill_tree --seeds 3   # focus + variance
python3 score.py                      # parse runs/ -> reports/report.md (+ stdout)
```

Outputs land in `runs/` (raw stream-json per run) and `reports/report.md`; both
are gitignored (reproducible artifacts).

## Honest caveats

- **Availability ≠ usage.** The benchmark conflates "does the agent reach for
  specslice" with "is specslice better once used". The `purity`/`spec` columns
  separate the two: they show what the agent *actually* called.
- **LLM nondeterminism.** Use `--seeds N` and read the per-(task,arm) mean;
  a single run is anecdote, not signal.
- **Grading is heuristic** (substring / set match on free-form answers). Raw
  answers are kept in `runs/` for human audit; tighten `tasks.json` checks if an
  answer is right but scored wrong.
- **v1 benchmarks the specslice CLI**, not the MCP server integration. A v2 can
  attach `specslice-mcp` via a per-arm `.cursor/mcp.json` to test the real agent
  wiring.
- **Small N tasks.** This is a skeleton that closes the record→score→table loop;
  add tasks (especially trace/impact/dead-code, which grep cannot answer at all)
  to make it a real signal.
