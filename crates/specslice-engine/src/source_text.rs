//! Deterministic source-span reading + lexical noise stripping.
//!
//! Several "behavioural fact" analyses (P24 — [`symbol_facts`](crate::symbol_facts),
//! [`constants`](crate::constants), [`data_contract`](crate::data_contract))
//! need to look at the *body text* of a graph node, not just its edges.
//! The code graph already records `path` + `start_line` / `end_line` for
//! every code symbol, so we can recover the exact source span by reading
//! the file and slicing the lines.
//!
//! Raw source is hostile to keyword/operator counting: an `if` inside a
//! string literal or a `// for loop` comment must not be counted. This
//! module provides [`strip_noise`], a single-pass lexer that blanks out
//! comment and string-literal *contents* while preserving every newline
//! (and therefore line numbering). Downstream scanners run their cheap
//! word / substring matching over the stripped text but show the *raw*
//! line as evidence.
//!
//! This is intentionally heuristic and dependency-free (no tree-sitter
//! re-parse): the goal is honest, deterministic *signals*, not a compiler.

use std::path::Path;

use specslice_core::language_traits::{language_of, Language};
use specslice_core::Node;

/// The recovered source span of a graph node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSource {
    /// 1-based line number of the first returned line.
    pub start_line: u32,
    /// Exact source lines for `[start_line, end_line]`, joined by `\n`.
    pub raw: String,
    /// The language family inferred from the node kind.
    pub language: Language,
}

/// Read the `[start_line, end_line]` (1-based, inclusive) source span of
/// `node` from disk, relative to `repo_root`. Returns `None` when the node
/// has no path/line range, the file is missing, or the range is empty.
pub fn read_node_source(repo_root: &Path, node: &Node) -> Option<NodeSource> {
    let path = node.path.as_deref()?;
    let (start, end) = match (node.start_line, node.end_line) {
        (Some(s), Some(e)) if e >= s && s >= 1 => (s, e),
        _ => return None,
    };
    let abs = repo_root.join(path);
    let text = std::fs::read_to_string(&abs).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    if start > total {
        return None;
    }
    let start_idx = usize::try_from(start - 1).unwrap_or(usize::MAX);
    let end_idx = usize::try_from(end.min(total)).unwrap_or(usize::MAX);
    if start_idx >= lines.len() {
        return None;
    }
    let raw = lines[start_idx..end_idx].join("\n");
    Some(NodeSource {
        start_line: start,
        raw,
        language: language_of(node.kind),
    })
}

/// True when this language uses `#` for line comments (Python). Everything
/// else SpecSlice indexes uses `//` + `/* */`.
fn uses_hash_comments(lang: Language) -> bool {
    matches!(lang, Language::Python)
}

