#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-$(sed -n '/^\[workspace.package\]/,/^\[/p' "$ROOT/Cargo.toml" | sed -n 's/^version = "\(.*\)"/\1/p' | head -n 1)}"
PACKAGE_NAME="specslice-${VERSION}-macos-universal"
DIST_DIR="$ROOT/dist"
STAGING="$DIST_DIR/$PACKAGE_NAME"
ARCHIVE="$DIST_DIR/$PACKAGE_NAME.tar.gz"

if [ -n "${CARGO_CMD:-}" ]; then
  # shellcheck disable=SC2206
  CARGO_RUN=( $CARGO_CMD )
  RUSTC_RUN="${RUSTC:-}"
  RUSTUP_TARGET_TOOLCHAIN="${RUSTUP_TARGET_TOOLCHAIN:-}"
elif [ -x "$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo" ] &&
  [ -x "$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc" ]; then
  CARGO_RUN=("$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo")
  RUSTC_RUN="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc"
  RUSTUP_TARGET_TOOLCHAIN="${RUSTUP_TARGET_TOOLCHAIN:-stable-aarch64-apple-darwin}"
else
  CARGO_RUN=(cargo)
  RUSTC_RUN="${RUSTC:-}"
  RUSTUP_TARGET_TOOLCHAIN="${RUSTUP_TARGET_TOOLCHAIN:-}"
fi

if ! command -v lipo >/dev/null 2>&1; then
  echo "error: lipo is required on macOS" >&2
  exit 1
fi

if [ -n "$RUSTUP_TARGET_TOOLCHAIN" ]; then
  TARGETS_INSTALLED="$(rustup target list --installed --toolchain "$RUSTUP_TARGET_TOOLCHAIN")"
else
  TARGETS_INSTALLED="$(rustup target list --installed)"
fi

if ! grep -qx "aarch64-apple-darwin" <<<"$TARGETS_INSTALLED"; then
  echo "error: missing Rust target aarch64-apple-darwin" >&2
  echo "run: rustup target add aarch64-apple-darwin" >&2
  exit 1
fi

if ! grep -qx "x86_64-apple-darwin" <<<"$TARGETS_INSTALLED"; then
  echo "error: missing Rust target x86_64-apple-darwin" >&2
  echo "run: rustup target add x86_64-apple-darwin" >&2
  exit 1
fi

rm -rf "$STAGING" "$ARCHIVE"
mkdir -p \
  "$STAGING/bin" \
  "$STAGING/libexec" \
  "$STAGING/tool/specslice_dart_analyzer/bin" \
  "$STAGING/tool/specslice_dart_analyzer/lib" \
  "$STAGING/tool/specslice_dart_analyzer/test" \
  "$STAGING/skills"

echo "==> building arm64"
if [ -n "$RUSTC_RUN" ]; then
  RUSTC="$RUSTC_RUN" "${CARGO_RUN[@]}" build -p specslice-cli --release --locked --target aarch64-apple-darwin
else
  "${CARGO_RUN[@]}" build -p specslice-cli --release --locked --target aarch64-apple-darwin
fi

echo "==> building x86_64"
if [ -n "$RUSTC_RUN" ]; then
  RUSTC="$RUSTC_RUN" "${CARGO_RUN[@]}" build -p specslice-cli --release --locked --target x86_64-apple-darwin
else
  "${CARGO_RUN[@]}" build -p specslice-cli --release --locked --target x86_64-apple-darwin
fi

echo "==> creating universal binary"
lipo -create \
  "$ROOT/target/aarch64-apple-darwin/release/specslice" \
  "$ROOT/target/x86_64-apple-darwin/release/specslice" \
  -output "$STAGING/libexec/specslice"
chmod 755 "$STAGING/libexec/specslice"

cat > "$STAGING/bin/specslice" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

SOURCE="${BASH_SOURCE[0]}"
while [ -h "$SOURCE" ]; do
  DIR="$(cd -P "$(dirname "$SOURCE")" >/dev/null 2>&1 && pwd)"
  SOURCE="$(readlink "$SOURCE")"
  case "$SOURCE" in
    /*) ;;
    *) SOURCE="$DIR/$SOURCE" ;;
  esac
done

ROOT="$(cd -P "$(dirname "$SOURCE")/.." >/dev/null 2>&1 && pwd)"
exec "$ROOT/libexec/specslice" "$@"
EOF
chmod 755 "$STAGING/bin/specslice"

echo "==> copying Dart analyzer sidecar"
cp "$ROOT/tool/specslice_dart_analyzer/README.md" "$STAGING/tool/specslice_dart_analyzer/README.md"
cp "$ROOT/tool/specslice_dart_analyzer/analysis_options.yaml" "$STAGING/tool/specslice_dart_analyzer/analysis_options.yaml"
cp "$ROOT/tool/specslice_dart_analyzer/pubspec.yaml" "$STAGING/tool/specslice_dart_analyzer/pubspec.yaml"
if [ -f "$ROOT/tool/specslice_dart_analyzer/pubspec.lock" ]; then
  cp "$ROOT/tool/specslice_dart_analyzer/pubspec.lock" "$STAGING/tool/specslice_dart_analyzer/pubspec.lock"
fi
cp "$ROOT/tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart" "$STAGING/tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart"
cp "$ROOT/tool/specslice_dart_analyzer/lib/protocol.dart" "$STAGING/tool/specslice_dart_analyzer/lib/protocol.dart"
cp "$ROOT/tool/specslice_dart_analyzer/lib/walker.dart" "$STAGING/tool/specslice_dart_analyzer/lib/walker.dart"
cp "$ROOT/tool/specslice_dart_analyzer/test/walker_test.dart" "$STAGING/tool/specslice_dart_analyzer/test/walker_test.dart"

echo "==> copying AI skill"
cp -R "$ROOT/packaging/skills/specslice" "$STAGING/skills/specslice"

echo "==> copying package docs"
cp "$ROOT/packaging/macos/README.md" "$STAGING/README.md"
cp "$ROOT/packaging/macos/README-AI-SKILL.md" "$STAGING/README-AI-SKILL.md"

GIT_REV="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
BUILD_TIME="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
cat > "$STAGING/BUILD-INFO.txt" <<EOF
name: specslice
version: $VERSION
package: $PACKAGE_NAME
git_revision: $GIT_REV
build_time_utc: $BUILD_TIME
binary: macOS universal arm64+x86_64
sidecar: Dart analyzer source included under tool/specslice_dart_analyzer
EOF

echo "==> verifying package binary"
lipo -info "$STAGING/libexec/specslice"
"$STAGING/bin/specslice" --help >/dev/null
rm -rf "$STAGING/tool/specslice_dart_analyzer/.dart_tool"

echo "==> creating archive"
(
  cd "$DIST_DIR"
  tar -czf "$PACKAGE_NAME.tar.gz" "$PACKAGE_NAME"
)

shasum -a 256 "$ARCHIVE" > "$ARCHIVE.sha256"

echo "archive: $ARCHIVE"
cat "$ARCHIVE.sha256"
