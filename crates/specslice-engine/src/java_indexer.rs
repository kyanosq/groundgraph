//! P20/P23.3 — Java language adapter.
//!
//! Since the P23 收敛, the in-process tree-sitter driver
//! ([`crate::java_treesitter`]) is the **sole structural source of truth**
//! for Java: classes / interfaces / enums / records, methods + constructors,
//! JUnit `@Test` cases, and `import x.y.Z;` resolved to repo-relative file
//! ids. Output is tagged `indexer = java_treesitter`.
//!
//! `jdtls` is an **optional Tier-3 enrichment**: when discovered it
//! contributes only the semantic `Calls` / `References` edges, overlaid onto
//! the existing tree-sitter symbol ids (the two id schemes are identical by
//! construction). LSP edges are tagged `indexer = java_lsp`. When no LSP is
//! present the structural graph is already complete; there is no longer any
//! second structural pass.

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_core::language_batch::LanguageIndexBatch;
use specslice_core::NodeKind;
use specslice_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;
use crate::lsp_client::LspSymbolKind;
use crate::lsp_indexer::{
    binary_on_path, run_profile, LspIndexOptions, LspIndexOutcome, LspProfile,
};
use crate::treesitter::{self, TsIndexOptions};

pub const JAVA_INDEXER_NAME: &str = "java_lsp";
pub const JAVA_LANGUAGE_ID: &str = "java";
pub const JAVA_LSP_COMMAND_ENV: &str = "SPECSLICE_JAVA_LSP_BIN";

#[derive(Debug, Clone, Default)]
pub struct JavaIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    pub lsp_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct JavaIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub imports: usize,
    /// Number of `Calls` / `References` edges contributed by the optional
    /// Tier-3 LSP enrichment pass (0 when no LSP was available).
    #[serde(default)]
    pub references: usize,
    /// `java_treesitter` when the structural pass produced anything, empty
    /// when no Java files were found.
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

/// Top-level entrypoint. The tree-sitter driver produces the entire
/// structural graph (symbols + JUnit tests + resolved imports); an optional
/// LSP pass then overlays `Calls` / `References`.
pub fn index_java(store: &mut Store, options: &JavaIndexOptions) -> Result<JavaIndexResult> {
    let spec = &crate::java_treesitter::JAVA_SPEC;
    let ts_name = treesitter::indexer_name(spec);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous Java tree-sitter outputs")?;
    store
        .clear_indexer_outputs(JAVA_INDEXER_NAME)
        .context("clearing previous Java LSP outputs")?;

    let ts = treesitter::index_repo_with_spec(
        store,
        spec,
        &TsIndexOptions {
            repo_root: options.repo_root.clone(),
            code_roots: options.code_roots.clone(),
            exclude_globs: options.exclude_globs.clone(),
            resolution_paths: Vec::new(),
        },
    )
    .context("indexing Java structure via tree-sitter")?;

    // Id set of structural nodes so the optional LSP pass attaches edges
    // without dangling targets.
    let mut known_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in store.list_all_nodes().context("listing nodes")? {
        if node.indexer.as_deref() == Some(ts_name.as_str()) {
            known_ids.insert(node.id.to_string());
        }
    }

    // Tier 3 (optional): LSP `Calls` / `References` enrichment overlaid onto
    // the tree-sitter symbol ids (identical id scheme).
    let probe = ProbeOutcome::from_options(options);
    let mut references = 0usize;
    let skip_reason = match probe.command.clone() {
        Some(cmd) => {
            let profile = java_profile();
            let lsp_options = LspIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: options.code_roots.clone(),
                exclude_globs: options.exclude_globs.clone(),
                lsp_command: Some(cmd),
            };
            match run_profile(&profile, &lsp_options)? {
                LspIndexOutcome::Indexed(boxed) => {
                    let crate::lsp_indexer::LspIndexedBatch { batch, stats } = *boxed;
                    let refs: Vec<_> = batch
                        .references
                        .into_iter()
                        .filter(|r| {
                            known_ids.contains(r.from_symbol_id.as_str())
                                && known_ids.contains(r.to_symbol_id.as_str())
                        })
                        .collect();
                    references = refs.len();
                    if !refs.is_empty() {
                        let refs_batch = LanguageIndexBatch {
                            language: JAVA_LANGUAGE_ID.into(),
                            references: refs,
                            ..Default::default()
                        };
                        ingest_language_batch_minimal(store, &refs_batch, JAVA_INDEXER_NAME)
                            .context("ingesting Java LSP reference edges")?;
                    }
                    stats.skip_reason
                }
                LspIndexOutcome::Skipped { reason, .. } => reason,
            }
        }
        None => probe.skip_reason,
    };

    Ok(JavaIndexResult {
        files: ts.files,
        symbols: ts.symbols,
        tests: ts.tests,
        imports: ts.imports,
        references,
        resolver_used: ts.resolver_used,
        sidecar_skip_reason: skip_reason,
    })
}

