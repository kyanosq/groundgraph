//! P20 — TypeScript language adapter.
//!
//! Mirrors the Python adapter: LSP-first via
//! `typescript-language-server --stdio`, with a tolerant AST scanner
//! always running alongside so imports and jest / vitest test cases are
//! captured even when LSP is unavailable. When LSP is missing the AST
//! scanner also takes over the structural pass (top-level functions /
//! classes / interfaces / enums).
//!
//! Confidence:
//! - Symbols + Calls / References from the LSP pass are tagged
//!   `indexer = typescript_lsp` so callers can reason about provenance.
//! - Symbols + imports + jest cases from the AST pass are tagged
//!   `indexer = typescript_ast`. Both indexers can coexist on the same
//!   symbol id; `upsert_node` dedupes by id.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use specslice_core::artifact_id::{file_id, slugify, ArtifactId};
use specslice_core::language_batch::{
    FileArtifact, ImportEdge, LanguageIndexBatch, SymbolArtifact, TestArtifact,
};
use specslice_core::NodeKind;
use specslice_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;
use crate::lsp_client::LspSymbolKind;
use crate::lsp_indexer::{
    binary_on_path, run_profile, LspIndexOptions, LspIndexOutcome, LspProfile,
};
use crate::typescript_ast::{scan, TypescriptScan};

pub const TYPESCRIPT_INDEXER_NAME: &str = "typescript_lsp";
pub const TYPESCRIPT_AST_INDEXER_NAME: &str = "typescript_ast";
pub const TYPESCRIPT_LANGUAGE_ID: &str = "typescript";
pub const TYPESCRIPT_LSP_COMMAND_ENV: &str = "SPECSLICE_TYPESCRIPT_LSP_BIN";

#[derive(Debug, Clone, Default)]
pub struct TypescriptIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    pub lsp_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TypescriptIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub imports: usize,
    /// `typescript_lsp` when an LSP server ran the structural pass,
    /// `typescript_ast` when only the AST scanner contributed, empty
    /// when both passes skipped.
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

pub fn index_typescript(
    store: &mut Store,
    options: &TypescriptIndexOptions,
) -> Result<TypescriptIndexResult> {
    let probe = ProbeOutcome::from_options(options);

    let mut lsp_batch: Option<LanguageIndexBatch> = None;
    let mut lsp_files = 0usize;
    let mut lsp_symbols = 0usize;
    let mut skip_reason = String::new();
    let mut resolver_used = String::new();

    if let Some(cmd) = probe.command.clone() {
        let profile = typescript_profile();
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
                ingest_language_batch_minimal(store, &batch, TYPESCRIPT_INDEXER_NAME)
                    .context("ingesting TypeScript LSP batch")?;
                lsp_batch = Some(batch);
                resolver_used = TYPESCRIPT_INDEXER_NAME.into();
            }
            LspIndexOutcome::Skipped { reason, .. } => {
                skip_reason = reason;
            }
        }
    } else {
        skip_reason = probe.skip_reason;
    }

    let ast_outcome =
        run_ast_pass(store, options, lsp_batch.as_ref()).context("running TypeScript AST pass")?;

    if resolver_used.is_empty() && ast_outcome.symbols + ast_outcome.tests > 0 {
        resolver_used = TYPESCRIPT_AST_INDEXER_NAME.into();
    }

    let total_files = if lsp_files > 0 {
        lsp_files
    } else {
        ast_outcome.files
    };
    let total_symbols = lsp_symbols + ast_outcome.symbols;

    Ok(TypescriptIndexResult {
        files: total_files,
        symbols: total_symbols,
        tests: ast_outcome.tests,
        imports: ast_outcome.imports,
        resolver_used,
        sidecar_skip_reason: skip_reason,
    })
}

