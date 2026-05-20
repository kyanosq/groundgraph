//! Code-graph search — `grep` replacement for code that lives in
//! `.specslice/graph.db`.
//!
//! ```text
//! 关键词 / 代码片段 / 文件行号 / symbol id
//!   → 命中图节点
//!   → 扩展相关代码事实边
//!   → 返回可解释子图
//! ```
//!
//! Three input modes (see [`SearchQuery`]):
//!
//! 1. **Keywords** — free-form text, whitespace-tokenised. Score is
//!    deterministic (see [`SCORE_*`](#scoring)). Suitable for a human
//!    operator typing `specslice search "login auth session"`.
//!
//! 2. **Code snippet** — the operator pastes a fragment of code; we
//!    extract identifiers, string literals and PascalCase/camelCase
//!    tokens *without doing any AI / language model work*. The CLI is
//!    deterministic; AI expansion happens in the calling agent.
//!
//! 3. **Position** — `--file <path> --line <n>` resolves to the
//!    enclosing symbol in `symbol_ranges`, then runs subgraph expansion
//!    around that anchor.
//!
//! For every match we emit `match_reasons: Vec<String>` so the calling
//! agent can explain why it found the result. The follow-up
//! `graph_commands` field embeds a `specslice graph --focus …`
//! invocation the agent can hand back to the operator for visual
//! drill-down.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{ArtifactId, EdgeAssertion, Node, NodeKind};
use specslice_store::Store;

use crate::business_candidates::load_business_candidates;
use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};

// ---------------------------------------------------------------------------
// Scoring weights — bucketed so `match_reasons` lines up 1:1 with score
// contributions.
// ---------------------------------------------------------------------------

/// Exact match against the artifact id, e.g.
/// `dart_method::lib/auth/auth_service.dart#AuthService.signIn`.
pub const SCORE_EXACT_ID: i32 = 100;
/// Exact match against the artifact's `name` field (case insensitive).
pub const SCORE_EXACT_NAME: i32 = 80;
/// Token matches a path segment, e.g. `auth` against `lib/auth/…`.
pub const SCORE_PATH_SEGMENT: i32 = 60;
/// Token matches a camelCase / snake_case sub-token of the name.
pub const SCORE_NAME_TOKEN: i32 = 50;
/// Token appears in a test name (`test_case` or `test_group`).
pub const SCORE_TEST_NAME: i32 = 45;
/// Token appears in a candidate's description / rationale / risks.
pub const SCORE_CANDIDATE_TEXT: i32 = 40;
/// Token appears in an edge evidence snippet attached to a hit node.
pub const SCORE_EDGE_EVIDENCE: i32 = 30;
/// Node is a 1-hop neighbour of a directly-matched node.
pub const SCORE_NEIGHBOR: i32 = 20;
/// Weak substring match against id, path or badge text.
pub const SCORE_WEAK_SUBSTRING: i32 = 10;

/// Maximum number of matches returned by default. Operator can lift
/// with `--limit`.
pub const DEFAULT_LIMIT: usize = 25;
/// Default 1-hop subgraph expansion.
pub const DEFAULT_DEPTH: usize = 1;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchQuery {
    Keywords(String),
    Code(String),
    Position { path: String, line: u32 },
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub repo_root: PathBuf,
    pub query: SearchQuery,
    /// Hops to expand from each direct match. `0` means "no expansion,
    /// just return matches"; `1` is the default operator surface.
    pub depth: usize,
    /// When non-empty, only nodes of these kinds participate as direct
    /// matches. Neighbour expansion still uses every kind so the
    /// returned subgraph can show context. Empty means "use the
    /// default kind set".
    pub kinds: Vec<NodeKind>,
    /// Cap on the number of *direct* matches; neighbour nodes are not
    /// counted against this limit.
    pub limit: usize,
    /// When `true`, neighbour expansion keeps framework-noise edges
    /// (toString / build / dispose / …). Off by default to mirror
    /// `specslice graph`.
    pub include_noise: bool,
}

