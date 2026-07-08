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

`groundgraph-core` has no internal crate dependency, so it can always be
packaged directly. Downstream crates use internal dependencies with both `path`
and `version` requirements, for example:

```toml
groundgraph-core = { path = "../groundgraph-core", version = "0.2.0" }
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
