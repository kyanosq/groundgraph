//! P20 — Java language adapter.
//!
//! Mirrors the TypeScript / Python adapters: LSP-first via `jdtls`,
//! with a tolerant AST scanner always running alongside so package
//! declarations, imports and JUnit `@Test` methods are captured even
//! when LSP is unavailable.
//!
//! Confidence:
//! - Symbols + references from the LSP pass are tagged
//!   `indexer = java_lsp`.
//! - Symbols + imports + JUnit cases from the AST pass are tagged
//!   `indexer = java_ast`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use specslice_core::artifact_id::{file_id, slugify, ArtifactId};
use specslice_core::language_batch::{
    FileArtifact, ImportEdge, LanguageIndexBatch, SymbolArtifact, TestArtifact,
};
use specslice_core::NodeKind;
use specslice_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;
use crate::java_ast::{scan, JavaScan};
use crate::lsp_client::LspSymbolKind;
use crate::lsp_indexer::{
    binary_on_path, run_profile, LspIndexOptions, LspIndexOutcome, LspProfile,
};

pub const JAVA_INDEXER_NAME: &str = "java_lsp";
pub const JAVA_AST_INDEXER_NAME: &str = "java_ast";
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
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

pub fn index_java(store: &mut Store, options: &JavaIndexOptions) -> Result<JavaIndexResult> {
    let probe = ProbeOutcome::from_options(options);

    let mut lsp_batch: Option<LanguageIndexBatch> = None;
    let mut lsp_files = 0usize;
    let mut lsp_symbols = 0usize;
    let mut skip_reason = String::new();
    let mut resolver_used = String::new();

    if let Some(cmd) = probe.command.clone() {
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
                lsp_files = stats.files;
                lsp_symbols = stats.symbols;
                if !stats.skip_reason.is_empty() {
                    skip_reason = stats.skip_reason;
                }
                ingest_language_batch_minimal(store, &batch, JAVA_INDEXER_NAME)
                    .context("ingesting Java LSP batch")?;
                lsp_batch = Some(batch);
                resolver_used = JAVA_INDEXER_NAME.into();
            }
            LspIndexOutcome::Skipped { reason, .. } => {
                skip_reason = reason;
            }
        }
    } else {
        skip_reason = probe.skip_reason;
    }

    let ast_outcome =
        run_ast_pass(store, options, lsp_batch.as_ref()).context("running Java AST pass")?;

    if resolver_used.is_empty() && ast_outcome.symbols + ast_outcome.tests > 0 {
        resolver_used = JAVA_AST_INDEXER_NAME.into();
    }

    let total_files = if lsp_files > 0 {
        lsp_files
    } else {
        ast_outcome.files
    };
    let total_symbols = lsp_symbols + ast_outcome.symbols;

    Ok(JavaIndexResult {
        files: total_files,
        symbols: total_symbols,
        tests: ast_outcome.tests,
        imports: ast_outcome.imports,
        resolver_used,
        sidecar_skip_reason: skip_reason,
    })
}

pub fn java_lsp_available(options: &JavaIndexOptions) -> bool {
    ProbeOutcome::from_options(options).command.is_some()
}

#[derive(Debug, Default)]
struct AstOutcome {
    files: usize,
    symbols: usize,
    tests: usize,
    imports: usize,
}

fn run_ast_pass(
    store: &mut Store,
    options: &JavaIndexOptions,
    lsp_batch: Option<&LanguageIndexBatch>,
) -> Result<AstOutcome> {
    let files = discover_java_files(
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
    )?;
    if files.is_empty() {
        return Ok(AstOutcome::default());
    }

    let mut outcome = AstOutcome::default();
    let mut batch = LanguageIndexBatch {
        language: JAVA_LANGUAGE_ID.into(),
        ..Default::default()
    };

    let lsp_symbol_ids: std::collections::BTreeSet<String> = lsp_batch
        .map(|b| b.symbols.iter().map(|s| s.id.to_string()).collect())
        .unwrap_or_default();

    // Group files by package so we can emit one JavaPackage per
    // distinct package name (and tie file-level symbols to it).
    let mut seen_packages: std::collections::BTreeMap<String, ArtifactId> =
        std::collections::BTreeMap::new();

    for file in &files {
        let source = std::fs::read_to_string(&file.absolute)
            .with_context(|| format!("reading {}", file.absolute.display()))?;
        let scan_result: JavaScan = scan(&source);
        outcome.files += 1;

        let file_artifact_id = file_id(&file.relative);
        let total_lines = u32::try_from(source.lines().count().max(1)).unwrap_or(u32::MAX);
        batch.files.push(FileArtifact {
            id: file_artifact_id.clone(),
            path: file.relative.clone(),
            language: JAVA_LANGUAGE_ID.into(),
            content_hash: sha256_hex(source.as_bytes()),
        });

        // JavaPackage node — one per distinct package across the
        // index. We use `java_package::<dotted>` so the id is stable
        // across files.
        let pkg = scan_result
            .package_name
            .clone()
            .unwrap_or_else(|| "<default>".to_string());
        let pkg_id = seen_packages
            .entry(pkg.clone())
            .or_insert_with(|| ArtifactId::new(format!("java_package::{pkg}")))
            .clone();
        if !batch
            .symbols
            .iter()
            .any(|s| s.id.to_string() == pkg_id.to_string())
        {
            batch.symbols.push(SymbolArtifact {
                id: pkg_id.clone(),
                kind: NodeKind::JavaPackage,
                path: file.relative.clone(),
                name: pkg.clone(),
                qualified_name: pkg.clone(),
                start_line: 1,
                end_line: total_lines,
                parent_symbol_id: None,
                metadata_json: None,
            });
        }

        for sym in &scan_result.symbols {
            if matches!(sym.kind, NodeKind::TestCase | NodeKind::TestGroup) {
                batch.tests.push(TestArtifact {
                    id: ArtifactId::new(format!(
                        "test::{file}::{slug}",
                        file = file.relative,
                        slug = slugify(&sym.name)
                    )),
                    kind: sym.kind,
                    path: file.relative.clone(),
                    name: sym.name.clone(),
                    start_line: sym.start_line,
                    end_line: sym.end_line,
                    parent_symbol_id: None,
                });
                outcome.tests += 1;
                continue;
            }
            let qualified_with_pkg = format!("{pkg}.{}", sym.qualified_name);
            let id = symbol_id(&qualified_with_pkg);
            if lsp_symbol_ids.contains(&id.to_string()) {
                continue;
            }
            let parent_id = match sym.parent_qualified_name.as_ref() {
                Some(p) => Some(symbol_id(&format!("{pkg}.{p}"))),
                None => Some(pkg_id.clone()),
            };
            batch.symbols.push(SymbolArtifact {
                id,
                kind: sym.kind,
                path: file.relative.clone(),
                name: sym.name.clone(),
                qualified_name: qualified_with_pkg,
                start_line: sym.start_line,
                end_line: sym.end_line,
                parent_symbol_id: parent_id,
                metadata_json: None,
            });
            outcome.symbols += 1;
        }

        for imp in &scan_result.imports {
            batch.imports.push(ImportEdge {
                from_file: file_artifact_id.clone(),
                to_path: imp.module_specifier.clone(),
            });
            outcome.imports += 1;
        }
    }

    if outcome.files > 0 {
        ingest_language_batch_minimal(store, &batch, JAVA_AST_INDEXER_NAME)
            .context("ingesting Java AST batch")?;
    }
    Ok(outcome)
}

