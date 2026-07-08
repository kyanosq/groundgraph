#!/usr/bin/env python3
"""Parse captured cursor-agent runs and score grep-arm vs groundgraph-arm.

Reads `runs/<task>__<arm>__s<seed>.jsonl` produced by run.py, extracts the
final answer + every tool call from the stream-json event log, grades the
answer against the INDEPENDENT ground truth in tasks.json, and renders a
markdown comparison table.

Run `python3 score.py --selftest` to validate the parser/grader on a synthetic
fixture without spending any model calls.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
_NON_TOOL_KEYS = {"hookAdditionalContexts", "toolCallId"}


def _norm(s: str) -> str:
    """Lowercase, collapse whitespace, and tighten `key = val` to `key=val`."""
    s = re.sub(r"\s+", " ", s.lower())
    s = re.sub(r"\s*=\s*", "=", s)
    return s


_SHELL_SPLIT = re.compile(r"[;|&]+|\$\(|\)|`|&&|\|\|")


def _cmd_invokes(command: str, binaries: set[str]) -> bool:
    """True iff a pipeline segment's *executable* is one of `binaries`.

    Splits on shell separators and inspects the first token of each segment, so
    a search TERM or PATH that merely contains the binary's name (e.g. the
    `groundgraph` in the path `crates/groundgraph-engine/...` or the workspace
    `.../groundgraph-bench/wt`) does not count as an invocation.
    """
    if not command:
        return False
    for seg in _SHELL_SPLIT.split(command):
        toks = seg.strip().split()
        if not toks:
            continue
        head = toks[0]
        # skip a leading env-assignment or `command`/`sudo` wrapper
        i = 0
        while i < len(toks) and ("=" in toks[i] or toks[i] in {"command", "sudo", "nice", "time", "env"}):
            i += 1
        head = toks[i] if i < len(toks) else head
        exe = head.rsplit("/", 1)[-1]  # basename, in case of an absolute path
        if exe in binaries:
            return True
        if exe == "git" and len(toks) > i + 1 and toks[i + 1] == "grep" and "git grep" in binaries:
            return True
    return False


@dataclass
class ToolCall:
    name: str
    args_str: str
    command: str = ""

    def tags(self) -> set[str]:
        name_l = self.name.lower()
        tags: set[str] = set()
        if "groundgraph" in name_l or _cmd_invokes(self.command, {"groundgraph"}):
            tags.add("groundgraph")
        if re.search(r"grep|ripgrep", name_l) or _cmd_invokes(
            self.command, {"rg", "grep", "egrep", "fgrep", "git grep"}
        ):
            tags.add("grep")
        if "read" in name_l:
            tags.add("read")
        if re.search(r"shell|terminal|command|bash", name_l):
            tags.add("shell")
        if not tags:
            tags.add("other")
        return tags


@dataclass
class RunData:
    task: str
    arm: str
    seed: int
    final_answer: str = ""
    tool_calls: list[ToolCall] = field(default_factory=list)
    in_tokens: int = 0
    out_tokens: int = 0
    duration_ms: int = 0
    is_error: bool = False
    missing: bool = False
    has_result: bool = False

    def count(self, tag: str) -> int:
        return sum(1 for tc in self.tool_calls if tag in tc.tags())


_RUN_RE = re.compile(r"^(?P<task>.+)__(?P<arm>[^_]+)__s(?P<seed>\d+)\.jsonl$")


def parse_run(path: Path) -> RunData:
    m = _RUN_RE.match(path.name)
    task = m.group("task") if m else path.stem
    arm = m.group("arm") if m else "?"
    seed = int(m.group("seed")) if m else 0
    rd = RunData(task=task, arm=arm, seed=seed)

    text = path.read_text(encoding="utf-8") if path.exists() else ""
    if not text.strip():
        rd.missing = True
        return rd

    last_assistant_text = ""
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        etype = ev.get("type")
        if etype == "tool_call" and ev.get("subtype") == "started":
            tc_obj = ev.get("tool_call", {})
            tool_name = next((k for k in tc_obj if k not in _NON_TOOL_KEYS), "unknown")
            args = tc_obj.get(tool_name, {}).get("args", {}) if isinstance(tc_obj.get(tool_name), dict) else {}
            command = ""
            if isinstance(args, dict):
                for k in ("command", "cmd", "script"):
                    if isinstance(args.get(k), str):
                        command = args[k]
                        break
            rd.tool_calls.append(
                ToolCall(name=tool_name, args_str=json.dumps(args, ensure_ascii=False), command=command)
            )
        elif etype == "assistant":
            content = ev.get("message", {}).get("content", [])
            for block in content:
                if isinstance(block, dict) and block.get("type") == "text":
                    last_assistant_text = block.get("text", "")
        elif etype == "result":
            rd.has_result = True
            rd.duration_ms = int(ev.get("duration_ms", 0) or 0)
            rd.is_error = bool(ev.get("is_error", False))
            usage = ev.get("usage", {}) or {}
            rd.in_tokens = int(usage.get("inputTokens", 0) or 0)
            rd.out_tokens = int(usage.get("outputTokens", 0) or 0)
            if isinstance(ev.get("result"), str) and ev["result"].strip():
                rd.final_answer = ev["result"]
    if not rd.final_answer:
        rd.final_answer = last_assistant_text
    return rd


# ---- grading ----------------------------------------------------------------


def grade_fields(answer: str, checks: list[dict]) -> tuple[float, dict]:
    norm = _norm(answer)
    details = {}
    passed = 0
    for chk in checks:
        ok = all(_norm(tok) in norm for tok in chk["must_contain_all"])
        details[chk["name"]] = ok
        passed += int(ok)
    score = passed / len(checks) if checks else 0.0
    return score, details


def _basenames_in(text: str) -> list[str]:
    return [m.lower() for m in re.findall(r"[A-Za-z0-9_]+\.rs", text)]


def _clean_token(t: str) -> str:
    return t.strip().strip("`'\"(),;*").rstrip(".").strip().lower()


def _extract_set_value(answer: str, key: str) -> str | None:
    """Return the comma-list value of the *last* `KEY=` occurrence.

    Agents often emit the formatted answer line and then keep talking on the
    SAME line (no newline), or repeat it inside a markdown-bold summary
    (`**DEAD=a,b**`). So: (1) take the LAST `KEY=` — that is the agent's
    conclusion, not its opening narration; (2) capture only the leading run of
    comma-separated identifiers / file basenames, stopping at the first char
    that cannot belong to a bare token (a space, backtick, or `*`). This keeps
    trailing prose (e.g. "...betaBoth and ... are unreachable") from being
    scored as dozens of bogus set members (which silently tanked precision).
    """
    matches = list(re.finditer(rf"{re.escape(key)}\s*=\s*", answer, re.IGNORECASE))
    if not matches:
        return None
    tail = answer[matches[-1].end() :]
    m = re.match(r"`?[\w.]+`?(?:\s*,\s*`?[\w.]+`?)*", tail)
    return m.group(0) if m else ""


def grade_set(answer: str, key: str, truth: list[str]) -> tuple[float, dict]:
    """Set-graded P/R/F1. Works for file-basename sets and bare-identifier sets.

    Prefers the `KEY=a,b,c` one-line answer format; falls back to scanning when
    that line is absent (`.rs` basenames for file-type truth, membership scan
    otherwise).
    """
    truth_set = {_clean_token(t) for t in truth}
    raw = _extract_set_value(answer, key)
    if raw is not None:
        predicted = {_clean_token(t) for t in raw.split(",") if _clean_token(t)}
        source = "key-line"
    elif all(t.endswith(".rs") for t in truth_set):
        predicted = set(_basenames_in(answer))
        source = "fallback-rs"
    else:
        predicted = {t for t in truth_set if t in answer.lower()}
        source = "fallback-membership"
    tp = len(predicted & truth_set)
    precision = tp / len(predicted) if predicted else 0.0
    recall = tp / len(truth_set) if truth_set else 0.0
    f1 = (2 * precision * recall / (precision + recall)) if (precision + recall) else 0.0
    return f1, {
        "precision": round(precision, 2),
        "recall": round(recall, 2),
        "f1": round(f1, 2),
        "predicted": sorted(predicted),
        "missing": sorted(truth_set - predicted),
        "extra": sorted(predicted - truth_set),
        "source": source,
    }


def grade_task(task: dict, answer: str) -> tuple[float, dict]:
    g = task["grade"]
    if g["type"] == "fields":
        return grade_fields(answer, g["checks"])
    if g["type"] == "set":
        return grade_set(answer, g["key"], g["truth"])
    raise ValueError(f"unknown grade type {g['type']!r}")


# ---- rendering --------------------------------------------------------------


def purity_flag(rd: RunData) -> str:
    spec = rd.count("groundgraph")
    if rd.arm == "grep":
        return "ok" if spec == 0 else f"LEAK({spec} groundgraph)"
    if rd.arm == "groundgraph":
        return "used" if spec > 0 else "UNUSED"
    return "-"


def render(tasks: dict, runs: list[RunData]) -> str:
    by_id = {t["id"]: t for t in tasks["tasks"]}
    lines = ["# Agent code-lookup benchmark — grep vs groundgraph", ""]
    lines.append(f"model={tasks.get('defaults', {}).get('model', '?')}  |  runs={len(runs)}")
    lines.append("")
    lines.append("| task | arm | score | pass | purity | tools | grep | spec | read | dur(s) | in/out tok | answer |")
    lines.append("|---|---|--:|:--:|---|--:|--:|--:|--:|--:|--:|---|")
    agg: dict[tuple[str, str], list[float]] = {}
    for rd in sorted(runs, key=lambda r: (r.task, r.arm, r.seed)):
        task = by_id.get(rd.task)
        if rd.missing or task is None:
            score, det = 0.0, {"note": "missing run" if rd.missing else "unknown task"}
        else:
            score, det = grade_task(task, rd.final_answer)
        agg.setdefault((rd.task, rd.arm), []).append(score)
        ans = re.sub(r"\s+", " ", rd.final_answer).strip()
        incomplete = not rd.missing and not rd.has_result
        if incomplete:
            ans = "[TIMEOUT/no result event] " + ans
        if len(ans) > 60:
            ans = ans[:57] + "..."
        passed = "TO" if incomplete else ("Y" if score >= 0.999 else ("." if score > 0 else "N"))
        lines.append(
            f"| {rd.task} | {rd.arm} | {score:.2f} | {passed} | {purity_flag(rd)} | "
            f"{len(rd.tool_calls)} | {rd.count('grep')} | {rd.count('groundgraph')} | {rd.count('read')} | "
            f"{rd.duration_ms/1000:.1f} | {rd.in_tokens}/{rd.out_tokens} | {ans} |"
        )

    lines += ["", "## per (task,arm) mean score", "", "| task | arm | mean | n |", "|---|---|--:|--:|"]
    for (task_id, arm), scores in sorted(agg.items()):
        lines.append(f"| {task_id} | {arm} | {sum(scores)/len(scores):.2f} | {len(scores)} |")

    # arm-level rollup
    arm_scores: dict[str, list[float]] = {}
    arm_tools: dict[str, list[int]] = {}
    for rd in runs:
        task = by_id.get(rd.task)
        s = grade_task(task, rd.final_answer)[0] if (task and not rd.missing) else 0.0
        arm_scores.setdefault(rd.arm, []).append(s)
        arm_tools.setdefault(rd.arm, []).append(len(rd.tool_calls))
    lines += ["", "## arm rollup", "", "| arm | mean score | mean tool calls | n |", "|---|--:|--:|--:|"]
    for arm in sorted(arm_scores):
        ss, tt = arm_scores[arm], arm_tools[arm]
        lines.append(f"| {arm} | {sum(ss)/len(ss):.2f} | {sum(tt)/len(tt):.1f} | {len(ss)} |")
    return "\n".join(lines) + "\n"


def load_runs(runs_dir: Path) -> list[RunData]:
    return [parse_run(p) for p in sorted(runs_dir.glob("*.jsonl")) if not p.name.startswith("_")]


# ---- selftest ---------------------------------------------------------------


def _selftest() -> int:
    import tempfile

    fixture = "\n".join(
        json.dumps(ev)
        for ev in [
            {"type": "system", "subtype": "init"},
            {"type": "user"},
            {
                "type": "tool_call",
                "subtype": "started",
                # path contains "groundgraph" — must NOT be miscounted as a groundgraph invocation
                "tool_call": {"shellToolCall": {"args": {"command": "rg -n detach_process_group /Users/x/groundgraph-bench/wt/crates/groundgraph-engine"}}, "toolCallId": "a"},
            },
            {
                "type": "tool_call",
                "subtype": "started",
                "tool_call": {"shellToolCall": {"args": {"command": "groundgraph search detach_process_group"}}, "toolCallId": "b"},
            },
            {"type": "tool_call", "subtype": "completed", "tool_call": {"shellToolCall": {"args": {}, "result": {}}}},
            {"type": "assistant", "message": {"content": [{"type": "text", "text": "FILES=lsp_client.rs,scip_runner.rs"}]}},
            {
                "type": "result",
                "subtype": "success",
                "duration_ms": 12000,
                "is_error": False,
                "result": "FILES=dart_sidecar.rs,lsp_client.rs,scip_runner.rs,lsp_probe.rs",
                "usage": {"inputTokens": 5, "outputTokens": 40},
            },
        ]
    )
    failures = []
    with tempfile.TemporaryDirectory() as d:
        p = Path(d) / "t3_callers_detach__groundgraph__s1.jsonl"
        p.write_text(fixture, encoding="utf-8")
        rd = parse_run(p)
        if rd.task != "t3_callers_detach" or rd.arm != "groundgraph" or rd.seed != 1:
            failures.append(f"filename parse: {rd.task}/{rd.arm}/{rd.seed}")
        if len(rd.tool_calls) != 2:
            failures.append(f"expected 2 started tool calls, got {len(rd.tool_calls)}")
        if rd.count("grep") != 1:
            failures.append(f"expected 1 grep call, got {rd.count('grep')}")
        if rd.count("groundgraph") != 1:
            failures.append(f"expected 1 groundgraph call, got {rd.count('groundgraph')}")
        if rd.duration_ms != 12000 or rd.out_tokens != 40:
            failures.append("result fields not parsed")
        if "result event" not in rd.final_answer and "FILES=dart_sidecar" not in rd.final_answer:
            failures.append(f"final answer should prefer result event, got {rd.final_answer!r}")

    # grading
    f1, det = grade_set("FILES=dart_sidecar.rs,lsp_client.rs,scip_runner.rs,lsp_probe.rs", "FILES",
                        ["dart_sidecar.rs", "lsp_client.rs", "scip_runner.rs", "lsp_probe.rs"])
    if f1 < 0.999:
        failures.append(f"perfect set should score 1.0, got {f1} ({det})")
    f1b, _ = grade_set("FILES=lsp_client.rs", "FILES", ["dart_sidecar.rs", "lsp_client.rs", "scip_runner.rs", "lsp_probe.rs"])
    if not (0.0 < f1b < 0.6):
        failures.append(f"partial set f1 out of range: {f1b}")
    # identifier set (function names, not files)
    f1c, dc = grade_set("FUNCS=`kill_tree`, reap_within", "FUNCS", ["kill_tree", "reap_within"])
    if f1c < 0.999:
        failures.append(f"identifier set should score 1.0, got {f1c} ({dc})")
    # regression: agent glues prose onto the answer line AND repeats a bold
    # summary later. The OLD scorer matched the first `DEAD=` and swept the
    # prose words in as bogus members (precision ~0.11). Must score 1.0 now.
    glued = (
        "I'll run dead-code analysis.\n"
        "DEAD=bench_dead_alpha,bench_dead_betaBoth `bench_dead_alpha` and "
        "`bench_dead_beta` are unreachable from any entry point.\n"
        "**DEAD=bench_dead_alpha,bench_dead_beta**"
    )
    f1d, dd = grade_set(glued, "DEAD", ["bench_dead_alpha", "bench_dead_beta"])
    if f1d < 0.999:
        failures.append(f"glued-prose answer should score 1.0, got {f1d} ({dd})")
    # an honest miss must still be penalized (no over-rescue)
    f1e, _ = grade_set("DEAD=bench_dead_beta", "DEAD", ["bench_dead_alpha", "bench_dead_beta"])
    if not (0.0 < f1e < 0.8):
        failures.append(f"partial DEAD f1 out of range: {f1e}")
    sc, d = grade_fields("CALLS = 1 | SITES=crates/x/proc.rs:91", [
        {"name": "call_count", "must_contain_all": ["CALLS=1"]},
        {"name": "call_site", "must_contain_all": ["proc.rs", "91"]},
    ])
    if sc < 0.999:
        failures.append(f"fields grading should tolerate 'CALLS = 1', got {sc} ({d})")

    if failures:
        print("SELFTEST FAILED:")
        for f in failures:
            print("  -", f)
        return 1
    print("selftest ok")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--tasks", type=Path, default=HERE / "tasks.json")
    ap.add_argument("--runs", type=Path, default=HERE / "runs")
    ap.add_argument("--out", type=Path, default=HERE / "reports" / "report.md")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args(argv)

    if args.selftest:
        return _selftest()

    tasks = json.loads(args.tasks.read_text(encoding="utf-8"))
    runs = load_runs(args.runs)
    if not runs:
        print(f"no runs found in {args.runs} (run.py first)", file=sys.stderr)
        return 1
    report = render(tasks, runs)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(report, encoding="utf-8")
    print(report)
    print(f"(written to {args.out})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