/// True when an optional Java LSP enrichment server is discoverable.
/// Structural indexing no longer depends on it — this only gates the Tier-3
/// `Calls` / `References` overlay.
pub fn java_lsp_available(options: &JavaIndexOptions) -> bool {
    ProbeOutcome::from_options(options).command.is_some()
}

fn java_profile() -> LspProfile {
    LspProfile {
        language: JAVA_LANGUAGE_ID,
        language_id: JAVA_LANGUAGE_ID,
        file_extensions: &["java"],
        skip_dirs: &[".git", "target", "build", "out", ".idea", ".gradle", "bin"],
        skip_suffixes: &[],
        default_command: "jdtls",
        default_args: &[],
        command_env_var: JAVA_LSP_COMMAND_ENV,
        map_kind: java_map_kind,
        qualify: java_qualify,
    }
}

fn java_map_kind(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
    match kind {
        LspSymbolKind::Package | LspSymbolKind::Namespace => Some(NodeKind::JavaPackage),
        LspSymbolKind::Class => Some(NodeKind::JavaClass),
        LspSymbolKind::Interface => Some(NodeKind::JavaInterface),
        LspSymbolKind::Enum => Some(NodeKind::JavaEnum),
        LspSymbolKind::Constructor => Some(NodeKind::JavaConstructor),
        LspSymbolKind::Method | LspSymbolKind::Function => Some(NodeKind::JavaMethod),
        _ => None,
    }
}

/// Mirror the tree-sitter id scheme so LSP overlay edges land on the same
/// nodes: nested members join with `.`, top-level types are file-scoped
/// (`<file>::<name>`), exactly like [`treesitter::index_repo_with_spec`].
fn java_qualify(file_rel: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}.{name}"),
        None => format!("{file_rel}::{name}"),
    }
}

#[derive(Debug, Default)]
struct ProbeOutcome {
    command: Option<String>,
    skip_reason: String,
}

impl ProbeOutcome {
    fn from_options(options: &JavaIndexOptions) -> Self {
        if let Ok(env_cmd) = std::env::var(JAVA_LSP_COMMAND_ENV) {
            if java_binary_runnable(&env_cmd) {
                return Self {
                    command: Some(env_cmd),
                    skip_reason: String::new(),
                };
            }
            return Self {
                command: None,
                skip_reason: format!(
                    "{JAVA_LSP_COMMAND_ENV}=`{env_cmd}` smoke launch 未通过，跳过 Calls/References 富化"
                ),
            };
        }
        if let Some(cmd) = options.lsp_command.as_deref() {
            if java_binary_runnable(cmd) {
                return Self {
                    command: Some(cmd.to_string()),
                    skip_reason: String::new(),
                };
            }
            return Self {
                command: None,
                skip_reason: format!(
                    "`java.lsp_command = {cmd}` smoke launch 未通过，跳过 Calls/References 富化"
                ),
            };
        }
        if java_binary_runnable("jdtls") {
            return Self {
                command: Some("jdtls".into()),
                skip_reason: String::new(),
            };
        }
        Self {
            command: None,
            skip_reason: "未在 PATH 找到可启动的 jdtls，跳过 Calls/References 富化".into(),
        }
    }
}

