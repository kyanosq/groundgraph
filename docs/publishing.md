# Publishing GroundGraph crates

GroundGraph is a multi-crate workspace. Publish crates in dependency order so
each downstream crate can resolve its sibling dependencies from crates.io during
`cargo publish --dry-run`.

## Release gates

Run these checks from the workspace root before tagging a release:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo package -p groundgraph-core --locked
```

## Dependency version strategy (issues.md #226)

GroundGraph pins reproducibility at the *lock* layer, not at the manifest layer.
Two complementary mechanisms keep a release buildable and auditable:

- **Wide SemVer requirements in the manifest.** Production dependencies are
  declared with a wide range (`anyhow = "1"`, `serde = "1"`, `serde_json = "1"`,
  `serde_norway = "0.9"`, …). This lets `cargo update` pull in patch and minor
  fixes — including security fixes — without churning every manifest each time,
  and it avoids artificial upper bounds that block dependabot-style refreshes.
- **A fully committed `Cargo.lock`, rebuilt under CI `--locked`.** Every release
  tag is built with `cargo <cmd> --locked`, so the exact transitive graph that
  passed the release gates is the exact graph that ships. Drift between the
  manifest ranges and the lock is caught by CI rather than silently resolved.

The exception is **core indexing dependencies whose patch releases can shift
indexer output**: `rusqlite` (`bundled`, where a different vendored SQLite
changes FTS5/WAL/migration behaviour — issues.md #213) and the `tree-sitter`
grammar family. These are pinned *exactly* with `=` so a node id, query, or
SQLite plan cannot change under a `cargo update`:

- `rusqlite = { version = "0.40", features = ["bundled"] }` — exact 0.40 line.
- `tree-sitter-dart = "=0.0.4"` — 0.0.x crates carry no SemVer promise and the
  upstream grammar is stalled (issues.md #211); the `=` makes the requirement
  explicit, while the committed lock + `--locked` pins the exact patch.

This policy is descriptive of the current manifests — existing declarations are
not changed as part of #226 (the only manifest edit was the `#211` exact pin
above). New core-indexing dependencies should default to `=`; new ordinary
dependencies should default to a wide range and rely on the lock for precision.

## Precompiled release binaries

Pushing a `v*` tag triggers `.github/workflows/release.yml`, which builds and
attaches precompiled packages to a GitHub Release for macOS (universal arm64 +
x86_64), Linux (x86_64 and aarch64 musl) and Windows (x86_64) — each package
contains both the `groundgraph` CLI and the `groundgraph-mcp` server. Release
notes are generated from commits. A `workflow_dispatch` run builds and uploads
the same artifacts as workflow artifacts without publishing a Release, so the
pipeline can be dry-run between tags.

`groundgraph-core` has no internal crate dependency, so it can always be
packaged directly. Downstream crates use internal dependencies with both `path`
and `version` requirements, for example:

```toml
groundgraph-core = { path = "../groundgraph-core", version = "0.3.0" }
```

That is the correct crates.io manifest shape. Before the upstream sibling crate
has been published, however, packaging a downstream crate is expected to fail
with "no matching package named ..." because Cargo validates against the public
registry copy.

## First publication order

For a first release of a new version, dry-run and publish one layer at a time:

```bash
cargo publish --dry-run -p groundgraph-core --locked
cargo publish -p groundgraph-core --locked

cargo publish --dry-run -p groundgraph-store --locked
cargo publish -p groundgraph-store --locked

cargo publish --dry-run -p groundgraph-lang-dart --locked
cargo publish -p groundgraph-lang-dart --locked

cargo publish --dry-run -p groundgraph-engine --locked
cargo publish -p groundgraph-engine --locked

cargo publish --dry-run -p groundgraph-cli --locked
cargo publish -p groundgraph-cli --locked

cargo publish --dry-run -p groundgraph-mcp --locked
cargo publish -p groundgraph-mcp --locked
```

Wait for crates.io indexing between layers if a dry-run cannot yet resolve the
just-published package. Re-run the dry-run rather than skipping it.

## External library API

Applications embedding GroundGraph should import:

```rust
use groundgraph_engine::prelude::*;
```

The prelude is the supported high-level facade for the `0.x` line. Lower-level
modules remain public for first-party binaries and advanced integrations, but
new external examples should prefer the prelude so API changes stay easier to
manage.
