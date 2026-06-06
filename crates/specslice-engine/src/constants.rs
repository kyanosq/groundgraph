//! P24 — constants & literals catalogue (gap #2).
//!
//! Magic values decide behaviour just as much as branches do: the free-tier
//! limit `3`, the lunar epoch, an alarm-id prefix string, a `0xFF6236FF`
//! colour. When porting a codebase you must reproduce every one of them, and
//! the code graph does not record literals at all.
//!
//! This module reads each code symbol's body and extracts integer / float /
//! string / bool / char literals with their **enclosing symbol + line**,
//! then groups them into a catalogue (value → occurrence sites). A literal
//! that appears in many places is exactly the kind of constant you must not
//! get wrong, so the catalogue is sorted by occurrence count.
//!
//! The scanner is language-aware about quotes (Rust `'a'` chars and `'static`
//! lifetimes are not Dart strings) and skips comments. Trivial values
//! (`0`, `1`, empty strings, bools, chars) are filtered out by default so the
//! catalogue stays focused on real magic values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::language_traits::{is_callable, is_type, Language};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::source_text::read_node_source;

pub const CONSTANTS_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Data contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiteralKind {
    Int,
    Float,
    Str,
    Bool,
    Char,
}

impl LiteralKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LiteralKind::Int => "int",
            LiteralKind::Float => "float",
            LiteralKind::Str => "str",
            LiteralKind::Bool => "bool",
            LiteralKind::Char => "char",
        }
    }
}

/// One occurrence of a literal at a specific source site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiteralSite {
    pub symbol_id: String,
    pub symbol_name: Option<String>,
    pub path: Option<String>,
    pub line: u32,
}

/// A distinct literal value and everywhere it appears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstantEntry {
    pub value: String,
    pub kind: LiteralKind,
    pub occurrences: usize,
    pub sites: Vec<LiteralSite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConstantsStats {
    pub analyzed: usize,
    pub with_source: usize,
    pub total_literals: usize,
    pub distinct_values: usize,
    pub returned: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstantsReport {
    pub schema_version: u32,
    pub stats: ConstantsStats,
    pub entries: Vec<ConstantEntry>,
}

#[derive(Debug, Clone)]
pub struct ConstantsOptions {
    pub repo_root: PathBuf,
    pub include_types: bool,
    /// Keep `0` / `1` / empty strings / bools / chars (default `false`).
    pub include_trivial: bool,
    /// Only report values appearing at least this many times (default `1`).
    pub min_occurrences: usize,
    /// Restrict to a single literal kind.
    pub kind_filter: Option<LiteralKind>,
    pub max_entries: usize,
    pub max_sites_per_entry: usize,
}

impl Default for ConstantsOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            include_types: false,
            include_trivial: false,
            min_occurrences: 1,
            kind_filter: None,
            max_entries: 0,
            max_sites_per_entry: 25,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn analyze_constants(options: ConstantsOptions) -> Result<ConstantsReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    analyze_constants_with_store(&store, &options)
}

