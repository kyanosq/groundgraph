//! SpecSlice checks — declared-link consistency *and* content-level
//! doc→code drift.
//!
//! Declared-link checks (MVP-5 scope, store-only — [`compute_checks`]):
//! - `broken_link`: a manifest-declared relationship points to a node that
//!   does not exist. Severity: error.
//! - `missing_linked_test`: a requirement has at least one declared
//!   implementation but no declared verification. Severity: warning.
//! - `orphan_requirement`: a requirement has no declared implementation and
//!   no declared verification. Severity: warning.
//! - `impact_review`: synthesised from an [`ImpactReport`]; warns when no
//!   linked tests changed alongside a requirement.
//!
//! Content-level drift checks (need the working tree — only via
//! [`run_checks`], which has a `repo_root`):
//! - `doc_stale_code_ref`: a doc section's *body* references, in inline
//!   backticks, a repo path that doesn't exist or a symbol whose container
//!   resolves but whose member doesn't (`Engine::not_real()`) — the document
//!   describes code that is gone, was renamed, or was never implemented.
//!   Precision-first: fenced code blocks and external-crate symbols
//!   (`rusqlite::Connection`) are never flagged.
//! - `requirement_implementation_hint`: every orphan requirement is searched
//!   against the graph (structural + fulltext content layer). Plausible
//!   implementations are suggested (`specslice connect` confirms them); zero
//!   plausible hits is surfaced as a likely implementation gap.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{EdgeKind, NodeKind};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::impact::ImpactReport;
use crate::source_text::is_multi_word_identifier;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSeverity {
    Error,
    Warning,
    Info,
}

impl CheckSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckSeverity::Error => "error",
            CheckSeverity::Warning => "warning",
            CheckSeverity::Info => "info",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckFinding {
    pub code: String,
    pub severity: CheckSeverity,
    pub message: String,
    pub artifact_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CheckReport {
    pub findings: Vec<CheckFinding>,
}

impl CheckReport {
    pub fn errors(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == CheckSeverity::Error)
            .count()
    }
    pub fn warnings(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == CheckSeverity::Warning)
            .count()
    }
    pub fn has_errors(&self) -> bool {
        self.errors() > 0
    }
}

#[derive(Debug, Clone)]
pub struct CheckOptions {
    pub repo_root: PathBuf,
    /// If `Some`, additionally synthesise checks from the impact report.
    pub impact: Option<ImpactReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckPolicy {
    pub broken_link_level: String,
    pub missing_linked_test_level: String,
    pub orphan_requirement_level: String,
    /// `doc_stale_code_ref` severity (default `warning`).
    pub stale_doc_ref_level: String,
    /// `requirement_implementation_hint` severity (default `info`).
    pub requirement_hint_level: String,
    /// Globs muting `doc_stale_code_ref` (matched against referenced paths
    /// and against the document's own path).
    pub doc_drift_ignore: Vec<String>,
}

impl Default for CheckPolicy {
    fn default() -> Self {
        Self {
            broken_link_level: "error".into(),
            missing_linked_test_level: "warning".into(),
            orphan_requirement_level: "warning".into(),
            stale_doc_ref_level: "warning".into(),
            requirement_hint_level: "info".into(),
            doc_drift_ignore: Vec::new(),
        }
    }
}

impl From<&crate::config::ChecksConfig> for CheckPolicy {
    fn from(value: &crate::config::ChecksConfig) -> Self {
        Self {
            broken_link_level: value.broken_link_level.clone(),
            missing_linked_test_level: value.missing_linked_test_level.clone(),
            orphan_requirement_level: value.orphan_requirement_level.clone(),
            stale_doc_ref_level: value.stale_doc_ref_level.clone(),
            requirement_hint_level: value.requirement_hint_level.clone(),
            doc_drift_ignore: value.doc_drift_ignore.clone(),
        }
    }
}

pub fn run_checks(options: CheckOptions) -> Result<CheckReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    let policy = CheckPolicy::from(&config.checks);
    let mut report = compute_checks_with_policy(&store, options.impact.as_ref(), policy.clone())?;
    // Content-level drift needs the working tree (doc bodies, file existence),
    // so it lives here rather than in the store-only `compute_checks`.
    compute_doc_drift(&store, &options.repo_root, &policy, &mut report)?;
    compute_requirement_hints(&store, &options.repo_root, &policy, &mut report)?;
    Ok(report)
}