pub fn typescript_lsp_available(options: &TypescriptIndexOptions) -> bool {
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
    options: &TypescriptIndexOptions,
    lsp_batch: Option<&LanguageIndexBatch>,
) -> Result<AstOutcome> {
    let files = discover_typescript_files(
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
    )?;
    if files.is_empty() {
        return Ok(AstOutcome::default());
    }

    let mut outcome = AstOutcome::default();
    let mut batch = LanguageIndexBatch {
        language: TYPESCRIPT_LANGUAGE_ID.into(),
        ..Default::default()
    };

    let lsp_symbol_ids: std::collections::BTreeSet<String> = lsp_batch
        .map(|b| b.symbols.iter().map(|s| s.id.to_string()).collect())
        .unwrap_or_default();

    for file in &files {
        let source = std::fs::read_to_string(&file.absolute)
            .with_context(|| format!("reading {}", file.absolute.display()))?;
        let scan_result: TypescriptScan = scan(&source);
        outcome.files += 1;

        let file_artifact_id = file_id(&file.relative);
        let total_lines = u32::try_from(source.lines().count().max(1)).unwrap_or(u32::MAX);
        batch.files.push(FileArtifact {
            id: file_artifact_id.clone(),
            path: file.relative.clone(),
            language: TYPESCRIPT_LANGUAGE_ID.into(),
            content_hash: sha256_hex(source.as_bytes()),
        });

        // Module-level node — anchors imports for resolution. We
        // create one TypescriptModule per source file, named after
        // the file's stem to give graph viewers a friendlier label.
        let module_name = module_name_for(&file.relative);
        let module_id = ArtifactId::new(format!("ts_module::{}", file.relative));
        batch.symbols.push(SymbolArtifact {
            id: module_id.clone(),
            kind: NodeKind::TypescriptModule,
            path: file.relative.clone(),
            name: module_name.clone(),
            qualified_name: format!("module::{module_name}"),
            start_line: 1,
            end_line: total_lines,
            parent_symbol_id: None,
            metadata_json: None,
        });

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
            let id = symbol_id(&file.relative, &sym.qualified_name);
            if lsp_symbol_ids.contains(&id.to_string()) {
                continue;
            }
            let parent_id = sym
                .parent_qualified_name
                .as_ref()
                .map(|p| symbol_id(&file.relative, p));
            batch.symbols.push(SymbolArtifact {
                id,
                kind: sym.kind,
                path: file.relative.clone(),
                name: sym.name.clone(),
                qualified_name: sym.qualified_name.clone(),
                start_line: sym.start_line,
                end_line: sym.end_line,
                parent_symbol_id: parent_id,
                metadata_json: None,
            });
            outcome.symbols += 1;
        }

        for imp in &scan_result.imports {
            let to_path = resolve_typescript_import(&files, &file.relative, &imp.module_specifier)
                .unwrap_or_else(|| imp.module_specifier.clone());
            batch.imports.push(ImportEdge {
                from_file: file_artifact_id.clone(),
                to_path,
            });
            outcome.imports += 1;
        }
    }

    if outcome.files > 0 {
        ingest_language_batch_minimal(store, &batch, TYPESCRIPT_AST_INDEXER_NAME)
            .context("ingesting TypeScript AST batch")?;
    }
    Ok(outcome)
}

fn module_name_for(rel: &str) -> String {
    let stem = std::path::Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(rel);
    stem.to_string()
}