impl SearchOptions {
    pub fn keywords(repo_root: impl Into<PathBuf>, query: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            query: SearchQuery::Keywords(query.into()),
            depth: DEFAULT_DEPTH,
            kinds: Vec::new(),
            limit: DEFAULT_LIMIT,
            include_noise: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub query: String,
    pub tokens: Vec<String>,
    pub matches: Vec<SearchMatch>,
    pub subgraph: SearchSubgraph,
    /// Ready-to-run CLI suggestions the agent can hand back to the
    /// operator — first entry is always a `specslice graph --focus …`
    /// for the top hit so visual drill-down is one paste away.
    pub graph_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
    pub line_range: Option<(u32, u32)>,
    pub score: i32,
    pub source: Option<String>,
    pub match_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchSubgraph {
    pub nodes: Vec<SearchNode>,
    pub edges: Vec<SearchEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
    pub line_range: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub source_file: Option<String>,
    pub line_range: Option<(u32, u32)>,
    pub snippet: Option<String>,
}

// ---------------------------------------------------------------------------
// Default kind set + edge kinds for expansion
// ---------------------------------------------------------------------------

/// Kinds searched when the operator does not pass `--kind`. Mirrors the
/// list in the design doc.
pub fn default_search_kinds() -> Vec<NodeKind> {
    vec![
        NodeKind::File,
        NodeKind::DartClass,
        NodeKind::DartMethod,
        NodeKind::DartFunction,
        NodeKind::DartConstructor,
        NodeKind::TestCase,
        NodeKind::TestGroup,
        NodeKind::Route,
        NodeKind::Storage,
        NodeKind::DartProvider,
        NodeKind::DocSection,
        NodeKind::BusinessCandidate,
    ]
}

/// Edge kinds we follow during subgraph expansion. Keeping this list
/// explicit (and exposed) lets the CLI and tests reason about *which*
/// kinds of relationships will be shown.
pub const EXPANSION_EDGE_KINDS: &[&str] = &[
    "contains",
    "calls",
    "references",
    "reads_provider",
    "persists_to",
    "navigates_to",
    "subscribes_stream",
    "derives_from",
    "declares_implementation",
    "declares_verification",
    "documents",
];

/// Framework-noise calls — same list used by [`crate::graph`]. We
/// inline the constant rather than re-export it from graph.rs to keep
/// the dependency arrow one-way.
const NOISE_TARGETS: &[&str] = &[
    "toString",
    "hashCode",
    "noSuchMethod",
    "runtimeType",
    "copyWith",
    "dispose",
    "initState",
    "build",
    "didChangeDependencies",
    "didUpdateWidget",
    "deactivate",
    "reassemble",
    "createState",
    "createElement",
];

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn run_search(options: SearchOptions) -> Result<SearchResult> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    run_search_with_store(&store, options)
}

pub fn run_search_with_store(store: &Store, mut options: SearchOptions) -> Result<SearchResult> {
    if options.kinds.is_empty() {
        options.kinds = default_search_kinds();
    }
    let limit = options.limit.max(1);
    let kinds_set: HashSet<NodeKind> = options.kinds.iter().copied().collect();

    let (query_text, tokens) = build_tokens(&options.query)?;
    let mut matches = match &options.query {
        SearchQuery::Position { path, line } => position_matches(store, path, *line)?,
        _ => keyword_matches(store, &options.repo_root, &tokens, &kinds_set)?,
    };

    matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    if matches.len() > limit {
        matches.truncate(limit);
    }

    let subgraph = expand_subgraph(store, &matches, options.depth, options.include_noise)?;
    let graph_commands = build_graph_commands(&matches, &options.repo_root);

    Ok(SearchResult {
        query: query_text,
        tokens,
        matches,
        subgraph,
        graph_commands,
    })
}

// ---------------------------------------------------------------------------
// Tokenisation
// ---------------------------------------------------------------------------

/// Returns `(canonical query text, tokens)`. Tokens are lowercase,
/// deduplicated, length ≥ 2 (single characters like `a` would match
/// everything).
fn build_tokens(query: &SearchQuery) -> Result<(String, Vec<String>)> {
    match query {
        SearchQuery::Keywords(raw) => {
            let toks = tokenise_keywords(raw);
            Ok((raw.clone(), toks))
        }
        SearchQuery::Code(raw) => {
            let toks = tokenise_code(raw);
            Ok((raw.clone(), toks))
        }
        SearchQuery::Position { path, line } => {
            // For positional searches we still report a "query" string
            // so JSON consumers have something to display. The tokens
            // are derived from the resolved symbol's name + path
            // segments in [`position_matches`].
            Ok((format!("{path}:{line}"), Vec::new()))
        }
    }
}

/// Split free-form keywords on whitespace + punctuation, lower-case
/// and dedupe.
pub fn tokenise_keywords(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for piece in raw.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let p = piece.trim().to_ascii_lowercase();
        if p.len() < 2 {
            continue;
        }
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

/// Tokenise a code snippet *deterministically*. We pull out
/// identifiers, string literals, type-like (PascalCase) tokens, and
/// path-like segments. No language-server, no AI.
pub fn tokenise_code(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |t: String, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if t.len() < 2 {
            return;
        }
        if seen.insert(t.clone()) {
            out.push(t);
        }
    };
    // String literal contents — keep what's inside the quotes.
    for hit in extract_string_literals(raw) {
        for t in tokenise_keywords(&hit) {
            push(t, &mut out, &mut seen);
        }
    }
    // Identifiers + their camelCase/snake_case sub-tokens.
    for ident in extract_identifiers(raw) {
        push(ident.to_ascii_lowercase(), &mut out, &mut seen);
        for sub in split_identifier(&ident) {
            push(sub.to_ascii_lowercase(), &mut out, &mut seen);
        }
    }
    // Path-like tokens (anything containing a slash).
    for path in raw.split_whitespace() {
        if path.contains('/') {
            for seg in path.split('/') {
                let cleaned: String = seg
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                    .collect();
                if !cleaned.is_empty() {
                    push(cleaned.to_ascii_lowercase(), &mut out, &mut seen);
                }
            }
        }
    }
    out
}

/// Pull out double-quoted and single-quoted contents. Naive — no
/// escape-handling, no triple-quotes — but enough for token extraction
/// from typical Dart / TS snippets.
fn extract_string_literals(raw: &str) -> Vec<String> {
    let mut hits: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    for ch in raw.chars() {
        match quote {
            Some(q) if ch == q => {
                if !buf.is_empty() {
                    hits.push(std::mem::take(&mut buf));
                }
                quote = None;
            }
            Some(_) => buf.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None => {}
        }
    }
    hits
}

/// Pull out identifier-looking tokens (letters/digits/underscore, must
/// start with a letter or underscore).
fn extract_identifiers(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in raw.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if let Some(first) = cur.chars().next() {
                if first.is_alphabetic() || first == '_' {
                    out.push(std::mem::take(&mut cur));
                }
            }
            cur.clear();
        }
    }
    if let Some(first) = cur.chars().next() {
        if first.is_alphabetic() || first == '_' {
            out.push(cur);
        }
    }
    out
}