pub fn compute_checks(store: &Store, impact: Option<&ImpactReport>) -> Result<CheckReport> {
    compute_checks_with_policy(store, impact, CheckPolicy::default())
}

pub fn compute_checks_with_policy(
    store: &Store,
    impact: Option<&ImpactReport>,
    policy: CheckPolicy,
) -> Result<CheckReport> {
    let mut report = CheckReport::default();

    let requirement_ids: BTreeSet<_> = store
        .list_nodes_by_kind(NodeKind::Requirement)?
        .into_iter()
        .map(|n| n.id)
        .collect();
    let all_node_ids: BTreeSet<_> = store.list_all_nodes()?.into_iter().map(|n| n.id).collect();

    // Manifest-declared relationship edges whose endpoints do not exist.
    for edge in store.list_all_edges()? {
        match edge.kind {
            EdgeKind::Documents
            | EdgeKind::DeclaresImplementation
            | EdgeKind::DeclaresVerification => {
                if !all_node_ids.contains(&edge.to_id) {
                    push_configured_finding(
                        &mut report,
                        &policy.broken_link_level,
                        "broken_link",
                        format!(
                            "Link `{}` points to unknown target `{}`.",
                            edge.kind.as_str(),
                            edge.to_id
                        ),
                        Some(edge.from_id.to_string()),
                        None,
                    );
                }
                if !all_node_ids.contains(&edge.from_id) {
                    push_configured_finding(
                        &mut report,
                        &policy.broken_link_level,
                        "broken_link",
                        format!(
                            "Link `{}` originates from unknown artifact `{}`.",
                            edge.kind.as_str(),
                            edge.from_id
                        ),
                        Some(edge.from_id.to_string()),
                        None,
                    );
                }
            }
            _ => {}
        }
    }

    // missing_linked_test / orphan_requirement.
    for req_id in &requirement_ids {
        let incoming = store.list_edges_to(req_id)?;
        let has_impl = incoming
            .iter()
            .any(|e| e.kind == EdgeKind::DeclaresImplementation);
        let has_test = incoming
            .iter()
            .any(|e| e.kind == EdgeKind::DeclaresVerification);
        let path = store.find_node(req_id)?.and_then(|n| n.path);
        if has_impl && !has_test {
            push_configured_finding(
                &mut report,
                &policy.missing_linked_test_level,
                "missing_linked_test",
                format!(
                    "Requirement {} has linked implementation but no linked verification test.",
                    req_id
                ),
                Some(req_id.to_string()),
                path.clone(),
            );
        }
        if !has_impl && !has_test {
            push_configured_finding(
                &mut report,
                &policy.orphan_requirement_level,
                "orphan_requirement",
                format!(
                    "Requirement {} has no declared implementation or test.",
                    req_id
                ),
                Some(req_id.to_string()),
                path,
            );
        }
    }

    if let Some(impact) = impact {
        for w in &impact.warnings {
            report.findings.push(CheckFinding {
                code: "impact_review".into(),
                severity: CheckSeverity::Warning,
                message: w.clone(),
                artifact_id: None,
                path: None,
            });
        }
        for info in &impact.info {
            report.findings.push(CheckFinding {
                code: "impact_review".into(),
                severity: CheckSeverity::Info,
                message: info.clone(),
                artifact_id: None,
                path: None,
            });
        }
    }

    Ok(report)
}

fn push_configured_finding(
    report: &mut CheckReport,
    level: &str,
    code: &str,
    message: String,
    artifact_id: Option<String>,
    path: Option<String>,
) {
    let Some(severity) = severity_from_level(level) else {
        return;
    };
    report.findings.push(CheckFinding {
        code: code.into(),
        severity,
        message,
        artifact_id,
        path,
    });
}

fn severity_from_level(level: &str) -> Option<CheckSeverity> {
    match level.trim().to_ascii_lowercase().as_str() {
        "error" => Some(CheckSeverity::Error),
        "warning" | "warn" => Some(CheckSeverity::Warning),
        "info" => Some(CheckSeverity::Info),
        "off" | "none" | "ignore" => None,
        _ => Some(CheckSeverity::Warning),
    }
}