pub fn analyze_constants_with_store(
    store: &Store,
    options: &ConstantsOptions,
) -> Result<ConstantsReport> {
    // group key: (kind, value) -> ConstantEntry-in-progress
    let mut grouped: BTreeMap<(LiteralKind, String), ConstantEntry> = BTreeMap::new();
    let mut stats = ConstantsStats::default();

    for node in store.list_all_nodes()? {
        let eligible = is_callable(node.kind) || (options.include_types && is_type(node.kind));
        if !eligible {
            continue;
        }
        stats.analyzed += 1;
        let Some(src) = read_node_source(&options.repo_root, &node) else {
            continue;
        };
        stats.with_source += 1;

        for lit in scan_literals(&src.raw, src.language) {
            if !options.include_trivial && is_trivial(lit.kind, &lit.value) {
                continue;
            }
            if let Some(filter) = options.kind_filter {
                if lit.kind != filter {
                    continue;
                }
            }
            stats.total_literals += 1;
            let entry = grouped
                .entry((lit.kind, lit.value.clone()))
                .or_insert_with(|| ConstantEntry {
                    value: lit.value.clone(),
                    kind: lit.kind,
                    occurrences: 0,
                    sites: Vec::new(),
                });
            entry.occurrences += 1;
            if entry.sites.len() < options.max_sites_per_entry {
                entry.sites.push(LiteralSite {
                    symbol_id: node.id.to_string(),
                    symbol_name: node.name.clone(),
                    path: node.path.clone(),
                    line: src.start_line + lit.line_offset,
                });
            }
        }
    }

    let mut entries: Vec<ConstantEntry> = grouped
        .into_values()
        .filter(|e| e.occurrences >= options.min_occurrences.max(1))
        .collect();
    stats.distinct_values = entries.len();

    // Most-repeated first; ties broken deterministically by kind then value.
    entries.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then(a.kind.cmp(&b.kind))
            .then(a.value.cmp(&b.value))
    });

    stats.returned = entries.len();
    if options.max_entries > 0 && entries.len() > options.max_entries {
        entries.truncate(options.max_entries);
        stats.truncated = true;
        stats.returned = entries.len();
    }

    Ok(ConstantsReport {
        schema_version: CONSTANTS_SCHEMA_VERSION,
        stats,
        entries,
    })
}

// ---------------------------------------------------------------------------
// Literal scanner
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLiteral {
    /// Line offset (0-based) from the start of the scanned span.
    pub line_offset: u32,
    pub kind: LiteralKind,
    /// Normalised display value. Strings keep their delimiters; numbers keep
    /// their original spelling (`0xFF`, `1_000`, `3.14`).
    pub value: String,
}

const MAX_STR_LEN: usize = 120;

