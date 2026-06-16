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
| `t5_deadcode_island` | whole-graph reachability / dead island | injected fixture: `bench_dead_beta` calls `bench_dead_alpha`, nothing calls `beta` → BOTH dead. Oracle = Rust compiler `never used` (flags both). grep's trap: `alpha` *is* textually referenced (by the equally-dead `beta`) |
| `t6_biz_guardrail_closure` | business-logic assembly (real domain repo) | **runs against MetaQuant** (A-share quant, Python). The 17-fn downstream closure of the anti-backtest-deception guardrail (`evaluate_research_guardrails`). Oracle = hand-traced from the 530-line source; the only excluded module fn is the `*_to_dict` serializer the entry point never calls. Vocabulary gap: docs say "防回测欺骗门禁 / anti-deception gate", code says "guardrails" |

## Findings (composer-2.5, N≤3 seeds — anecdote, not proof)

- **t2 is the only decisive win.** The `grep` arm could not converge on the true
  call count (1) within a 600 s budget — bare text search drowns in the
  definition + 4 doc-comment mentions of `kill_tree`. The `specslice` arm
  answered in ~44 s. This "precision against textual noise" — distinguishing
  real uses from textual hits at a scale where reading every hit is infeasible —
  is the one place the graph clearly paid off.
- **t1 / t3 / t4 / t5 all tie at 1.00.** Definitional lookup, cross-file caller
  listing, a *shallow* (2-node) call closure, **and even a transitively-dead
  island** are all grep-tractable for a capable model: on t5 the `grep` arm got
  {alpha, beta} 3/3 by *reasoning* (`beta` has no caller → dead; `alpha` is only
  called by the dead `beta` → transitively dead). So the benchmark **falsifies
  the over-strong claims that "trace / dead-code are things grep can't do"** at
  trivial graph sizes. `specslice` matched accuracy but on t4/t5 often spent
  *more* tool calls and tokens exploring.
- **A specslice rough edge t5 surfaced:** `specslice dead-code` defaults to
  `--min-confidence medium`, which deliberately *hides* the dead island
  (`alpha` is classified a Low-confidence "dead island" because it has an inbound
  edge from the also-dead `beta`). The full set only appears with
  `--min-confidence low`. Good for precision, but an agent that trusts the
  default under-reports — the recall/precision default is a real usability
  trade-off worth surfacing to MCP/CLI consumers.
- **The scorer had to be hardened (self-fix).** The first t5 scoring falsely gave
  one `specslice` run 0.11: the agent glued prose onto its answer line
  (`DEAD=alpha,betaBoth alpha and beta are unreachable...`) and `grade_set`
  matched the *first* `KEY=` and swept every prose word in as a bogus set member.
  Fix: take the *last* `KEY=` and capture only the leading comma-run of bare
  identifiers (regression-locked in `score.py --selftest`). Lesson: a buggy
  grader silently maligns whichever arm phrases its answer chattily.
- **t6 (business logic, MetaQuant) also ties at 1.00 — and exposed a specslice
  default bug.** Both arms perfectly enumerated the 17-fn guardrail closure
  (grep reads the 530-line file and traces; specslice runs `trace`). But the
  specslice arm was *not* cheaper: `specslice trace <symbol>` defaulted to 6
  fuzzy seeds, so a bare trace of `evaluate_research_guardrails` pulled in
  unrelated same-token symbols (`_write_artifacts` in three other scripts) and
  blew the closure up to 38 nodes — forcing the agent to re-filter with grep +
  `--json | python` + file reads. **Fix shipped** (`trace.rs::select_seeds`): an
  exact-name query now pins to the exact symbol(s) only; a bare trace of the
  guardrail entry dropped from *6 seeds / 38 nodes* to *1 seed / 20 nodes*
  (unit-test-locked). This is the literal answer to "make business-logic lookup
  more effective": the one-command closure is now trustworthy without manual
  cleanup. (Agent-level tool counts still vary — composer-2.5 cross-checks
  regardless — so the win is at the tool-output level, not yet a decisive
  accuracy/effort gap at this scale.)
- **Implication for the next iteration:** trivial fixtures don't separate the
  arms — a strong model reasons through small graphs with grep. To show a real
  *accuracy* gap you need **scale**: deep/wide call closures (10+ hops, fan-out),
  large dead clusters, or whole-repo reachability where reading every grep hit is
  infeasible. The honest thesis so far is narrow: *specslice wins when the answer
  requires separating real structure from textual noise at a scale that defeats
  read-every-hit* (t2), and otherwise mostly ties on accuracy — though it has
  twice now surfaced its own tool/usability bugs (t5 dead-island default, t6
  trace-seed pollution), which is itself a payoff of running the benchmark.

## Setup (pinned, isolated workspace)

The agent runs against a detached **git worktree** pinned to a commit, indexed
once, so runs are reproducible and the real working tree is never touched:

```bash
SS=/Users/qjs/Code/Projects/specslice
git -C "$SS" worktree add --detach /Users/qjs/Code/Projects/specslice-bench/wt HEAD
( cd /Users/qjs/Code/Projects/specslice-bench/wt && specslice index )   # builds .specslice/graph.db

# t5 only: inject the dead-code fixture, print the compiler oracle, reindex
./setup_fixture.sh /Users/qjs/Code/Projects/specslice-bench/wt
```

`specslice` must be on `PATH` (`cargo install --path crates/specslice-cli`).
The `t5` fixture (`fixtures/_bench_deadcode_fixture.rs` + `setup_fixture.sh`)
lives in this dir and is injected only into the disposable worktree — never the
real crate. Its ground truth is whatever `cargo check` reports as `never used`.

A task may override `workspace` (and `timeout_secs`) in `tasks.json` to target a
*different* repo — `t6` runs against **MetaQuant** (a Python quant repo) to test
real business-logic lookup, not just the Rust self-host. Set it up as a pinned,
read-only worktree, reusing the already-built index so no re-index is needed:

```bash
MQ=/path/to/MetaQuant
git -C "$MQ" worktree add --detach /Users/qjs/Code/Projects/specslice-bench/mq "$(git -C "$MQ" rev-parse HEAD)"
cp -R "$MQ/.specslice" "$MQ/.specslice.yaml" /Users/qjs/Code/Projects/specslice-bench/mq/   # graph.db stores RELATIVE paths → portable
```

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
- **Small N tasks.** This is a skeleton that closes the record→score→table loop.
  Note the measured reality: at *trivial* graph sizes grep + a capable model can
  answer trace (t4) and dead-island (t5) too — the structural advantage needs
  **scale** (deep/wide closures, large dead clusters) to become an accuracy gap.
