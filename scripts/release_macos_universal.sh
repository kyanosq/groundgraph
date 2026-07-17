#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-$(sed -n '/^\[workspace.package\]/,/^\[/p' "$ROOT/Cargo.toml" | sed -n 's/^version = "\(.*\)"/\1/p' | head -n 1)}"
PACKAGE_NAME="groundgraph-${VERSION}-macos-universal"
DIST_DIR="$ROOT/dist"
STAGING="$DIST_DIR/$PACKAGE_NAME"
ARCHIVE="$DIST_DIR/$PACKAGE_NAME.tar.gz"
SIGN_IDENTITY="${GROUNDGRAPH_SIGN_IDENTITY:-}"
NOTARY_PROFILE="${GROUNDGRAPH_NOTARY_PROFILE:-}"
ENTITLEMENTS="${GROUNDGRAPH_ENTITLEMENTS:-}"

# Always go through PATH `cargo` (the rustup shim honours
# `rust-toolchain.toml`'s 1.96.0 pin). Earlier versions hard-wired a
# `$HOME/.rustup/toolchains/stable-…/bin/cargo` binary that bypassed the pin and
# could silently drift from the toolchain CI enforces. Override with CARGO_CMD /
# RUSTC if a release machine genuinely needs a bespoke toolchain.
if [ -n "${CARGO_CMD:-}" ]; then
  # shellcheck disable=SC2206
  CARGO_RUN=( $CARGO_CMD )
else
  CARGO_RUN=(cargo)
fi
RUSTC_RUN="${RUSTC:-}"

if ! command -v lipo >/dev/null 2>&1; then
  echo "error: lipo is required on macOS" >&2
  exit 1
fi

# An operator can pin a specific installed toolchain via RUSTUP_TARGET_TOOLCHAIN.
RUSTUP_TARGET_TOOLCHAIN="${RUSTUP_TARGET_TOOLCHAIN:-}"

# Auto-install the Apple targets instead of bailing out. On a fresh CI runner
# the targets are absent; failing the whole job there is unfriendly when the fix
# is a one-line `rustup target add`. Each target is checked+added at most once.
ensure_target() {
  local target="$1"
  if [ -n "$RUSTUP_TARGET_TOOLCHAIN" ]; then
    if ! rustup target list --installed --toolchain "$RUSTUP_TARGET_TOOLCHAIN" | grep -qx "$target"; then
      echo "==> installing missing Rust target $target (toolchain $RUSTUP_TARGET_TOOLCHAIN)"
      rustup target add --toolchain "$RUSTUP_TARGET_TOOLCHAIN" "$target"
    fi
  else
    if ! rustup target list --installed | grep -qx "$target"; then
      echo "==> installing missing Rust target $target"
      rustup target add "$target"
    fi
  fi
}

ensure_target aarch64-apple-darwin
ensure_target x86_64-apple-darwin

rm -rf "$STAGING" "$ARCHIVE"
mkdir -p \
  "$STAGING/bin" \
  "$STAGING/libexec" \
  "$STAGING/tool/groundgraph_dart_analyzer/bin" \
  "$STAGING/tool/groundgraph_dart_analyzer/lib" \
  "$STAGING/tool/groundgraph_dart_analyzer/test" \
  "$STAGING/skills"

# Build both the CLI and the MCP server for each arch in one cargo invocation
# (shared dependency graph, one linker pass per package).
build_target() {
  local arch="$1"
  echo "==> building $arch (groundgraph-cli + groundgraph-mcp)"
  if [ -n "$RUSTC_RUN" ]; then
    RUSTC="$RUSTC_RUN" "${CARGO_RUN[@]}" build \
      -p groundgraph-cli -p groundgraph-mcp \
      --release --locked --target "$arch"
  else
    "${CARGO_RUN[@]}" build \
      -p groundgraph-cli -p groundgraph-mcp \
      --release --locked --target "$arch"
  fi
}

build_target aarch64-apple-darwin
build_target x86_64-apple-darwin

echo "==> creating universal binaries"
lipo -create \
  "$ROOT/target/aarch64-apple-darwin/release/groundgraph" \
  "$ROOT/target/x86_64-apple-darwin/release/groundgraph" \
  -output "$STAGING/libexec/groundgraph"
chmod 755 "$STAGING/libexec/groundgraph"

lipo -create \
  "$ROOT/target/aarch64-apple-darwin/release/groundgraph-mcp" \
  "$ROOT/target/x86_64-apple-darwin/release/groundgraph-mcp" \
  -output "$STAGING/libexec/groundgraph-mcp"
chmod 755 "$STAGING/libexec/groundgraph-mcp"

# ---------------------------------------------------------------------------
# Developer ID signing (issues.md #82). Gated on $GROUNDGRAPH_SIGN_IDENTITY so a
# secret-less CI still produces a build; a release machine that exports the
# identity gets a Gatekeeper-passable binary. To sign + notarise locally:
#
#   # one-time: store an App Store Connect API key (or Apple ID) as a profile
#   xcrun notarytool store-credentials groundgraph-notary \
#     --team-id TEAMID --key AuthKey.p8 --key-id KEYID --issuer ISSUER_UUID
#
#   export GROUNDGRAPH_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
#   export GROUNDGRAPH_NOTARY_PROFILE="groundgraph-notary"   # enables notarisation below
#   # optional: export GROUNDGRAPH_ENTITLEMENTS=/path/to/entitlements.plist
#
# A hardened runtime (`--options runtime`) plus a secure `--timestamp` are
# *required* for notarisation to be accepted; only the Mach-Os under libexec/
# are signed (the bin/ wrappers are shell scripts and need none).
# ---------------------------------------------------------------------------
sign_macho() {
  local macho="$1"
  codesign --force --timestamp --options runtime \
    ${ENTITLEMENTS:+--entitlements "$ENTITLEMENTS"} \
    --sign "$SIGN_IDENTITY" \
    "$macho"
  codesign --verify --strict --verbose=2 "$macho"
}

