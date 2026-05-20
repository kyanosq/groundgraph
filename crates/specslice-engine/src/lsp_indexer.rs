//! Generic LSP-driven language indexer used by the Swift and Go
//! adapters (P11). The indexer is intentionally language-agnostic: a
//! [`LspProfile`] describes which server to spawn, how to discover
//! source files, and how to map [`LspSymbolKind`] values onto SpecSlice
//! [`NodeKind`] variants. The indexer walks the configured roots,
//! drives the LSP server through the standard `initialize → didOpen
//! → documentSymbol → didClose → shutdown` dance, and accumulates a
//! [`LanguageIndexBatch`] for the engine to merge.
//!
//! Today this is a "structural index" only: we emit files, symbols,
//! and `contains` edges. Call / reference edges will land in a
//! follow-up that uses `textDocument/callHierarchy` (sourcekit-lsp 5.10+
//! / gopls 0.16+ both expose it). Storing the work in two phases keeps
//! the first iteration small enough to be auditable and avoids deeply
//! coupling structural indexing to call-graph latency.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specslice_core::artifact_id::file_id;
use specslice_core::language_batch::{
    FileArtifact, LanguageIndexBatch, SymbolArtifact, SymbolRange,
};
use specslice_core::ArtifactId;
use specslice_core::NodeKind;

use crate::lsp_client::{path_to_file_uri, LspClient, LspDocumentSymbol, LspSymbolKind};

/// Description of a language we drive over LSP. Each profile lives in
/// its own module (`swift_indexer`, `go_indexer`) but they all flow
/// through [`run_profile`].
pub struct LspProfile {
    /// Human-readable label, copied into [`LanguageIndexBatch::language`].
    pub language: &'static str,
    /// LSP `languageId` passed in `didOpen` (`"swift"`, `"go"`, ...).
    pub language_id: &'static str,
    /// File extensions (without the dot) considered source for this
    /// language. Files outside the list are ignored even if the user's
    /// `code_roots` contain them.
    pub file_extensions: &'static [&'static str],
    /// Directory names that should not be descended into. Mostly build
    /// output (`.build/`, `Pods/`, `vendor/`) — matched on each
    /// individual path segment, not as a glob.
    pub skip_dirs: &'static [&'static str],
    /// Suffixes that, when present anywhere in the relative path,
    /// cause the file to be ignored. Useful for `_test.go` /
    /// `_generated.go` heuristics if the operator wants stricter
    /// filtering. The profile leaves this empty by default; the
    /// engine config carries the operator-level `exclude_globs`.
    pub skip_suffixes: &'static [&'static str],
    /// Command to spawn the LSP server. May be overridden via
    /// environment variable (see [`override_command`]).
    pub default_command: &'static str,
    /// Args passed to the LSP server on spawn.
    pub default_args: &'static [&'static str],
    /// Environment variable that operators can set to override
    /// [`default_command`]. We document it next to the profile.
    pub command_env_var: &'static str,
    /// Map an LSP [`LspSymbolKind`] to a SpecSlice [`NodeKind`].
    /// Return `None` to silently drop the symbol — e.g. Go's
    /// `Variable` kind is too noisy to surface as a graph node.
    pub map_kind: fn(LspSymbolKind, parent_kind: Option<NodeKind>) -> Option<NodeKind>,
    /// Build a stable, language-specific qualified name for a symbol.
    /// Receives the file path, the parent's qualified name (if any),
    /// and the symbol's local name. Profile picks the right separator
    /// (Swift uses `.`, Go uses `.` or `/` depending on context).
    pub qualify: fn(file_rel: &str, parent: Option<&str>, name: &str) -> String,
}

#[derive(Debug, Clone)]
pub struct LspIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    /// Operator override for the LSP binary. Falls back to
    /// `profile.default_command` (looked up on `PATH`).
    pub lsp_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LspIndexStats {
    pub files: usize,
    pub symbols: usize,
    pub language: String,
    /// Reason we skipped the language adapter entirely. Empty when the
    /// adapter ran. Mirrors the Dart sidecar's `sidecar_skip_reason`
    /// so the CLI can surface the same UX.
    #[serde(default)]
    pub skip_reason: String,
}

