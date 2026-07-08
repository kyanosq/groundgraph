//! P25 — effectiveness golden tests: Redis-class C patterns, end to end.
//!
//! Validation methodology: every analytic claim GroundGraph makes ("this is
//! dead", "this module is named X", "main is an entry point") is exercised
//! against a miniature repo reproducing the exact patterns found while
//! scanning Redis (~200k LOC). Each test asserts BOTH directions:
//!
//! * true positives  — genuinely dead code IS still reported;
//! * true negatives — macro-reached / cross-TU-reached / runtime-invoked
//!   code is NOT reported.
//!
//! If a future resolver change re-breaks any of these, the failure names the
//! real-world pattern (not an abstract unit), which is what makes this an
//! effectiveness suite rather than a unit suite.

use std::fs;
use std::path::Path;

use groundgraph_engine::dead_code::{analyze_dead_code, DeadCodeOptions};
use groundgraph_engine::index::index_repository;
use groundgraph_engine::{IndexOptions, IndexResult};

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn enable_c(root: &Path) {
    fs::create_dir_all(root.join(".groundgraph")).unwrap();
    write(
        root,
        ".groundgraph.yaml",
        "treesitter:\n  enabled: true\n  languages: [c]\n  paths: [src]\n",
    );
}

fn index(root: &Path) -> IndexResult {
    index_repository(IndexOptions::all(root)).expect("index must succeed")
}

/// The Redis compilation model in miniature:
///
/// * `dict.h` declares the API, `dict.c` defines it (cross-TU resolution);
/// * `server.c` includes only the header and calls through it;
/// * `main` is invoked by the C runtime (implicit entry point);
/// * `serverAssert` is a function-like macro mediating `_serverAssert`;
/// * `orphanHelper` is genuinely dead and must stay reported.
#[test]
fn c_cross_tu_call_graph_keeps_real_code_alive_and_dead_code_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    enable_c(root);

    write(
        root,
        "src/dict.h",
        "#ifndef DICT_H\n#define DICT_H\n\
         typedef struct _dictObject { struct _dictObject *next; } dictObject;\n\
         dictObject *dictCreateObject(void);\n\
         int dictAddEntry(dictObject *d);\n\
         #endif\n",
    );
    write(
        root,
        "src/dict.c",
        "#include \"dict.h\"\n\
         dictObject *dictCreateObject(void) { return 0; }\n\
         int dictAddEntry(dictObject *d) { return d != 0; }\n",
    );
    write(
        root,
        "src/debug.c",
        "void _serverAssertImpl(const char *e, const char *f, int l) { (void)e; (void)f; (void)l; }\n\
         #define serverAssert(_e) ((_e) ? (void)0 : _serverAssertImpl(#_e, __FILE__, __LINE__))\n\
         int checkInvariants(int v) { serverAssert(v > 0); return v; }\n",
    );
    write(
        root,
        "src/server.c",
        "#include \"dict.h\"\n\
         int checkInvariants(int v);\n\
         static int initServerConfig(void) { dictObject *d = dictCreateObject(); return dictAddEntry(d); }\n\
         int main(int argc, char **argv) { (void)argc; (void)argv; return initServerConfig() + checkInvariants(1); }\n",
    );
    // Redis's `redisassert.c` shape: an alternative implementation file that
    // nothing in this build includes or calls — genuinely dead here.
    write(
        root,
        "src/orphan.c",
        "void orphanHelperNobodyCalls(void) { }\n",
    );

    let result = index(root);
    assert!(!result.treesitter.is_empty(), "tree-sitter pass must run");

    let report = analyze_dead_code(DeadCodeOptions {
        repo_root: root.to_path_buf(),
        ..DeadCodeOptions::default()
    })
    .expect("dead-code analysis");

    let dead_ids: Vec<&str> = report.candidates.iter().map(|c| c.id.as_str()).collect();

    // -- true negatives: everything reachable from `main` through the
    //    cross-TU / macro-mediated call graph stays alive.
    for alive in [
        "main",
        "initServerConfig",
        "dictCreateObject",
        "dictAddEntry",
        "checkInvariants",
        "_serverAssertImpl",
    ] {
        assert!(
            !dead_ids.iter().any(|id| id.ends_with(alive)),
            "{alive} is reachable (Redis pattern) and must not be dead: {dead_ids:?}"
        );
    }

    // -- true positive: the genuine orphan is still caught.
    assert!(
        dead_ids
            .iter()
            .any(|id| id.ends_with("orphanHelperNobodyCalls")),
        "a genuinely-dead static helper must stay reported: {dead_ids:?}"
    );
}

/// `typedef struct _Tag {…} Name;` (Redis `cluster_legacy.h`): the symbol must
/// take the typedef name users actually reference, so the struct does not
/// strand as a dead `_Tag`.
#[test]
fn c_tagged_typedef_struct_is_referenced_via_typedef_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    enable_c(root);

    write(
        root,
        "src/cluster.h",
        "typedef struct _clusterNode { struct _clusterNode *next; int slot; } clusterNode;\n\
         clusterNode *clusterCreateNode(void);\n",
    );
    write(
        root,
        "src/cluster.c",
        "#include \"cluster.h\"\n\
         clusterNode *clusterCreateNode(void) { return 0; }\n\
         int main(void) { clusterNode *n = clusterCreateNode(); return n == 0; }\n",
    );

    let _ = index(root);
    let store = groundgraph_store::Store::open(root.join(".groundgraph/graph.db")).unwrap();
    let nodes = store.list_all_nodes().unwrap();
    assert!(
        nodes
            .iter()
            .any(|n| n.name.as_deref() == Some("clusterNode")),
        "struct symbol must carry the typedef name"
    );
    assert!(
        !nodes
            .iter()
            .any(|n| n.name.as_deref() == Some("_clusterNode")),
        "the internal tag must not surface as a separate symbol"
    );
}

/// `deps/`-style vendored trees (jemalloc, lua, …) must not become code
/// roots during init detection — Redis's `deps/` is 10× its `src/`.
#[test]
fn init_detection_skips_deps_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "src/server.c", "int main(void) { return 0; }\n");
    write(
        root,
        "deps/jemalloc/src/arena.c",
        "int arena_boot(void) { return 0; }\n",
    );

    groundgraph_engine::init::init_repository(groundgraph_engine::init::InitOptions::new(root))
        .expect("init");
    let config = fs::read_to_string(root.join(".groundgraph.yaml")).unwrap();
    assert!(
        !config.contains("- deps"),
        "deps/ must never be detected as a code root:\n{config}"
    );
    assert!(config.contains("- src"), "src/ must be detected:\n{config}");
}
