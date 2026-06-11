//! Fulltext content pass — populates the FTS5 layer the search engine ranks
//! with BM25.
//!
//! Runs once at the end of `specslice index`, *after* every structural pass:
//! it mirrors whatever nodes are in the store right now, so it needs no
//! per-indexer ownership or cleanup. For every content-bearing node (a node
//! with a path + line span — code symbols, doc sections, requirements) it
//! reads the span from the source file, pre-tokenises it bilingually
//! ([`crate::fts_text`]) and writes one `node_fts` row. The whole table is
//! rebuilt per run; on a repo with tens of thousands of symbols this is a
//! single file-read pass and a couple of seconds.
//!
//! `NodeKind::File` rows are skipped: a file's span contains every symbol in
//! it, so indexing files would double-count bodies and let a 2000-line file
//! outrank the one function that actually matches. Files remain findable
//! through the structural path/name scoring.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::NodeKind;
use specslice_store::{FulltextRow, Store};

use crate::fts_text::fts_tokens;

/// Per-node body cap (chars, pre-tokenisation). Bounds index size and keeps a
/// giant generated function from dominating term statistics.
const MAX_BODY_CHARS: usize = 8_000;

/// How many contiguous comment lines directly above a code symbol get folded
/// into its body. Doc comments (`///`, `//!`, `#`, `--`, `/** … */`) are where
/// intent text lives, yet tree-sitter spans start at the declaration — without
/// this, "find the code that guards against X" misses the very function whose
/// doc comment says so.
const MAX_LEADING_COMMENT_LINES: usize = 40;

/// Outcome of one fulltext rebuild.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FulltextIndexResult {
    /// Nodes whose bodies landed in the FTS table.
    pub nodes_indexed: usize,
    /// Distinct source files read.
    pub files_read: usize,
    /// Nodes skipped because their file could not be read (moved/deleted
    /// between the structural pass and this one — rare, never fatal).
    pub skipped_unreadable: usize,
}

/// Rebuild the `node_fts` content layer from the current node set.
pub fn rebuild_fulltext_index(store: &mut Store, repo_root: &Path) -> Result<FulltextIndexResult> {
    let nodes = store
        .list_all_nodes()
        .context("listing nodes for fulltext")?;

    // Group span nodes by path so each file is read exactly once.
    let mut by_path: BTreeMap<String, Vec<(String, u32, u32, bool)>> = BTreeMap::new();
    for node in &nodes {
        if node.kind == NodeKind::File {
            continue;
        }
        let (Some(path), Some(start), Some(end)) = (&node.path, node.start_line, node.end_line)
        else {
            continue;
        };
        if end < start {
            continue;
        }
        // Doc-structure nodes already start at their heading; only code
        // symbols get the leading-comment extension (a markdown section must
        // not absorb the previous section's tail).
        let extend_comments = !matches!(
            node.kind,
            NodeKind::DocSection
                | NodeKind::Requirement
                | NodeKind::AcceptanceCriterion
                | NodeKind::Adr
        );
        by_path.entry(path.clone()).or_default().push((
            node.id.to_string(),
            start,
            end,
            extend_comments,
        ));
    }

    // Read + slice + tokenise per file in parallel (pure CPU + IO, no shared
    // state — tokenisation dominated the single-thread profile on 84k-symbol
    // repos), then insert serially. Deterministic: per-file row order is
    // preserved and the files keep their BTreeMap path order.
    use rayon::prelude::*;
    let per_file: Vec<(usize, usize, Vec<FulltextRow>)> = by_path
        .par_iter()
        .map(|(path, spans)| {
            let abs = repo_root.join(path);
            let Ok(contents) = std::fs::read_to_string(&abs) else {
                return (0usize, spans.len(), Vec::new());
            };
            let lines: Vec<&str> = contents.lines().collect();
            let mut rows: Vec<FulltextRow> = Vec::new();
            for (node_id, start, end, extend_comments) in spans {
                let start = if *extend_comments {
                    extend_over_leading_comments(&lines, *start)
                } else {
                    *start
                };
                let body = slice_span(&lines, start, *end);
                if body.is_empty() {
                    continue;
                }
                let tokens = fts_tokens(&body);
                if tokens.is_empty() {
                    continue;
                }
                rows.push(FulltextRow {
                    node_id: node_id.clone(),
                    body: tokens.join(" "),
                });
            }
            (1usize, 0usize, rows)
        })
        .collect();

    let mut result = FulltextIndexResult::default();
    let mut rows: Vec<FulltextRow> = Vec::new();
    for (read, skipped, file_rows) in per_file {
        result.files_read += read;
        result.skipped_unreadable += skipped;
        rows.extend(file_rows);
    }

    result.nodes_indexed = store
        .rebuild_fulltext(&rows)
        .context("rebuilding fulltext table")?;
    Ok(result)
}

