//! P23.8 — cross-language totality & determinism under adversarial input.
//!
//! Each language's structural backend must (a) never panic and (b) produce the
//! same counts on repeat runs, even for deeply nested, oversized, or
//! unusual-but-valid UTF-8 sources, and must degrade gracefully (no panic) on
//! files that are not valid UTF-8 at all. This complements the per-spec unit
//! proptests by driving the *real file → read → parse → ingest* path end to
//! end through a fresh SQLite store.

use std::fs;
use std::path::Path;

use proptest::prelude::*;
use specslice_engine::dart_indexer::{index_dart, DartIndexOptions};
use specslice_engine::treesitter::{index_repo_with_spec, spec_for_language, TsIndexOptions};
use specslice_store::Store;
use tempfile::TempDir;

/// Generic tree-sitter languages (Dart is handled via its own indexer).
const GENERIC: &[(&str, &str)] = &[
    ("rust", "rs"),
    ("typescript", "ts"),
    ("python", "py"),
    ("go", "go"),
    ("java", "java"),
    ("swift", "swift"),
    ("c", "c"),
    ("cpp", "cpp"),
];

fn fresh_store(tmp: &Path) -> Store {
    let mut store = Store::open(tmp.join("graph.db")).expect("open");
    store.migrate().expect("migrate");
    store
}

/// Index a single source file for a generic spec; returns count fingerprint.
fn index_generic(lang: &str, ext: &str, src: &str) -> (usize, usize, usize, usize) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join(format!("f.{ext}")), src).unwrap();
    let mut store = fresh_store(root);
    let spec = spec_for_language(lang).expect("spec");
    let r = index_repo_with_spec(
        &mut store,
        spec,
        &TsIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec!["src".into()],
            exclude_globs: vec![],
            resolution_paths: vec![],
        },
    )
    .expect("index must not error");
    (r.files, r.symbols, r.imports, r.tests)
}

/// Index a single Dart source file (structure-only); returns count fingerprint.
fn index_dart_str(src: &str) -> (usize, usize, usize) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(root.join("lib/f.dart"), src).unwrap();
    let mut store = fresh_store(root);
    let r = index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec!["lib".into()],
            exclude_globs: vec![],
            disable_analyzer: true,
        },
    )
    .expect("index must not error");
    (r.files, r.symbols, r.tests)
}

/// Build an adversarial-but-valid-UTF-8 source from a seed.
fn adversarial(seed: &str, shape: u8) -> String {
    match shape % 4 {
        // Deeply nested brackets around the seed.
        1 => format!("{}{}{}", "{[(<".repeat(300), seed, ">)]}".repeat(300)),
        // Oversized: repeat the seed many times.
        2 => seed.repeat(400),
        // Many short lines.
        3 => (0..400).map(|i| format!("a{i} {seed}\n")).collect(),
        _ => seed.to_string(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    /// Every language is total (no panic) and deterministic on adversarial
    /// valid-UTF-8 input.
    #[test]
    fn every_language_is_total_and_deterministic(seed in ".{0,48}", shape in 0u8..4) {
        let src = adversarial(&seed, shape);
        for (lang, ext) in GENERIC {
            let a = index_generic(lang, ext, &src);
            let b = index_generic(lang, ext, &src);
            prop_assert_eq!(a, b, "{} not deterministic", lang);
        }
        let da = index_dart_str(&src);
        let db = index_dart_str(&src);
        prop_assert_eq!(da, db, "dart not deterministic");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Arbitrary bytes — including invalid UTF-8 — must not panic the read +
    /// index path (the file is parsed if valid, otherwise skipped).
    #[test]
    fn indexing_arbitrary_bytes_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/f.rs"), &bytes).unwrap();
        let mut store = fresh_store(root);
        let spec = spec_for_language("rust").unwrap();
        let res = index_repo_with_spec(
            &mut store,
            spec,
            &TsIndexOptions {
                repo_root: root.to_path_buf(),
                code_roots: vec!["src".into()],
                exclude_globs: vec![],
                resolution_paths: vec![],
            },
        );
        prop_assert!(res.is_ok(), "arbitrary bytes must not error the indexer");
    }
}