// ---------------------------------------------------------------------------
// Content-level doc→code drift
// ---------------------------------------------------------------------------

/// An inline-backtick code reference extracted from a doc body.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CodeRef {
    /// `src/engine.rs`, `crates/foo/src/lib.rs#L10` — a repo-relative path.
    Path(String),
    /// `engine.rs` — a source filename without a directory; resolved by
    /// basename against the indexed paths.
    BareFile(String),
    /// `Engine::start()`, `Store.open` — `container` is `None` for a bare
    /// `start()` reference.
    Symbol {
        container: Option<String>,
        member: String,
        raw: String,
    },
}

/// File extensions that make a slash-containing backtick span count as a code
/// path reference. Anything else (URLs, globs, prose) is ignored.
const PATH_REF_EXTS: &[&str] = &[
    "rs", "dart", "py", "go", "ts", "tsx", "js", "jsx", "swift", "java", "kt", "c", "cc", "cpp",
    "h", "hpp", "m", "mm", "sql", "yaml", "yml", "toml", "json", "md", "mdx", "html", "css", "sh",
];

/// Subset of [`PATH_REF_EXTS`] for which a *bare filename* (`engine.rs`, no
/// directory) is unambiguous enough to verify. Resource/config filenames
/// (`links.yaml`, `schema.sql`, `CHANGELOG.md`, `go.mod`) routinely name
/// templates, runtime outputs, or other repos' files; bare C/C++ headers
/// (`archive.h`, `zlib.h`) usually name system or vendored headers — scanning
/// real repos showed flagging either is mostly noise, so only unambiguous
/// source-code extensions report.
const BARE_FILE_REPORT_EXTS: &[&str] = &[
    "rs", "dart", "py", "go", "ts", "tsx", "js", "jsx", "swift", "java", "kt", "c", "cc", "cpp",
    "m", "mm",
];

/// Extensions of *generated artifacts* (`index.scip`, `graph.db`). A doc
/// mentioning one is never code drift — they are not supposed to be in the
/// graph at all, so flagging them would be pure noise.
const ARTIFACT_EXTS: &[&str] = &[
    "scip", "db", "lock", "png", "svg", "gif", "jpg", "jpeg", "txt", "csv", "log", "zip", "gz",
    "wasm", "so", "dylib", "dll", "a", "o", "ipa", "apk",
];

/// Scan inline single-backtick spans on the non-fenced lines of a markdown
/// body and classify each as a [`CodeRef`] when it looks like one.
fn extract_inline_code_refs(body: &str) -> Vec<CodeRef> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else { break };
            let span = &after[..close];
            if let Some(code_ref) = classify_code_ref(span) {
                out.push(code_ref);
            }
            rest = &after[close + 1..];
        }
    }
    out
}

/// True when any indexed *code* body mentions the identifier (all its split
/// words on one node). Documentation nodes are excluded — otherwise the very
/// doc that contains the stale reference would vouch for it. Degrades to
/// `false` on pre-FTS databases.
fn fulltext_mentions(store: &Store, identifier: &str) -> bool {
    let tokens = crate::fts_text::fts_query_tokens(identifier);
    if tokens.is_empty() {
        return false;
    }
    let expr = crate::fts_text::fts_all_expr(&tokens);
    let Ok(hits) = store.fulltext_match(&expr, 50) else {
        return false;
    };
    hits.iter().any(|hit| {
        store
            .find_node(&specslice_core::ArtifactId::new(hit.node_id.clone()))
            .ok()
            .flatten()
            .is_some_and(|n| {
                !matches!(
                    n.kind,
                    NodeKind::DocSection
                        | NodeKind::Adr
                        | NodeKind::Requirement
                        | NodeKind::AcceptanceCriterion
                        | NodeKind::BusinessCandidate
                )
            })
    })
}

/// Lowercased basenames of every file in the working tree, skipping VCS
/// internals and well-known build/vendor output directories.
fn working_tree_basenames(repo_root: &Path) -> BTreeSet<String> {
    const SKIP_DIRS: &[&str] = &[
        ".git",
        "target",
        "node_modules",
        "build",
        "dist",
        ".dart_tool",
        "vendor",
        ".venv",
    ];
    walkdir::WalkDir::new(repo_root)
        .into_iter()
        .filter_entry(|e| {
            e.file_type().is_file()
                || e.file_name()
                    .to_str()
                    .is_none_or(|n| !SKIP_DIRS.contains(&n))
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_ascii_lowercase))
        .collect()
}

