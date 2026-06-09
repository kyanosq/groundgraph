#!/usr/bin/env python3
"""Export a SpecSlice graph.db into a network JSON for the WebGL viewer.

Usage:
    python3 export_graph.py <graph.db> <out.json> [--keep-isolated]

The viewer wants a light `{meta, nodes, links}` shape:
  nodes: {id, kind, name, path, line, deg}
  links: {source, target, kind}
Degree (in+out) is precomputed so the renderer can size nodes by connectivity.
"""
import json
import sqlite3
import sys
from collections import defaultdict


def export(db_path: str, out_path: str, keep_isolated: bool = False) -> None:
    con = sqlite3.connect(db_path)
    con.row_factory = sqlite3.Row

    nodes = {}
    for r in con.execute(
        "SELECT id, kind, name, path, start_line, end_line FROM nodes"
    ):
        nodes[r["id"]] = {
            "id": r["id"],
            "kind": r["kind"],
            "name": r["name"] or r["id"].split("::")[-1],
            "path": r["path"] or "",
            "line": r["start_line"],
        }

    links = []
    deg = defaultdict(int)
    seen = set()
    for r in con.execute(
        "SELECT from_id, to_id, kind FROM edge_assertions"
    ):
        a, b = r["from_id"], r["to_id"]
        if a not in nodes or b not in nodes or a == b:
            continue
        key = (a, b, r["kind"])
        if key in seen:
            continue
        seen.add(key)
        links.append({"source": a, "target": b, "kind": r["kind"]})
        deg[a] += 1
        deg[b] += 1
    con.close()

    for nid, n in nodes.items():
        n["deg"] = deg.get(nid, 0)

    if keep_isolated:
        out_nodes = list(nodes.values())
    else:
        out_nodes = [n for nid, n in nodes.items() if deg.get(nid, 0) > 0]

    repo = db_path.split("/.specslice")[0].rstrip("/").split("/")[-1]
    payload = {
        "meta": {
            "repo": repo,
            "nodes": len(out_nodes),
            "links": len(links),
        },
        "nodes": out_nodes,
        "links": links,
    }
    with open(out_path, "w") as f:
        json.dump(payload, f, separators=(",", ":"))
    print(
        f"wrote {out_path}: {len(out_nodes)} nodes, {len(links)} links "
        f"(repo={repo})"
    )


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    if len(args) < 2:
        print(__doc__)
        sys.exit(2)
    export(args[0], args[1], keep_isolated="--keep-isolated" in flags)
