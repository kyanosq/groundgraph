#!/usr/bin/env bash
# Build + package GroundGraph for a single Linux target triple.
#
#   scripts/release_linux.sh --target x86_64-unknown-linux-musl
#   scripts/release_linux.sh --target aarch64-unknown-linux-musl
#
# The package layout mirrors the macOS one: relocatable bin/ wrappers over
# libexec/ binaries, the Dart analyzer sidecar source, the AI skill, both
# package READMEs and a BUILD-INFO manifest. Output is
# dist/groundgraph-<ver>-linux-<arch>.tar.gz plus a relative-path .sha256.
#
# `--target` selects the triple; the package-name suffix is derived from it
# (x86_64-* -> linux-x86_64, aarch64-* -> linux-aarch64). The build is run
# through `$BUILDER` (default `cargo`). For aarch64 musl on an x86_64 host, set
# `BUILDER=cross` in CI — its Docker image ships the cross gcc, C/C++ deps
# (rusqlite bundled + 13 tree-sitter grammars) link cleanly, and the runner's
# docker is present on GitHub ubuntu. NOTE: if aarch64-unknown-linux-musl ever
# hits a C-toolchain wall, fall back to aarch64-unknown-linux-gnu + cross; the
# product name below stays `linux-aarch64` so download links are stable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-$(sed -n '/^\[workspace.package\]/,/^\[/p' "$ROOT/Cargo.toml" | sed -n 's/^version = "\(.*\)"/\1/p' | head -n 1)}"
BUILDER="${BUILDER:-cargo}"

# --- parse args -------------------------------------------------------------
TARGET=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --target=*) TARGET="${1#--target=}"; shift ;;
    -h|--help)
      echo "usage: $0 --target <triple>   (e.g. x86_64-unknown-linux-musl)"
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [ -z "$TARGET" ]; then
  echo "usage: $0 --target <triple>   (e.g. x86_64-unknown-linux-musl)" >&2
  exit 2
fi

case "$TARGET" in
  x86_64-*) ARCH="x86_64" ;;
  aarch64-*) ARCH="aarch64" ;;
  *)
    echo "error: unsupported target '$TARGET' (expected x86_64-* or aarch64-*)" >&2
    exit 1
    ;;
esac

PACKAGE_NAME="groundgraph-${VERSION}-linux-${ARCH}"
DIST_DIR="$ROOT/dist"
STAGING="$DIST_DIR/$PACKAGE_NAME"
ARCHIVE="$DIST_DIR/$PACKAGE_NAME.tar.gz"

# sha256sum on Linux, shasum on macOS (so this script can stage a musl/cross
# build from a macOS dev box too). The checksum is written against a relative
# filename so a downloader can verify with `sha256sum -c` from the dist dir.
if command -v sha256sum >/dev/null 2>&1; then
  SHA256=(sha256sum)
else
  SHA256=(shasum -a 256)
fi

read -ra BUILDER_RUN <<<"$BUILDER"

echo "==> building $TARGET (groundgraph-cli + groundgraph-mcp) via ${BUILDER_RUN[*]}"
"${BUILDER_RUN[@]}" build \
  --release --locked --target "$TARGET" \
  -p groundgraph-cli -p groundgraph-mcp

rm -rf "$STAGING" "$ARCHIVE"
mkdir -p \
  "$STAGING/bin" \
  "$STAGING/libexec" \
  "$STAGING/tool/groundgraph_dart_analyzer/bin" \
  "$STAGING/tool/groundgraph_dart_analyzer/lib" \
  "$STAGING/tool/groundgraph_dart_analyzer/test" \
  "$STAGING/skills"

echo "==> staging binaries"
cp "$ROOT/target/$TARGET/release/groundgraph" "$STAGING/libexec/groundgraph"
cp "$ROOT/target/$TARGET/release/groundgraph-mcp" "$STAGING/libexec/groundgraph-mcp"
chmod 755 "$STAGING/libexec/groundgraph" "$STAGING/libexec/groundgraph-mcp"

