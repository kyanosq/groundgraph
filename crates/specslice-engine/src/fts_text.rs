//! Bilingual pre-tokenisation for the FTS5 content layer.
//!
//! SQLite's stock `unicode61` tokenizer treats a run of CJK characters as one
//! opaque token, so `错位竞争` would only match the *exact* run — useless for
//! phrase-in-paragraph search. Instead of shipping a custom SQLite tokenizer
//! extension, both the *index side* ([`crate::fulltext_indexer`]) and the
//! *query side* ([`crate::search`]) pass their text through [`fts_tokens`]:
//!
//! - ASCII identifier runs are lower-cased and split on camelCase/snake_case
//!   (`ensureQueryIndexes` → `ensure query indexes`), so concept queries match
//!   identifiers inside bodies.
//! - CJK runs become overlapping **bigrams** (`错位竞争` → `错位 位竞 竞争`),
//!   the classic CJK full-text trick: any ≥2-char phrase query becomes an AND
//!   of its bigrams and matches the indexed body.
//!
//! The result is space-joined before insertion, so `unicode61` only has to
//! split on whitespace. The contract is symmetric: tokenize the same way on
//! both sides and FTS5's `MATCH` + `bm25()` do the rest.

use crate::search::split_identifier;

/// Upper bound on tokens taken from one *query* (not bodies); keeps the FTS
/// `MATCH` expression bounded no matter what the operator pastes in.
pub const MAX_QUERY_TOKENS: usize = 24;

fn is_cjk(ch: char) -> bool {
    matches!(ch,
        '\u{3400}'..='\u{4DBF}'   // CJK ext A
        | '\u{4E00}'..='\u{9FFF}' // CJK unified
        | '\u{F900}'..='\u{FAFF}' // CJK compat ideographs
        | '\u{3040}'..='\u{309F}' // hiragana
        | '\u{30A0}'..='\u{30FF}' // katakana
        | '\u{AC00}'..='\u{D7AF}' // hangul
    )
}

fn flush_ascii(buf: &mut String, out: &mut Vec<String>) {
    if buf.is_empty() {
        return;
    }
    for sub in split_identifier(buf) {
        if !sub.is_empty() {
            out.push(sub);
        }
    }
    buf.clear();
}

fn flush_cjk(run: &mut Vec<char>, out: &mut Vec<String>) {
    match run.len() {
        0 => {}
        1 => out.push(run[0].to_string()),
        n => {
            // `n` CJK chars yield `n - 1` overlapping bigrams (the `0`/`1`
            // arms above guarantee `n >= 2`). Reserve once up front so the
            // inner pushes never re-grow `out` — this is the FTS index's
            // innermost loop on CJK-heavy corpora (issues3.md #141).
            out.reserve(n - 1);
            for pair in run.windows(2) {
                out.push(pair.iter().collect());
            }
        }
    }
    run.clear();
}

/// Tokenise arbitrary text for the FTS5 content layer (see module docs).
/// Total: never panics on any UTF-8; deterministic.
pub fn fts_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(ch);
        } else if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

/// Tokenise a *query*, deduped and bounded by [`MAX_QUERY_TOKENS`] so the
/// generated `MATCH` expression stays small and deterministic.
pub fn fts_query_tokens(query: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for tok in fts_tokens(query) {
        if seen.insert(tok.clone()) {
            out.push(tok);
            if out.len() >= MAX_QUERY_TOKENS {
                break;
            }
        }
    }
    out
}

/// FTS5 expression requiring **all** tokens (implicit AND between quoted
/// phrases). Tokens come from [`fts_tokens`] so they never contain `"`.
pub fn fts_all_expr(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// FTS5 expression matching **any** token (`OR`).
pub fn fts_any_expr(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_identifiers_split_into_searchable_subtokens() {
        assert_eq!(
            fts_tokens("ensureQueryIndexes called from parse_sql_tables"),
            vec!["ensure", "query", "indexes", "called", "from", "parse", "sql", "tables"]
        );
    }

    #[test]
    fn cjk_runs_become_overlapping_bigrams() {
        assert_eq!(fts_tokens("错位竞争"), vec!["错位", "位竞", "竞争"]);
        assert_eq!(fts_tokens("图"), vec!["图"]);
    }

    #[test]
    fn mixed_text_keeps_both_families_in_order() {
        assert_eq!(
            fts_tokens("内化 retrieval 层"),
            vec!["内化", "retrieval", "层"]
        );
    }

    #[test]
    fn exprs_quote_every_token() {
        let toks = vec!["byte".to_string(), "错位".to_string()];
        assert_eq!(fts_all_expr(&toks), "\"byte\" \"错位\"");
        assert_eq!(fts_any_expr(&toks), "\"byte\" OR \"错位\"");
    }

    #[test]
    fn query_tokens_dedupe_and_bound() {
        let raw = "foo_bar foo_bar ".repeat(60);
        let toks = fts_query_tokens(&raw);
        assert_eq!(toks, vec!["foo".to_string(), "bar".to_string()]);
        let many: String = (0..100).map(|i| format!("tok{i} ")).collect();
        assert_eq!(fts_query_tokens(&many).len(), MAX_QUERY_TOKENS);
    }

    #[test]
    fn single_char_ascii_tokens_are_noise_and_dropped() {
        // Inherited from `split_identifier`: loop vars (`i`, `x`) would bloat
        // the index and match everything. CJK single chars stay (they carry
        // meaning).
        assert_eq!(fts_tokens("for i in xs"), vec!["for", "in", "xs"]);
        assert_eq!(fts_tokens("图"), vec!["图"]);
    }
}
