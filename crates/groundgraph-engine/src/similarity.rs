//! P18 — structural duplicate detection (MVP, tier 1).
//!
//! The goal is to surface *structural* code duplicates as candidate
//! review items, not to auto-rewrite anything. Two functions /
//! methods are reported as duplicates of each other when, after
//! stripping identifiers, literals, comments, docstrings and
//! whitespace, their normalized token streams hash to the same
//! 64-bit fingerprint. This catches the "I copy-pasted handler X
//! and renamed a couple of fields" scenario that grep cannot see
//! and that ad-hoc reviews routinely miss.
//!
//! Out of scope for this iteration (deferred to later passes):
//!
//! - **Tier 2 (near-duplicate, ~70-95% similar):** SimHash /
//!   MinHash over token shingles. The fingerprint computed here
//!   can later be split into shingles to feed a SimHash without
//!   recomputing the lexer pass.
//! - **Tier 3 (behavior duplicate):** comparing call / route /
//!   storage neighborhoods in the graph.
//!
//! ### Language support
//!
//! - **Python** (`python_function`, `python_method`): full lexer
//!   pass with docstring stripping and operator preservation.
//! - **C-family** — Dart, Rust, Go, Swift, TypeScript, Java, C and
//!   C++ all share the same normalizer with `//` and `/* */` comment
//!   handling (instead of Python's `#`). Each contributes its own
//!   structural-keyword set so control-flow / declaration shape is
//!   preserved while identifiers collapse to `ID`.
//!
//! Every language whose function/method nodes are emitted by an
//! indexer is mapped in [`node_language`]; adding a new one is a
//! `Language` arm plus a keyword set — the lexer is shared.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_core::NodeKind;
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

/// Schema version emitted alongside [`SimilarityReport`] so future
/// consumers can refuse to deserialize incompatible payloads
/// without guessing.
pub const SIMILARITY_SCHEMA_VERSION: u32 = 1;

/// Default lower bound on a function body in normalized tokens.
/// Sub-six-token bodies are usually `return None` / `pass` and
/// would dominate any duplicate report with trivial hits.
pub const DEFAULT_MIN_TOKENS: usize = 12;

/// Default lower bound on the SimHash-derived similarity score
/// for tier 2 reporting. 0.85 ≈ hamming distance ≤ 9 / 64 bits.
pub const DEFAULT_MIN_SIMILARITY: f32 = 0.85;

/// Default shingle width when generating SimHash. Five tokens has
/// the property that small renames (1–2 tokens) move just one or
/// two shingles, so the SimHash drifts by a handful of bits — not
/// dozens.
pub const DEFAULT_SHINGLE_K: usize = 5;

/// Pairwise SimHash comparison is O(N²); above this many uncovered
/// symbols we skip tier 2 with a warning rather than risk a
/// multi-minute run on a huge repo. LSH-based bucketing is left
/// to a future iteration.
pub const DEFAULT_MAX_PAIRWISE_SYMBOLS: usize = 20_000;

/// Languages the normalizer currently understands. New entries
/// MUST keep the existing token grammar so old fingerprints stay
/// comparable across versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Python,
    Dart,
    Rust,
    Go,
    Swift,
    TypeScript,
    Java,
    C,
    Cpp,
}

/// Which duplicate tiers to run. `All` is the default in the CLI;
/// `Exact` matches the original P18 tier 1 behaviour and is what
/// the existing tests assume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimilarityMode {
    Exact,
    Near,
    All,
}

impl SimilarityMode {
    pub fn runs_exact(self) -> bool {
        matches!(self, SimilarityMode::Exact | SimilarityMode::All)
    }
    pub fn runs_near(self) -> bool {
        matches!(self, SimilarityMode::Near | SimilarityMode::All)
    }
}

#[derive(Debug, Clone)]
pub struct SimilarityOptions {
    pub repo_root: PathBuf,
    /// Lower bound on the size of a body (in normalized tokens).
    pub min_tokens: usize,
    /// Minimum number of distinct symbols sharing a fingerprint to
    /// report it. Defaults to 2 (any duplicate).
    pub min_cluster_size: usize,
    /// When `Some`, only clusters that contain this symbol id are
    /// returned. Powers `groundgraph similar --node SYMBOL_ID`.
    pub focus_symbol_id: Option<String>,
    /// Tier(s) to run.
    pub mode: SimilarityMode,
    /// Lower bound on tier-2 similarity score in `[0, 1]`.
    pub min_similarity: f32,
    /// Shingle width for SimHash. Smaller k = more sensitive to
    /// small renames; larger k = more sensitive to structural
    /// drift. 5 is a long-established sweet spot for code.
    pub shingle_k: usize,
    /// Safety guard against runaway O(N²) on huge repos.
    pub max_pairwise_symbols: usize,
}

