#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: scripts/validate_macos_package.sh dist/groundgraph-<version>-macos-universal.tar.gz" >&2
  exit 2
fi

ARCHIVE="$1"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# issues.md #80: refuse archives whose members use absolute paths or `..`
# components before extracting — bsdtar warns but still writes such members,
# letting a tampered upstream archive escape "$TMP".
if tar -tzf "$ARCHIVE" | grep -Eq '^/|(^|/)\.\.(/|$)'; then
  echo "error: archive contains absolute or parent-relative (..) member paths; refusing to extract" >&2
  exit 1
fi

tar -xzf "$ARCHIVE" -C "$TMP"

# A well-formed package has exactly one top-level directory. `find ... | head`
# silently picked the first of several; require exactly one so a multi-root
# (or traversal-padded) archive is rejected rather than mis-validated.
TOP_COUNT="$(find "$TMP" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
if [ "$TOP_COUNT" != "1" ]; then
  echo "error: archive must contain exactly one top-level directory, found $TOP_COUNT" >&2
  exit 1
fi
ROOT="$(find "$TMP" -mindepth 1 -maxdepth 1 -type d | head -n 1)"

test -x "$ROOT/bin/groundgraph"
test -x "$ROOT/libexec/groundgraph"
test -f "$ROOT/tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart"
test -f "$ROOT/tool/groundgraph_dart_analyzer/pubspec.yaml"
test -f "$ROOT/skills/groundgraph/SKILL.md"
test -f "$ROOT/skills/groundgraph/agents/openai.yaml"
test -f "$ROOT/README.md"
test -f "$ROOT/README-AI-SKILL.md"

if find "$ROOT" -path "*/.dart_tool/*" -print -quit | grep -q .; then
  echo "error: package must not contain Dart cache files under .dart_tool" >&2
  exit 1
fi

lipo -info "$ROOT/libexec/groundgraph" | grep -q "x86_64"
lipo -info "$ROOT/libexec/groundgraph" | grep -q "arm64"
"$ROOT/bin/groundgraph" --help >/dev/null

echo "package is valid: $ARCHIVE"