/// Date/sequence template tokens (`round-XX-report-YYYY-MM-DD.md`) mark a
/// *naming pattern*, not one file. Tokens are exact-matched per hyphen/dot/
/// slash-separated word so real names like `README` never trip this.
fn has_template_tokens(s: &str) -> bool {
    const TEMPLATE_TOKENS: &[&str] = &["XX", "XXX", "XXXX", "YYYY", "MM", "DD", "HH", "NN"];
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| TEMPLATE_TOKENS.contains(&tok))
}

/// Tutorial placeholder names (`foo.rs`, `lib/foo.rs`, `pages/api/foo.ts`)
/// describe *shapes* of code, not this repository's code.
fn is_placeholder_path(p: &str) -> bool {
    const PLACEHOLDER_STEMS: &[&str] = &[
        "foo",
        "bar",
        "baz",
        "qux",
        "quux",
        "example",
        "sample",
        "demo",
        "dummy",
        "placeholder",
        "my_app",
        "my-app",
        "myapp",
    ];
    p.split(['/', '\\']).any(|seg| {
        let stem = seg.split('.').next().unwrap_or(seg).to_ascii_lowercase();
        PLACEHOLDER_STEMS.contains(&stem.as_str())
    })
}

/// Classify one backtick span. Precision-first: reject anything ambiguous —
/// a missed stale ref costs little, a false alarm erodes trust in `check`.
fn classify_code_ref(span: &str) -> Option<CodeRef> {
    let s = span.trim();
    if s.len() < 3 || s.len() > 200 || s.chars().any(char::is_whitespace) {
        return None;
    }
    if s.contains('*') || s.contains("://") || s.contains('<') || s.contains('>') {
        return None; // glob, URL, or `<placeholder>` template
    }
    if s.contains('{') || s.contains('}') || s.contains("...") {
        return None; // brace expansion `{a,b}.rs` / ellipsis shorthand
    }
    if s.starts_with("../") {
        // Relative to the *document*, not the repo root — docs of nested
        // packages legitimately point above their own indexing scope.
        return None;
    }
    if has_template_tokens(s) {
        return None; // `round-XX-report-YYYY-MM-DD.md` names a pattern
    }
    if s.starts_with(".specslice/") || s.starts_with(".specslice\\") {
        return None; // SpecSlice's own runtime workspace — docs cite outputs
    }
    if s.contains('/') && !s.starts_with('/') {
        // Strip `#fragment` / `:line` suffixes before the extension test.
        let path_part = s.split(['#', ':']).next().unwrap_or(s);
        let ext = Path::new(path_part)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if PATH_REF_EXTS.contains(&ext) {
            let cleaned = path_part.trim_start_matches("./");
            if is_placeholder_path(cleaned) {
                return None;
            }
            return Some(CodeRef::Path(cleaned.to_string()));
        }
        return None;
    }
    // Symbol shapes: `A::b`, `A::b()`, `A.b()`, bare `f()`. But first:
    // `engine.rs` / `index.scip` are *filenames*, not method calls — a dot
    // whose tail is a file extension means "file", never "member".
    let call_like = s.ends_with("()");
    let body = s.trim_end_matches("()");
    if body.is_empty() || body.contains('/') {
        return None;
    }
    if !call_like {
        if let Some((stem, tail)) = body.rsplit_once('.') {
            let ext = tail.to_ascii_lowercase();
            let filename_chars = |t: &str| {
                t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            };
            if PATH_REF_EXTS.contains(&ext.as_str()) {
                // An empty stem (`.ts`) or non-filename chars in the stem
                // (`.js<->.ts` arrows) are prose about extensions, not file
                // references. Resource/config filenames are recognised as
                // files (so they never fall through to the symbol branch)
                // but only source-code filenames are worth verifying.
                if stem.is_empty()
                    || !filename_chars(stem)
                    || !BARE_FILE_REPORT_EXTS.contains(&ext.as_str())
                    || is_placeholder_path(body)
                {
                    return None;
                }
                return Some(CodeRef::BareFile(body.to_string()));
            }
            if ARTIFACT_EXTS.contains(&ext.as_str()) || ext == "mod" || ext == "sum" {
                return None; // generated artifact / go module manifests
            }
        }
    }
    // Member references without call parens (`SearchMatch::framework_role`,
    // `sym.file_rel`, `python.lsp_command`) are routinely struct fields or
    // config keys — kinds the graph has no nodes for. Only verified shapes:
    // explicit calls `A::b()` / `A.b()` / `f()`.
    if !call_like {
        return None;
    }
    let (container, member) = if let Some((head, tail)) = body.rsplit_once("::") {
        (Some(head.to_string()), tail.to_string())
    } else if let Some((head, tail)) = body.rsplit_once('.') {
        // `path.is_file()` — a lowercase receiver is a local variable being
        // narrated, not a type we can resolve. Only `Type.method()` verifies.
        if head.chars().next().is_none_or(char::is_lowercase) {
            return None;
        }
        (Some(head.to_string()), tail.to_string())
    } else {
        // A bare call: `click()` / `focus()` / `pop()` are platform or
        // stdlib APIs in every ecosystem — a single generic word carries no
        // signal. Only multi-word names (`python_lsp_available()`,
        // `handleSaveProject()`) identify project code worth verifying.
        if !is_multi_word_identifier(body) {
            return None;
        }
        (None, body.to_string())
    };
    let ident_ok = |t: &str| {
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
    };
    if !ident_ok(&member) || container.as_deref().is_some_and(|c| !ident_ok(c)) {
        return None;
    }
    // Uppercase members (`EdgeConfidence::High`, `Engine::Started`) are enum
    // variants / associated consts — kinds the indexers do not emit nodes
    // for, so "member not in graph" would be a guaranteed false positive.
    if member.chars().next().is_some_and(char::is_uppercase) {
        return None;
    }
    Some(CodeRef::Symbol {
        container,
        member,
        raw: s.to_string(),
    })
}

