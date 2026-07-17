# Contributing to GroundGraph

Thanks for your interest in improving GroundGraph! This guide covers the workflow and the conventions the project holds itself to.

## Getting started

```bash
git clone https://github.com/groundgraph/groundgraph.git
cd groundgraph
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
- **Non-invasive by design.** GroundGraph must never write outside `.groundgraph/` in a target repo. Anything that would edit, annotate, or commit to user source is out of scope.
- **Robust scanners.** Hand-rolled (byte/char) scanners must be total: add a `proptest` that feeds arbitrary UTF-8 and asserts *no panic + determinism* (see `crates/groundgraph-engine/tests/p25_scanner_totality_proptest.rs`).
- **Layered tests.** The test suite is organised in six layers (unit / property / golden / capability regression / self-host / end-to-end). Before adding a test, read [`crates/groundgraph-engine/tests/README.md`](crates/groundgraph-engine/tests/README.md) and place it in the right layer; new integration files are named by *domain* (`check_doc_drift.rs`), not by phase number.
- **Test names follow `feature_under_test_condition_expected_outcome`.** A reader should learn what the test asserts from its name alone — e.g. `apply_list_rolls_back_failed_migration_and_does_not_advance_version`, `sqlite_value_to_json_collapses_non_finite_reals_to_null`, `slice_requirement_missing_workspace_errors_with_message`. Avoid generic names (`test1`, `misc`, `works`) that force the reader to open the body to learn anything. The three flavours you will see in the tree are: unit tests under `src/**/mod.rs` (`feature_condition_expected`), golden regression tests (`p[0-9]+_*`), and capability checks (`check_NN_*`). The legacy `p[0-9]+_*` / `check_NN_*` prefixes are grandfathered **only** because each carries a full feature suffix (`p7_dead_code_lists_unreached_pixcraft_symbols_with_confidence`, `check_01_top_level_function_calls_class_method`); new tests use the plain `feature_*` form and name by what is under test, never by the phase that added it.
- **Commit messages are written in Chinese** (项目约定：Git commit 信息使用中文), focused on the *why*.

## Pull requests

1. Branch from `main`.
2. Keep PRs focused and reviewable; include a short description of the *why*.
3. Make sure the full gate (fmt + clippy + tests) is green locally.
4. If you changed user-facing behavior or commands, update `README.md` and `README.zh-CN.md`.

## Project layout

| Crate | Responsibility |
| --- | --- |
| `groundgraph-core` | Graph domain model: nodes, edges, evidence, ids |
| `groundgraph-store` | SQLite store + migrations |
| `groundgraph-engine` | Indexers, scanners, search, analyses |
| `groundgraph-lang-dart` | Dart language support |
| `groundgraph-cli` | The `groundgraph` CLI |
| `groundgraph-mcp` | MCP server |

## Reporting bugs

Open an issue with a minimal reproduction (ideally a small repo or snippet) and the exact command + output. Because GroundGraph is non-invasive, a copy of the target tree under `/tmp` is usually enough to reproduce indexing issues.

## License

By contributing, you agree that your contributions will be dual-licensed under the MIT and Apache-2.0 licenses, as described in the [README](README.md#license).
