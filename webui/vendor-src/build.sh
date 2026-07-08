#!/usr/bin/env bash
# Regenerate webui/vendor/groundgraph-viewer.bundle.js — the offline viewer bundle
# (three + 3d-force-graph + UnrealBloomPass as one classic IIFE). The bundle is
# checked in so neither the dev page nor the CLI export needs a network; rerun
# this only when bumping a dependency. Versions are pinned here and in the
# checked-in package-lock.json so CI can audit the exact dependency graph.
set -euo pipefail
cd "$(dirname "$0")/.."   # -> webui/

THREE=0.180.0
FORCE_GRAPH=1.73.4
ESBUILD=0.28.1

npm i -D "three@${THREE}" "3d-force-graph@${FORCE_GRAPH}" "esbuild@${ESBUILD}"
mkdir -p vendor
./node_modules/.bin/esbuild vendor-src/entry.js \
  --bundle --format=iife --minify --legal-comments=none \
  --outfile=vendor/groundgraph-viewer.bundle.js

echo "wrote vendor/groundgraph-viewer.bundle.js ($(du -h vendor/groundgraph-viewer.bundle.js | cut -f1))"
