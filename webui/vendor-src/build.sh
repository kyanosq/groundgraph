#!/usr/bin/env bash
# Regenerate webui/vendor/specslice-viewer.bundle.js — the offline viewer bundle
# (three + 3d-force-graph + UnrealBloomPass as one classic IIFE). The bundle is
# checked in so neither the dev page nor the CLI export needs a network; rerun
# this only when bumping a dependency. Versions are pinned for a reproducible
# artifact (package.json is intentionally git-ignored in webui/).
set -euo pipefail
cd "$(dirname "$0")/.."   # -> webui/

THREE=0.180.0
FORCE_GRAPH=1.73.4
ESBUILD=0.24.0

npm i -D "three@${THREE}" "3d-force-graph@${FORCE_GRAPH}" "esbuild@${ESBUILD}"
mkdir -p vendor
./node_modules/.bin/esbuild vendor-src/entry.js \
  --bundle --format=iife --minify --legal-comments=none \
  --outfile=vendor/specslice-viewer.bundle.js

echo "wrote vendor/specslice-viewer.bundle.js ($(du -h vendor/specslice-viewer.bundle.js | cut -f1))"
