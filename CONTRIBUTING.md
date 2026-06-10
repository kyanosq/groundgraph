# Contributing to SpecSlice

Thanks for your interest in improving SpecSlice! This guide covers the workflow and the conventions the project holds itself to.

## Getting started

```bash
git clone https://github.com/specslice/specslice.git
cd specslice
cargo build            # the pinned toolchain (rust-toolchain.toml) installs automatically
cargo test --workspace
```

The workspace pins its toolchain in [`rust-toolchain.toml`](rust-toolchain.toml). `rustup` will fetch it on first build, so your local checks match CI exactly.

## The bar for a change

Every change must pass the same gate CI enforces (`.github/workflows/ci.yml`):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # zero warnings
cargo test --workspace
```

### Conventions

- **Test-driven development.** New behavior starts with a *failing* test, then the minimal code to make it pass. Bug fixes start with a test that reproduces the bug.
- **Zero warnings.** `clippy -D warnings` is part of the gate. The toolchain is pinned precisely so a linter version bump can't silently introduce warnings.
- **Acceptance by real output.** Claims of "it works" are backed by actual command output / test results, never by prose alone.
- **Non-invasive by design.** SpecSlice must never write outside `.specslice/` in a target repo. Anything that would edit, annotate, or commit to user source is out of scope.
- **Robust scanners.** Hand-rolled (byte/char) scanners must be total: add a `proptest` that feeds arbitrary UTF-8 and asserts *no panic + determinism* (see `crates/specslice-engine/tests/p25_scanner_totality_proptest.rs`).
- **Layered tests.** The test suite is organised in six layers (unit / property / golden / capability regression / self-host / end-to-end). Before adding a test, read [`crates/specslice-engine/tests/README.md`](crates/specslice-engine/tests/README.md) and place it in the right layer; new integration files are named by *domain* (`check_doc_drift.rs`), not by phase number.
- **Commit messages are written in Chinese** (项目约定：Git commit 信息使用中文), focused on the *why*.

## Pull requests

1. Branch from `main`.
2. Keep PRs focused and reviewable; include a short description of the *why*.
3. Make sure the full gate (fmt + clippy + tests) is green locally.
4. If you changed user-facing behavior or commands, update `README.md` and `README.zh-CN.md`.

## Project layout

| Crate | Responsibility |
| --- | --- |
| `specslice-core` | Graph domain model: nodes, edges, evidence, ids |
| `specslice-store` | SQLite store + migrations |
| `specslice-engine` | Indexers, scanners, search, analyses |
| `specslice-lang-dart` | Dart language support |
| `specslice-cli` | The `specslice` binary |
| `specslice-mcp` | The `specslice-mcp` MCP server |

## Reporting bugs

Open an issue with a minimal reproduction (ideally a small repo or snippet) and the exact command + output. Because SpecSlice is non-invasive, a copy of the target tree under `/tmp` is usually enough to reproduce indexing issues.

## License

By contributing, you agree that your contributions will be dual-licensed under the MIT and Apache-2.0 licenses, as described in the [README](README.md#license).