fn symbol_id(file_rel: &str, qualified: &str) -> ArtifactId {
    ArtifactId::new(format!("ts::{file_rel}::{qualified}"))
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
struct DiscoveredTsFile {
    relative: String,
    absolute: PathBuf,
}

fn discover_typescript_files(
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
) -> Result<Vec<DiscoveredTsFile>> {
    let mut out: Vec<DiscoveredTsFile> = Vec::new();
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
            .filter_entry(|e| !is_typescript_skip_dir(e))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if !matches!(ext, "ts" | "tsx" | "mts" | "cts") {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel.ends_with(".d.ts") {
                continue;
            }
            if exclude_globs
                .iter()
                .any(|g| crate::lsp_indexer::simple_glob_match(g, &rel))
            {
                continue;
            }
            if !seen.insert(rel.clone()) {
                continue;
            }
            out.push(DiscoveredTsFile {
                relative: rel,
                absolute: repo_root.join(path.strip_prefix(repo_root).unwrap_or(path)),
            });
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

fn is_typescript_skip_dir(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    matches!(
        name,
        "node_modules"
            | ".next"
            | ".nuxt"
            | "dist"
            | "build"
            | ".turbo"
            | ".cache"
            | "coverage"
            | ".git"
    )
}

fn resolve_typescript_import(
    files: &[DiscoveredTsFile],
    source_rel: &str,
    specifier: &str,
) -> Option<String> {
    if !specifier.starts_with('.') && !specifier.starts_with('/') {
        // Bare npm specifier — leave to the caller to mark as external.
        return None;
    }
    let source_dir = std::path::Path::new(source_rel)
        .parent()
        .unwrap_or(std::path::Path::new(""));
    let joined = source_dir.join(specifier);
    let mut canonical = String::new();
    for comp in joined.components() {
        use std::path::Component;
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(idx) = canonical.rfind('/') {
                    canonical.truncate(idx);
                } else {
                    canonical.clear();
                }
            }
            Component::Normal(part) => {
                if !canonical.is_empty() {
                    canonical.push('/');
                }
                canonical.push_str(&part.to_string_lossy());
            }
            _ => {}
        }
    }
    // The specifier may omit extension. Try the common ones, plus
    // `<spec>/index.ts(x)` for folder imports.
    let candidates = [
        canonical.clone(),
        format!("{canonical}.ts"),
        format!("{canonical}.tsx"),
        format!("{canonical}/index.ts"),
        format!("{canonical}/index.tsx"),
    ];
    for c in &candidates {
        if files.iter().any(|f| f.relative == *c) {
            return Some(c.clone());
        }
    }
    None
}

fn typescript_profile() -> LspProfile {
    LspProfile {
        language: TYPESCRIPT_LANGUAGE_ID,
        language_id: TYPESCRIPT_LANGUAGE_ID,
        file_extensions: &["ts", "tsx", "mts", "cts"],
        skip_dirs: &[
            "node_modules",
            ".next",
            ".nuxt",
            "dist",
            "build",
            ".turbo",
            ".cache",
            "coverage",
            ".git",
        ],
        skip_suffixes: &[".d.ts"],
        default_command: "typescript-language-server",
        default_args: &["--stdio"],
        command_env_var: TYPESCRIPT_LSP_COMMAND_ENV,
        map_kind: typescript_map_kind,
        qualify: typescript_qualify,
    }
}

fn typescript_map_kind(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
    match kind {
        LspSymbolKind::Module | LspSymbolKind::Namespace => Some(NodeKind::TypescriptModule),
        LspSymbolKind::Class => Some(NodeKind::TypescriptClass),
        LspSymbolKind::Interface => Some(NodeKind::TypescriptInterface),
        LspSymbolKind::Enum => Some(NodeKind::TypescriptEnum),
        LspSymbolKind::Method | LspSymbolKind::Constructor => Some(NodeKind::TypescriptMethod),
        LspSymbolKind::Function => Some(NodeKind::TypescriptFunction),
        _ => None,
    }
}

fn typescript_qualify(file_rel: &str, parent: Option<&str>, name: &str) -> String {
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
    fn from_options(options: &TypescriptIndexOptions) -> Self {
        if let Ok(env_cmd) = std::env::var(TYPESCRIPT_LSP_COMMAND_ENV) {
            if binary_on_path(&env_cmd) {
                return Self {
                    command: Some(env_cmd),
                    skip_reason: String::new(),
                };
            }
            return Self {
                command: None,
                skip_reason: format!(
                    "{TYPESCRIPT_LSP_COMMAND_ENV}=`{env_cmd}` 未找到对应可执行文件，已退化为 AST fallback"
                ),
            };
        }
        if let Some(cmd) = options.lsp_command.as_deref() {
            if binary_on_path(cmd) {
                return Self {
                    command: Some(cmd.to_string()),
                    skip_reason: String::new(),
                };
            }
            return Self {
                command: None,
                skip_reason: format!(
                    "`typescript.lsp_command = {cmd}` 未找到对应可执行文件，已退化为 AST fallback"
                ),
            };
        }
        // Project-local: `node_modules/.bin/typescript-language-server`.
        let local = options
            .repo_root
            .join("node_modules/.bin/typescript-language-server");
        if local.is_file() {
            return Self {
                command: Some(local.to_string_lossy().into_owned()),
                skip_reason: String::new(),
            };
        }
        if binary_on_path("typescript-language-server") {
            return Self {
                command: Some("typescript-language-server".into()),
                skip_reason: String::new(),
            };
        }
        Self {
            command: None,
            skip_reason:
                "未在 PATH / node_modules/.bin 找到 typescript-language-server，已退化为 AST fallback"
                    .into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_fixture(root: &Path) {
        for (rel, body) in [
            (
                "src/greeter.ts",
                "export class Greeter {\n  greet(name: string): string {\n    return `hi ${name}`;\n  }\n}\n",
            ),
            (
                "src/index.ts",
                "import { Greeter } from \"./greeter\";\nexport function makeGreeter() { return new Greeter(); }\n",
            ),
            (
                "tests/greeter.test.ts",
                "import { describe, it, expect } from \"vitest\";\nimport { Greeter } from \"../src/greeter\";\n\ndescribe(\"greeter\", () => {\n  it(\"greets\", () => {\n    expect(new Greeter().greet(\"Ada\")).toBe(\"hi Ada\");\n  });\n});\n",
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
    fn ast_pass_runs_against_typescript_hello_fixture_without_lsp() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let (mut store, _db) = open_temp_store(tmp.path());

        let opts = TypescriptIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src"), PathBuf::from("tests")],
            exclude_globs: Vec::new(),
            lsp_command: Some("specslice_nonexistent_ts_lsp_999".into()),
        };

        let result = index_typescript(&mut store, &opts).expect("typescript indexer ran");
        assert!(result.resolver_used == TYPESCRIPT_AST_INDEXER_NAME);
        assert!(result.files >= 3, "{result:?}");
        assert!(result.tests >= 1, "{result:?}");
        assert!(result.imports >= 2, "{result:?}");

        let nodes = store.list_all_nodes().unwrap();
        let kinds: std::collections::BTreeSet<&str> =
            nodes.iter().map(|n| n.kind.as_str()).collect();
        for required in [
            "typescript_module",
            "typescript_class",
            "typescript_method",
            "typescript_function",
            "test_case",
            "test_group",
        ] {
            assert!(
                kinds.contains(required),
                "expected `{required}` in {:?}",
                kinds
            );
        }
    }

    #[test]
    fn module_name_for_uses_file_stem() {
        assert_eq!(module_name_for("src/greeter.ts"), "greeter");
        assert_eq!(module_name_for("pages/api/route.ts"), "route");
    }

    #[test]
    fn resolve_relative_import_with_implicit_extension() {
        let files = vec![
            DiscoveredTsFile {
                relative: "src/greeter.ts".into(),
                absolute: PathBuf::from("/x/src/greeter.ts"),
            },
            DiscoveredTsFile {
                relative: "src/index.ts".into(),
                absolute: PathBuf::from("/x/src/index.ts"),
            },
        ];
        assert_eq!(
            resolve_typescript_import(&files, "src/index.ts", "./greeter"),
            Some("src/greeter.ts".into())
        );
        assert_eq!(
            resolve_typescript_import(&files, "tests/foo.ts", "../src/greeter"),
            Some("src/greeter.ts".into())
        );
        assert_eq!(
            resolve_typescript_import(&files, "src/index.ts", "react"),
            None
        );
    }
}
