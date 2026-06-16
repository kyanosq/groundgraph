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

# ---------------------------------------------------------------------------
# Developer ID signing (issues.md #82). Gated on $SPECSLICE_SIGN_IDENTITY so a
# secret-less CI still produces a build; a release machine that exports the
# identity gets a Gatekeeper-passable binary. To sign + notarise locally:
#
#   # one-time: store an App Store Connect API key (or Apple ID) as a profile
#   xcrun notarytool store-credentials specslice-notary \
#     --team-id TEAMID --key AuthKey.p8 --key-id KEYID --issuer ISSUER_UUID
#
#   export SPECSLICE_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
#   export SPECSLICE_NOTARY_PROFILE="specslice-notary"   # enables notarisation below
#   # optional: export SPECSLICE_ENTITLEMENTS=/path/to/entitlements.plist
#
# A hardened runtime (`--options runtime`) plus a secure `--timestamp` are
# *required* for notarisation to be accepted; only the Mach-O under libexec/
# is signed (the bin/ wrapper is a shell script and needs none).
# ---------------------------------------------------------------------------
if [ -n "${SPECSLICE_SIGN_IDENTITY:-}" ]; then
  echo "==> codesigning universal binary (Developer ID, hardened runtime)"
  codesign --force --timestamp --options runtime \
    ${SPECSLICE_ENTITLEMENTS:+--entitlements "$SPECSLICE_ENTITLEMENTS"} \
    --sign "$SPECSLICE_SIGN_IDENTITY" \
    "$STAGING/libexec/specslice"
  codesign --verify --strict --verbose=2 "$STAGING/libexec/specslice"
else
  echo "warning: SPECSLICE_SIGN_IDENTITY unset — shipping an ad-hoc/unsigned binary." >&2
  echo "         macOS Gatekeeper will reject it on download (issues.md #82)." >&2
  echo "         Set SPECSLICE_SIGN_IDENTITY to a 'Developer ID Application: …' identity to sign." >&2
fi

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
# #99: ship the *single* source-of-truth skill (`skills/specslice`, the same one
# the repo dogfoods) instead of a separate, drift-prone `packaging/` copy. The
# former duplicate described only "graph + business-logic analysis" while the
# real skill also covers code search, port/rewrite ledgers and behaviour-fact
# extraction — so end users got a stale skill. One source, no diff to keep green.
cp -R "$ROOT/skills/specslice" "$STAGING/skills/specslice"

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

# ---------------------------------------------------------------------------
# Notarisation (issues.md #82). `notarytool` accepts a .zip/.pkg/.dmg — not a
# bare binary or .tar.gz — so we zip the *signed* Mach-O, submit, and wait for
# Apple's verdict. NOTE: a loose CLI binary / .tar.gz cannot be *stapled*
# (stapling targets app bundles, .dmg, .pkg). A signed + notarised binary still
# passes Gatekeeper when the machine is online; for offline first-run, ship a
# stapled .pkg/.dmg (future step). Gated on $SPECSLICE_NOTARY_PROFILE.
# ---------------------------------------------------------------------------
if [ -n "${SPECSLICE_NOTARY_PROFILE:-}" ]; then
  if [ -z "${SPECSLICE_SIGN_IDENTITY:-}" ]; then
    echo "error: SPECSLICE_NOTARY_PROFILE set but SPECSLICE_SIGN_IDENTITY is not — cannot notarise an unsigned binary." >&2
    exit 1
  fi
  echo "==> notarising signed binary (notarytool submit --wait)"
  NOTARIZE_ZIP="$DIST_DIR/$PACKAGE_NAME-notarize.zip"
  rm -f "$NOTARIZE_ZIP"
  ditto -c -k "$STAGING/libexec/specslice" "$NOTARIZE_ZIP"
  xcrun notarytool submit "$NOTARIZE_ZIP" \
    --keychain-profile "$SPECSLICE_NOTARY_PROFILE" --wait
  rm -f "$NOTARIZE_ZIP"
  echo "==> verifying Gatekeeper assessment (online)"
  spctl --assess --type execute --verbose=2 "$STAGING/libexec/specslice" ||
    echo "warning: spctl assessment did not pass — inspect the notarytool log above." >&2
else
  echo "note: SPECSLICE_NOTARY_PROFILE unset — skipping notarisation (issues.md #82)." >&2
fi

echo "==> creating archive"
(
  cd "$DIST_DIR"
  tar -czf "$PACKAGE_NAME.tar.gz" "$PACKAGE_NAME"
)

# issues.md #80: write the checksum against a *relative* filename so a
# downloader can `shasum -a 256 -c specslice-...tar.gz.sha256` from the dist
# directory. Running shasum on the absolute "$ARCHIVE" embeds the build
# machine's path and makes every user's `-c` fail with "No such file".
(
  cd "$DIST_DIR"
  shasum -a 256 "$PACKAGE_NAME.tar.gz" > "$PACKAGE_NAME.tar.gz.sha256"
)

echo "archive: $ARCHIVE"
cat "$ARCHIVE.sha256"