if [ -n "$SIGN_IDENTITY" ]; then
  echo "==> codesigning universal binaries (Developer ID, hardened runtime)"
  sign_macho "$STAGING/libexec/groundgraph"
  sign_macho "$STAGING/libexec/groundgraph-mcp"
else
  echo "warning: GROUNDGRAPH_SIGN_IDENTITY unset — shipping an ad-hoc/unsigned binary." >&2
  echo "         macOS Gatekeeper will reject it on download (issues.md #82)." >&2
  echo "         Set GROUNDGRAPH_SIGN_IDENTITY to a 'Developer ID Application: …' identity to sign." >&2
fi

# bin/ wrappers: a small relocatable shell shim that execs its sibling libexec
# binary so the package works from any install prefix. One writer builds both.
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
# #99: ship the *single* source-of-truth skill (`skills/groundgraph`, the same one
# the repo dogfoods) instead of a separate, drift-prone `packaging/` copy. The
# former duplicate described only "graph + business-logic analysis" while the
# real skill also covers code search, port/rewrite ledgers and behaviour-fact
# extraction — so end users got a stale skill. One source, no diff to keep green.
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
binaries: macOS universal arm64+x86_64 (groundgraph CLI + groundgraph-mcp)
sidecar: Dart analyzer source included under tool/groundgraph_dart_analyzer
EOF

echo "==> verifying package binaries"
lipo -info "$STAGING/libexec/groundgraph"
lipo -info "$STAGING/libexec/groundgraph-mcp"
"$STAGING/bin/groundgraph" --help >/dev/null
"$STAGING/bin/groundgraph-mcp" --help >/dev/null
rm -rf "$STAGING/tool/groundgraph_dart_analyzer/.dart_tool"

# ---------------------------------------------------------------------------
# Notarisation (issues.md #82). `notarytool` accepts a .zip/.pkg/.dmg — not a
# bare binary or .tar.gz — so we zip the *signed* Mach-Os, submit, and wait for
# Apple's verdict. NOTE: a loose CLI binary / .tar.gz cannot be *stapled*
# (stapling targets app bundles, .dmg, .pkg). A signed + notarised binary still
# passes Gatekeeper when the machine is online; for offline first-run, ship a
# stapled .pkg/.dmg (future step). Gated on $GROUNDGRAPH_NOTARY_PROFILE.
# ---------------------------------------------------------------------------
if [ -n "$NOTARY_PROFILE" ]; then
  if [ -z "$SIGN_IDENTITY" ]; then
    echo "error: GROUNDGRAPH_NOTARY_PROFILE set but GROUNDGRAPH_SIGN_IDENTITY is not — cannot notarise an unsigned binary." >&2
    exit 1
  fi
  echo "==> notarising signed binaries (notarytool submit --wait)"
  NOTARIZE_DIR="$DIST_DIR/$PACKAGE_NAME-notarize"
  NOTARIZE_ZIP="$NOTARIZE_DIR.zip"
  rm -rf "$NOTARIZE_DIR" "$NOTARIZE_ZIP"
  mkdir -p "$NOTARIZE_DIR"
  cp "$STAGING/libexec/groundgraph" "$NOTARIZE_DIR/groundgraph"
  cp "$STAGING/libexec/groundgraph-mcp" "$NOTARIZE_DIR/groundgraph-mcp"
  ditto -c -k "$NOTARIZE_DIR" "$NOTARIZE_ZIP"
  xcrun notarytool submit "$NOTARIZE_ZIP" \
    --keychain-profile "$NOTARY_PROFILE" --wait
  rm -rf "$NOTARIZE_DIR" "$NOTARIZE_ZIP"
  echo "==> verifying Gatekeeper assessment (online)"
  spctl --assess --type execute --verbose=2 "$STAGING/libexec/groundgraph" ||
    echo "warning: spctl assessment did not pass for groundgraph — inspect the notarytool log above." >&2
  spctl --assess --type execute --verbose=2 "$STAGING/libexec/groundgraph-mcp" ||
    echo "warning: spctl assessment did not pass for groundgraph-mcp — inspect the notarytool log above." >&2
else
  echo "note: GROUNDGRAPH_NOTARY_PROFILE unset — skipping notarisation (issues.md #82)." >&2
fi

echo "==> creating archive"
(
  cd "$DIST_DIR"
  tar -czf "$PACKAGE_NAME.tar.gz" "$PACKAGE_NAME"
)

# issues.md #80: write the checksum against a *relative* filename so a
# downloader can `shasum -a 256 -c groundgraph-...tar.gz.sha256` from the dist
# directory. Running shasum on the absolute "$ARCHIVE" embeds the build
# machine's path and makes every user's `-c` fail with "No such file".
(
  cd "$DIST_DIR"
  shasum -a 256 "$PACKAGE_NAME.tar.gz" > "$PACKAGE_NAME.tar.gz.sha256"
)

echo "archive: $ARCHIVE"
cat "$ARCHIVE.sha256"