impl Default for SimilarityOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            min_tokens: DEFAULT_MIN_TOKENS,
            min_cluster_size: 2,
            focus_symbol_id: None,
            mode: SimilarityMode::All,
            min_similarity: DEFAULT_MIN_SIMILARITY,
            shingle_k: DEFAULT_SHINGLE_K,
            max_pairwise_symbols: DEFAULT_MAX_PAIRWISE_SYMBOLS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimilarityReport {
    pub schema_version: u32,
    pub stats: SimilarityStats,
    pub clusters: Vec<SimilarityCluster>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SimilarityStats {
    /// How many symbols the normalizer actually fingerprinted.
    pub symbols_scanned: usize,
    /// How many symbols were skipped because the body was below
    /// `min_tokens` or the source file could not be read.
    pub symbols_skipped: usize,
    /// Number of clusters returned after filtering.
    pub clusters_reported: usize,
    /// Subset of `clusters_reported` that came from tier 1 exact
    /// AST matching. Always 0 when `mode = Near`.
    pub exact_clusters: usize,
    /// Subset of `clusters_reported` that came from tier 2 SimHash
    /// near-duplicate matching. Always 0 when `mode = Exact`.
    pub near_clusters: usize,
    /// `true` when the near-duplicate pass was skipped because
    /// `uncovered_symbols > max_pairwise_symbols`. Operators
    /// should re-run with a tighter `--code-roots` or raise the
    /// guard explicitly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub near_pairwise_skipped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimilarityCluster {
    /// Hex form of the structural fingerprint shared by every
    /// member of the cluster. Useful as a stable cluster id.
    pub fingerprint: String,
    /// Duplicate kind. Tier 1 emits `"exact_ast"`; tier 2 emits
    /// `"near_token"`. tier 3 (graph behavior) is reserved.
    pub duplicate_type: String,
    pub members: Vec<SimilarityMember>,
    /// Token count of the cluster. For `exact_ast` every member
    /// shares the same value by construction; for `near_token` it
    /// is the median across members (so a single rogue tiny body
    /// cannot drag it to zero).
    pub normalized_token_count: usize,
    /// Conservative recommendation surfaced to humans / AI. Both
    /// tiers always say `"review"` — never auto-merge.
    pub recommendation: String,
    /// Tier-2 only: the conservative lower-bound similarity in
    /// `[0, 1]` (minimum pairwise score among cluster members).
    /// `None` for tier-1 clusters because those are always 1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_score: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimilarityMember {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: String,
    pub line_range: Option<(u32, u32)>,
}

/// Convenience entry point — opens the workspace store and runs
/// [`analyze_similarity_with_store`].
pub fn analyze_similarity(options: SimilarityOptions) -> Result<SimilarityReport> {
    let db_path = crate::config::storage_path_for_repo(&options.repo_root)?;
    let store = Store::open(&db_path).with_context(|| {
        format!(
            "opening graph store at {} for similarity report",
            db_path.display()
        )
    })?;
    analyze_similarity_with_store(&store, options)
}

/// Walk every Python / Dart function-like node in the store, read
/// its source range, normalize → hash, then either bucket by
/// exact fingerprint (tier 1) or pairwise-compare SimHashes
/// (tier 2). Results are sorted so the largest, most confident
/// clusters surface first.
pub fn analyze_similarity_with_store(
    store: &Store,
    options: SimilarityOptions,
) -> Result<SimilarityReport> {
    let nodes = store.list_all_nodes().context("listing nodes")?;
    let repo_root = options.repo_root.clone();
    let shingle_k = options.shingle_k.max(1);

    struct Scanned {
        member: SimilarityMember,
        token_count: usize,
        exact_fp: u64,
        simhash: u64,
    }
    let mut scanned: Vec<Scanned> = Vec::new();
    let mut skipped = 0usize;

    for node in &nodes {
        let Some(language) = node_language(node.kind) else {
            continue;
        };
        let Some(path_rel) = node.path.as_deref() else {
            skipped += 1;
            continue;
        };
        let (Some(start), Some(end)) = (node.start_line, node.end_line) else {
            skipped += 1;
            continue;
        };
        if end < start {
            skipped += 1;
            continue;
        }

        let abs = repo_root.join(path_rel);
        // Skip a file that has grown past the index byte budget rather than
        // slurp it whole just to fingerprint one span (#245).
        if crate::source_text::is_oversized_source(&abs) {
            skipped += 1;
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&abs) else {
            skipped += 1;
            continue;
        };
        let Some(body) = extract_lines(&source, start, end) else {
            skipped += 1;
            continue;
        };

        let tokens = normalize(language, &body);
        if tokens.len() < options.min_tokens {
            skipped += 1;
            continue;
        }
        let exact_fp = fingerprint_tokens(&tokens);
        let simhash = simhash_tokens(&tokens, shingle_k);
        let member = SimilarityMember {
            id: node.id.to_string(),
            kind: node.kind.as_str().into(),
            label: node
                .name
                .clone()
                .unwrap_or_else(|| node.stable_key.clone().unwrap_or_default()),
            path: path_rel.to_string(),
            line_range: Some((start, end)),
        };
        scanned.push(Scanned {
            member,
            token_count: tokens.len(),
            exact_fp,
            simhash,
        });
    }

    // ---- tier 1: exact AST clusters --------------------------
    let mut exact_clusters: Vec<SimilarityCluster> = Vec::new();
    let mut covered: HashSet<usize> = HashSet::new();
    if options.mode.runs_exact() {
        let mut buckets: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (idx, s) in scanned.iter().enumerate() {
            buckets.entry(s.exact_fp).or_default().push(idx);
        }
        for (fp, indices) in buckets {
            if indices.len() < options.min_cluster_size {
                continue;
            }
            for &i in &indices {
                covered.insert(i);
            }
            let token_count = scanned[indices[0]].token_count;
            let mut members: Vec<SimilarityMember> =
                indices.iter().map(|&i| scanned[i].member.clone()).collect();
            members.sort_by(|a, b| a.path.cmp(&b.path).then(a.label.cmp(&b.label)));
            exact_clusters.push(SimilarityCluster {
                fingerprint: format!("{fp:016x}"),
                duplicate_type: "exact_ast".into(),
                members,
                normalized_token_count: token_count,
                recommendation: "review".into(),
                similarity_score: None,
            });
        }
    }

    // ---- tier 2: SimHash near-duplicate pairs ----------------
    let mut near_clusters: Vec<SimilarityCluster> = Vec::new();
    let mut near_skipped = false;
    if options.mode.runs_near() {
        // Only consider symbols NOT already in an exact cluster;
        // tier 1 always wins when both fire.
        let candidates: Vec<usize> = (0..scanned.len())
            .filter(|i| !covered.contains(i))
            .collect();
        if candidates.len() > options.max_pairwise_symbols {
            near_skipped = true;
        } else {
            // Hamming threshold corresponding to `min_similarity`.
            // similarity = 1 - h/64 => h = (1 - similarity) * 64.
            // Round UP so the boundary is permissive. Clamp to
            // [0, 64] BEFORE casting so the value is provably
            // in-range; clippy's pessimistic lint can't see that,
            // so we suppress the cast-sign-loss/truncation lints
            // for this one expression.
            let max_hamming = {
                let raw = ((1.0 - options.min_similarity).max(0.0)) * 64.0;
                let clamped = raw.ceil().clamp(0.0, 64.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    clamped as u32
                }
            };
            let n = candidates.len();
            let mut uf = UnionFind::new(n);
            for i in 0..n {
                for j in (i + 1)..n {
                    let a = scanned[candidates[i]].simhash;
                    let b = scanned[candidates[j]].simhash;
                    let h = (a ^ b).count_ones();
                    if h <= max_hamming {
                        uf.union(i, j);
                    }
                }
            }
            // Group by root → cluster.
            let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            for idx in 0..n {
                let root = uf.find(idx);
                groups.entry(root).or_default().push(idx);
            }
            for indices in groups.into_values() {
                if indices.len() < options.min_cluster_size {
                    continue;
                }
                // Worst-case similarity inside the cluster. Union-find
                // merges transitively (A~B, B~C ⇒ {A,B,C}), so the worst
                // pair may never have passed the hamming gate — recompute
                // every pair instead of trusting discovered links
                // (issues.md #12).
                let mut min_sim: f32 = 1.0;
                for a in 0..indices.len() {
                    for b in (a + 1)..indices.len() {
                        let ha = scanned[candidates[indices[a]]].simhash;
                        let hb = scanned[candidates[indices[b]]].simhash;
                        let h = (ha ^ hb).count_ones();
                        let sim = 1.0 - (h as f32 / 64.0);
                        if sim < min_sim {
                            min_sim = sim;
                        }
                    }
                }
                let mut token_counts: Vec<usize> = indices
                    .iter()
                    .map(|i| scanned[candidates[*i]].token_count)
                    .collect();
                token_counts.sort_unstable();
                let median = token_counts[token_counts.len() / 2];
                let mut members: Vec<SimilarityMember> = indices
                    .iter()
                    .map(|i| scanned[candidates[*i]].member.clone())
                    .collect();
                members.sort_by(|a, b| a.path.cmp(&b.path).then(a.label.cmp(&b.label)));
                let canonical_fp = scanned[candidates[indices[0]]].simhash;
                near_clusters.push(SimilarityCluster {
                    fingerprint: format!("{canonical_fp:016x}"),
                    duplicate_type: "near_token".into(),
                    members,
                    normalized_token_count: median,
                    recommendation: "review".into(),
                    similarity_score: Some(min_sim),
                });
            }
        }
    }

    let mut clusters: Vec<SimilarityCluster> =
        exact_clusters.into_iter().chain(near_clusters).collect();

    if let Some(focus) = options.focus_symbol_id.as_deref() {
        clusters.retain(|c| c.members.iter().any(|m| m.id == focus));
    }

    clusters.sort_by(|a, b| {
        // Exact clusters first (more confident), then by size.
        let dt = duplicate_type_priority(a).cmp(&duplicate_type_priority(b));
        if dt != std::cmp::Ordering::Equal {
            return dt;
        }
        b.normalized_token_count
            .cmp(&a.normalized_token_count)
            .then_with(|| b.members.len().cmp(&a.members.len()))
            .then_with(|| {
                // similarity_score: prefer higher.
                let sa = a.similarity_score.unwrap_or(1.0);
                let sb = b.similarity_score.unwrap_or(1.0);
                sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });

    let exact_count = clusters
        .iter()
        .filter(|c| c.duplicate_type == "exact_ast")
        .count();
    let near_count = clusters
        .iter()
        .filter(|c| c.duplicate_type == "near_token")
        .count();
    let total = clusters.len();
    Ok(SimilarityReport {
        schema_version: SIMILARITY_SCHEMA_VERSION,
        stats: SimilarityStats {
            symbols_scanned: scanned.len(),
            symbols_skipped: skipped,
            clusters_reported: total,
            exact_clusters: exact_count,
            near_clusters: near_count,
            near_pairwise_skipped: near_skipped,
        },
        clusters,
    })
}

fn duplicate_type_priority(cluster: &SimilarityCluster) -> u8 {
    match cluster.duplicate_type.as_str() {
        "exact_ast" => 0,
        "near_token" => 1,
        _ => 2,
    }
}

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }
    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                self.parent[rb] = ra;
                self.rank[ra] += 1;
            }
        }
    }
}