/// Extract literals from a source span. Language-aware about single quotes
/// (string vs char/lifetime) and comment syntax. Heuristic, deterministic,
/// dependency-free.
pub fn scan_literals(src: &str, lang: Language) -> Vec<RawLiteral> {
    let hash = matches!(lang, Language::Python);
    let single_is_string = matches!(lang, Language::Dart | Language::Python | Language::Typescript);
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line: u32 = 0;
    let mut prev_word = false; // previous code char was [A-Za-z0-9_]

    while i < n {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let next2 = chars.get(i + 2).copied();

        if c == '\n' {
            line += 1;
            i += 1;
            prev_word = false;
            continue;
        }
        // Comments -------------------------------------------------------
        if hash && c == '#' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if !hash && c == '/' && next == Some('/') {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if !hash && c == '/' && next == Some('*') {
            i += 2;
            while i < n && !(chars[i] == '*' && chars.get(i + 1).copied() == Some('/')) {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2; // skip closing */ (saturates harmlessly at EOF)
            prev_word = false;
            continue;
        }
        // Triple-quoted strings -----------------------------------------
        if (c == '"' || c == '\'') && next == Some(c) && next2 == Some(c) {
            let start_line = line;
            let delim = c;
            i += 3;
            let mut buf = String::new();
            while i < n
                && !(chars[i] == delim
                    && chars.get(i + 1).copied() == Some(delim)
                    && chars.get(i + 2).copied() == Some(delim))
            {
                if chars[i] == '\n' {
                    line += 1;
                }
                buf.push(chars[i]);
                i += 1;
            }
            i += 3;
            out.push(RawLiteral {
                line_offset: start_line,
                kind: LiteralKind::Str,
                value: format!("{delim}{delim}{delim}{}{delim}{delim}{delim}", truncate(&buf)),
            });
            prev_word = false;
            continue;
        }
        // Double / backtick strings (all langs); single quote depends -----
        if c == '"' || c == '`' || (c == '\'' && single_is_string) {
            let start_line = line;
            let delim = c;
            i += 1;
            let mut buf = String::new();
            while i < n && chars[i] != delim {
                if chars[i] == '\\' {
                    buf.push(chars[i]);
                    i += 1;
                    if i < n && chars[i] != '\n' {
                        buf.push(chars[i]);
                        i += 1;
                    }
                    continue;
                }
                if chars[i] == '\n' {
                    line += 1;
                }
                buf.push(chars[i]);
                i += 1;
            }
            i += 1; // closing delim
            out.push(RawLiteral {
                line_offset: start_line,
                kind: LiteralKind::Str,
                value: format!("{delim}{}{delim}", truncate(&buf)),
            });
            prev_word = false;
            continue;
        }
        // Single quote in char/lifetime languages ------------------------
        if c == '\'' && !single_is_string {
            // `'a'` or `'\n'` => char; otherwise a lifetime/label => skip.
            if next == Some('\\') {
                // escaped char literal: '\?'
                if let Some(close_off) = find_char_close(&chars, i + 2) {
                    let value: String = chars[i..=close_off].iter().collect();
                    out.push(RawLiteral {
                        line_offset: line,
                        kind: LiteralKind::Char,
                        value: truncate(&value),
                    });
                    i = close_off + 1;
                    prev_word = false;
                    continue;
                }
            } else if next2 == Some('\'') {
                let value: String = chars[i..=i + 2].iter().collect();
                out.push(RawLiteral {
                    line_offset: line,
                    kind: LiteralKind::Char,
                    value,
                });
                i += 3;
                prev_word = false;
                continue;
            }
            // lifetime / label: consume just the quote
            i += 1;
            prev_word = false;
            continue;
        }
        // Numbers --------------------------------------------------------
        let starts_number = (c.is_ascii_digit() && !prev_word)
            || (c == '.' && !prev_word && next.map_or(false, |x| x.is_ascii_digit()));
        if starts_number {
            let start = i;
            let mut seen_dot = c == '.';
            let mut seen_exp = false;
            i += 1;
            while i < n {
                let d = chars[i];
                if d.is_ascii_alphanumeric() || d == '_' {
                    if d == 'e' || d == 'E' {
                        seen_exp = true;
                    }
                    i += 1;
                } else if d == '.' && !seen_dot && chars.get(i + 1).map_or(false, |x| x.is_ascii_digit())
                {
                    seen_dot = true;
                    i += 1;
                } else if (d == '+' || d == '-') && seen_exp && matches!(chars.get(i - 1), Some('e') | Some('E')) {
                    i += 1;
                } else {
                    break;
                }
            }
            let value: String = chars[start..i].iter().collect();
            let kind = classify_number(&value, seen_dot);
            out.push(RawLiteral {
                line_offset: line,
                kind,
                value,
            });
            prev_word = true;
            continue;
        }
        // Identifiers (bool detection) -----------------------------------
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if word == "true" || word == "false" {
                out.push(RawLiteral {
                    line_offset: line,
                    kind: LiteralKind::Bool,
                    value: word,
                });
            }
            prev_word = true;
            continue;
        }
        // Anything else
        prev_word = false;
        i += 1;
    }
    out
}

fn find_char_close(chars: &[char], from: usize) -> Option<usize> {
    // expects an escaped char like '\n' '\\' '\'' — closing quote within a
    // couple chars.
    let mut j = from;
    while j < chars.len() && j < from + 3 {
        if chars[j] == '\'' {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn classify_number(value: &str, seen_dot: bool) -> LiteralKind {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("0x") || lower.starts_with("0b") || lower.starts_with("0o") {
        return LiteralKind::Int;
    }
    if seen_dot {
        return LiteralKind::Float;
    }
    // exponent without dot (1e9) is still a float
    if lower.contains('e') && lower.trim_start_matches(|c: char| c.is_ascii_digit()).starts_with('e')
    {
        return LiteralKind::Float;
    }
    LiteralKind::Int
}

fn truncate(s: &str) -> String {
    let cleaned = s.replace('\n', "\\n").replace('\r', "");
    if cleaned.chars().count() > MAX_STR_LEN {
        let prefix: String = cleaned.chars().take(MAX_STR_LEN).collect();
        format!("{prefix}…")
    } else {
        cleaned
    }
}

/// Values too common to be interesting magic constants.
fn is_trivial(kind: LiteralKind, value: &str) -> bool {
    match kind {
        LiteralKind::Bool | LiteralKind::Char => true,
        LiteralKind::Int => {
            let normal: String = value
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>();
            matches!(normal.as_str(), "0" | "1" | "")
        }
        LiteralKind::Float => matches!(value, "0.0" | "1.0" | "0." | "1."),
        LiteralKind::Str => {
            let inner = value.trim_matches(|c| c == '"' || c == '\'' || c == '`');
            inner.trim().is_empty()
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace helpers
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
    let raw = Path::new(&config.storage.path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{ArtifactId, Node, NodeKind};

    fn vals(src: &str, lang: Language) -> Vec<(LiteralKind, String)> {
        scan_literals(src, lang)
            .into_iter()
            .map(|l| (l.kind, l.value))
            .collect()
    }

    #[test]
    fn scans_ints_floats_strings_bools() {
        let src = "let a = 42; let b = 3.14; let s = \"hello\"; let ok = true;";
        let got = vals(src, Language::Rust);
        assert!(got.contains(&(LiteralKind::Int, "42".to_string())));
        assert!(got.contains(&(LiteralKind::Float, "3.14".to_string())));
        assert!(got.contains(&(LiteralKind::Str, "\"hello\"".to_string())));
        assert!(got.contains(&(LiteralKind::Bool, "true".to_string())));
    }

    #[test]
    fn ignores_digits_inside_identifiers_and_type_suffix() {
        // `u32`, `i64`, `x1` must not become Int literals; `0xFF6236FF` must.
        let src = "let c: u32 = 0xFF6236FF; let x1 = read_i64();";
        let got = vals(src, Language::Rust);
        let ints: Vec<&String> = got
            .iter()
            .filter(|(k, _)| *k == LiteralKind::Int)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(ints, vec![&"0xFF6236FF".to_string()]);
    }

    #[test]
    fn rust_lifetime_is_not_a_char_literal() {
        let src = "fn f<'a>(x: &'a str) -> char { 'z' }";
        let got = vals(src, Language::Rust);
        let chars: Vec<&String> = got
            .iter()
            .filter(|(k, _)| *k == LiteralKind::Char)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(chars, vec![&"'z'".to_string()]);
    }

    #[test]
    fn dart_single_quotes_are_strings() {
        let src = "final t = 'alarm.title'; final n = 'x';";
        let got = vals(src, Language::Dart);
        assert!(got.contains(&(LiteralKind::Str, "'alarm.title'".to_string())));
        // 'x' is a string here, not a char
        assert!(got.iter().all(|(k, _)| *k != LiteralKind::Char));
    }

    #[test]
    fn comments_and_string_contents_are_not_mined_for_numbers() {
        let src = "let a = 7; // 99 magic\nlet s = \"port 8080\";";
        let got = vals(src, Language::Rust);
        let ints: Vec<&String> = got
            .iter()
            .filter(|(k, _)| *k == LiteralKind::Int)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(ints, vec![&"7".to_string()]);
        // the string is captured whole, but `8080` inside is not a separate int
        assert!(got.contains(&(LiteralKind::Str, "\"port 8080\"".to_string())));
    }

    #[test]
    fn report_groups_and_filters_trivial() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "fn limits() -> i32 {\n    let free = 3;\n    let again = 3;\n    let one = 1;\n    return free + again + one;\n}",
        )
        .unwrap();
        let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let mut n = Node::new(
            ArtifactId::new("rust::src/lib.rs#limits"),
            NodeKind::RustFunction,
        );
        n.path = Some("src/lib.rs".to_string());
        n.name = Some("limits".to_string());
        n.start_line = Some(1);
        n.end_line = Some(6);
        store.upsert_node(&n).unwrap();

        let report =
            analyze_constants_with_store(&store, &ConstantsOptions {
                repo_root: tmp.path().to_path_buf(),
                ..Default::default()
            })
            .unwrap();
        // `1` is trivial and filtered; `3` survives with 2 occurrences.
        assert_eq!(report.entries.len(), 1);
        let e = &report.entries[0];
        assert_eq!(e.value, "3");
        assert_eq!(e.kind, LiteralKind::Int);
        assert_eq!(e.occurrences, 2);
        assert_eq!(e.sites.len(), 2);
        assert_eq!(e.sites[0].line, 2);
        assert_eq!(e.sites[1].line, 3);
    }
}