/// Build a matcher from the repository-root `.gitignore`. Best-effort: a
/// missing file or a parse error yields an empty matcher (ignores nothing), so
/// repos without a `.gitignore` behave exactly as before. Nested per-directory
/// `.gitignore`s and the global excludes file are intentionally out of scope —
/// the repo-root file covers the overwhelming majority of generated-artifact
/// patterns (`/artifacts/`, `/data/`, `*.log`).
fn build_repo_gitignore(repo_root: &Path) -> ignore::gitignore::Gitignore {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(repo_root);
    let _ = builder.add(repo_root.join(".gitignore"));
    builder
        .build()
        .unwrap_or_else(|_| ignore::gitignore::Gitignore::empty())
}

/// True when a repo-relative referenced path `rel` is git-ignored — checking
/// parent directories too, so a file *under* an ignored directory
/// (`artifacts/x/y.json` beneath `/artifacts/`) also matches. A git-ignored
/// path is deliberately untracked (generated artifact, credential, runtime
/// data); a doc that mentions it is citing local evidence, not making a broken
/// code reference, so it must not surface as `doc_stale_code_ref`.
fn is_repo_gitignored(gi: &ignore::gitignore::Gitignore, repo_root: &Path, rel: &str) -> bool {
    let is_dir = repo_root.join(rel).is_dir();
    gi.matched_path_or_any_parents(Path::new(rel), is_dir)
        .is_ignore()
}