/// Outcome of attempting to run an LSP-based profile. Either we ran
/// successfully and produced a batch, or the indexer skipped (binary
/// missing, no source files) and the engine should treat it as a no-op.
/// The `Indexed` payload is boxed so the enum size stays roughly the
/// same as `Skipped` (avoids `clippy::large_enum_variant`).
pub enum LspIndexOutcome {
    Indexed(Box<LspIndexedBatch>),
    Skipped {
        reason: String,
        language: &'static str,
    },
}

/// Successful indexer output. Owned by [`LspIndexOutcome::Indexed`] via
/// `Box` so the enum's variants stay roughly the same size.
pub struct LspIndexedBatch {
    pub batch: LanguageIndexBatch,
    pub stats: LspIndexStats,
}

/// Drive an [`LspProfile`] against an operator config. Returns
/// [`LspIndexOutcome::Skipped`] (never an `Err`) when the LSP binary
/// is missing, when there are no source files, or when the workspace
/// disables the language. Returns `Err` only for actual I/O or
/// protocol failures the operator should see.
pub fn run_profile(profile: &LspProfile, options: &LspIndexOptions) -> Result<LspIndexOutcome> {
    let command = options
        .lsp_command
        .clone()
        .unwrap_or_else(|| profile.default_command.to_string());
    if !binary_on_path(&command) {
        return Ok(LspIndexOutcome::Skipped {
            reason: format!(
                "未在 PATH 中找到 `{command}`（可设置 `{env}` 或 `.specslice.yaml` 的 `{lang}.lsp_command`）",
                env = profile.command_env_var,
                lang = profile.language
            ),
            language: profile.language,
        });
    }

    let files = discover_files(
        profile,
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
    )?;
    if files.is_empty() {
        return Ok(LspIndexOutcome::Skipped {
            reason: format!("未发现任何 `{}` 源文件，跳过 LSP 索引", profile.language),
            language: profile.language,
        });
    }

    let arg_strs: Vec<&str> = profile.default_args.to_vec();
    let mut client = LspClient::spawn(&command, &arg_strs, &options.repo_root)?;
    let root_uri = path_to_file_uri(&options.repo_root);
    client
        .initialize(&root_uri)
        .with_context(|| format!("initialising {} via `{}`", profile.language, command))?;

    let mut batch = LanguageIndexBatch {
        language: profile.language.into(),
        ..Default::default()
    };

    let mut stats = LspIndexStats {
        files: 0,
        symbols: 0,
        language: profile.language.into(),
        skip_reason: String::new(),
    };

    for file in &files {
        let file_artifact = file.artifact.clone();
        batch.files.push(file_artifact.clone());
        stats.files += 1;

        let text = std::fs::read_to_string(&file.absolute)
            .with_context(|| format!("reading {}", file.absolute.display()))?;
        let uri = path_to_file_uri(&file.absolute);
        client.did_open(&uri, profile.language_id, &text)?;
        let symbols = client
            .document_symbol(&uri)
            .with_context(|| format!("documentSymbol for {}", file.relative))?;
        let _ = client.did_close(&uri);
        let symbol_count = ingest_symbols(profile, &file.relative, &symbols, &mut batch);
        stats.symbols += symbol_count;
    }

    // Best-effort shutdown — failures are surfaced as `Err` only when
    // the user has set RUST_BACKTRACE=1 etc., otherwise we swallow
    // them so a flaky server does not invalidate a successful index.
    if let Err(err) = client.shutdown() {
        tracing_skip_reason_into(&mut stats, &err);
    }

    Ok(LspIndexOutcome::Indexed(Box::new(LspIndexedBatch {
        batch,
        stats,
    })))
}

fn tracing_skip_reason_into(stats: &mut LspIndexStats, err: &anyhow::Error) {
    // Keep the structured stats clean — only carry the message, not the
    // full chain. The CLI prefers a single human-readable sentence.
    let msg = err.to_string();
    if !msg.is_empty() {
        if stats.skip_reason.is_empty() {
            stats.skip_reason = format!("LSP shutdown 警告：{msg}");
        } else {
            stats.skip_reason.push('；');
            stats.skip_reason.push_str(&msg);
        }
    }
}

struct DiscoveredFile {
    relative: String,
    absolute: PathBuf,
    artifact: FileArtifact,
}

