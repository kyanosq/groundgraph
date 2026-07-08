//! Shared helpers for the Dart-sidecar golden suites (p4/p5/p7/p8/p9).
//!
//! **#65 (env data race).** `GROUNDGRAPH_DART_ANALYZER*` are process-global, and
//! `cargo test` runs a binary's `#[test]`s on parallel threads. The old
//! per-test `EnvGuard` (set on entry, `remove_var` on drop) raced: one test's
//! drop could unset the var while another was mid-index, and concurrent
//! `set_var`/`remove_var` is a data race (why `std::env::set_var` is `unsafe`
//! since edition 2024). Every golden test in a binary wants the *same* sidecar
//! env, so we set it **exactly once** per process under a [`Once`] and never
//! restore it — no removes, no interleaving, no race. (Env is per-process, so
//! leaving it set never leaks across the separate test binaries.)
//!
//! **#66 (silent pass without a Dart SDK).** On a host with no `dart` the
//! golden regression net used to `eprintln!` + `return`, so the whole suite
//! showed green while testing nothing (stderr is hidden by default). Route the
//! skip through [`dart_golden_ready`]: it prints to **stdout** (visible in
//! `cargo test` output) and, when `GROUNDGRAPH_GOLDEN_REQUIRED` is set, turns
//! the missing toolchain into a hard failure so CI can opt into enforcement.

#![allow(dead_code)] // each test binary includes the whole module but uses a subset.

use std::path::Path;
use std::sync::Once;

static SET_ENV: Once = Once::new();

/// Enable the Dart analyzer sidecar for this test process — once.
///
/// Sets `GROUNDGRAPH_DART_ANALYZER=1` and `GROUNDGRAPH_DART_ANALYZER_BIN=dart run
/// <sidecar>`. Safe to call from every test; only the first call writes.
pub fn enable_dart_sidecar_env(sidecar_abs: &Path) {
    let bin = format!("dart run {}", sidecar_abs.display());
    SET_ENV.call_once(|| {
        std::env::set_var("GROUNDGRAPH_DART_ANALYZER", "1");
        std::env::set_var("GROUNDGRAPH_DART_ANALYZER_BIN", &bin);
    });
}

/// #66 gate. Returns `true` when the golden body should run.
///
/// When the sidecar is unavailable it either panics (CI opted in via
/// `GROUNDGRAPH_GOLDEN_REQUIRED`) or prints a *visible* skip to stdout and
/// returns `false` so the caller can bail without faking a pass.
#[must_use]
pub fn dart_golden_ready(available: bool, ctx: &str) -> bool {
    if available {
        return true;
    }
    assert!(
        std::env::var_os("GROUNDGRAPH_GOLDEN_REQUIRED").is_none()
            && std::env::var_os("GROUNDGRAPH_GOLDEN_REQUIRED").is_none(),
        "{ctx}: GROUNDGRAPH_GOLDEN_REQUIRED is set but the Dart sidecar is unavailable \
         (missing `dart` on PATH or sidecar source) — the golden regression cannot run",
    );
    println!(
        "skipping {ctx}: Dart sidecar unavailable; set GROUNDGRAPH_GOLDEN_REQUIRED=1 to enforce",
    );
    false
}