# bin/ wrappers: a relocatable shell shim that execs its sibling libexec binary,
# so the package works from any install prefix (same shape as the macOS build).
write_wrapper() {
  local out="$1"
  local binname="$2"
  cat > "$out" <<EOF
#!/usr/bin/env bash
set -euo pipefail

SOURCE="\${BASH_SOURCE[0]}"
while [ -h "\$SOURCE" ]; do
  DIR="\$(cd -P "\$(dirname "\$SOURCE")" >/dev/null 2>&1 && pwd)"
  SOURCE="\$(readlink "\$SOURCE")"
  case "\$SOURCE" in
    /*) ;;
    *) SOURCE="\$DIR/\$SOURCE" ;;
  esac
done

ROOT="\$(cd -P "\$(dirname "\$SOURCE")/.." >/dev/null 2>&1 && pwd)"
exec "\$ROOT/libexec/$binname" "\$@"
EOF
  chmod 755 "$out"
}

write_wrapper "$STAGING/bin/groundgraph" groundgraph
write_wrapper "$STAGING/bin/groundgraph-mcp" groundgraph-mcp

echo "==> copying Dart analyzer sidecar"
cp "$ROOT/tool/groundgraph_dart_analyzer/README.md" "$STAGING/tool/groundgraph_dart_analyzer/README.md"
cp "$ROOT/tool/groundgraph_dart_analyzer/analysis_options.yaml" "$STAGING/tool/groundgraph_dart_analyzer/analysis_options.yaml"
cp "$ROOT/tool/groundgraph_dart_analyzer/pubspec.yaml" "$STAGING/tool/groundgraph_dart_analyzer/pubspec.yaml"
if [ -f "$ROOT/tool/groundgraph_dart_analyzer/pubspec.lock" ]; then
  cp "$ROOT/tool/groundgraph_dart_analyzer/pubspec.lock" "$STAGING/tool/groundgraph_dart_analyzer/pubspec.lock"
fi
cp "$ROOT/tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart" "$STAGING/tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart"
cp "$ROOT/tool/groundgraph_dart_analyzer/lib/protocol.dart" "$STAGING/tool/groundgraph_dart_analyzer/lib/protocol.dart"
cp "$ROOT/tool/groundgraph_dart_analyzer/lib/walker.dart" "$STAGING/tool/groundgraph_dart_analyzer/lib/walker.dart"
cp "$ROOT/tool/groundgraph_dart_analyzer/test/walker_test.dart" "$STAGING/tool/groundgraph_dart_analyzer/test/walker_test.dart"

echo "==> copying AI skill"
# #99: ship the single source-of-truth skill (skills/groundgraph), not a
# drift-prone packaging copy.
cp -R "$ROOT/skills/groundgraph" "$STAGING/skills/groundgraph"

echo "==> copying package docs"
cp "$ROOT/packaging/macos/README.md" "$STAGING/README.md"
cp "$ROOT/packaging/macos/README-AI-SKILL.md" "$STAGING/README-AI-SKILL.md"

GIT_REV="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
BUILD_TIME="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
cat > "$STAGING/BUILD-INFO.txt" <<EOF
name: groundgraph
version: $VERSION
package: $PACKAGE_NAME
git_revision: $GIT_REV
build_time_utc: $BUILD_TIME
binaries: linux $ARCH (groundgraph CLI + groundgraph-mcp), target $TARGET
sidecar: Dart analyzer source included under tool/groundgraph_dart_analyzer
EOF

rm -rf "$STAGING/tool/groundgraph_dart_analyzer/.dart_tool"

echo "==> creating archive"
(
  cd "$DIST_DIR"
  tar -czf "$PACKAGE_NAME.tar.gz" "$PACKAGE_NAME"
)

# issues.md #80: relative filename in the checksum so `sha256sum -c` works for a
# downloader from the dist directory.
(
  cd "$DIST_DIR"
  "${SHA256[@]}" "$PACKAGE_NAME.tar.gz" > "$PACKAGE_NAME.tar.gz.sha256"
)

echo "archive: $ARCHIVE"
cat "$ARCHIVE.sha256"