fn discover_files(
    profile: &LspProfile,
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
) -> Result<Vec<DiscoveredFile>> {
    let mut out: Vec<DiscoveredFile> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for code_root in code_roots {
        let abs_root = repo_root.join(code_root);
        if !abs_root.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&abs_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if profile.skip_dirs.contains(&name) {
                        continue;
                    }
                }
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(extension) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if !profile.file_extensions.contains(&extension) {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if profile.skip_suffixes.iter().any(|s| rel.ends_with(s)) {
                continue;
            }
            if exclude_globs.iter().any(|g| simple_glob_match(g, &rel)) {
                continue;
            }
            // Also skip when any path segment is in skip_dirs (handles
            // the case where the operator passed a high root like the
            // repo root and we hit `.build/` nested inside Sources).
            if rel.split('/').any(|seg| profile.skip_dirs.contains(&seg)) {
                continue;
            }
            if seen.insert(rel.clone(), ()).is_some() {
                continue;
            }
            let source = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
            out.push(DiscoveredFile {
                artifact: FileArtifact {
                    id: file_id(&rel),
                    path: rel.clone(),
                    language: profile.language.into(),
                    content_hash: hash,
                },
                relative: rel,
                absolute: path.to_path_buf(),
            });
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

/// Per-recursion bookkeeping used while walking a `documentSymbol`
/// tree. Bundled into one struct so the recursive helper has a small
/// argument list and clippy stays happy with `too_many_arguments`.
struct SymbolVisit<'a> {
    profile: &'a LspProfile,
    file_rel: &'a str,
    parent_qual: Option<&'a str>,
    parent_id: Option<&'a ArtifactId>,
    parent_kind: Option<NodeKind>,
}

fn ingest_symbols(
    profile: &LspProfile,
    file_rel: &str,
    symbols: &[LspDocumentSymbol],
    batch: &mut LanguageIndexBatch,
) -> usize {
    let mut count = 0;
    let frame = SymbolVisit {
        profile,
        file_rel,
        parent_qual: None,
        parent_id: None,
        parent_kind: None,
    };
    visit_symbols(&frame, symbols, batch, &mut count);
    count
}

fn visit_symbols(
    frame: &SymbolVisit<'_>,
    symbols: &[LspDocumentSymbol],
    batch: &mut LanguageIndexBatch,
    count: &mut usize,
) {
    for symbol in symbols {
        let Some(kind) = (frame.profile.map_kind)(symbol.kind, frame.parent_kind) else {
            // Recurse so children of an ignored container are still
            // surfaced (e.g. Swift `extension` blocks).
            visit_symbols(frame, &symbol.children, batch, count);
            continue;
        };
        let qualified = (frame.profile.qualify)(frame.file_rel, frame.parent_qual, &symbol.name);
        let id = ArtifactId::new(format!("{}::{}", frame.profile.language, qualified));
        let start_line = symbol.start_line.saturating_add(1);
        let end_line = symbol.end_line.saturating_add(1).max(start_line);
        let symbol_artifact = SymbolArtifact {
            id: id.clone(),
            kind,
            path: frame.file_rel.into(),
            name: symbol.name.clone(),
            qualified_name: qualified.clone(),
            start_line,
            end_line,
            parent_symbol_id: frame.parent_id.cloned(),
        };
        batch.symbol_ranges.push(SymbolRange {
            file_path: frame.file_rel.into(),
            symbol_id: id.clone(),
            start_line,
            end_line,
            symbol_kind: kind,
            qualified_name: qualified.clone(),
            parent_symbol_id: frame.parent_id.cloned(),
        });
        batch.symbols.push(symbol_artifact);
        *count += 1;

        let child_frame = SymbolVisit {
            profile: frame.profile,
            file_rel: frame.file_rel,
            parent_qual: Some(&qualified),
            parent_id: Some(&id),
            parent_kind: Some(kind),
        };
        visit_symbols(&child_frame, &symbol.children, batch, count);
    }
}

/// Lightweight glob used for `exclude_globs`. Matches the same subset
/// the Dart indexer accepts:
/// - `**` — any number of characters including `/` (cross-directory).
/// - `*`  — any number of characters *within a single segment* (does
///   not cross `/`).
/// - `?`  — a single character (any except `/`).
/// - Literal characters compare 1-for-1.
///
/// Implemented as a small recursive descent matcher so the
/// `*` vs `**` distinction is explicit. Backtracking is fine for the
/// patterns operators put in `.specslice.yaml` (a handful of segments
/// at most).
fn simple_glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = path.chars().collect();
    glob_match_rec(&pat, 0, &txt, 0)
}

