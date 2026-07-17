#!/usr/bin/env bash
# issues.md #221 — unit-test coverage ratchet for the engine crate.
#
# `cargo test` exercises the engine almost entirely through integration tests
# under tests/. The pure-logic modules in src/ used to carry almost no
# `#[cfg(test)]` guards (38 files when #221 was filed); each round of work
# chips away at that debt. This script makes sure it never grows back.
#
# It counts `crates/groundgraph-engine/src/**/*.rs` files that have NO
# `#[cfg(test)]` module and fails when the count exceeds the baseline. The
# baseline only ever goes DOWN: when you add a `#[cfg(test)]` module to a file
# that previously lacked one, lower BASELINE by the same amount so the ratchet
# stays tight.
#
# Files like lib.rs (module aggregation) or generated code (prost output) are
# legitimately test-free and stay in the count — they are the floor the
# baseline accounts for.
#
# Run locally:  bash scripts/check_unit_test_coverage.sh

set -euo pipefail

# Baseline = number of engine/src/*.rs files without a `#[cfg(test)]` module.
# Lower this (never raise) when you add unit tests to a file that lacked them.
BASELINE=4

SRC_DIR="crates/groundgraph-engine/src"

missing=0
offenders=()
while IFS= read -r -d '' f; do
    if ! grep -qE '#\[cfg\([^)]*\btest\b' "$f"; then
        missing=$((missing + 1))
        offenders+=("$f")
    fi
done < <(find "$SRC_DIR" -type f -name '*.rs' -print0)

echo "engine/src files without #[cfg(test)]: $missing (baseline: $BASELINE)"

if (( missing > BASELINE )); then
    echo
    echo "FAIL: unit-test coverage regressed — $missing files lack #[cfg(test)],"
    echo "      baseline allows $BASELINE. Add a #[cfg(test)] module to one of:"
    printf '      %s\n' "${offenders[@]}"
    echo
    echo "      (If a new file is genuinely test-free by design, raise BASELINE"
    echo "       — but treat that as a last resort, not a habit.)"
    exit 1
fi

if (( missing < BASELINE )); then
    echo
    echo "note: coverage improved ($missing < baseline $BASELINE). Lower BASELINE"
    echo "      in scripts/check_unit_test_coverage.sh to $missing to lock the gain in."
fi

echo "OK"
