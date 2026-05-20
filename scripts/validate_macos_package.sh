#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: scripts/validate_macos_package.sh dist/specslice-<version>-macos-universal.tar.gz" >&2
  exit 2
fi

ARCHIVE="$1"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

tar -xzf "$ARCHIVE" -C "$TMP"
ROOT="$(find "$TMP" -mindepth 1 -maxdepth 1 -type d | head -n 1)"

test -x "$ROOT/bin/specslice"
test -x "$ROOT/libexec/specslice"
test -f "$ROOT/tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart"
test -f "$ROOT/tool/specslice_dart_analyzer/pubspec.yaml"
test -f "$ROOT/skills/specslice/SKILL.md"
test -f "$ROOT/skills/specslice/agents/openai.yaml"
test -f "$ROOT/README.md"
test -f "$ROOT/README-AI-SKILL.md"

if find "$ROOT" -path "*/.dart_tool/*" -print -quit | grep -q .; then
  echo "error: package must not contain Dart cache files under .dart_tool" >&2
  exit 1
fi

lipo -info "$ROOT/libexec/specslice" | grep -q "x86_64"
lipo -info "$ROOT/libexec/specslice" | grep -q "arm64"
"$ROOT/bin/specslice" --help >/dev/null

echo "package is valid: $ARCHIVE"