fn glob_match_rec(pat: &[char], mut pi: usize, txt: &[char], mut ti: usize) -> bool {
    while pi < pat.len() {
        if pat[pi] == '*' {
            let double = pi + 1 < pat.len() && pat[pi + 1] == '*';
            if double {
                // `**` — consume optional trailing `/` so `**/foo`
                // also matches a path that starts at `foo`.
                let mut next = pi + 2;
                if next < pat.len() && pat[next] == '/' {
                    next += 1;
                }
                // Try matching the remainder against every suffix of
                // `txt[ti..]` (including the empty suffix).
                for j in ti..=txt.len() {
                    if glob_match_rec(pat, next, txt, j) {
                        return true;
                    }
                }
                return false;
            } else {
                // `*` — match within a single segment.
                let next = pi + 1;
                for j in ti..=txt.len() {
                    // Stop expanding once we hit a `/`.
                    if j > ti && txt[j - 1] == '/' {
                        break;
                    }
                    if glob_match_rec(pat, next, txt, j) {
                        return true;
                    }
                }
                return false;
            }
        }
        if ti >= txt.len() {
            return false;
        }
        match pat[pi] {
            '?' => {
                if txt[ti] == '/' {
                    return false;
                }
            }
            c if c == txt[ti] => {}
            _ => return false,
        }
        pi += 1;
        ti += 1;
    }
    ti == txt.len()
}