fn symbol_id(qualified: &str) -> ArtifactId {
    ArtifactId::new(format!("java::{qualified}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for b in out {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

#[derive(Debug, Clone)]
struct DiscoveredJavaFile {
    relative: String,
    absolute: PathBuf,
}

fn discover_java_files(
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
) -> Result<Vec<DiscoveredJavaFile>> {
    let mut out: Vec<DiscoveredJavaFile> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let roots: Vec<PathBuf> = if code_roots.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        code_roots.to_vec()
    };
    for root in &roots {
        let abs = repo_root.join(root);
        if !abs.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&abs)
            .into_iter()
            .filter_entry(|e| !is_java_skip_dir(e))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if ext != "java" {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if exclude_globs
                .iter()
                .any(|g| crate::lsp_indexer::simple_glob_match(g, &rel))
            {
                continue;
            }
            if !seen.insert(rel.clone()) {
                continue;
            }
            out.push(DiscoveredJavaFile {
                relative: rel,
                absolute: repo_root.join(path.strip_prefix(repo_root).unwrap_or(path)),
            });
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

fn is_java_skip_dir(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    matches!(
        name,
        ".git" | "target" | "build" | "out" | ".idea" | ".gradle" | "bin"
    )
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

fn java_qualify(_file_rel: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}.{name}"),
        None => name.to_string(),
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
                    "{JAVA_LSP_COMMAND_ENV}=`{env_cmd}` smoke launch 未通过，已退化为 AST fallback"
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
                    "`java.lsp_command = {cmd}` smoke launch 未通过，已退化为 AST fallback"
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
            skip_reason: "未在 PATH 找到可启动的 jdtls，已退化为 AST fallback".into(),
        }
    }
}

/// Java probe gate: a binary is "available" only when it both
/// resolves on PATH and survives the shared `lsp_probe` smoke launch.
/// `jdtls` is a Python launcher that bootstraps a JVM; smoke catches
/// the common failure of `java` not being on PATH at all.
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
    fn ast_pass_runs_against_java_hello_fixture_without_lsp() {
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
        assert!(result.files >= 2, "should have indexed both Java files");
        assert!(
            result.symbols >= 2,
            "should have at least 1 package + 1 class (got {})",
            result.symbols
        );
        assert!(
            result.tests >= 1,
            "JUnit @Test should be recovered (got {})",
            result.tests
        );
        assert!(
            result.resolver_used == JAVA_AST_INDEXER_NAME || result.resolver_used.is_empty(),
            "expected AST fallback resolver, got {:?}",
            result.resolver_used
        );
    }

    #[test]
    fn package_qualification_is_applied_to_classes() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let (mut store, _db) = open_temp_store(tmp.path());

        let opts = JavaIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
            lsp_command: Some("/nonexistent/jdtls".into()),
        };
        index_java(&mut store, &opts).unwrap();
        let nodes = store.list_all_nodes().unwrap();
        // Greeter qualified by package must appear.
        assert!(
            nodes.iter().any(|n| n.kind == NodeKind::JavaClass
                && n.id.to_string() == "java::com.example.Greeter"),
            "expected JavaClass `com.example.Greeter` in store; got {:?}",
            nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::JavaClass | NodeKind::JavaPackage))
                .map(|n| (n.kind, n.id.to_string()))
                .collect::<Vec<_>>()
        );
        assert!(nodes
            .iter()
            .any(|n| n.kind == NodeKind::JavaPackage
                && n.id.to_string() == "java_package::com.example"));
    }
}