fn node_language(kind: NodeKind) -> Option<Language> {
    match kind {
        NodeKind::PythonFunction | NodeKind::PythonMethod => Some(Language::Python),
        NodeKind::DartFunction | NodeKind::DartMethod | NodeKind::DartConstructor => {
            Some(Language::Dart)
        }
        NodeKind::RustFunction | NodeKind::RustMethod => Some(Language::Rust),
        NodeKind::GoFunction | NodeKind::GoMethod => Some(Language::Go),
        NodeKind::SwiftFunction | NodeKind::SwiftMethod | NodeKind::SwiftInitializer => {
            Some(Language::Swift)
        }
        NodeKind::TypescriptFunction | NodeKind::TypescriptMethod => Some(Language::TypeScript),
        NodeKind::JavaMethod | NodeKind::JavaConstructor => Some(Language::Java),
        NodeKind::CFunction => Some(Language::C),
        NodeKind::CppFunction | NodeKind::CppMethod => Some(Language::Cpp),
        _ => None,
    }
}

fn extract_lines(source: &str, start_line: u32, end_line: u32) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let start = (start_line as usize).saturating_sub(1);
    let end = (end_line as usize).min(lines.len());
    if start >= end {
        return None;
    }
    Some(lines[start..end].join("\n"))
}

