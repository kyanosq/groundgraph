//! Search hot-path micro-benchmarks (issues.md #143/#144/#156/#158/#160/#162).
//!
//! Drives the full `run_search` ranking pipeline — `keyword_matches`
//! (`score_node` per candidate: `split_identifier` / `compact_segments` /
//! path-segment split), the BM25 content layer, and `attach_snippets`
//! (per-line `to_lowercase`) — over a deterministic synthetic corpus so the
//! search optimisation sprint has before/after numbers rather than vibes.
//!
//! Run: `cargo bench -p groundgraph-engine`.

use std::path::Path;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use groundgraph_engine::{run_search_with_store, SearchOptions, SearchQuery};
use groundgraph_store::Store;
use tempfile::TempDir;

/// Languages cycled through the synthetic corpus, each mapped to a kind that
/// participates in [`groundgraph_engine::default_search_kinds`] so keyword
/// ranking actually scores the nodes (otherwise the corpus is invisible).
const LANGS: [(&str, NodeKind); 6] = [
    ("dart", NodeKind::DartMethod),
    ("py", NodeKind::PythonFunction),
    ("ts", NodeKind::TypescriptFunction),
    ("java", NodeKind::JavaMethod),
    ("go", NodeKind::GoFunction),
    ("swift", NodeKind::SwiftMethod),
];

/// Name shapes exercising `split_identifier` / `compact_segments` /
/// path-segment scoring across camelCase, snake_case, PascalCase and
/// dot-qualified identifiers.
fn name_for(i: usize) -> String {
    match i % 5 {
        0 => format!("AuthService{i}"),
        1 => format!("handleRequest{i}"),
        2 => format!("sign_in_user_{i}"),
        3 => format!("Item{i}.applyPurchase"),
        _ => format!("verifyAuth{i}"),
    }
}

const FILE_LINES: usize = 50;

/// Build a deterministic synthetic repo: `n` nodes (varied languages / name
/// shapes), a linear `Calls` chain so neighbour expansion has work, and a real
/// on-disk source file per node so the snippet attachment hot path (#156) is
/// exercised rather than short-circuited by a missing file.
fn synthetic_repo(n: usize) -> (Store, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(".groundgraph").join("graph.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let mut store = Store::open(&db).unwrap();
    store.migrate().unwrap();

    let mut aids = Vec::with_capacity(n);
    for i in 0..n {
        let (ext, kind) = LANGS[i % LANGS.len()];
        let name = name_for(i);
        // Dart files keep the `.dart` suffix so the path-segment trim_end
        // branch in `score_node` (#144) is on the hot path.
        let path = if ext == "dart" {
            format!("lib/src/mod{}/page{i}.dart", i / 50)
        } else {
            format!("src/{ext}/mod{}/file{i}.{ext}", i / 50)
        };
        // Materialise a source file so `attach_snippets` reads + lowercases
        // it instead of bailing on a missing file.
        let abs = dir.path().join(&path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut body = String::new();
        for line in 0..FILE_LINES {
            // Mixed-case tokens keep `to_lowercase` non-trivial; the query
            // words appear so needle scanning picks a real best line.
            body.push_str(&format!(
                "L{}: Auth Handle SignIn Request Purchase Verify Token Case {:02}\n",
                line + 1,
                line
            ));
        }
        std::fs::write(&abs, body).unwrap();

        let aid = ArtifactId::new(format!("{ext}::src/mod/file{i}#{name}"));
        let mut node = Node::new(aid.clone(), kind);
        node.name = Some(name);
        node.path = Some(path);
        node.start_line = Some(1);
        node.end_line = Some(u32::try_from(FILE_LINES).expect("FILE_LINES is a small constant"));
        store.upsert_node(&node).unwrap();
        aids.push(aid);
    }
    for pair in aids.windows(2) {
        let e = EdgeAssertion::fact(
            pair[0].clone(),
            pair[1].clone(),
            EdgeKind::Calls,
            EdgeSource::LanguageAdapter,
        );
        store.upsert_edge(&e).unwrap();
    }
    (store, dir)
}

fn opts(repo_root: &Path, query: &str) -> SearchOptions {
    SearchOptions {
        repo_root: repo_root.to_path_buf(),
        query: SearchQuery::Keywords(query.to_string()),
        // depth 0 focuses the measurement on the ranking hot path
        // (keyword_matches + content layer + snippet attachment) without
        // the DB-round-trip-heavy neighbour expansion.
        depth: 0,
        kinds: Vec::new(),
        limit: 100,
        include_noise: false,
    }
}

fn bench_run_search(c: &mut Criterion) {
    let (store, dir) = synthetic_repo(8_000);
    let root = dir.path().to_path_buf();
    // `dir` (TempDir) is held until the function returns so the on-disk
    // store + source files live for the whole benchmark group.
    let mut group = c.benchmark_group("run_search");
    for query in ["auth", "signIn", "handle_request", "request", "purchase"] {
        group.bench_with_input(BenchmarkId::from_parameter(query), query, |b, q| {
            b.iter(|| {
                let _ =
                    run_search_with_store(black_box(&store), black_box(opts(&root, q))).unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_run_search);
criterion_main!(benches);
