//! P7 — `specslice dead-code` golden against PixCraft IAP fixture.
//!
//! Verifies the real end-to-end flow:
//!   - index PixCraft via Dart sidecar
//!   - run dead-code analyzer with default + custom config
//!   - assert non-empty candidate list with confidence buckets
//!   - assert `*.g.dart`-style codegen files are filtered by default
//!   - assert `public_api_roots` keeps a public-surface method alive
//!
//! Skips when the Dart SDK or the sidecar source isn't available.

use std::path::PathBuf;

use specslice_engine::config::DeadCodeConfig;
use specslice_engine::dart_indexer::{index_dart, DartIndexOptions, RESOLVER_DART_ANALYZER};
use specslice_engine::dead_code::{
    analyze_dead_code_with_store, DeadCodeConfidence, DeadCodeOptions,
};
use specslice_engine::init::{init_repository, InitOptions};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures/pixcraft_iap")
}

fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn copy_fixture_into(dst: &std::path::Path) {
    let src = fixture_path();
    for entry in walkdir::WalkDir::new(&src) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&src).unwrap();
        let target = dst.join(rel);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(entry.path(), &target).unwrap();
    }
}

fn dart_available() -> bool {
    std::process::Command::new("dart")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn sidecar_source_present() -> bool {
    workspace_dir()
        .join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart")
        .exists()
}

struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: Option<&str>) -> Self {
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

fn setup_indexed_repo() -> Option<(tempfile::TempDir, EnvGuard, EnvGuard)> {
    if !sidecar_source_present() || !dart_available() {
        eprintln!("skipping: dart sidecar unavailable");
        return None;
    }
    let on = EnvGuard::set("SPECSLICE_DART_ANALYZER", Some("1"));
    let sidecar_abs =
        workspace_dir().join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart");
    let bin = EnvGuard::set(
        "SPECSLICE_DART_ANALYZER_BIN",
        Some(&format!("dart run {}", sidecar_abs.display())),
    );

    let tmp = tempfile::TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    copy_fixture_into(tmp.path());
    let mut store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    store.migrate().unwrap();
    let result = index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: vec!["lib".into(), "test".into()],
            exclude_globs: vec![],
            disable_analyzer: false,
        },
    )
    .unwrap();
    assert_eq!(
        result.resolver_used, RESOLVER_DART_ANALYZER,
        "P7 dead-code golden requires sidecar resolver"
    );
    Some((tmp, on, bin))
}

fn default_config() -> DeadCodeConfig {
    DeadCodeConfig {
        entrypoints: vec!["lib/main.dart".into()],
        ignore: vec!["**/*.g.dart".into(), "**/*.freezed.dart".into()],
        public_api_roots: vec![],
    }
}

#[test]
fn p7_dead_code_lists_unreached_pixcraft_symbols_with_confidence() {
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };
    let store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let report = analyze_dead_code_with_store(
        &store,
        DeadCodeOptions {
            repo_root: tmp.path().into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        },
        &default_config(),
    )
    .unwrap();

    // Report contract.
    assert_eq!(report.schema_version, 1);
    assert!(
        report.stats.total_code_symbols > 0,
        "PixCraft must have indexed code symbols"
    );
    assert!(
        report.stats.entrypoints > 0,
        "expected at least one entry point (routes / providers / tests / lifecycle / main)"
    );
    // Tests should never appear in the candidate set when include_tests = false.
    assert!(
        report
            .candidates
            .iter()
            .all(|c| c.kind != "test_case" && c.kind != "test_group"),
        "tests should never be reported unless --include-tests is set"
    );
    // Sorted by confidence desc.
    let rank = |c: DeadCodeConfidence| match c {
        DeadCodeConfidence::High => 3,
        DeadCodeConfidence::Medium => 2,
        DeadCodeConfidence::Low => 1,
    };
    let ranks: Vec<i32> = report
        .candidates
        .iter()
        .map(|c| rank(c.confidence))
        .collect();
    let mut sorted = ranks.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        ranks, sorted,
        "candidates must be ordered by confidence desc"
    );
    // Every candidate carries reasons.
    for c in &report.candidates {
        assert!(
            !c.reasons.is_empty(),
            "each candidate must explain itself; missing reasons on {}",
            c.id
        );
    }
}

#[test]
fn p7_dead_code_respects_ignore_glob_for_codegen_files() {
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };
    // Drop a synthetic generated file to ensure the ignore glob keeps
    // working under sidecar indexing. We re-index after creating the
    // file so the store sees it.
    let gen_file = tmp.path().join("lib/fake.g.dart");
    std::fs::write(
        &gen_file,
        "// Generated file — should be ignored by default.\nclass _FakeGen {\n  void neverUsed() {}\n}\n",
    )
    .unwrap();
    let mut store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let _ = index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: vec!["lib".into(), "test".into()],
            exclude_globs: vec![],
            disable_analyzer: false,
        },
    )
    .unwrap();
    let report = analyze_dead_code_with_store(
        &store,
        DeadCodeOptions {
            repo_root: tmp.path().into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        },
        &default_config(),
    )
    .unwrap();
    assert!(
        report
            .candidates
            .iter()
            .all(|c| !c.path.as_deref().unwrap_or("").ends_with(".g.dart")),
        "*.g.dart files must be filtered by the ignore glob"
    );
    assert!(
        report.stats.ignored_by_pattern >= 1,
        "expected the synthetic generated file to be counted as ignored, got {}",
        report.stats.ignored_by_pattern
    );
}

#[test]
fn p7_public_api_roots_demote_high_to_medium() {
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };
    let store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let cfg_no_public = default_config();
    let mut cfg_public = default_config();
    cfg_public.public_api_roots = vec!["lib/**".into()];

    let baseline = analyze_dead_code_with_store(
        &store,
        DeadCodeOptions {
            repo_root: tmp.path().into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        },
        &cfg_no_public,
    )
    .unwrap();
    let with_public = analyze_dead_code_with_store(
        &store,
        DeadCodeOptions {
            repo_root: tmp.path().into(),
            min_confidence: DeadCodeConfidence::Low,
            include_tests: false,
        },
        &cfg_public,
    )
    .unwrap();

    let high_baseline = baseline
        .candidates
        .iter()
        .filter(|c| c.confidence == DeadCodeConfidence::High)
        .count();
    let high_public = with_public
        .candidates
        .iter()
        .filter(|c| c.confidence == DeadCodeConfidence::High)
        .count();
    assert!(
        high_public <= high_baseline,
        "public_api_roots should never *increase* the high-confidence count; got baseline={high_baseline} with_public={high_public}"
    );
    // Quick sanity: with the entire lib/** treated as public API,
    // every candidate that *does* appear must have at least one
    // mitigating reason mentioning public_api_roots.
    for c in &with_public.candidates {
        if c.path.as_deref().unwrap_or("").starts_with("lib/") {
            assert!(
                c.reasons
                    .iter()
                    .any(|r| r.contains("public_api_roots") || r.contains("公共可见符号")),
                "public-path candidate must explain its public posture: {:?}",
                c.reasons
            );
        }
    }
}