/// Replace comment and string-literal *contents* with spaces, preserving
/// every `\n` (so the result has the same number of lines as the input and
/// `out.lines().nth(i)` aligns with `src.lines().nth(i)`). Delimiters and
/// structural punctuation are kept so token boundaries survive.
///
/// Handled, in a single pass:
/// - line comments (`//`, or `#` for Python),
/// - block comments (`/* … */`),
/// - `"…"`, `'…'` and `` `…` `` string literals with `\` escapes,
/// - Python triple-quoted strings (`"""…"""`, `'''…'''`).
///
/// Rust raw strings / lifetimes and similar exotica are *not* special-cased;
/// the worst case is a slightly noisier count, never a panic.
pub fn strip_noise(src: &str, lang: Language) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        Line,
        Block,
        Str(char),
        Triple(char),
    }

    let hash = uses_hash_comments(lang);
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut state = State::Code;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let next2 = chars.get(i + 2).copied();
        match state {
            State::Code => {
                if c == '\n' {
                    out.push('\n');
                    i += 1;
                } else if !hash && c == '/' && next == Some('/') {
                    state = State::Line;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                } else if hash && c == '#' {
                    state = State::Line;
                    out.push(' ');
                    i += 1;
                } else if !hash && c == '/' && next == Some('*') {
                    state = State::Block;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                } else if (c == '"' || c == '\'') && next == Some(c) && next2 == Some(c) {
                    // Triple-quoted string (Python docstrings, also tolerated
                    // elsewhere — harmless if it never appears).
                    state = State::Triple(c);
                    out.push(c);
                    out.push(' ');
                    out.push(' ');
                    i += 3;
                } else if c == '"' || c == '\'' || c == '`' {
                    state = State::Str(c);
                    out.push(c);
                    i += 1;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            State::Line => {
                if c == '\n' {
                    state = State::Code;
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            State::Block => {
                if c == '*' && next == Some('/') {
                    state = State::Code;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
            State::Str(delim) => {
                if c == '\\' {
                    // Skip the escaped char (but never swallow a newline so
                    // line counts stay aligned).
                    out.push(' ');
                    if next.is_some() && next != Some('\n') {
                        out.push(' ');
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if c == delim {
                    state = State::Code;
                    out.push(delim);
                    i += 1;
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
            State::Triple(delim) => {
                if c == delim && next == Some(delim) && next2 == Some(delim) {
                    state = State::Code;
                    out.push(' ');
                    out.push(' ');
                    out.push(delim);
                    i += 3;
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
        }
    }
    out
}

/// Split text into identifier tokens (`[A-Za-z0-9_]+`), in order. Used for
/// exact-word keyword counting so `information` does not match `for`.
pub fn identifier_tokens(src: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut start: Option<usize> = None;
    for (idx, &b) in bytes.iter().enumerate() {
        let is_word = b.is_ascii_alphanumeric() || b == b'_';
        match (is_word, start) {
            (true, None) => start = Some(idx),
            (false, Some(s)) => {
                out.push(&src[s..idx]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(&src[s..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{ArtifactId, Node, NodeKind};

    #[test]
    fn strip_noise_blanks_line_comments_but_keeps_newlines() {
        let src = "let x = 1; // if for while\nlet y = 2;";
        let out = strip_noise(src, Language::Rust);
        assert_eq!(out.lines().count(), 2);
        assert!(out.contains("let x = 1;"));
        assert!(!out.contains("if for while"));
        // second line untouched
        assert!(out.lines().nth(1).unwrap().contains("let y = 2;"));
    }

    #[test]
    fn strip_noise_blanks_string_contents() {
        let src = r#"print("if while for return");"#;
        let out = strip_noise(src, Language::Dart);
        assert!(out.contains("print("));
        assert!(!out.contains("if while for return"));
        // delimiters preserved
        assert_eq!(out.matches('"').count(), 2);
    }

    #[test]
    fn strip_noise_handles_block_comments_across_lines() {
        let src = "a();\n/* if\n for */\nb();";
        let out = strip_noise(src, Language::Cpp);
        assert_eq!(out.lines().count(), 4);
        assert!(!out.contains("if"));
        assert!(out.contains("a();"));
        assert!(out.contains("b();"));
    }

    #[test]
    fn strip_noise_python_hash_and_triple_quotes() {
        let src = "x = 1  # if for\n\"\"\"if\nfor\"\"\"\ny = 2";
        let out = strip_noise(src, Language::Python);
        assert!(!out.contains("if for"));
        assert!(out.contains("x = 1"));
        assert!(out.contains("y = 2"));
        // 4 lines preserved
        assert_eq!(out.lines().count(), 4);
    }

    #[test]
    fn identifier_tokens_split_on_non_word() {
        let toks = identifier_tokens("if (a == b) { return c_1.d; }");
        assert_eq!(toks, vec!["if", "a", "b", "return", "c_1", "d"]);
        // `information` must not be split into `for`
        assert_eq!(identifier_tokens("information"), vec!["information"]);
    }

    #[test]
    fn read_node_source_slices_the_span() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = "src/x.rs";
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join(file),
            "line1\nfn foo() {\n    return 1;\n}\nline5",
        )
        .unwrap();
        let mut node = Node::new(ArtifactId::new("rust::src/x.rs#foo"), NodeKind::RustFunction);
        node.path = Some(file.to_string());
        node.start_line = Some(2);
        node.end_line = Some(4);
        let src = read_node_source(tmp.path(), &node).unwrap();
        assert_eq!(src.start_line, 2);
        assert_eq!(src.raw, "fn foo() {\n    return 1;\n}");
        assert_eq!(src.language, Language::Rust);
    }

    #[test]
    fn read_node_source_none_when_missing_range() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut node = Node::new(ArtifactId::new("rust::a.rs#foo"), NodeKind::RustFunction);
        node.path = Some("a.rs".to_string());
        node.start_line = None;
        node.end_line = None;
        assert!(read_node_source(tmp.path(), &node).is_none());
    }
}
