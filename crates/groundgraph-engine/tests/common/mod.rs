//! Shared helpers for the Dart-sidecar golden suites (p4/p5/p7/p8/p9)
//! and the `dart_sidecar_acceptance` tests.
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
//!
//! **#236 (copy-pasted scaffolding).** Six Dart test binaries each used to carry
//! their own `EnvGuard` / `copy_fixture_into` / `setup_indexed_repo` (~100 lines
//! of identical setup). That logic now lives here once: path + probe helpers,
//! a high-level [`setup_indexed_dart_repo`] for the golden suites that all index
//! `pixcraft_iap` the same way, and an [`EnvGuard`] / [`env_lock`] pair for the
//! acceptance suite whose two tests flip the sidecar env in opposite directions
//! (and therefore cannot use the once-set-never-restore golden pattern).

#![allow(dead_code)] // each test binary includes the whole module but uses a subset.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};

use groundgraph_engine::dart_indexer::{index_dart, DartIndexOptions, RESOLVER_DART_ANALYZER};
use groundgraph_engine::init::{init_repository, InitOptions};

static SET_ENV: Once = Once::new();

// ---------------------------------------------------------------------------
// Path + probe helpers
// ---------------------------------------------------------------------------

/// Two levels up from this crate's `CARGO_MANIFEST_DIR` — the repo root,
/// where `tests/fixtures/` and `tool/groundgraph_dart_analyzer/` live.
pub fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR is nested under the workspace root")
        .to_path_buf()
}

/// Absolute path of a named fixture directory under `tests/fixtures/`.
pub fn fixture_dir(name: &str) -> PathBuf {
    workspace_dir().join("tests/fixtures").join(name)
}

/// Absolute path of the Dart analyzer sidecar entry point.
pub fn sidecar_path() -> PathBuf {
    workspace_dir().join("tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart")
}

/// `true` when the in-repo sidecar source file is present.
pub fn sidecar_source_present() -> bool {
    sidecar_path().exists()
}

/// `true` when a `dart` binary answering `--version` is reachable on `PATH`.
pub fn dart_available() -> bool {
    std::process::Command::new("dart")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Recursively copy every regular file under `src` into `dst`, preserving
/// the relative subdirectory structure.
pub fn copy_fixture_into(src: &Path, dst: &Path) {
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.expect("walking fixture directory");
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("entry path sits under fixture root");
        let target = dst.join(rel);
        std::fs::create_dir_all(target.parent().expect("target has a parent dir"))
            .expect("create fixture target dir");
        std::fs::copy(entry.path(), &target).expect("copy fixture file");
    }
}

// ---------------------------------------------------------------------------
// Sidecar env (once, never restored — golden suites only)
// ---------------------------------------------------------------------------

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
        std::env::var_os("GROUNDGRAPH_GOLDEN_REQUIRED").is_none(),
        "{ctx}: GROUNDGRAPH_GOLDEN_REQUIRED is set but the Dart sidecar is unavailable \
         (missing `dart` on PATH or sidecar source) — the golden regression cannot run",
    );
    println!(
        "skipping {ctx}: Dart sidecar unavailable; set GROUNDGRAPH_GOLDEN_REQUIRED=1 to enforce",
    );
    false
}

/// Materialise `fixture_name` into a fresh repo, enable the analyzer sidecar,
/// migrate the store, and index the fixture through the sidecar.
///
/// Returns `None` when the sidecar is unavailable (soft-skip via
/// [`dart_golden_ready`]); otherwise the indexed temp repo. Asserts the
/// analyzer resolver was actually used so a silent heuristic fallback cannot
/// pass a golden test.
pub fn setup_indexed_dart_repo(
    ctx: &str,
    fixture_name: &str,
    code_roots: &[&str],
) -> Option<tempfile::TempDir> {
    if !dart_golden_ready(sidecar_source_present() && dart_available(), ctx) {
        return None;
    }
    enable_dart_sidecar_env(&sidecar_path());

    let tmp = tempfile::TempDir::new().expect("allocate temp repo");
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .expect("init_repository");
    copy_fixture_into(&fixture_dir(fixture_name), tmp.path());

    let mut store = groundgraph_store::Store::open(tmp.path().join(".groundgraph/graph.db"))
        .expect("open store");
    store.migrate().expect("migrate store");
    let result = index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: code_roots.iter().map(|r| PathBuf::from(*r)).collect(),
            exclude_globs: vec![],
            disable_analyzer: false,
        },
    )
    .expect("index_dart");
    assert_eq!(
        result.resolver_used, RESOLVER_DART_ANALYZER,
        "{ctx}: golden requires the analyzer sidecar to actually run \
         (resolver={:?}, skip_reason={:?})",
        result.resolver_used, result.sidecar_skip_reason
    );
    Some(tmp)
}

// ---------------------------------------------------------------------------
// Scoped env override (acceptance suite — tests that flip env both ways)
// ---------------------------------------------------------------------------

/// Process-wide env mutations race between parallel tests that flip the same
/// var in opposite directions. Each such test takes this lock for its whole
/// body before touching the environment.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Take the env-mutation lock for the duration of a test body.
pub fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Scoped env-var override — restores the previous value (or unsets it) on
/// drop. Pair with [`env_lock`] whenever a test binary contains tests that
/// flip the same env var in opposite directions.
pub struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    /// Set `key` to `value` (or remove it when `value` is `None`), recording
    /// the prior value so [`Drop`] can restore it.
    pub fn set(key: &str, value: Option<&str>) -> Self {
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        Self {
            key: key.into(),
            prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(p) => std::env::set_var(&self.key, p),
            None => std::env::remove_var(&self.key),
        }
    }
}