/// Tokenize and normalize. Identifiers collapse to `ID`, numeric
/// literals to `NUM`, string literals to `STR`. Comments (per
/// language), Python docstrings, and whitespace are dropped. The
/// returned vector is stable in order so callers can hash it and
/// also feed it to a future SimHash without re-tokenizing.
pub fn normalize(language: Language, source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut chars = source.chars().peekable();
    let mut in_python_docstring: Option<&'static str> = None;
    while let Some(c) = chars.peek().copied() {
        // Inside an open Python triple-quoted docstring: skip
        // until the closing triple.
        if let Some(closer) = in_python_docstring {
            chars.next();
            if c == closer.chars().next().unwrap() {
                if let Some(next1) = chars.peek().copied() {
                    if next1 == c {
                        chars.next();
                        if let Some(next2) = chars.peek().copied() {
                            if next2 == c {
                                chars.next();
                                in_python_docstring = None;
                            }
                        }
                    }
                }
            }
            continue;
        }
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        // Comment handling per language.
        if matches!(language, Language::Python) && c == '#' {
            while let Some(&next) = chars.peek() {
                chars.next();
                if next == '\n' {
                    break;
                }
            }
            continue;
        }
        if matches!(
            language,
            Language::Dart
                | Language::Rust
                | Language::Go
                | Language::Swift
                | Language::TypeScript
                | Language::Java
                | Language::C
                | Language::Cpp
        ) && c == '/'
        {
            chars.next();
            match chars.peek().copied() {
                Some('/') => {
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next == '\n' {
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for next in chars.by_ref() {
                        if prev == '*' && next == '/' {
                            break;
                        }
                        prev = next;
                    }
                    continue;
                }
                _ => {
                    out.push("/".into());
                    continue;
                }
            }
        }
        // Python docstrings: look for `"""` or `'''` at the start
        // of a token. We treat them as if they were comments to
        // avoid polluting the normalized stream with copyright /
        // documentation text.
        if matches!(language, Language::Python) && (c == '"' || c == '\'') {
            if let Some(triple) = peek_triple(&mut chars, c) {
                in_python_docstring = Some(triple);
                continue;
            }
        }
        if c == '"' || c == '\'' {
            consume_string_literal(&mut chars, c);
            out.push("STR".into());
            continue;
        }
        if c.is_ascii_digit() {
            consume_number_literal(&mut chars);
            out.push("NUM".into());
            continue;
        }
        // Unicode-aware: CJK / Greek / etc. identifiers must fold to `ID`
        // exactly like ASCII names, or renaming a Chinese identifier
        // changes the structural fingerprint (issues2.md #47).
        if c.is_alphabetic() || c == '_' {
            let ident = consume_identifier(&mut chars);
            if is_structural_keyword(language, &ident) {
                out.push(ident);
            } else {
                out.push("ID".into());
            }
            continue;
        }
        // Multi-char operators that matter for shape — keep them
        // as single tokens so `a == b` and `a = b` don't collide.
        if let Some(op) = consume_operator(&mut chars) {
            out.push(op);
            continue;
        }
        // Single-character punctuation.
        out.push(c.to_string());
        chars.next();
    }
    out
}

fn peek_triple(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    quote: char,
) -> Option<&'static str> {
    // Looks for `quote, quote, quote` starting at the current peek
    // position. Consumes them on match.
    let mut clone = chars.clone();
    let _ = clone.next();
    if clone.next() != Some(quote) {
        return None;
    }
    if clone.next() != Some(quote) {
        return None;
    }
    chars.next();
    chars.next();
    chars.next();
    if quote == '"' {
        Some("\"\"\"")
    } else {
        Some("'''")
    }
}

fn consume_string_literal(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, quote: char) {
    chars.next();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
            continue;
        }
        if c == quote {
            break;
        }
    }
}

fn consume_number_literal(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            chars.next();
        } else {
            break;
        }
    }
}

fn consume_identifier(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_alphanumeric() || c == '_' {
            s.push(c);
            chars.next();
        } else {
            break;
        }
    }
    s
}

fn consume_operator(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let two: String = {
        let mut clone = chars.clone();
        let a = clone.next()?;
        let b = clone.next()?;
        format!("{a}{b}")
    };
    if matches!(
        two.as_str(),
        "==" | "!="
            | "<="
            | ">="
            | "+="
            | "-="
            | "*="
            | "/="
            | "%="
            | "**"
            | "//"
            | "&&"
            | "||"
            | "->"
            | "=>"
            | "::"
            | ".."
    ) {
        chars.next();
        chars.next();
        return Some(two);
    }
    None
}