/// `AuthService` → `["auth", "service"]`; `sign_in_user` →
/// `["sign", "in", "user"]`; `AuthService.signIn` →
/// `["auth", "service", "sign", "in"]`. Lower-cased. Segments shorter
/// than 2 chars are dropped to keep the noise out.
pub fn split_identifier(ident: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in ident.chars() {
        // Treat `_ - .` as hard separators so `AuthService.signIn`,
        // `sign_in`, and `auth-service` all break apart.
        if ch == '_' || ch == '-' || ch == '.' {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower {
            parts.push(std::mem::take(&mut cur));
        }
        cur.push(ch.to_ascii_lowercase());
        prev_lower = ch.is_lowercase();
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts.into_iter().filter(|p| p.len() >= 2).collect()
}

/// Compact, lower-cased forms of the dot-separated segments of an
/// identifier. `AuthService.signIn` →
/// `["authservice", "signin"]`. These cover the common case where the
/// operator types a sub-symbol name with the casing collapsed
/// (`signin`, `applypurchase`).
pub fn compact_segments(ident: &str) -> Vec<String> {
    ident
        .split('.')
        .map(|seg| {
            seg.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Direct match scoring
// ---------------------------------------------------------------------------

fn keyword_matches(
    store: &Store,
    repo_root: &Path,
    tokens: &[String],
    kinds: &HashSet<NodeKind>,
) -> Result<Vec<SearchMatch>> {
    let nodes = store.list_all_nodes()?;

    // Candidates live in `business_logic.yaml`, not in the `nodes`
    // table, so we have to merge them in here. `load_business_candidates`
    // is tolerant of "no workspace" — empty list is fine.
    let candidates = load_business_candidates(repo_root)
        .ok()
        .map(|o| o.document.candidates)
        .unwrap_or_default();
    let candidate_text: BTreeMap<String, String> = candidates
        .iter()
        .map(|c| {
            let mut t = String::new();
            t.push_str(&c.name);
            t.push('\n');
            t.push_str(&c.description);
            t.push('\n');
            for r in &c.risks {
                t.push_str(r);
                t.push('\n');
            }
            if let Some(rec) = &c.recommendation {
                t.push_str(rec);
            }
            (
                crate::business_candidates::candidate_artifact_id(&c.id).to_string(),
                t.to_ascii_lowercase(),
            )
        })
        .collect();

    let mut hits: Vec<SearchMatch> = Vec::new();
    for node in &nodes {
        if !kinds.contains(&node.kind) {
            continue;
        }
        let (score, reasons) = score_node(node, tokens, &candidate_text);
        if score == 0 {
            continue;
        }
        hits.push(make_match(node, score, reasons));
    }

    // Score candidates from YAML against the same tokens so they
    // become first-class search results even though they don't live
    // in `nodes`.
    if kinds.contains(&NodeKind::BusinessCandidate) {
        for c in &candidates {
            let id = crate::business_candidates::candidate_artifact_id(&c.id).to_string();
            let (score, reasons) = score_candidate(c, &id, tokens);
            if score == 0 {
                continue;
            }
            hits.push(SearchMatch {
                id,
                kind: NodeKind::BusinessCandidate.as_str().into(),
                label: c.name.clone(),
                path: None,
                line_range: None,
                score,
                source: Some("business_logic.yaml".into()),
                match_reasons: reasons,
            });
        }
    }

    Ok(hits)
}

fn score_candidate(
    c: &crate::business_candidates::BusinessCandidate,
    id: &str,
    tokens: &[String],
) -> (i32, Vec<String>) {
    let mut score = 0;
    let mut reasons: Vec<String> = Vec::new();
    let id_lower = id.to_ascii_lowercase();
    let name_lower = c.name.to_ascii_lowercase();
    let name_compact = compact_segments(&c.name).join("");
    let name_subtokens = split_identifier(&c.name);
    let mut candidate_text_counted = false;
    let mut blob = String::new();
    blob.push_str(&c.description);
    for r in &c.risks {
        blob.push('\n');
        blob.push_str(r);
    }
    if let Some(rec) = &c.recommendation {
        blob.push('\n');
        blob.push_str(rec);
    }
    let blob_lower = blob.to_ascii_lowercase();

    for tok in tokens {
        if id_lower == *tok {
            score += SCORE_EXACT_ID;
            reasons.push(format!("id exactly matches `{tok}`"));
            continue;
        }
        if name_lower == *tok {
            score += SCORE_EXACT_NAME;
            reasons.push(format!("name exactly matches `{tok}`"));
            continue;
        }
        if name_subtokens.iter().any(|t| t == tok) || name_compact == *tok {
            score += SCORE_NAME_TOKEN;
            reasons.push(format!("name token `{tok}` matches"));
            continue;
        }
        if blob_lower.contains(tok) && !candidate_text_counted {
            score += SCORE_CANDIDATE_TEXT;
            reasons.push(format!("candidate description mentions `{tok}`"));
            candidate_text_counted = true;
            continue;
        }
        if id_lower.contains(tok) || name_lower.contains(tok) {
            score += SCORE_WEAK_SUBSTRING;
            reasons.push(format!("weak substring `{tok}`"));
            continue;
        }
    }
    (score, reasons)
}

fn score_node(
    node: &Node,
    tokens: &[String],
    candidate_text: &BTreeMap<String, String>,
) -> (i32, Vec<String>) {
    let mut score = 0;
    let mut reasons: Vec<String> = Vec::new();
    let id = node.id.as_str();
    let id_lower = id.to_ascii_lowercase();
    let name_lower = node
        .name
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let stable_lower = node
        .stable_key
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let path_lower = node
        .path
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let path_segments: Vec<String> = path_lower
        .split(['/', '\\'])
        .map(|s| s.trim_end_matches(".dart").to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let name_subtokens = node
        .name
        .as_deref()
        .map(split_identifier)
        .unwrap_or_default();
    let name_compacts = node
        .name
        .as_deref()
        .map(compact_segments)
        .unwrap_or_default();
    let mut candidate_text_counted = false;

    for tok in tokens {
        // Exact id (case insensitive).
        if id_lower == *tok {
            score += SCORE_EXACT_ID;
            reasons.push(format!("id exactly matches `{tok}`"));
            continue;
        }
        // Exact name match.
        if name_lower == *tok || stable_lower == *tok {
            score += SCORE_EXACT_NAME;
            reasons.push(format!("name exactly matches `{tok}`"));
            continue;
        }
        // Path segment.
        if path_segments.iter().any(|seg| seg == tok) {
            score += SCORE_PATH_SEGMENT;
            reasons.push(format!("path contains segment `{tok}`"));
            continue;
        }
        // Name camel/snake sub-token OR compact `.`-segment (covers
        // operator typing `signin` for `signIn`).
        if name_subtokens.iter().any(|t| t == tok) || name_compacts.iter().any(|seg| seg == tok) {
            let w = if matches!(node.kind, NodeKind::TestCase | NodeKind::TestGroup) {
                SCORE_TEST_NAME
            } else {
                SCORE_NAME_TOKEN
            };
            score += w;
            reasons.push(format!("name token `{tok}` matches"));
            continue;
        }
        // Candidate description / risks / recommendation.
        if node.kind == NodeKind::BusinessCandidate {
            if let Some(blob) = candidate_text.get(id) {
                if blob.contains(tok) && !candidate_text_counted {
                    score += SCORE_CANDIDATE_TEXT;
                    reasons.push(format!("candidate description mentions `{tok}`"));
                    candidate_text_counted = true;
                    continue;
                }
            }
        }
        // Weak fallback — substring against id / path / name. Covers
        // partial words like `Auth` against `AuthService`.
        if id_lower.contains(tok)
            || path_lower.contains(tok)
            || name_lower.contains(tok)
            || stable_lower.contains(tok)
        {
            score += SCORE_WEAK_SUBSTRING;
            reasons.push(format!("weak substring `{tok}`"));
            continue;
        }
    }
    (score, reasons)
}

fn make_match(node: &Node, score: i32, reasons: Vec<String>) -> SearchMatch {
    let label = node
        .name
        .clone()
        .or_else(|| node.stable_key.clone())
        .unwrap_or_else(|| node.id.to_string());
    let line_range = match (node.start_line, node.end_line) {
        (Some(s), Some(e)) => Some((s, e)),
        _ => None,
    };
    SearchMatch {
        id: node.id.to_string(),
        kind: node.kind.as_str().into(),
        label,
        path: node.path.clone(),
        line_range,
        score,
        source: node.indexer.clone(),
        match_reasons: reasons,
    }
}

// ---------------------------------------------------------------------------
// Position-based search (--file --line)
// ---------------------------------------------------------------------------

fn position_matches(store: &Store, path: &str, line: u32) -> Result<Vec<SearchMatch>> {
    let ranges = store.find_symbols_intersecting(path, line, line)?;
    let mut by_symbol: BTreeMap<String, _> = BTreeMap::new();
    for r in ranges {
        by_symbol.insert(r.symbol_id.to_string(), r);
    }
    // Pick the most-specific (smallest line span) symbol covering the
    // line — that's almost always the method the operator cared about.
    let chosen = by_symbol
        .values()
        .min_by_key(|r| r.end_line.saturating_sub(r.start_line));
    let mut hits: Vec<SearchMatch> = Vec::new();
    if let Some(r) = chosen {
        // Resolve to a node so we can carry name / kind in the output.
        if let Some(node) = store.find_node(&r.symbol_id)? {
            hits.push(make_match(
                &node,
                SCORE_EXACT_ID,
                vec![format!("symbol at {path}:{line}")],
            ));
        } else {
            // Range exists but the node row vanished — surface a thin
            // match so the operator at least sees the symbol id and
            // can hand it to `graph --focus`.
            hits.push(SearchMatch {
                id: r.symbol_id.to_string(),
                kind: r.symbol_kind.as_str().into(),
                label: r.qualified_name.clone(),
                path: Some(r.file_path.clone()),
                line_range: Some((r.start_line, r.end_line)),
                score: SCORE_EXACT_ID,
                source: None,
                match_reasons: vec![format!("symbol at {path}:{line}")],
            });
        }
    }
    Ok(hits)
}

// ---------------------------------------------------------------------------
// Subgraph expansion
// ---------------------------------------------------------------------------

fn expand_subgraph(
    store: &Store,
    matches: &[SearchMatch],
    depth: usize,
    include_noise: bool,
) -> Result<SearchSubgraph> {
    let mut node_ids: BTreeSet<String> = matches.iter().map(|m| m.id.clone()).collect();
    let mut frontier: Vec<String> = node_ids.iter().cloned().collect();
    let mut kept_edges: Vec<EdgeAssertion> = Vec::new();

    let allow_kind: HashSet<&str> = EXPANSION_EDGE_KINDS.iter().copied().collect();

    for _ in 0..depth {
        let mut next: BTreeSet<String> = BTreeSet::new();
        for id in &frontier {
            let aid = ArtifactId::new(id.clone());
            for edge in store.list_edges_from(&aid)? {
                if !allow_kind.contains(edge.kind.as_str()) {
                    continue;
                }
                if !include_noise && is_noise_edge(&edge) {
                    continue;
                }
                if !node_ids.contains(edge.to_id.as_str()) {
                    next.insert(edge.to_id.to_string());
                }
                kept_edges.push(edge);
            }
            for edge in store.list_edges_to(&aid)? {
                if !allow_kind.contains(edge.kind.as_str()) {
                    continue;
                }
                if !include_noise && is_noise_edge(&edge) {
                    continue;
                }
                if !node_ids.contains(edge.from_id.as_str()) {
                    next.insert(edge.from_id.to_string());
                }
                kept_edges.push(edge);
            }
        }
        if next.is_empty() {
            break;
        }
        for id in &next {
            node_ids.insert(id.clone());
        }
        frontier = next.into_iter().collect();
    }

    // Materialise nodes for every id we kept (matches + frontier).
    let mut subgraph_nodes: Vec<SearchNode> = Vec::new();
    for id in &node_ids {
        let aid = ArtifactId::new(id.clone());
        if let Some(node) = store.find_node(&aid)? {
            subgraph_nodes.push(SearchNode {
                id: node.id.to_string(),
                kind: node.kind.as_str().into(),
                label: node
                    .name
                    .clone()
                    .or_else(|| node.stable_key.clone())
                    .unwrap_or_else(|| node.id.to_string()),
                path: node.path.clone(),
                line_range: match (node.start_line, node.end_line) {
                    (Some(s), Some(e)) => Some((s, e)),
                    _ => None,
                },
            });
        }
    }
    subgraph_nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let mut subgraph_edges: Vec<SearchEdge> = Vec::new();
    let mut seen_edges: HashSet<String> = HashSet::new();
    for e in kept_edges {
        if !seen_edges.insert(e.id.to_string()) {
            continue;
        }
        let (line_range, snippet) = parse_evidence(e.evidence_json.as_deref());
        subgraph_edges.push(SearchEdge {
            id: e.id.to_string(),
            from: e.from_id.to_string(),
            to: e.to_id.to_string(),
            kind: e.kind.as_str().into(),
            source_file: e.source_file.clone(),
            line_range,
            snippet,
        });
    }
    subgraph_edges.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(SearchSubgraph {
        nodes: subgraph_nodes,
        edges: subgraph_edges,
    })
}

fn is_noise_edge(edge: &EdgeAssertion) -> bool {
    if edge.kind.as_str() != "calls" {
        return false;
    }
    let target_name = target_method_name(edge.to_id.as_str());
    NOISE_TARGETS.contains(&target_name.as_str())
}

fn target_method_name(id: &str) -> String {
    let Some((_, tail)) = id.split_once('#') else {
        return String::new();
    };
    match tail.rsplit_once('.') {
        Some((_, method)) => method.into(),
        None => tail.into(),
    }
}

fn parse_evidence(raw: Option<&str>) -> (Option<(u32, u32)>, Option<String>) {
    let Some(json) = raw else {
        return (None, None);
    };
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let line = v
        .get("line")
        .and_then(|n| n.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    let line_range = line.map(|l| (l, l));
    let snippet = v
        .get("snippet")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    (line_range, snippet)
}

// ---------------------------------------------------------------------------
// `graph_commands` — follow-up CLI suggestions
// ---------------------------------------------------------------------------

fn build_graph_commands(matches: &[SearchMatch], repo_root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(top) = matches.first() {
        out.push(format!(
            "specslice --repo-root {} graph --view focus --focus {} --format html",
            shell_quote(&repo_root.to_string_lossy()),
            shell_quote(&top.id)
        ));
    }
    out
}

fn shell_quote(raw: &str) -> String {
    if raw.is_empty() {
        return "''".into();
    }
    if raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '@' | '+'))
    {
        return raw.into();
    }
    format!("'{}'", raw.replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------
// Storage path helper (mirrors slice/impact/logic_confidence)
// ---------------------------------------------------------------------------

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
    let cfg: EngineConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("parsing config {}", path.display()))?;
    Ok(cfg)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = config.storage.path.clone();
    if raw.is_empty() {
        repo_root.join(".specslice/graph.db")
    } else {
        let candidate = PathBuf::from(&raw);
        if candidate.is_absolute() {
            candidate
        } else {
            repo_root.join(candidate)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus};
    use specslice_store::Store;

    fn empty_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn insert_method(store: &mut Store, file: &str, qualified: &str, line: (u32, u32)) -> String {
        let id = format!("dart_method::{file}#{qualified}");
        let node = Node {
            id: ArtifactId::new(id.clone()),
            kind: NodeKind::DartMethod,
            path: Some(file.into()),
            name: Some(qualified.into()),
            start_line: Some(line.0),
            end_line: Some(line.1),
            content_hash: None,
            stable_key: None,
            source_file: Some(file.into()),
            source_hash: None,
            indexer: Some("dart_analyzer".into()),
            index_generation: None,
            metadata_json: None,
        };
        store.upsert_node(&node).unwrap();
        store
            .upsert_symbol_range(&specslice_core::SymbolRange {
                file_path: file.into(),
                symbol_id: ArtifactId::new(id.clone()),
                start_line: line.0,
                end_line: line.1,
                symbol_kind: NodeKind::DartMethod,
                qualified_name: qualified.into(),
                parent_symbol_id: None,
            })
            .unwrap();
        id
    }

    fn insert_calls_edge(store: &mut Store, from: &str, to: &str) {
        let edge = EdgeAssertion {
            id: ArtifactId::new(format!("calls::{from}->{to}")),
            from_id: ArtifactId::new(from.to_string()),
            to_id: ArtifactId::new(to.to_string()),
            kind: EdgeKind::Calls,
            source: EdgeSource::LanguageAdapter,
            certainty: EdgeCertainty::Fact,
            status: EdgeStatus::Confirmed,
            confidence: 1.0,
            evidence_json: None,
            source_file: None,
            source_hash: None,
            indexer: Some("dart_analyzer".into()),
            index_generation: None,
            metadata_json: None,
        };
        store.upsert_edge(&edge).unwrap();
    }

    #[test]
    fn tokenise_keywords_splits_on_whitespace_and_punctuation_lowercase() {
        let toks = tokenise_keywords("Login Auth, session/token");
        assert_eq!(toks, vec!["login", "auth", "session", "token"]);
    }

    #[test]
    fn tokenise_keywords_drops_single_char_noise() {
        let toks = tokenise_keywords("a IAP b iap");
        assert_eq!(toks, vec!["iap"]);
    }

    #[test]
    fn tokenise_code_extracts_idents_strings_and_path_segments() {
        let snippet =
            r#"authService.signIn("user@example.com", "secret"); // lib/auth/auth_service.dart"#;
        let toks = tokenise_code(snippet);
        assert!(toks.contains(&"authservice".to_string()));
        assert!(toks.contains(&"signin".to_string()));
        // sub-tokens of camelCase
        assert!(toks.contains(&"auth".to_string()));
        assert!(toks.contains(&"service".to_string()));
        // string literal contents
        assert!(toks.contains(&"user".to_string()));
        assert!(toks.contains(&"example".to_string()));
        // path-like
        assert!(toks.contains(&"auth_service".to_string()));
    }

    #[test]
    fn split_identifier_handles_camel_snake_and_pascal() {
        assert_eq!(split_identifier("AuthService"), vec!["auth", "service"]);
        assert_eq!(split_identifier("sign_in_user"), vec!["sign", "in", "user"]);
        assert_eq!(split_identifier("signIn"), vec!["sign", "in"]);
        // Dot-separated qualified names split too.
        assert_eq!(
            split_identifier("AuthService.signIn"),
            vec!["auth", "service", "sign", "in"]
        );
    }

    #[test]
    fn compact_segments_returns_dot_separated_lowercase_pieces() {
        assert_eq!(
            compact_segments("AuthService.signIn"),
            vec!["authservice", "signin"]
        );
        assert_eq!(compact_segments("Foo"), vec!["foo"]);
        assert_eq!(
            compact_segments("ProNotifier.applyPurchase"),
            vec!["pronotifier", "applypurchase"]
        );
    }

    #[test]
    fn keyword_search_exact_id_beats_name_beats_path_segment() {
        let (mut store, _dir) = empty_store();
        let exact_id = insert_method(
            &mut store,
            "lib/auth/auth_service.dart",
            "AuthService.signIn",
            (10, 20),
        );
        let _other = insert_method(
            &mut store,
            "lib/login/login.dart",
            "LoginController.handle",
            (1, 5),
        );

        let kinds: HashSet<_> = default_search_kinds().into_iter().collect();
        let tokens = vec![exact_id.to_ascii_lowercase()];
        let mut hits = keyword_matches(&store, Path::new("."), &tokens, &kinds).unwrap();
        hits.sort_by(|a, b| b.score.cmp(&a.score));
        assert_eq!(hits[0].id, exact_id);
        assert!(
            hits[0].match_reasons.iter().any(|r| r.starts_with("id ")),
            "top reason should be id-match, got {:?}",
            hits[0].match_reasons
        );
        assert!(hits[0].score >= SCORE_EXACT_ID);
    }

    #[test]
    fn keyword_search_matches_name_and_path_segments_with_match_reasons() {
        let (mut store, _dir) = empty_store();
        insert_method(
            &mut store,
            "lib/auth/auth_service.dart",
            "AuthService.signIn",
            (10, 20),
        );
        insert_method(&mut store, "lib/other.dart", "OtherThing.run", (1, 4));

        let kinds: HashSet<_> = default_search_kinds().into_iter().collect();
        let tokens = vec!["auth".into(), "signin".into()];
        let mut hits = keyword_matches(&store, Path::new("."), &tokens, &kinds).unwrap();
        hits.sort_by(|a, b| b.score.cmp(&a.score));
        let top = &hits[0];
        assert!(top.id.contains("AuthService.signIn"));
        // Two reasons expected: path segment + name token.
        assert!(
            top.match_reasons
                .iter()
                .any(|r| r.contains("path contains segment `auth`")),
            "reasons: {:?}",
            top.match_reasons
        );
        assert!(
            top.match_reasons.iter().any(|r| r.contains("name token")),
            "reasons: {:?}",
            top.match_reasons
        );
    }

    #[test]
    fn run_search_returns_subgraph_neighbours_at_depth_1() {
        let (mut store, dir) = empty_store();
        // Set up a workspace so run_search_with_store has somewhere
        // sane to find candidates (not strictly required since
        // load_business_candidates falls back gracefully).
        std::fs::write(
            dir.path().join(".specslice.yaml"),
            "schema_version: 1\nstorage:\n  backend: sqlite\n  path: \".specslice/graph.db\"\n",
        )
        .unwrap();

        let a = insert_method(&mut store, "lib/a.dart", "LoginCtl.handle", (1, 5));
        let b = insert_method(&mut store, "lib/b.dart", "AuthService.signIn", (10, 20));
        insert_calls_edge(&mut store, &a, &b);

        let opts = SearchOptions {
            repo_root: dir.path().into(),
            query: SearchQuery::Keywords("LoginCtl".into()),
            depth: 1,
            kinds: Vec::new(),
            limit: 10,
            include_noise: false,
        };
        let result = run_search_with_store(&store, opts).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].id, a);
        // Subgraph: LoginCtl + AuthService + the calls edge between them.
        let ids: BTreeSet<&str> = result
            .subgraph
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert!(ids.contains(a.as_str()));
        assert!(
            ids.contains(b.as_str()),
            "1-hop expansion must pull in callee {b}, got {ids:?}"
        );
        assert!(
            result.subgraph.edges.iter().any(|e| e.kind == "calls"),
            "calls edge missing from subgraph"
        );
        // graph_commands seeded with focus on the top match.
        assert_eq!(result.graph_commands.len(), 1);
        assert!(
            result.graph_commands[0].contains(a.as_str()),
            "graph_commands should focus on top match, got {:?}",
            result.graph_commands
        );
        assert!(
            result.graph_commands[0].contains("--repo-root"),
            "graph_commands must be paste-ready outside the target repo: {:?}",
            result.graph_commands
        );
        assert!(
            result.graph_commands[0].contains(&dir.path().display().to_string()),
            "graph_commands must include the searched repo root: {:?}",
            result.graph_commands
        );
    }

    #[test]
    fn code_search_prefers_direct_code_symbols_over_candidate_text_mentions() {
        let (mut store, dir) = empty_store();
        std::fs::create_dir_all(dir.path().join(".specslice/candidates")).unwrap();
        std::fs::write(
            dir.path().join(".specslice/candidates/business_logic.yaml"),
            r#"
schema_version: 1
candidates:
  - id: complete_purchase_unlocks_pro
    name: "Completes an in-app purchase and unlocks Pro for the user"
    description: |
      proNotifier applyPurchase productId pro_entitlement entitled hive box
      pro notifier apply purchase product id entitlement true
    evidence:
      - dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase
    confidence: 0.72
    status: proposed
"#,
        )
        .unwrap();
        let method = insert_method(
            &mut store,
            "lib/core/settings/pro_provider.dart",
            "ProNotifier.applyPurchase",
            (7, 11),
        );

        let opts = SearchOptions {
            repo_root: dir.path().into(),
            query: SearchQuery::Code(
                r#"proNotifier.applyPurchase(productId); Hive.box("pro_entitlement").put("entitled", true);"#
                    .into(),
            ),
            depth: 0,
            kinds: Vec::new(),
            limit: 10,
            include_noise: false,
        };

        let result = run_search_with_store(&store, opts).unwrap();
        let top = result.matches.first().expect("search must return hits");
        assert_eq!(
            top.id,
            method,
            "direct code-symbol matches must outrank candidate prose matches: {:?}",
            result
                .matches
                .iter()
                .map(|m| (&m.id, m.score, &m.match_reasons))
                .collect::<Vec<_>>()
        );
        let candidate = result
            .matches
            .iter()
            .find(|m| m.id == "business_candidate::complete_purchase_unlocks_pro")
            .expect("candidate should still be searchable");
        assert!(
            top.score > candidate.score,
            "candidate text should not swamp a direct code symbol hit"
        );
    }

    #[test]
    fn noise_calls_are_dropped_from_subgraph_by_default() {
        let (mut store, dir) = empty_store();
        let a = insert_method(&mut store, "lib/a.dart", "EditorView.build", (1, 5));
        let b = insert_method(&mut store, "lib/b.dart", "RenderObject.build", (10, 20));
        insert_calls_edge(&mut store, &a, &b);

        let opts = SearchOptions {
            repo_root: dir.path().into(),
            query: SearchQuery::Keywords("EditorView".into()),
            depth: 1,
            kinds: Vec::new(),
            limit: 10,
            include_noise: false,
        };
        let result = run_search_with_store(&store, opts).unwrap();
        assert!(
            result
                .subgraph
                .edges
                .iter()
                .all(|e| !(e.kind == "calls" && e.to.ends_with(".build"))),
            "noise `.build` calls must be filtered from subgraph by default; got {:?}",
            result.subgraph.edges
        );
    }

    #[test]
    fn position_query_resolves_to_enclosing_symbol() {
        let (mut store, dir) = empty_store();
        let target = insert_method(
            &mut store,
            "lib/auth/auth_service.dart",
            "AuthService.signIn",
            (10, 50),
        );
        // A sibling symbol on a different range so we know we picked
        // the most-specific one.
        insert_method(
            &mut store,
            "lib/auth/auth_service.dart",
            "AuthService",
            (1, 200),
        );

        let opts = SearchOptions {
            repo_root: dir.path().into(),
            query: SearchQuery::Position {
                path: "lib/auth/auth_service.dart".into(),
                line: 25,
            },
            depth: 0,
            kinds: Vec::new(),
            limit: 10,
            include_noise: false,
        };
        let result = run_search_with_store(&store, opts).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].id, target);
        assert!(
            result.matches[0]
                .match_reasons
                .iter()
                .any(|r| r.starts_with("symbol at")),
            "reason should call out the source location, got {:?}",
            result.matches[0].match_reasons
        );
    }
}