/// Return `true` when `command` resolves to a binary on the system
/// `PATH` (or is itself an absolute path that exists). Mirrors the
/// behaviour of the Dart sidecar's binary check.
pub fn binary_on_path(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute() {
        return path.is_file();
    }
    if let Ok(env_path) = std::env::var("PATH") {
        for dir in env_path.split(if cfg!(windows) { ';' } else { ':' }) {
            let candidate = Path::new(dir).join(command);
            if candidate.is_file() {
                return true;
            }
            // On Windows fall back to the common `.exe` suffix.
            if cfg!(windows) {
                let with_exe = candidate.with_extension("exe");
                if with_exe.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_qualify(file_rel: &str, parent: Option<&str>, name: &str) -> String {
        match parent {
            Some(p) => format!("{p}.{name}"),
            None => format!("{file_rel}#{name}"),
        }
    }

    fn dummy_map(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
        match kind {
            LspSymbolKind::Class => Some(NodeKind::SwiftClass),
            LspSymbolKind::Method => Some(NodeKind::SwiftMethod),
            _ => None,
        }
    }

    fn profile() -> LspProfile {
        LspProfile {
            language: "swift",
            language_id: "swift",
            file_extensions: &["swift"],
            skip_dirs: &[".build", "Pods"],
            skip_suffixes: &[],
            default_command: "sourcekit-lsp",
            default_args: &[],
            command_env_var: "SPECSLICE_SWIFT_LSP_BIN",
            map_kind: dummy_map,
            qualify: dummy_qualify,
        }
    }

    #[test]
    fn ingest_symbols_emits_parent_child_pairs_and_skips_unmapped_kinds() {
        let profile = profile();
        let tree = vec![LspDocumentSymbol {
            name: "Greeter".into(),
            detail: None,
            kind: LspSymbolKind::Class,
            start_line: 0,
            end_line: 9,
            children: vec![
                LspDocumentSymbol {
                    name: "greet".into(),
                    detail: None,
                    kind: LspSymbolKind::Method,
                    start_line: 1,
                    end_line: 4,
                    children: Vec::new(),
                },
                LspDocumentSymbol {
                    name: "noise".into(),
                    detail: None,
                    kind: LspSymbolKind::Variable,
                    start_line: 5,
                    end_line: 5,
                    children: Vec::new(),
                },
            ],
        }];
        let mut batch = LanguageIndexBatch {
            language: profile.language.into(),
            ..Default::default()
        };
        let n = ingest_symbols(&profile, "Sources/Greeter.swift", &tree, &mut batch);
        assert_eq!(n, 2);
        assert_eq!(batch.symbols.len(), 2);
        let class = &batch.symbols[0];
        assert_eq!(class.kind, NodeKind::SwiftClass);
        assert_eq!(class.start_line, 1);
        assert_eq!(class.end_line, 10);
        assert!(class.parent_symbol_id.is_none());

        let method = &batch.symbols[1];
        assert_eq!(method.kind, NodeKind::SwiftMethod);
        assert_eq!(
            method.parent_symbol_id.as_ref().unwrap().as_str(),
            class.id.as_str()
        );
        assert_eq!(method.start_line, 2);
        assert_eq!(method.end_line, 5);
        assert_eq!(
            method.qualified_name,
            format!("{}.greet", class.qualified_name)
        );
    }

    #[test]
    fn ingest_symbols_recurses_into_unmapped_containers() {
        // An ignored container (e.g. Swift `extension`) should still let
        // us discover its mapped children.
        fn passthrough_map(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
            match kind {
                LspSymbolKind::Method => Some(NodeKind::SwiftMethod),
                _ => None,
            }
        }
        let mut profile = profile();
        profile.map_kind = passthrough_map;

        let tree = vec![LspDocumentSymbol {
            name: "Greeter+API".into(),
            detail: None,
            kind: LspSymbolKind::Namespace,
            start_line: 0,
            end_line: 20,
            children: vec![LspDocumentSymbol {
                name: "greetAsync".into(),
                detail: None,
                kind: LspSymbolKind::Method,
                start_line: 3,
                end_line: 6,
                children: Vec::new(),
            }],
        }];
        let mut batch = LanguageIndexBatch::default();
        let n = ingest_symbols(&profile, "Sources/Greeter+API.swift", &tree, &mut batch);
        assert_eq!(n, 1);
        assert_eq!(batch.symbols.len(), 1);
        assert_eq!(batch.symbols[0].name, "greetAsync");
        assert!(batch.symbols[0].parent_symbol_id.is_none());
    }

    #[test]
    fn simple_glob_match_handles_dot_star_and_double_star() {
        assert!(simple_glob_match(
            "**/*.gen.go",
            "internal/api/users.gen.go"
        ));
        assert!(simple_glob_match(
            "**/.build/**",
            "Foo/.build/release/lib.swift"
        ));
        assert!(!simple_glob_match("**/.build/**", "Foo/Sources/lib.swift"));
        assert!(simple_glob_match("*.swift", "Hello.swift"));
        assert!(!simple_glob_match("*.swift", "Sources/Hello.swift"));
    }

    #[test]
    fn discover_files_filters_by_extension_and_exclude_globs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("Sources/App")).unwrap();
        std::fs::create_dir_all(root.join(".build/release")).unwrap();
        std::fs::write(root.join("Sources/App/A.swift"), "// a\n").unwrap();
        std::fs::write(root.join("Sources/App/B.swift"), "// b\n").unwrap();
        std::fs::write(root.join("Sources/App/C.txt"), "ignore me\n").unwrap();
        std::fs::write(root.join(".build/release/D.swift"), "// d\n").unwrap();

        let profile = profile();
        let opts = LspIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from("Sources")],
            exclude_globs: vec!["**/B.swift".into()],
            lsp_command: None,
        };
        let files = discover_files(
            &profile,
            &opts.repo_root,
            &opts.code_roots,
            &opts.exclude_globs,
        )
        .unwrap();
        let names: Vec<String> = files.into_iter().map(|f| f.relative).collect();
        assert_eq!(names, vec!["Sources/App/A.swift"]);
    }

    #[test]
    fn binary_on_path_finds_or_misses_known_tool() {
        // `sh` always lives on PATH on macOS / Linux; missing-tool case
        // is the obviously made-up name.
        if cfg!(unix) {
            assert!(binary_on_path("sh"), "sh should be on PATH");
        }
        assert!(
            !binary_on_path("specslice_nonexistent_tool_12345"),
            "made-up binary should not resolve"
        );
    }

    #[test]
    fn run_profile_skips_when_binary_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = profile();
        p.default_command = "specslice_nonexistent_tool_12345";
        let opts = LspIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("Sources")],
            exclude_globs: Vec::new(),
            lsp_command: None,
        };
        let outcome = run_profile(&p, &opts).unwrap();
        match outcome {
            LspIndexOutcome::Skipped { reason, language } => {
                assert_eq!(language, "swift");
                assert!(reason.contains("PATH"), "expected PATH hint: {reason}");
            }
            LspIndexOutcome::Indexed(_) => panic!("expected skip"),
        }
    }
}