/// `doc_stale_code_ref` — walk every doc-structure node's body and verify its
/// inline code references against the working tree + graph.
fn compute_doc_drift(
    store: &Store,
    repo_root: &Path,
    policy: &CheckPolicy,
    report: &mut CheckReport,
) -> Result<()> {
    if severity_from_level(&policy.stale_doc_ref_level).is_none() {
        return Ok(());
    }
    let nodes = store.list_all_nodes().context("listing nodes for drift")?;
    let node_paths: BTreeSet<&str> = nodes.iter().filter_map(|n| n.path.as_deref()).collect();
    let mut node_basenames: BTreeSet<String> = node_paths
        .iter()
        .filter_map(|p| p.rsplit(['/', '\\']).next())
        .map(|b| b.to_ascii_lowercase())
        .collect();
    // Files outside the indexed roots (`tool/`, `scripts/`) are still real —
    // fold the working tree's basenames in so bare refs to them resolve.
    node_basenames.extend(working_tree_basenames(repo_root));
    let node_names: BTreeSet<String> = nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .map(|n| n.to_ascii_lowercase())
        .collect();
    // One unique drift per (document, message): the same stale ref repeated
    // across a doc must read as one action item, not N lines of noise.
    let mut reported: BTreeSet<(String, String)> = BTreeSet::new();

    // User-muted globs — matched against referenced paths and doc paths.
    let mut ignore_builder = globset::GlobSetBuilder::new();
    for pat in &policy.doc_drift_ignore {
        if let Ok(glob) = globset::Glob::new(pat) {
            ignore_builder.add(glob);
        }
    }
    let ignored = ignore_builder
        .build()
        .unwrap_or_else(|_| globset::GlobSet::empty());

    // The repo's own `.gitignore` mutes references to deliberately-untracked
    // paths (generated artifacts, credentials, runtime data). Real-repo
    // dogfooding (MetaQuant) showed ~93% of `doc_stale_code_ref` warnings were
    // exactly such gitignored paths — noise that buried the few genuine drifts.
    let gitignore = build_repo_gitignore(repo_root);

    // Read each doc file once; slice every section's span out of it.
    let doc_sections: Vec<_> = nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::DocSection | NodeKind::Adr)
                && n.path.is_some()
                && n.start_line.is_some()
                && n.end_line.is_some()
        })
        .collect();
    let mut file_cache: BTreeMap<&str, Option<Vec<String>>> = BTreeMap::new();
    for section in doc_sections {
        let path = section.path.as_deref().unwrap_or_default();
        if ignored.is_match(path) {
            continue; // whole document muted (e.g. notes on another repo)
        }
        let lines = file_cache.entry(path).or_insert_with(|| {
            std::fs::read_to_string(repo_root.join(path))
                .ok()
                .map(|c| c.lines().map(|l| l.to_string()).collect())
        });
        let Some(lines) = lines else { continue };
        let start = (section.start_line.unwrap_or(1).max(1) as usize - 1).min(lines.len());
        let end = (section.end_line.unwrap_or(0) as usize).min(lines.len());
        if start >= end {
            continue;
        }
        let body = lines[start..end].join("\n");
        for code_ref in extract_inline_code_refs(&body) {
            let missing_msg = match &code_ref {
                CodeRef::Path(p) => {
                    // Resolution order: exact file on disk → exact indexed
                    // path → *suffix* match against indexed paths (docs often
                    // abbreviate `crates/x/src/commands/propose.rs` to
                    // `commands/propose.rs`).
                    let suffix = format!("/{p}");
                    let exists = repo_root.join(p).exists()
                        || node_paths.contains(p.as_str())
                        || node_paths.iter().any(|np| np.ends_with(&suffix));
                    // A first segment that exists neither on disk nor in any
                    // indexed path means the doc narrates another repository's
                    // layout (`src/main/java/...` in a Rust repo) — out of
                    // scope, not drift.
                    let first_seg = p.split('/').next().unwrap_or(p);
                    let seg_prefix = format!("{first_seg}/");
                    let in_scope = repo_root.join(first_seg).exists()
                        || node_paths.iter().any(|np| np.starts_with(&seg_prefix));
                    (!exists
                        && in_scope
                        && !ignored.is_match(p.as_str())
                        && !is_repo_gitignored(&gitignore, repo_root, p))
                    .then(|| {
                        format!("文档引用的路径 `{p}` 在仓库与图中都不存在（文档过期或实现缺失）")
                    })
                }
                CodeRef::BareFile(name) => {
                    // Basename match against indexed paths, falling back to a
                    // root-level file probe (`Cargo.toml`, `README.md` are
                    // real files that never become graph nodes).
                    let known = node_basenames.contains(&name.to_ascii_lowercase())
                        || repo_root.join(name).exists();
                    (!known
                        && !ignored.is_match(name.as_str())
                        && !is_repo_gitignored(&gitignore, repo_root, name))
                    .then(|| {
                        format!(
                            "文档引用的文件 `{name}` 没有任何已索引路径与之匹配（文档过期或实现缺失）"
                        )
                    })
                }
                CodeRef::Symbol {
                    container,
                    member,
                    raw,
                } => {
                    // Known when a node *defines* it — or, as a fallback, when
                    // any indexed source body *uses* it (platform APIs like
                    // `computeLuminance()` have call sites but no definition
                    // node; a doc describing such a call is not drift).
                    let member_known = node_names.contains(&member.to_ascii_lowercase())
                        || fulltext_mentions(store, member);
                    match container {
                        // `A::b` — only drift when the container itself is
                        // ours (external crates skip silently).
                        Some(c) => {
                            let container_known = c
                                .rsplit("::")
                                .next()
                                .map(|last| node_names.contains(&last.to_ascii_lowercase()))
                                .unwrap_or(false);
                            (container_known && !member_known).then(|| {
                                format!(
                                    "文档引用 `{raw}`：容器 `{c}` 在图中存在，但成员 `{member}` 不存在（改名/删除/未实现）"
                                )
                            })
                        }
                        None => (!member_known).then(|| {
                            format!("文档引用的函数 `{raw}` 在图中不存在（文档过期或实现缺失）")
                        }),
                    }
                }
            };
            if let Some(message) = missing_msg {
                let key = (path.to_string(), message.clone());
                if !reported.insert(key) {
                    continue;
                }
                push_configured_finding(
                    report,
                    &policy.stale_doc_ref_level,
                    "doc_stale_code_ref",
                    message,
                    Some(section.id.to_string()),
                    section.path.clone(),
                );
            }
        }
    }
    Ok(())
}

