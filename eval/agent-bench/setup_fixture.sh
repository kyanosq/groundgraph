#!/usr/bin/env bash
# Inject the dead-code benchmark fixture into a disposable bench worktree and
# (re)build the groundgraph index, then print the Rust compiler's `never used`
# verdict — the INDEPENDENT oracle for the t5_deadcode task.
#
# Usage: ./setup_fixture.sh /path/to/groundgraph-bench/wt
set -euo pipefail

WT="${1:?usage: setup_fixture.sh <worktree-path>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ENGINE="$WT/crates/groundgraph-engine/src"
LIB="$ENGINE/lib.rs"

[ -d "$ENGINE" ] || { echo "not a groundgraph worktree: $WT" >&2; exit 1; }

cp "$HERE/fixtures/_bench_deadcode_fixture.rs" "$ENGINE/_bench_deadcode_fixture.rs"
if ! grep -q "_bench_deadcode_fixture" "$LIB"; then
  printf '\nmod _bench_deadcode_fixture;\n' >> "$LIB"
fi

echo "== compiler oracle (cargo check, expect both bench_dead_* never used) =="
( cd "$WT" && cargo check -p groundgraph-engine 2>&1 | grep -E "never used|bench_dead" || true )

echo "== reindexing worktree =="
( cd "$WT" && groundgraph index >/dev/null 2>&1 || /Users/qjs/Code/Projects/groundgraph/target/debug/groundgraph index >/dev/null 2>&1 )

echo "== groundgraph dead-code sanity (not the oracle; should also list both) =="
( cd "$WT" && groundgraph dead-code 2>/dev/null | grep -i "bench_dead" || true )
echo "done"