fn is_structural_keyword(language: Language, ident: &str) -> bool {
    match language {
        Language::Python => matches!(
            ident,
            "if" | "elif"
                | "else"
                | "for"
                | "while"
                | "return"
                | "yield"
                | "def"
                | "class"
                | "import"
                | "from"
                | "as"
                | "with"
                | "try"
                | "except"
                | "finally"
                | "raise"
                | "pass"
                | "break"
                | "continue"
                | "lambda"
                | "and"
                | "or"
                | "not"
                | "in"
                | "is"
                | "None"
                | "True"
                | "False"
                | "async"
                | "await"
                | "global"
                | "nonlocal"
        ),
        Language::Dart => matches!(
            ident,
            "if" | "else"
                | "for"
                | "while"
                | "do"
                | "switch"
                | "case"
                | "default"
                | "return"
                | "break"
                | "continue"
                | "throw"
                | "try"
                | "catch"
                | "finally"
                | "new"
                | "const"
                | "final"
                | "var"
                | "void"
                | "true"
                | "false"
                | "null"
                | "this"
                | "super"
                | "async"
                | "await"
                | "yield"
                | "in"
                | "is"
                | "as"
                | "operator"
                | "static"
                | "abstract"
                | "extends"
                | "implements"
        ),
        Language::Rust => matches!(
            ident,
            "if" | "else"
                | "match"
                | "for"
                | "while"
                | "loop"
                | "return"
                | "break"
                | "continue"
                | "let"
                | "mut"
                | "fn"
                | "struct"
                | "enum"
                | "trait"
                | "impl"
                | "mod"
                | "use"
                | "pub"
                | "const"
                | "static"
                | "type"
                | "where"
                | "as"
                | "in"
                | "ref"
                | "move"
                | "dyn"
                | "async"
                | "await"
                | "unsafe"
                | "extern"
                | "self"
                | "Self"
                | "super"
                | "crate"
                | "true"
                | "false"
        ),
        Language::Go => matches!(
            ident,
            "if" | "else"
                | "for"
                | "range"
                | "switch"
                | "case"
                | "default"
                | "select"
                | "return"
                | "break"
                | "continue"
                | "goto"
                | "fallthrough"
                | "defer"
                | "go"
                | "func"
                | "var"
                | "const"
                | "type"
                | "struct"
                | "interface"
                | "map"
                | "chan"
                | "package"
                | "import"
                | "nil"
                | "true"
                | "false"
                | "iota"
        ),
        Language::Swift => matches!(
            ident,
            "if" | "else"
                | "guard"
                | "for"
                | "while"
                | "repeat"
                | "switch"
                | "case"
                | "default"
                | "fallthrough"
                | "return"
                | "break"
                | "continue"
                | "func"
                | "var"
                | "let"
                | "class"
                | "struct"
                | "enum"
                | "protocol"
                | "extension"
                | "init"
                | "deinit"
                | "self"
                | "Self"
                | "super"
                | "nil"
                | "true"
                | "false"
                | "throw"
                | "throws"
                | "try"
                | "catch"
                | "do"
                | "defer"
                | "in"
                | "where"
                | "as"
                | "is"
                | "async"
                | "await"
                | "static"
                | "override"
                | "final"
                | "import"
        ),
        Language::TypeScript => matches!(
            ident,
            "if" | "else"
                | "for"
                | "while"
                | "do"
                | "switch"
                | "case"
                | "default"
                | "return"
                | "break"
                | "continue"
                | "function"
                | "var"
                | "let"
                | "const"
                | "class"
                | "interface"
                | "enum"
                | "extends"
                | "implements"
                | "new"
                | "this"
                | "super"
                | "typeof"
                | "instanceof"
                | "in"
                | "of"
                | "void"
                | "null"
                | "undefined"
                | "true"
                | "false"
                | "throw"
                | "try"
                | "catch"
                | "finally"
                | "async"
                | "await"
                | "yield"
                | "import"
                | "export"
                | "from"
                | "as"
                | "type"
                | "public"
                | "private"
                | "protected"
                | "static"
                | "readonly"
                | "abstract"
        ),
        Language::Java => matches!(
            ident,
            "if" | "else"
                | "for"
                | "while"
                | "do"
                | "switch"
                | "case"
                | "default"
                | "return"
                | "break"
                | "continue"
                | "class"
                | "interface"
                | "enum"
                | "extends"
                | "implements"
                | "new"
                | "this"
                | "super"
                | "instanceof"
                | "void"
                | "null"
                | "true"
                | "false"
                | "throw"
                | "throws"
                | "try"
                | "catch"
                | "finally"
                | "synchronized"
                | "import"
                | "package"
                | "public"
                | "private"
                | "protected"
                | "static"
                | "final"
                | "abstract"
        ),
        Language::C => matches!(
            ident,
            "if" | "else"
                | "for"
                | "while"
                | "do"
                | "switch"
                | "case"
                | "default"
                | "return"
                | "break"
                | "continue"
                | "goto"
                | "struct"
                | "enum"
                | "union"
                | "typedef"
                | "const"
                | "static"
                | "extern"
                | "sizeof"
                | "void"
                | "NULL"
        ),
        Language::Cpp => matches!(
            ident,
            "if" | "else"
                | "for"
                | "while"
                | "do"
                | "switch"
                | "case"
                | "default"
                | "return"
                | "break"
                | "continue"
                | "goto"
                | "struct"
                | "enum"
                | "union"
                | "class"
                | "namespace"
                | "template"
                | "typename"
                | "typedef"
                | "using"
                | "const"
                | "constexpr"
                | "static"
                | "extern"
                | "virtual"
                | "override"
                | "final"
                | "friend"
                | "operator"
                | "new"
                | "delete"
                | "this"
                | "public"
                | "private"
                | "protected"
                | "sizeof"
                | "void"
                | "auto"
                | "try"
                | "catch"
                | "throw"
                | "true"
                | "false"
                | "nullptr"
        ),
    }
}

