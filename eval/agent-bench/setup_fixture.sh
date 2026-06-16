#!/usr/bin/env bash
# Inject the dead-code benchmark fixture into a disposable bench worktree and
# (re)build the specslice index, then print the Rust compiler's `never used`
# verdict — the INDEPENDENT oracle for the t5_deadcode task.
#
# Usage: ./setup_fixture.sh /path/to/specslice-bench/wt
set -euo pipefail

WT="${1:?usage: setup_fixture.sh <worktree-path>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ENGINE="$WT/crates/specslice-engine/src"
LIB="$ENGINE/lib.rs"

[ -d "$ENGINE" ] || { echo "not a specslice worktree: $WT" >&2; exit 1; }

cp "$HERE/fixtures/_bench_deadcode_fixture.rs" "$ENGINE/_bench_deadcode_fixture.rs"
if ! grep -q "_bench_deadcode_fixture" "$LIB"; then
  printf '\nmod _bench_deadcode_fixture;\n' >> "$LIB"
fi

echo "== compiler oracle (cargo check, expect both bench_dead_* never used) =="
( cd "$WT" && cargo check -p specslice-engine 2>&1 | grep -E "never used|bench_dead" || true )

echo "== reindexing worktree =="
( cd "$WT" && specslice index >/dev/null 2>&1 || /Users/qjs/Code/Projects/specslice/target/debug/specslice index >/dev/null 2>&1 )

echo "== specslice dead-code sanity (not the oracle; should also list both) =="
( cd "$WT" && specslice dead-code 2>/dev/null | grep -i "bench_dead" || true )
echo "done"
