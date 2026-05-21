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
//! - **Dart** (`dart_function`, `dart_method`, `dart_constructor`):
//!   same shared normalizer but with `//` and `/* */` comment
//!   handling instead of `#`.
//!
//! Other languages can opt in by adding a `Language` arm — the
//! normalizer is shared.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::NodeKind;
use specslice_store::Store;

/// Schema version emitted alongside [`SimilarityReport`] so future
/// consumers can refuse to deserialize incompatible payloads
/// without guessing.
pub const SIMILARITY_SCHEMA_VERSION: u32 = 1;

/// Default lower bound on a function body in normalized tokens.
/// Sub-six-token bodies are usually `return None` / `pass` and
/// would dominate any duplicate report with trivial hits.
pub const DEFAULT_MIN_TOKENS: usize = 12;

/// Languages the normalizer currently understands. New entries
/// MUST keep the existing token grammar so old fingerprints stay
/// comparable across versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Python,
    Dart,
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
    /// returned. Powers `specslice similar --node SYMBOL_ID`.
    pub focus_symbol_id: Option<String>,
}

impl Default for SimilarityOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            min_tokens: DEFAULT_MIN_TOKENS,
            min_cluster_size: 2,
            focus_symbol_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimilarityCluster {
    /// Hex form of the structural fingerprint shared by every
    /// member of the cluster. Useful as a stable cluster id.
    pub fingerprint: String,
    /// Duplicate kind. Tier 1 always emits `"exact_ast"`; future
    /// tiers will publish `"near_token"` and `"graph_behavior"`.
    pub duplicate_type: String,
    pub members: Vec<SimilarityMember>,
    /// Normalized token count, identical across members.
    pub normalized_token_count: usize,
    /// Conservative recommendation surfaced to humans / AI. Tier 1
    /// never auto-merges; it always says "review".
    pub recommendation: String,
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
    let db_path = options.repo_root.join(".specslice").join("graph.db");
    let store = Store::open(&db_path).with_context(|| {
        format!(
            "opening graph store at {} for similarity report",
            db_path.display()
        )
    })?;
    analyze_similarity_with_store(&store, options)
}

/// Walk every Python / Dart function-like node in the store, read
/// its source range, normalize → hash, then bucket by fingerprint.
/// Buckets with at least `min_cluster_size` members become
/// [`SimilarityCluster`]s, sorted by descending body size so the
/// most "interesting" duplicates surface first.
pub fn analyze_similarity_with_store(
    store: &Store,
    options: SimilarityOptions,
) -> Result<SimilarityReport> {
    let nodes = store.list_all_nodes().context("listing nodes")?;
    let repo_root = options.repo_root.clone();

    let mut buckets: BTreeMap<u64, Vec<(SimilarityMember, usize)>> = BTreeMap::new();
    let mut scanned = 0usize;
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
        scanned += 1;
        let fingerprint = fingerprint_tokens(&tokens);
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
        buckets
            .entry(fingerprint)
            .or_default()
            .push((member, tokens.len()));
    }

    let mut clusters: Vec<SimilarityCluster> = buckets
        .into_iter()
        .filter(|(_, members)| members.len() >= options.min_cluster_size)
        .filter_map(|(fingerprint, members)| {
            // All members of a cluster share the same normalized
            // token count by construction (same fingerprint <=>
            // same normalized token stream).
            let token_count = members.first().map(|(_, n)| *n).unwrap_or_default();
            let mut just_members: Vec<SimilarityMember> =
                members.into_iter().map(|(m, _)| m).collect();
            just_members.sort_by(|a, b| a.path.cmp(&b.path).then(a.label.cmp(&b.label)));
            if let Some(focus) = options.focus_symbol_id.as_deref() {
                if !just_members.iter().any(|m| m.id == focus) {
                    return None;
                }
            }
            Some(SimilarityCluster {
                fingerprint: format!("{fingerprint:016x}"),
                duplicate_type: "exact_ast".into(),
                members: just_members,
                normalized_token_count: token_count,
                recommendation: "review".into(),
            })
        })
        .collect();
    clusters.sort_by(|a, b| {
        b.normalized_token_count
            .cmp(&a.normalized_token_count)
            .then_with(|| b.members.len().cmp(&a.members.len()))
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });

    Ok(SimilarityReport {
        schema_version: SIMILARITY_SCHEMA_VERSION,
        stats: SimilarityStats {
            symbols_scanned: scanned,
            symbols_skipped: skipped,
            clusters_reported: clusters.len(),
        },
        clusters,
    })
}

fn node_language(kind: NodeKind) -> Option<Language> {
    match kind {
        NodeKind::PythonFunction | NodeKind::PythonMethod => Some(Language::Python),
        NodeKind::DartFunction | NodeKind::DartMethod | NodeKind::DartConstructor => {
            Some(Language::Dart)
        }
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
        if matches!(language, Language::Dart) && c == '/' {
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
        if c.is_ascii_alphabetic() || c == '_' {
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
        if c.is_ascii_alphanumeric() || c == '_' {
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

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{ArtifactId, Node};
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
                ..SimilarityOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.clusters.len(), 1);
    }
}