/// `requirement_implementation_hint` — search the graph (structural +
/// content layer) for plausible implementations of every requirement that
/// has none declared.
fn compute_requirement_hints(
    store: &Store,
    repo_root: &Path,
    policy: &CheckPolicy,
    report: &mut CheckReport,
) -> Result<()> {
    if severity_from_level(&policy.requirement_hint_level).is_none() {
        return Ok(());
    }
    // Kinds that count as "implementation" for hint purposes — everything
    // structural that is not a doc / requirement / test / candidate.
    let non_impl = [
        NodeKind::Requirement,
        NodeKind::AcceptanceCriterion,
        NodeKind::Adr,
        NodeKind::DocSection,
        NodeKind::File,
        NodeKind::TestCase,
        NodeKind::TestGroup,
        NodeKind::BusinessCandidate,
    ];
    for req in store.list_nodes_by_kind(NodeKind::Requirement)? {
        let incoming = store.list_edges_to(&req.id)?;
        if incoming
            .iter()
            .any(|e| e.kind == EdgeKind::DeclaresImplementation)
        {
            continue;
        }
        let Some(title) = req.name.as_deref().filter(|t| !t.trim().is_empty()) else {
            continue;
        };
        let mut options = crate::search::SearchOptions::keywords(repo_root, title);
        options.limit = 8;
        let result = match crate::search::run_search_with_store(store, options) {
            Ok(r) => r,
            // Hints are best-effort: a search failure must not fail `check`.
            Err(_) => continue,
        };
        let candidates: Vec<String> = result
            .matches
            .iter()
            .filter(|m| {
                m.score >= crate::search::SCORE_NAME_TOKEN
                    && !non_impl.iter().any(|k| k.as_str() == m.kind)
            })
            .take(3)
            .map(|m| match &m.path {
                Some(p) => format!("{} ({p})", m.label),
                None => m.label.clone(),
            })
            .collect();
        let message = if candidates.is_empty() {
            format!(
                "需求 {} 未声明实现，且图中未找到疑似实现（结构+全文均无匹配）——疑似实现疏漏",
                req.id
            )
        } else {
            format!(
                "需求 {} 未声明实现；图中疑似实现：{}（用 `specslice connect` 确认）",
                req.id,
                candidates.join("、")
            )
        };
        push_configured_finding(
            report,
            &policy.requirement_hint_level,
            "requirement_implementation_hint",
            message,
            Some(req.id.to_string()),
            req.path.clone(),
        );
    }
    Ok(())
}

fn load_config(repo_root: &Path) -> Result<EngineConfig> {
    let path = repo_root.join(DEFAULT_CONFIG_FILE_NAME);
    if !path.exists() {
        anyhow::bail!(
            "no SpecSlice workspace at {}: run `specslice init` first",
            repo_root.display()
        );
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let cfg: EngineConfig = serde_yml::from_str(&contents)
        .with_context(|| format!("parsing config {}", path.display()))?;
    Ok(cfg)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = Path::new(&config.storage.path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    }
}