fn fingerprint_tokens(tokens: &[String]) -> u64 {
    // FNV-1a — fast, no allocations beyond what `tokens` already
    // hold. A 64-bit hash is plenty for "did two functions
    // structurally collide?" given typical codebases have under
    // 1e6 functions.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for tok in tokens {
        for b in tok.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// SimHash over k-shingles of the normalized token stream. Two
/// bodies with similar shingles produce SimHashes with small
/// Hamming distance even if a few tokens were added, removed, or
/// renamed. We use FNV-1a per shingle (same hash family as
/// [`fingerprint_tokens`]) for cross-platform determinism.
pub fn simhash_tokens(tokens: &[String], k: usize) -> u64 {
    if tokens.is_empty() {
        return 0;
    }
    let k = k.max(1);
    let mut acc = [0i32; 64];
    let mut count = 0u32;
    if tokens.len() < k {
        // Fall back to hashing the whole body as a single shingle.
        let h = hash_shingle(tokens);
        return h;
    }
    for window in tokens.windows(k) {
        let h = hash_shingle(window);
        for (bit, slot) in acc.iter_mut().enumerate() {
            if (h >> bit) & 1 == 1 {
                *slot += 1;
            } else {
                *slot -= 1;
            }
        }
        count += 1;
    }
    debug_assert!(count > 0);
    let mut out = 0u64;
    for (bit, slot) in acc.iter().enumerate() {
        // Bias ties to 0 — deterministic and matches the common
        // SimHash convention.
        if *slot > 0 {
            out |= 1u64 << bit;
        }
    }
    out
}

fn hash_shingle(tokens: &[String]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for tok in tokens {
        for b in tok.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::{ArtifactId, Node};
    use tempfile::TempDir;

    #[test]
    fn normalize_strips_identifiers_literals_and_comments() {
        let a = r#"
def greet(name):
    # this is a comment
    return f"hello {name}"
"#;
        let b = r#"
def salute(person):
    # different comment
    return f"hello {person}"
"#;
        // Bodies are structurally identical once normalized: same
        // keyword skeleton, same operator stream, identifier &
        // literal blanks.
        assert_eq!(
            normalize(Language::Python, a),
            normalize(Language::Python, b)
        );
    }

    /// issues2.md #47: Unicode identifiers (`用户_count`, `α`) must fold to
    /// the same `ID` placeholder as ASCII names. The old ASCII-only
    /// scanner exploded each CJK char into its own punctuation token, so
    /// renaming a Chinese identifier changed the structural fingerprint.
    #[test]
    fn normalize_folds_unicode_identifiers_like_ascii_ones() {
        let cjk = "def 用户统计(数据):\n    return 数据\n";
        let ascii = "def stats(data):\n    return data\n";
        assert_eq!(
            normalize(Language::Python, cjk),
            normalize(Language::Python, ascii),
            "CJK and ASCII identifiers must normalize identically"
        );
        let mixed = "int 计数 = other;\n";
        let plain = "int counter = other;\n";
        assert_eq!(
            normalize(Language::Dart, mixed),
            normalize(Language::Dart, plain)
        );
    }

    #[test]
    fn normalize_drops_python_docstrings() {
        let with_doc = r#"
def f():
    """copyright 2026 megacorp"""
    return 1
"#;
        let without_doc = r#"
def f():
    return 1
"#;
        assert_eq!(
            normalize(Language::Python, with_doc),
            normalize(Language::Python, without_doc)
        );
    }

    #[test]
    fn normalize_dart_handles_line_and_block_comments() {
        let a = r#"
int sum(int a, int b) {
  // accumulate
  return a + b;
}
"#;
        let b = r#"
int total(int x, int y) {
  /* doc */
  return x + y;
}
"#;
        assert_eq!(normalize(Language::Dart, a), normalize(Language::Dart, b));
    }

    #[test]
    fn normalize_rust_abstracts_identifiers_but_keeps_keywords() {
        // Same control-flow skeleton, different identifier names + comments:
        // these must normalize to the *same* token stream so structural
        // clones cluster regardless of naming.
        let a = r#"
fn handle(input: &str) -> usize {
    // count something
    let mut total = 0;
    for ch in input.chars() {
        if ch == 'x' {
            total += 1;
        }
    }
    return total;
}
"#;
        let b = r#"
fn process(text: &str) -> usize {
    /* doc */
    let mut acc = 0;
    for c in text.chars() {
        if c == 'x' {
            acc += 1;
        }
    }
    return acc;
}
"#;
        assert_eq!(normalize(Language::Rust, a), normalize(Language::Rust, b));
        // Keywords survive; identifiers collapse to ID.
        let toks = normalize(Language::Rust, a);
        assert!(toks.iter().any(|t| t == "fn"), "fn kept: {toks:?}");
        assert!(toks.iter().any(|t| t == "for"), "for kept: {toks:?}");
        assert!(toks.iter().any(|t| t == "return"), "return kept: {toks:?}");
        assert!(
            toks.iter().any(|t| t == "ID"),
            "identifiers abstracted: {toks:?}"
        );
    }

    #[test]
    fn rust_node_kinds_are_recognised_as_scannable() {
        assert_eq!(node_language(NodeKind::RustFunction), Some(Language::Rust));
        assert_eq!(node_language(NodeKind::RustMethod), Some(Language::Rust));
    }

    #[test]
    fn normalize_go_abstracts_identifiers_but_keeps_keywords() {
        let a = r#"
func handle(input string) int {
    // count something
    total := 0
    for _, ch := range input {
        if ch == 'x' {
            total++
        }
    }
    return total
}
"#;
        let b = r#"
func process(text string) int {
    /* doc */
    acc := 0
    for _, c := range text {
        if c == 'x' {
            acc++
        }
    }
    return acc
}
"#;
        assert_eq!(normalize(Language::Go, a), normalize(Language::Go, b));
        let toks = normalize(Language::Go, a);
        assert!(toks.iter().any(|t| t == "func"), "func kept: {toks:?}");
        assert!(toks.iter().any(|t| t == "range"), "range kept: {toks:?}");
        assert!(
            toks.iter().any(|t| t == "ID"),
            "idents abstracted: {toks:?}"
        );
    }

    #[test]
    fn normalize_typescript_handles_comments_and_keeps_keywords() {
        let a = r#"
function handle(input: string): number {
    // count
    let total = 0;
    for (const ch of input) { if (ch === "x") { total += 1; } }
    return total;
}
"#;
        let b = r#"
function process(text: string): number {
    /* doc */
    let acc = 0;
    for (const c of text) { if (c === "x") { acc += 1; } }
    return acc;
}
"#;
        assert_eq!(
            normalize(Language::TypeScript, a),
            normalize(Language::TypeScript, b)
        );
        let toks = normalize(Language::TypeScript, a);
        assert!(toks.iter().any(|t| t == "function"), "kw kept: {toks:?}");
        assert!(toks.iter().any(|t| t == "for"), "for kept: {toks:?}");
    }

    #[test]
    fn extended_node_kinds_are_recognised_as_scannable() {
        assert_eq!(node_language(NodeKind::GoFunction), Some(Language::Go));
        assert_eq!(node_language(NodeKind::GoMethod), Some(Language::Go));
        assert_eq!(
            node_language(NodeKind::SwiftFunction),
            Some(Language::Swift)
        );
        assert_eq!(node_language(NodeKind::SwiftMethod), Some(Language::Swift));
        assert_eq!(
            node_language(NodeKind::SwiftInitializer),
            Some(Language::Swift)
        );
        assert_eq!(
            node_language(NodeKind::TypescriptFunction),
            Some(Language::TypeScript)
        );
        assert_eq!(
            node_language(NodeKind::TypescriptMethod),
            Some(Language::TypeScript)
        );
        assert_eq!(node_language(NodeKind::JavaMethod), Some(Language::Java));
        assert_eq!(
            node_language(NodeKind::JavaConstructor),
            Some(Language::Java)
        );
        assert_eq!(node_language(NodeKind::CFunction), Some(Language::C));
        assert_eq!(node_language(NodeKind::CppFunction), Some(Language::Cpp));
        assert_eq!(node_language(NodeKind::CppMethod), Some(Language::Cpp));
    }

    #[test]
    fn fingerprints_differ_when_structure_differs() {
        let plus = normalize(Language::Python, "def f(a, b): return a + b\n");
        let minus = normalize(Language::Python, "def f(a, b): return a - b\n");
        // The operator IS structural — `+` and `-` produce
        // different tokens, so fingerprints must differ. Anything
        // else would mask real semantic differences.
        assert_ne!(fingerprint_tokens(&plus), fingerprint_tokens(&minus));
    }

    fn empty_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn write_python(dir: &std::path::Path, rel: &str, body: &str) {
        let abs = dir.join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, body).unwrap();
    }

    fn insert_py_fn(store: &mut Store, file: &str, name: &str, lines: (u32, u32)) -> String {
        let id = format!("python::{file}::{name}");
        store
            .upsert_node(&Node {
                id: ArtifactId::new(id.clone()),
                kind: NodeKind::PythonFunction,
                path: Some(file.into()),
                name: Some(name.into()),
                start_line: Some(lines.0),
                end_line: Some(lines.1),
                content_hash: None,
                stable_key: None,
                source_file: Some(file.into()),
                source_hash: None,
                indexer: Some("python_ast".into()),
                index_generation: None,
                metadata_json: None,
            })
            .unwrap();
        id
    }

    #[test]
    fn analyze_returns_cluster_for_two_structurally_identical_python_functions() {
        let (mut store, dir) = empty_store();
        write_python(
            dir.path(),
            "app/a.py",
            "def fa(name):\n    msg = name.upper()\n    return f\"hi {msg}\"\n",
        );
        write_python(
            dir.path(),
            "app/b.py",
            "def fb(person):\n    label = person.upper()\n    return f\"hi {label}\"\n",
        );
        let a = insert_py_fn(&mut store, "app/a.py", "fa", (1, 3));
        let b = insert_py_fn(&mut store, "app/b.py", "fb", (1, 3));
        let report = analyze_similarity_with_store(
            &store,
            SimilarityOptions {
                repo_root: dir.path().into(),
                min_tokens: 4,
                mode: SimilarityMode::Exact,
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.stats.symbols_scanned, 2);
        assert_eq!(report.clusters.len(), 1);
        let ids: Vec<&str> = report.clusters[0]
            .members
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert!(ids.contains(&a.as_str()) && ids.contains(&b.as_str()));
        assert_eq!(report.clusters[0].duplicate_type, "exact_ast");
        assert_eq!(report.clusters[0].recommendation, "review");
        assert!(report.clusters[0].similarity_score.is_none());
        assert_eq!(report.stats.exact_clusters, 1);
        assert_eq!(report.stats.near_clusters, 0);
    }

    #[test]
    fn analyze_drops_clusters_below_min_tokens() {
        let (mut store, dir) = empty_store();
        write_python(dir.path(), "app/a.py", "def fa():\n    pass\n");
        write_python(dir.path(), "app/b.py", "def fb():\n    pass\n");
        insert_py_fn(&mut store, "app/a.py", "fa", (1, 2));
        insert_py_fn(&mut store, "app/b.py", "fb", (1, 2));
        let report = analyze_similarity_with_store(
            &store,
            SimilarityOptions {
                repo_root: dir.path().into(),
                min_tokens: 10,
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        assert!(
            report.clusters.is_empty(),
            "trivial `pass` bodies must not surface as duplicates"
        );
        assert_eq!(report.stats.symbols_skipped, 2);
    }

    #[test]
    fn analyze_filters_to_focus_symbol_when_requested() {
        let (mut store, dir) = empty_store();
        write_python(
            dir.path(),
            "app/a.py",
            "def fa(x):\n    y = x + 1\n    return y * 2\n",
        );
        write_python(
            dir.path(),
            "app/b.py",
            "def fb(x):\n    y = x + 1\n    return y * 2\n",
        );
        write_python(dir.path(), "app/c.py", "def fc(x):\n    return x.upper()\n");
        let a = insert_py_fn(&mut store, "app/a.py", "fa", (1, 3));
        let _b = insert_py_fn(&mut store, "app/b.py", "fb", (1, 3));
        let c = insert_py_fn(&mut store, "app/c.py", "fc", (1, 2));
        // Cluster of fa+fb exists; cluster of fc is solo.
        let report = analyze_similarity_with_store(
            &store,
            SimilarityOptions {
                repo_root: dir.path().into(),
                min_tokens: 4,
                focus_symbol_id: Some(c.clone()),
                mode: SimilarityMode::Exact,
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        assert!(
            report.clusters.is_empty(),
            "focus on a singleton symbol returns no clusters"
        );
        let report = analyze_similarity_with_store(
            &store,
            SimilarityOptions {
                repo_root: dir.path().into(),
                min_tokens: 4,
                focus_symbol_id: Some(a),
                mode: SimilarityMode::Exact,
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.clusters.len(), 1);
    }

    #[test]
    fn simhash_of_identical_token_streams_is_identical() {
        let toks = vec![
            "def".into(),
            "ID".into(),
            "(".into(),
            "ID".into(),
            ")".into(),
            ":".into(),
            "return".into(),
            "ID".into(),
            "+".into(),
            "NUM".into(),
        ];
        let a = simhash_tokens(&toks, 5);
        let b = simhash_tokens(&toks, 5);
        assert_eq!(a, b);
    }

    #[test]
    fn simhash_distance_grows_with_token_distance() {
        // Two near-identical bodies differing by a single inserted
        // statement should have small hamming distance.
        let original = normalize(
            Language::Python,
            "def f(x):\n    y = x + 1\n    z = y * 2\n    return z\n",
        );
        let renamed = normalize(
            Language::Python,
            "def g(aaa):\n    bbb = aaa + 1\n    ccc = bbb * 2\n    return ccc\n",
        );
        let unrelated = normalize(
            Language::Python,
            "def h(items):\n    for it in items:\n        if it is None:\n            raise ValueError\n    return items[0]\n",
        );
        let h_renamed = (simhash_tokens(&original, 5) ^ simhash_tokens(&renamed, 5)).count_ones();
        let h_unrelated =
            (simhash_tokens(&original, 5) ^ simhash_tokens(&unrelated, 5)).count_ones();
        // Renames flip ZERO bits (identifiers all collapse to ID
        // anyway), so renamed == original in normalized form.
        assert_eq!(h_renamed, 0);
        // An entirely different control-flow body must be much
        // further away than a rename.
        assert!(
            h_unrelated > 8,
            "unrelated body should drift far from original: got h={h_unrelated}",
        );
    }

    #[test]
    fn chained_near_cluster_reports_true_worst_case_similarity() {
        // A~B and B~C are within the hamming threshold but A~C is NOT.
        // Union-find still merges all three into one cluster (transitive
        // closure) — the reported score must then be the TRUE worst pair
        // (A,C), not just the worst *discovered* pair (issues.md #12).
        let body_a = "def fa(x):\n    a = x + 1\n    b = a * 2\n    c = b - 3\n    d = c + 4\n    e = d * 5\n    return e\n";
        let body_b = "def fb(x):\n    a = x + 1\n    b = a * 2\n    c = b - 3\n    d = c + 4\n    e = d * 5\n    f = e % 6\n    g = f + 7\n    return g\n";
        let body_c = "def fc(x):\n    a = x + 1\n    b = a * 2\n    c = b - 3\n    d = c + 4\n    e = d * 5\n    f = e % 6\n    g = f + 7\n    while g > 0:\n        g = g - 1\n    h = g * 9\n    return h\n";

        let ta = normalize(Language::Python, body_a);
        let tb = normalize(Language::Python, body_b);
        let tc = normalize(Language::Python, body_c);
        let h_ab = (simhash_tokens(&ta, 5) ^ simhash_tokens(&tb, 5)).count_ones();
        let h_bc = (simhash_tokens(&tb, 5) ^ simhash_tokens(&tc, 5)).count_ones();
        let h_ac = (simhash_tokens(&ta, 5) ^ simhash_tokens(&tc, 5)).count_ones();
        // Pick the threshold between the chain links and the far pair.
        let link_max = h_ab.max(h_bc);
        assert!(
            link_max + 2 <= h_ac,
            "test premise: need a chain, got h_ab={h_ab} h_bc={h_bc} h_ac={h_ac}"
        );
        let threshold = link_max + 1;
        let min_similarity = 1.0 - (threshold as f32 / 64.0);

        let (mut store, dir) = empty_store();
        write_python(dir.path(), "app/a.py", body_a);
        write_python(dir.path(), "app/b.py", body_b);
        write_python(dir.path(), "app/c.py", body_c);
        insert_py_fn(&mut store, "app/a.py", "fa", (1, 7));
        insert_py_fn(&mut store, "app/b.py", "fb", (1, 9));
        insert_py_fn(&mut store, "app/c.py", "fc", (1, 12));
        let report = analyze_similarity_with_store(
            &store,
            SimilarityOptions {
                repo_root: dir.path().into(),
                min_tokens: 8,
                mode: SimilarityMode::Near,
                min_similarity,
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        let cluster = report
            .clusters
            .iter()
            .find(|c| c.members.len() == 3)
            .expect("transitive closure must merge all three");
        let score = cluster.similarity_score.expect("score");
        let true_worst = 1.0 - (h_ac as f32 / 64.0);
        assert!(
            (score - true_worst).abs() < 1e-6,
            "score must reflect the true worst pair: got {score}, want {true_worst}"
        );
    }

    #[test]
    fn near_duplicate_pass_groups_pairs_with_extra_statement() {
        // Two Python functions that share the same skeleton but
        // one of them has a single extra arithmetic statement.
        // Tier 1 would NOT match them; tier 2 should.
        let (mut store, dir) = empty_store();
        write_python(
            dir.path(),
            "app/a.py",
            "def fa(items):\n    total = 0\n    for it in items:\n        total = total + it\n        total = total * 2\n    return total\n",
        );
        write_python(
            dir.path(),
            "app/b.py",
            "def fb(rows):\n    sum = 0\n    for r in rows:\n        sum = sum + r\n        sum = sum * 2\n        sum = sum - 1\n    return sum\n",
        );
        let a = insert_py_fn(&mut store, "app/a.py", "fa", (1, 6));
        let b = insert_py_fn(&mut store, "app/b.py", "fb", (1, 7));
        let report = analyze_similarity_with_store(
            &store,
            SimilarityOptions {
                repo_root: dir.path().into(),
                min_tokens: 8,
                mode: SimilarityMode::Near,
                min_similarity: 0.7,
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.stats.near_clusters, report.clusters.len());
        assert!(
            !report.clusters.is_empty(),
            "tier 2 must catch the near-duplicate pair"
        );
        let cluster = &report.clusters[0];
        assert_eq!(cluster.duplicate_type, "near_token");
        let score = cluster.similarity_score.expect("near cluster has score");
        assert!(
            (0.7..=1.0).contains(&score),
            "similarity_score within range: {score}"
        );
        let ids: Vec<&str> = cluster.members.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&a.as_str()) && ids.contains(&b.as_str()));
    }

    #[test]
    fn near_duplicate_pass_skips_symbols_already_in_exact_cluster() {
        // Two structurally identical functions + a third
        // near-duplicate of those two. Tier 1 must claim the
        // first two; tier 2 should NOT re-cluster them when both
        // tiers run together — instead the third one should be
        // its own singleton (and thus filtered out for cluster
        // size < 2).
        let (mut store, dir) = empty_store();
        write_python(
            dir.path(),
            "app/a.py",
            "def fa(x):\n    y = x + 1\n    z = y * 2\n    return z\n",
        );
        write_python(
            dir.path(),
            "app/b.py",
            "def fb(x):\n    y = x + 1\n    z = y * 2\n    return z\n",
        );
        write_python(
            dir.path(),
            "app/c.py",
            "def fc(x):\n    y = x + 1\n    z = y * 2\n    w = z - 1\n    return w\n",
        );
        insert_py_fn(&mut store, "app/a.py", "fa", (1, 4));
        insert_py_fn(&mut store, "app/b.py", "fb", (1, 4));
        insert_py_fn(&mut store, "app/c.py", "fc", (1, 5));
        let report = analyze_similarity_with_store(
            &store,
            SimilarityOptions {
                repo_root: dir.path().into(),
                min_tokens: 4,
                mode: SimilarityMode::All,
                min_similarity: 0.7,
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        // fa & fb form an exact cluster; fc is left alone (no
        // tier-2 partner) so total clusters == 1.
        assert_eq!(report.stats.exact_clusters, 1);
        assert_eq!(report.stats.near_clusters, 0);
        assert_eq!(report.clusters.len(), 1);
        assert_eq!(report.clusters[0].duplicate_type, "exact_ast");
    }

    #[test]
    fn near_pairwise_skipped_when_max_pairwise_guard_trips() {
        let (mut store, dir) = empty_store();
        for i in 0..5 {
            let name = format!("f{i}");
            let path = format!("app/{name}.py");
            write_python(
                dir.path(),
                &path,
                "def f(x):\n    y = x + 1\n    return y * 2\n",
            );
            insert_py_fn(&mut store, &path, &name, (1, 3));
        }
        let report = analyze_similarity_with_store(
            &store,
            SimilarityOptions {
                repo_root: dir.path().into(),
                min_tokens: 4,
                mode: SimilarityMode::Near,
                max_pairwise_symbols: 2,
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        assert!(
            report.stats.near_pairwise_skipped,
            "guard should report skip when uncovered > max"
        );
        assert!(report.clusters.is_empty());
    }
}
