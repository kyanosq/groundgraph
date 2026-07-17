#!/usr/bin/env bash
# Sync the webui viewer assets (canonical source under webui/) into a
# crate-local copy under crates/groundgraph-cli/webui/ so graph.rs can
# include_str! them without reaching across the crate boundary — referencing
# files outside the crate directory breaks `cargo package -p groundgraph-cli`.
#
#   scripts/sync_webui_assets.sh          # copy on mismatch (idempotent)
#   scripts/sync_webui_assets.sh --check  # CI freshness gate: exit 1 on drift, never write
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$ROOT/webui"
DST_DIR="$ROOT/crates/groundgraph-cli/webui"
FILES=(index.html vendor/groundgraph-viewer.bundle.js)

CHECK=0
for arg in "$@"; do
  case "$arg" in
    --check) CHECK=1 ;;
    -h|--help)
      echo "usage: $0 [--check]"
      exit 0
      ;;
    *)
      echo "error: unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

# Sources must exist in the canonical webui/ tree.
for f in "${FILES[@]}"; do
  if [ ! -f "$SRC_DIR/$f" ]; then
    echo "error: source asset missing: $SRC_DIR/$f" >&2
    exit 1
  fi
done

# In write mode only, materialise the destination tree before copying.
if [ "$CHECK" -eq 0 ]; then
  mkdir -p "$DST_DIR/vendor"
fi

status=0
for f in "${FILES[@]}"; do
  src="$SRC_DIR/$f"
  dst="$DST_DIR/$f"
  if [ -f "$dst" ] && cmp -s "$src" "$dst"; then
    continue
  fi
  if [ "$CHECK" -eq 1 ]; then
    echo "error: webui asset out of sync: crates/groundgraph-cli/webui/$f" >&2
    echo "  run scripts/sync_webui_assets.sh (no --check) to refresh the crate-local copy." >&2
    status=1
    continue
  fi
  cp "$src" "$dst"
  echo "synced crates/groundgraph-cli/webui/$f"
done

exit "$status"