/// Walk upward from a symbol's 1-based `start` line over contiguous comment
/// lines (and attribute/decorator lines) and return the extended start. The
/// prefixes cover the workspace's languages: `///` `//!` `//` (Rust/C-family/
/// Dart/Go/TS/Swift/Java), `#` (Python — also matches Rust `#[attr]`,
/// harmless), `--` (SQL), `*` `/*` `*/` (block comments), `"""` (Python
/// docstring fences). Bounded by [`MAX_LEADING_COMMENT_LINES`].
pub(crate) fn extend_over_leading_comments<S: AsRef<str>>(lines: &[S], start: u32) -> u32 {
    let mut idx = (start.max(1) as usize).saturating_sub(1); // 0-based decl line
    let mut taken = 0usize;
    while idx > 0 && taken < MAX_LEADING_COMMENT_LINES {
        let above = lines[idx - 1].as_ref().trim_start();
        let is_comment = above.starts_with("///")
            || above.starts_with("//!")
            || above.starts_with("//")
            || above.starts_with('#')
            || above.starts_with("--")
            || above.starts_with("/*")
            || above.starts_with('*')
            || above.starts_with("\"\"\"")
            || above.starts_with('@');
        if !is_comment {
            break;
        }
        idx -= 1;
        taken += 1;
    }
    // `idx` walked down from `start` (a u32), so `idx + 1 <= start` always
    // fits; saturate anyway rather than `as`-cast.
    u32::try_from(idx + 1).unwrap_or(u32::MAX)
}

/// Join the 1-based inclusive line span, capped at [`MAX_BODY_CHARS`]. Spans
/// beyond EOF (stale index vs. edited file) clamp instead of erroring.
fn slice_span(lines: &[&str], start: u32, end: u32) -> String {
    let start_idx = (start.max(1) as usize - 1).min(lines.len());
    let end_idx = (end as usize).min(lines.len());
    if start_idx >= end_idx {
        return String::new();
    }
    let mut out = String::new();
    for line in &lines[start_idx..end_idx] {
        if out.len() + line.len() + 1 > MAX_BODY_CHARS {
            let remaining = MAX_BODY_CHARS.saturating_sub(out.len());
            // Truncate on a char boundary — never split a multi-byte char.
            let mut cut = remaining.min(line.len());
            while cut > 0 && !line.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push_str(&line[..cut]);
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_span_clamps_out_of_range_and_caps_length() {
        let lines = vec!["alpha", "beta", "gamma"];
        assert_eq!(slice_span(&lines, 1, 2), "alpha\nbeta\n");
        assert_eq!(slice_span(&lines, 2, 99), "beta\ngamma\n");
        assert_eq!(slice_span(&lines, 9, 12), "");

        let long = "x".repeat(MAX_BODY_CHARS * 2);
        let refs = vec![long.as_str()];
        assert!(slice_span(&refs, 1, 1).len() <= MAX_BODY_CHARS);
    }

    #[test]
    fn leading_doc_comments_fold_into_the_symbol_body() {
        let lines = vec![
            "use std::fmt;",
            "",
            "/// Guards against the byte boundary panic.",
            "/// Second doc line.",
            "#[inline]",
            "pub fn advance() {}",
        ];
        // Declaration on 1-based line 6; comments + attribute walk up to line 3.
        assert_eq!(extend_over_leading_comments(&lines, 6), 3);
        // No comments above → unchanged.
        assert_eq!(extend_over_leading_comments(&lines, 1), 1);
    }

    #[test]
    fn slice_span_cap_respects_multibyte_char_boundaries() {
        // A line of 3-byte chars longer than the cap: the cut must land on a
        // char boundary, not panic mid-char.
        let long = "错".repeat(MAX_BODY_CHARS); // 3 bytes each
        let refs = vec![long.as_str()];
        let s = slice_span(&refs, 1, 1);
        assert!(!s.is_empty() && s.len() <= MAX_BODY_CHARS);
    }
}