/// Java probe gate: a binary is "available" only when it both resolves on
/// PATH and survives the shared `lsp_probe` smoke launch. `jdtls` is a
/// Python launcher that bootstraps a JVM; smoke catches the common failure
/// of `java` not being on PATH at all.
fn java_binary_runnable(cmd: &str) -> bool {
    if !binary_on_path(cmd) {
        return false;
    }
    crate::lsp_probe::probe_lsp_command(
        cmd,
        crate::lsp_probe::DEFAULT_SMOKE_ARGS,
        crate::lsp_probe::DEFAULT_TIMEOUT,
    )
    .is_runnable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_fixture(root: &Path) {
        for (rel, body) in [
            (
                "src/main/java/com/example/Greeter.java",
                "package com.example;\n\
                 public class Greeter {\n  \
                   public String greet(String name) { return \"hi \" + name; }\n\
                 }\n",
            ),
            (
                "src/test/java/com/example/GreeterTest.java",
                "package com.example;\n\
                 import org.junit.jupiter.api.Test;\n\
                 import com.example.Greeter;\n\
                 class GreeterTest {\n  \
                   @Test\n  \
                   void greetsByName() {}\n\
                 }\n",
            ),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
    }

    fn open_temp_store(root: &Path) -> (Store, PathBuf) {
        let db = root.join("graph.db");
        let mut store = Store::open(&db).unwrap();
        store.migrate().unwrap();
        (store, db)
    }

    #[test]
    fn treesitter_pass_runs_against_java_hello_fixture_without_lsp() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let (mut store, _db) = open_temp_store(tmp.path());

        let opts = JavaIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
            lsp_command: Some("/nonexistent/jdtls".into()),
        };
        let result = index_java(&mut store, &opts).unwrap();
        assert_eq!(
            result.resolver_used,
            treesitter::indexer_name(&crate::java_treesitter::JAVA_SPEC),
            "structure now comes from the tree-sitter driver: {result:?}"
        );
        assert!(result.files >= 2, "both Java files indexed: {result:?}");
        assert!(result.symbols >= 2, "class + method counted: {result:?}");
        assert!(result.tests >= 1, "JUnit @Test recovered: {result:?}");
        assert!(
            !result.sidecar_skip_reason.is_empty(),
            "skip reason recorded when no LSP available"
        );

        let nodes = store.list_all_nodes().unwrap();
        assert!(
            nodes
                .iter()
                .any(|n| n.kind == NodeKind::JavaClass && n.name.as_deref() == Some("Greeter")),
            "Greeter class present; got {:?}",
            nodes
                .iter()
                .map(|n| (n.kind, n.name.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            nodes
                .iter()
                .any(|n| n.kind == NodeKind::TestCase && n.name.as_deref() == Some("greetsByName")),
            "JUnit test case present"
        );

        // The intra-repo import resolves file → file.
        let edges = store.list_all_edges().unwrap();
        assert!(
            edges.iter().any(|e| {
                e.kind == specslice_core::EdgeKind::Imports
                    && e.from_id.as_str() == "file::src/test/java/com/example/GreeterTest.java"
                    && e.to_id.as_str() == "file::src/main/java/com/example/Greeter.java"
            }),
            "GreeterTest should import Greeter across the source tree"
        );
    }

    #[test]
    fn reindexing_is_idempotent() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let (mut store, _db) = open_temp_store(tmp.path());
        let opts = JavaIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
            lsp_command: Some("/nonexistent/jdtls".into()),
        };
        let first = index_java(&mut store, &opts).expect("first index ok");
        let nodes_1 = store.list_all_nodes().unwrap().len();
        let edges_1 = store.list_all_edges().unwrap().len();
        let second = index_java(&mut store, &opts).expect("second index ok");
        let nodes_2 = store.list_all_nodes().unwrap().len();
        let edges_2 = store.list_all_edges().unwrap().len();
        assert_eq!(first, second, "result counts stable across re-index");
        assert_eq!(nodes_1, nodes_2, "node count stable across re-index");
        assert_eq!(edges_1, edges_2, "edge count stable across re-index");
    }

    #[test]
    fn java_qualify_is_file_scoped_at_top_level() {
        assert_eq!(
            java_qualify("src/Greeter.java", None, "Greeter"),
            "src/Greeter.java::Greeter"
        );
        assert_eq!(
            java_qualify(
                "src/Greeter.java",
                Some("src/Greeter.java::Greeter"),
                "greet"
            ),
            "src/Greeter.java::Greeter.greet"
        );
    }
}
