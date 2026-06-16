#!/usr/bin/env python3
"""Run the agent code-lookup benchmark.

For each (task, arm, seed) it launches one headless `cursor-agent` run with
`--output-format stream-json`, capturing the full event stream (incl. every
tool call) to `runs/<task>__<arm>__s<seed>.jsonl`. Scoring is a separate step
(`score.py`) so runs can be re-graded without re-spending model calls.

The two arms differ ONLY by a one-paragraph constraint appended to the shared
task prompt (see tasks.json): the `grep` arm is told to use ripgrep/grep, the
`specslice` arm is told it has the specslice CLI. Same model, same workspace,
same task text — so any delta is attributable to the available tooling (and the
recorded tool calls let us verify each arm actually stayed in its lane).
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _as_text(data) -> str:
    if data is None:
        return ""
    if isinstance(data, bytes):
        return data.decode("utf-8", "replace")
    return data


def load_config(tasks_path: Path) -> dict:
    with tasks_path.open(encoding="utf-8") as fh:
        return json.load(fh)


def build_prompt(task: dict, arm_nudge: str) -> str:
    return f"{task['prompt']}\n\n{arm_nudge}"


def run_one(
    *,
    task: dict,
    arm: str,
    arm_nudge: str,
    seed: int,
    workspace: str,
    model: str,
    timeout_secs: int,
    out_dir: Path,
) -> dict:
    prompt = build_prompt(task, arm_nudge)
    out_file = out_dir / f"{task['id']}__{arm}__s{seed}.jsonl"
    cmd = [
        "cursor-agent",
        "-p",
        prompt,
        "--output-format",
        "stream-json",
        "--force",
        "--workspace",
        workspace,
        "--model",
        model,
    ]
    started = time.monotonic()
    status = "ok"
    # start_new_session so a timeout can SIGKILL the whole process group — the
    # headless agent forks children of its own, and an orphaned one would keep
    # holding the index lock (the very failure specslice's proc.rs guards against).
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout_secs)
    except subprocess.TimeoutExpired as exc:
        status = "timeout"
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            proc.kill()
        stdout, stderr = proc.communicate()
        stdout = stdout or _as_text(exc.stdout)
        stderr = stderr or _as_text(exc.stderr)
    except Exception as exc:  # noqa: BLE001 - one bad run must not kill the batch
        proc.kill()
        stdout, stderr = proc.communicate()
        status = f"error:{type(exc).__name__}"
    out_file.write_text(_as_text(stdout), encoding="utf-8")
    if status == "ok" and proc.returncode not in (0, None):
        status = f"exit={proc.returncode}"
    if _as_text(stderr).strip():
        (out_dir / f"{out_file.stem}.stderr.txt").write_text(_as_text(stderr), encoding="utf-8")
    wall = time.monotonic() - started
    return {
        "task": task["id"],
        "arm": arm,
        "seed": seed,
        "status": status,
        "wall_secs": round(wall, 1),
        "out_file": str(out_file.relative_to(HERE)),
    }


def main(argv: list[str] | None = None) -> int:
    cfg_default = HERE / "tasks.json"
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--tasks", type=Path, default=cfg_default)
    ap.add_argument("--workspace", default=None, help="repo under test (pinned worktree)")
    ap.add_argument("--model", default=None)
    ap.add_argument(
        "--arms",
        default="grep,specslice",
        help="comma-separated subset of arms defined in tasks.json",
    )
    ap.add_argument("--seeds", type=int, default=1)
    ap.add_argument("--only", default=None, help="comma-separated task ids to run")
    ap.add_argument("--out", type=Path, default=HERE / "runs")
    ap.add_argument("--timeout", type=int, default=None)
    args = ap.parse_args(argv)

    cfg = load_config(args.tasks)
    defaults = cfg.get("defaults", {})
    workspace = args.workspace or defaults.get("workspace")
    model = args.model or defaults.get("model", "composer-2.5")
    timeout_secs = args.timeout or int(defaults.get("timeout_secs", 300))
    arms_cfg = cfg["arms"]
    arms = [a.strip() for a in args.arms.split(",") if a.strip()]
    for a in arms:
        if a not in arms_cfg:
            ap.error(f"unknown arm '{a}'; defined arms: {sorted(arms_cfg)}")
    if not workspace or not Path(workspace).is_dir():
        ap.error(f"workspace not a directory: {workspace!r} (create the pinned worktree first; see README.md)")

    tasks = cfg["tasks"]
    if args.only:
        want = {t.strip() for t in args.only.split(",")}
        tasks = [t for t in tasks if t["id"] in want]
        if not tasks:
            ap.error(f"--only matched no tasks; available: {[t['id'] for t in cfg['tasks']]}")

    args.out.mkdir(parents=True, exist_ok=True)
    total = len(tasks) * len(arms) * args.seeds
    print(
        f"workspace={workspace}\nmodel={model}  arms={arms}  seeds={args.seeds}  "
        f"tasks={[t['id'] for t in tasks]}\n=> {total} run(s)\n",
        file=sys.stderr,
    )

    summary: list[dict] = []
    n = 0
    for task in tasks:
        for arm in arms:
            for seed in range(1, args.seeds + 1):
                n += 1
                print(f"[{n}/{total}] {task['id']} | {arm} | s{seed} ... ", end="", flush=True, file=sys.stderr)
                res = run_one(
                    task=task,
                    arm=arm,
                    arm_nudge=arms_cfg[arm],
                    seed=seed,
                    workspace=workspace,
                    model=model,
                    timeout_secs=timeout_secs,
                    out_dir=args.out,
                )
                summary.append(res)
                print(f"{res['status']} ({res['wall_secs']}s)", file=sys.stderr)

    (args.out / "_run_summary.json").write_text(
        json.dumps(summary, indent=2, ensure_ascii=False), encoding="utf-8"
    )
    print(f"\nwrote {len(summary)} run(s) to {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
