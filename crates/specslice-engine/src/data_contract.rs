//! P24 — data contract view (gap #3).
//!
//! A rewrite must reproduce two contracts the code graph never records:
//!
//! 1. **Persistence schema** — `CREATE TABLE` statements (table name +
//!    columns). These live *inside string literals* (`db.execute('CREATE
//!    TABLE …')`), so the scanner deliberately reads raw text.
//! 2. **Serialization keymap** — every `obj['key']` / `obj["key"]` subscript
//!    *and* every `obj.get("key"[, default])` map read in code, with the
//!    default applied via `?? <default>` (subscripts) or the second argument
//!    (`.get`). This is how Dart `fromJson` / `toJson` and Python
//!    `data.get(...)` encode the wire format; getting a key name or a default
//!    wrong silently corrupts data on the new platform.
//!
//! The keymap scan uses [`strip_noise`](crate::source_text::strip_noise) as
//! a *mask*: a code-level subscript shows up in the masked text as
//! `ident[" "]` (delimiters kept, key blanked), which lets us tell a real
//! `map['k']` access apart from a `['a','b']` array literal or a `['k']`
//! sequence sitting inside another string. The key text itself is then read
//! back from the raw source at the same character offsets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::language_traits::{is_code_symbol, language_of, Language};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::source_text::strip_noise;

pub const DATA_CONTRACT_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Data contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableColumn {
    pub name: String,
    /// The remainder of the column definition (type + constraints), trimmed.
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub path: String,
    pub line: u32,
    pub columns: Vec<TableColumn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySite {
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonKey {
    pub key: String,
    pub occurrences: usize,
    /// Distinct default expressions seen after `??` for this key (sorted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defaults: Vec<String>,
    pub sites: Vec<KeySite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DataContractStats {
    pub files_scanned: usize,
    pub tables: usize,
    pub json_keys_distinct: usize,
    pub json_key_refs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataContractReport {
    pub schema_version: u32,
    pub stats: DataContractStats,
    pub tables: Vec<TableSchema>,
    pub json_keys: Vec<JsonKey>,
}

#[derive(Debug, Clone)]
pub struct DataContractOptions {
    pub repo_root: PathBuf,
    /// Skip the JSON keymap scan (schema only).
    pub tables_only: bool,
    /// Skip the SQL schema scan (keymap only).
    pub keys_only: bool,
    pub max_sites_per_key: usize,
}

impl Default for DataContractOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            tables_only: false,
            keys_only: false,
            max_sites_per_key: 25,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn analyze_data_contract(options: DataContractOptions) -> Result<DataContractReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    analyze_data_contract_with_store(&store, &options)
}

pub fn analyze_data_contract_with_store(
    store: &Store,
    options: &DataContractOptions,
) -> Result<DataContractReport> {
    // Collect distinct code-bearing files + their language (from any code
    // node on that path). Deterministic order via BTreeMap.
    let mut files: BTreeMap<String, Language> = BTreeMap::new();
    for node in store.list_all_nodes()? {
        if !is_code_symbol(node.kind) {
            continue;
        }
        if let Some(path) = &node.path {
            files
                .entry(path.clone())
                .or_insert_with(|| language_of(node.kind));
        }
    }

    let mut stats = DataContractStats::default();
    let mut tables: Vec<TableSchema> = Vec::new();
    // key -> (occurrences, defaults set, sites)
    let mut keymap: BTreeMap<String, (usize, BTreeSet<String>, Vec<KeySite>)> = BTreeMap::new();

    for (path, lang) in &files {
        let abs = options.repo_root.join(path);
        // Skip a file that has grown past the index byte budget rather than
        // slurp it whole for the keymap scan (#245).
        if crate::source_text::is_oversized_source(&abs) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&abs) else {
            continue;
        };
        stats.files_scanned += 1;
        let raw: Vec<char> = text.chars().collect();
        let line_index = build_line_index(&raw);

        if !options.keys_only {
            for t in scan_tables(&raw, &line_index) {
                tables.push(TableSchema {
                    name: t.name,
                    path: path.clone(),
                    line: t.line,
                    columns: t.columns,
                });
            }
        }
        if !options.tables_only {
            let masked: Vec<char> = strip_noise(&text, *lang).chars().collect();
            let subscript_keys = scan_json_keys(&raw, &masked, &line_index);
            let get_keys = scan_get_call_keys(&raw, &masked, &line_index);
            for k in subscript_keys.into_iter().chain(get_keys) {
                let entry = keymap
                    .entry(k.key.clone())
                    .or_insert_with(|| (0, BTreeSet::new(), Vec::new()));
                entry.0 += 1;
                stats.json_key_refs += 1;
                if let Some(def) = k.default {
                    entry.1.insert(def);
                }
                if entry.2.len() < options.max_sites_per_key {
                    entry.2.push(KeySite {
                        path: path.clone(),
                        line: k.line,
                    });
                }
            }
        }
    }

    tables.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    stats.tables = tables.len();

    let mut json_keys: Vec<JsonKey> = keymap
        .into_iter()
        .map(|(key, (occ, defs, sites))| JsonKey {
            key,
            occurrences: occ,
            defaults: defs.into_iter().collect(),
            sites,
        })
        .collect();
    json_keys.sort_by(|a, b| b.occurrences.cmp(&a.occurrences).then(a.key.cmp(&b.key)));
    stats.json_keys_distinct = json_keys.len();

    Ok(DataContractReport {
        schema_version: DATA_CONTRACT_SCHEMA_VERSION,
        stats,
        tables,
        json_keys,
    })
}

// ---------------------------------------------------------------------------
// Line index
// ---------------------------------------------------------------------------

/// `line_index[i]` = 1-based line number of char `i`.
fn build_line_index(chars: &[char]) -> Vec<u32> {
    let mut out = Vec::with_capacity(chars.len());
    let mut line: u32 = 1;
    for &c in chars {
        out.push(line);
        if c == '\n' {
            line += 1;
        }
    }
    out
}

fn line_at(line_index: &[u32], idx: usize) -> u32 {
    line_index.get(idx).copied().unwrap_or(1)
}

// ---------------------------------------------------------------------------
// SQL CREATE TABLE scan
// ---------------------------------------------------------------------------

struct RawTable {
    name: String,
    line: u32,
    columns: Vec<TableColumn>,
}

const SQL_CONSTRAINT_LEADERS: &[&str] = &[
    "primary",
    "foreign",
    "unique",
    "check",
    "constraint",
    "key",
    "index",
];

fn scan_tables(raw: &[char], line_index: &[u32]) -> Vec<RawTable> {
    let mut out = Vec::new();
    let needle: Vec<char> = "create table".chars().collect();
    let mut i = 0usize;
    while i + needle.len() <= raw.len() {
        if matches_ci(raw, i, &needle) {
            let stmt_line = line_at(line_index, i);
            let mut j = i + needle.len();
            skip_ws(raw, &mut j);
            skip_ci_phrase(raw, &mut j, "if not exists");
            skip_ws(raw, &mut j);
            if let Some(name) = read_sql_name(raw, &mut j) {
                skip_ws(raw, &mut j);
                if raw.get(j).copied() == Some('(') {
                    if let Some((cols_text, end)) = balanced_parens(raw, j) {
                        let columns = parse_columns(&cols_text);
                        out.push(RawTable {
                            name,
                            line: stmt_line,
                            columns,
                        });
                        i = end;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

fn matches_ci(raw: &[char], at: usize, needle: &[char]) -> bool {
    if at + needle.len() > raw.len() {
        return false;
    }
    for (k, &nc) in needle.iter().enumerate() {
        if raw[at + k].to_ascii_lowercase() != nc {
            return false;
        }
    }
    true
}

fn skip_ws(raw: &[char], i: &mut usize) {
    while *i < raw.len() && raw[*i].is_whitespace() {
        *i += 1;
    }
}

fn skip_ci_phrase(raw: &[char], i: &mut usize, phrase: &str) {
    // phrase words separated by single spaces; tolerate arbitrary whitespace
    let words: Vec<&str> = phrase.split(' ').collect();
    let save = *i;
    for (wi, w) in words.iter().enumerate() {
        if wi > 0 {
            skip_ws(raw, i);
        }
        let wc: Vec<char> = w.chars().collect();
        if matches_ci(raw, *i, &wc) {
            *i += wc.len();
        } else {
            *i = save;
            return;
        }
    }
}

fn read_sql_name(raw: &[char], i: &mut usize) -> Option<String> {
    let open = raw.get(*i).copied();
    let (open_delim, close_delim) = match open {
        Some('"') => (Some('"'), '"'),
        Some('`') => (Some('`'), '`'),
        Some('[') => (Some('['), ']'),
        _ => (None, ' '),
    };
    if let Some(_od) = open_delim {
        *i += 1;
        let start = *i;
        while *i < raw.len() && raw[*i] != close_delim {
            *i += 1;
        }
        let name: String = raw[start..*i].iter().collect();
        if *i < raw.len() {
            *i += 1; // closing delim
        }
        // strip schema qualifier `main.foo`
        return Some(last_segment(&name));
    }
    let start = *i;
    while *i < raw.len() && (raw[*i].is_alphanumeric() || raw[*i] == '_' || raw[*i] == '.') {
        *i += 1;
    }
    if *i == start {
        return None;
    }
    let name: String = raw[start..*i].iter().collect();
    Some(last_segment(&name))
}

fn last_segment(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).to_string()
}

/// Given `raw[open] == '('`, return the inner text (excluding the outer
/// parens) and the index just past the matching `)`.
fn balanced_parens(raw: &[char], open: usize) -> Option<(Vec<char>, usize)> {
    debug_assert_eq!(raw.get(open).copied(), Some('('));
    let mut depth = 0i32;
    let mut i = open;
    let mut inner = Vec::new();
    while i < raw.len() {
        let c = raw[i];
        match c {
            '(' => {
                depth += 1;
                if depth > 1 {
                    inner.push(c);
                }
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((inner, i + 1));
                }
                inner.push(c);
            }
            _ => inner.push(c),
        }
        i += 1;
    }
    None
}

fn parse_columns(inner: &[char]) -> Vec<TableColumn> {
    // Drop string line-continuation backslashes (`\` immediately before a
    // newline), as used by multi-line Rust/C SQL string literals; the
    // newline itself stays and is treated as whitespace.
    let cleaned: Vec<char> = {
        let mut v = Vec::with_capacity(inner.len());
        let mut k = 0usize;
        while k < inner.len() {
            if inner[k] == '\\' && matches!(inner.get(k + 1), Some('\n') | Some('\r')) {
                k += 1;
                continue;
            }
            v.push(inner[k]);
            k += 1;
        }
        v
    };
    let mut out = Vec::new();
    for part in split_top_level_commas(&cleaned) {
        let joined: String = part.iter().collect();
        let trimmed = joined.trim();
        if trimmed.is_empty() {
            continue;
        }
        // first token = column name (or a constraint leader => skip)
        let mut name = String::new();
        let mut rest_start = 0usize;
        let pchars: Vec<char> = trimmed.chars().collect();
        // handle quoted column name
        if matches!(pchars.first(), Some('"') | Some('`') | Some('[')) {
            let mut k = 0usize;
            if let Some(n) = read_sql_name(&pchars, &mut k) {
                name = n;
                rest_start = k;
            }
        } else {
            let mut k = 0usize;
            while k < pchars.len() && (pchars[k].is_alphanumeric() || pchars[k] == '_') {
                k += 1;
            }
            name = pchars[..k].iter().collect();
            rest_start = k;
        }
        if name.is_empty() || SQL_CONSTRAINT_LEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        // Collapse internal whitespace/newlines so the definition reads on
        // one line.
        let definition: String = pchars[rest_start..]
            .iter()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        out.push(TableColumn { name, definition });
    }
    out
}

fn split_top_level_commas(inner: &[char]) -> Vec<Vec<char>> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = Vec::new();
    for &c in inner {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------------------
// JSON keymap scan
// ---------------------------------------------------------------------------

struct RawJsonKey {
    key: String,
    line: u32,
    default: Option<String>,
}

/// Detect code-level `ident['key']` subscripts using the masked text as a
/// guide; read the key back from `raw`.
fn scan_json_keys(raw: &[char], masked: &[char], line_index: &[u32]) -> Vec<RawJsonKey> {
    let mut out = Vec::new();
    let n = masked.len().min(raw.len());
    let mut i = 0usize;
    while i < n {
        if masked[i] != '[' {
            i += 1;
            continue;
        }
        // Must be a subscript: previous non-space *code* char is an
        // identifier / `)` / `]` (not `=`, `(`, `,`, `:` => array literal).
        let prev = prev_non_space(masked, i);
        let is_subscript =
            matches!(prev, Some(p) if p.is_alphanumeric() || p == '_' || p == ')' || p == ']');
        if !is_subscript {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        skip_ws_slice(masked, &mut j);
        let Some(q) = masked.get(j).copied() else {
            i += 1;
            continue;
        };
        if q != '"' && q != '\'' && q != '`' {
            i += 1;
            continue;
        }
        // The string content is blanked in masked; find the closing quote in
        // masked, then read the key from raw between the quotes.
        let key_start = j + 1;
        let mut k = key_start;
        while k < n && masked[k] != q {
            k += 1;
        }
        if k >= n {
            i += 1;
            continue;
        }
        let key: String = raw[key_start..k].iter().collect();
        let mut after = k + 1;
        skip_ws_slice(masked, &mut after);
        if masked.get(after).copied() != Some(']') {
            i += 1;
            continue;
        }
        after += 1;
        // optional `?? default`
        let mut default = None;
        let mut probe = after;
        skip_ws_slice(masked, &mut probe);
        if masked.get(probe).copied() == Some('?') && masked.get(probe + 1).copied() == Some('?') {
            probe += 2;
            skip_ws_slice(masked, &mut probe);
            let def_start = probe;
            while probe < n && !matches!(masked[probe], ',' | ';' | ')' | '\n' | '}') {
                probe += 1;
            }
            let def: String = raw[def_start..probe.min(raw.len())].iter().collect();
            let def = def.trim();
            if !def.is_empty() {
                default = Some(truncate_def(def));
            }
        }
        // Skip empty / placeholder keys (interpolations etc. collapse to "").
        let trimmed_key = key.trim();
        if !trimmed_key.is_empty() && is_plain_key(trimmed_key) {
            out.push(RawJsonKey {
                key: trimmed_key.to_string(),
                line: line_at(line_index, i),
                default,
            });
        }
        i = after;
    }
    out
}

/// Detect `.get("key")` / `.get('key', default)` map accesses — the idiomatic
/// dict/Map read in Python (`data.get("field", None)`), JS/Java (`map.get("k")`)
/// and friends — which subscript scanning alone misses. Mirrors
/// [`scan_json_keys`]: the masked text blanks string content, so the key is
/// read back from `raw` and the optional second argument becomes the default.
fn scan_get_call_keys(raw: &[char], masked: &[char], line_index: &[u32]) -> Vec<RawJsonKey> {
    let mut out = Vec::new();
    let n = masked.len().min(raw.len());
    let mut i = 0usize;
    while i + 4 <= n {
        // Match `.get` as a method name (word boundary after `get`).
        let is_get = masked[i] == '.'
            && masked.get(i + 1).copied() == Some('g')
            && masked.get(i + 2).copied() == Some('e')
            && masked.get(i + 3).copied() == Some('t')
            && !matches!(masked.get(i + 4), Some(c) if c.is_alphanumeric() || *c == '_');
        if !is_get {
            i += 1;
            continue;
        }
        let mut j = i + 4;
        skip_ws_slice(masked, &mut j);
        if masked.get(j).copied() != Some('(') {
            i += 1;
            continue;
        }
        let open_paren = j;
        j += 1;
        skip_ws_slice(masked, &mut j);
        let Some(q) = masked.get(j).copied() else {
            i += 1;
            continue;
        };
        if q != '"' && q != '\'' && q != '`' {
            i += 1;
            continue;
        }
        let key_start = j + 1;
        let mut k = key_start;
        while k < n && masked[k] != q {
            k += 1;
        }
        if k >= n {
            i += 1;
            continue;
        }
        let key: String = raw[key_start..k].iter().collect();
        let mut after = k + 1;
        skip_ws_slice(masked, &mut after);
        // The first argument must be the *only* argument (`.get("k")`) or be
        // followed by a single default (`.get("k", default)`); anything else
        // (e.g. `.get("k", a, b)`) is not a recognised map read.
        let mut default = None;
        match masked.get(after).copied() {
            Some(')') => {}
            Some(',') => {
                let def_start = after + 1;
                // Read the default up to the `)` that closes this `.get(`,
                // tracking nested parens so `.get("k", f(a, b))` is captured.
                let mut depth = 1usize;
                let mut p = open_paren + 1;
                while p < n {
                    match masked[p] {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    p += 1;
                }
                let def_end = p.min(raw.len());
                let ds = def_start.min(def_end);
                let def: String = raw[ds..def_end].iter().collect();
                let def = def.trim();
                if !def.is_empty() {
                    default = Some(truncate_def(def));
                }
            }
            _ => {
                i += 1;
                continue;
            }
        }
        let trimmed_key = key.trim();
        if !trimmed_key.is_empty() && is_plain_key(trimmed_key) {
            out.push(RawJsonKey {
                key: trimmed_key.to_string(),
                line: line_at(line_index, i),
                default,
            });
        }
        i = k + 1;
    }
    out
}

/// A "plain" map key: identifier-ish / dotted / snake — not an expression
/// (`$var`, interpolation, operators). Filters subscripts like `list[idx]`
/// that happened to use a string variable, and string-interpolated keys.
fn is_plain_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().all(|c| {
            c.is_alphanumeric() || c == '_' || c == '.' || c == '-' || c == ':' || c == ' '
        })
}

fn truncate_def(s: &str) -> String {
    const MAX: usize = 60;
    if s.chars().count() > MAX {
        let p: String = s.chars().take(MAX).collect();
        format!("{p}…")
    } else {
        s.to_string()
    }
}

fn prev_non_space(chars: &[char], from: usize) -> Option<char> {
    let mut k = from;
    while k > 0 {
        k -= 1;
        if !chars[k].is_whitespace() {
            return Some(chars[k]);
        }
    }
    None
}

fn skip_ws_slice(chars: &[char], i: &mut usize) {
    while *i < chars.len() && chars[*i].is_whitespace() {
        *i += 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(src: &str, lang: Language) -> Vec<RawJsonKey> {
        let raw: Vec<char> = src.chars().collect();
        let masked: Vec<char> = strip_noise(src, lang).chars().collect();
        let idx = build_line_index(&raw);
        scan_json_keys(&raw, &masked, &idx)
    }

    fn get_keys(src: &str, lang: Language) -> Vec<RawJsonKey> {
        let raw: Vec<char> = src.chars().collect();
        let masked: Vec<char> = strip_noise(src, lang).chars().collect();
        let idx = build_line_index(&raw);
        scan_get_call_keys(&raw, &masked, &idx)
    }

    #[test]
    fn scans_python_get_call_keys_with_defaults() {
        // Python `dict.get` is the idiomatic wire-format read; subscript
        // scanning alone is blind to it. Capture both the key and the default.
        let src = "def parse(data):\n    age = data.get(\"age\", 0)\n    city = payload.get('city')\n    n = items.get(\"count\", len(items))\n";
        let map: std::collections::HashMap<String, Option<String>> =
            get_keys(src, Language::Python)
                .into_iter()
                .map(|k| (k.key, k.default))
                .collect();
        assert_eq!(map.get("age"), Some(&Some("0".to_string())));
        assert_eq!(map.get("city"), Some(&None), "no default → None");
        assert_eq!(
            map.get("count"),
            Some(&Some("len(items)".to_string())),
            "default may itself be a call with nested parens/commas"
        );
    }

    #[test]
    fn get_call_ignores_non_string_and_string_literal_occurrences() {
        // `.get(idx)` (numeric/var) is not a string-keyed read; a `.get('x')`
        // sitting inside a string literal is masked away and must be skipped.
        let src = "v = arr.get(0)\nc = \"obj.get('fake')\"\nr = m.get('real')\n";
        let names: Vec<String> = get_keys(src, Language::Python)
            .into_iter()
            .map(|k| k.key)
            .collect();
        assert_eq!(names, vec!["real".to_string()]);
    }

    #[test]
    fn parses_create_table_columns() {
        let src = "await db.execute('CREATE TABLE shifts (id INTEGER PRIMARY KEY, name TEXT NOT NULL, color INTEGER, PRIMARY KEY(id))');";
        let raw: Vec<char> = src.chars().collect();
        let idx = build_line_index(&raw);
        let tables = scan_tables(&raw, &idx);
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.name, "shifts");
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        // `PRIMARY KEY(id)` row is a constraint, not a column.
        assert_eq!(names, vec!["id", "name", "color"]);
        assert_eq!(t.columns[0].definition, "INTEGER PRIMARY KEY");
        assert_eq!(t.columns[1].definition, "TEXT NOT NULL");
    }

    #[test]
    fn create_table_if_not_exists_and_quoted_name() {
        let src = "db.execute(\"CREATE TABLE IF NOT EXISTS \\\"my_tbl\\\" (a TEXT)\");";
        // The escaped quotes make this awkward; use a simpler raw form:
        let src2 = "CREATE TABLE if not exists `regulars` (rid INTEGER, label TEXT)";
        let _ = src;
        let raw: Vec<char> = src2.chars().collect();
        let idx = build_line_index(&raw);
        let tables = scan_tables(&raw, &idx);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "regulars");
        assert_eq!(tables[0].columns.len(), 2);
    }

    #[test]
    fn scans_json_keys_with_defaults() {
        let src = "factory Shift.fromJson(Map j) => Shift(\n  count: j['count'] ?? 0,\n  name: j[\"name\"],\n  flag: j['is_pro'] ?? false,\n);";
        let got = keys(src, Language::Dart);
        let map: std::collections::HashMap<String, Option<String>> =
            got.into_iter().map(|k| (k.key, k.default)).collect();
        assert_eq!(map.get("count"), Some(&Some("0".to_string())));
        assert_eq!(map.get("name"), Some(&None));
        assert_eq!(map.get("is_pro"), Some(&Some("false".to_string())));
    }

    #[test]
    fn array_literals_are_not_keys() {
        // `['a', 'b']` is a list literal (preceded by `=`), not a subscript.
        let src = "final xs = ['a', 'b'];\nfinal v = data['real_key'];";
        let got = keys(src, Language::Dart);
        let names: Vec<String> = got.into_iter().map(|k| k.key).collect();
        assert_eq!(names, vec!["real_key".to_string()]);
    }

    #[test]
    fn keys_inside_strings_are_ignored() {
        // The `['fake']` lives inside a string literal => masked => skipped.
        let src = "final s = \"obj['fake'] should be ignored\";\nfinal r = m['real'];";
        let got = keys(src, Language::Dart);
        let names: Vec<String> = got.into_iter().map(|k| k.key).collect();
        assert_eq!(names, vec!["real".to_string()]);
    }

    #[test]
    fn report_aggregates_keys_across_sites() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("lib")).unwrap();
        std::fs::write(
            tmp.path().join("lib/model.dart"),
            "class M {\n  factory M.fromJson(j) => M(c: j['count'] ?? 0);\n  Map toJson() => {'count': c};\n  int read(j) => j['count'] ?? 1;\n}",
        )
        .unwrap();
        let mut store = specslice_store::Store::open(tmp.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let mut n = specslice_core::Node::new(
            specslice_core::ArtifactId::new("dart::lib/model.dart#M"),
            specslice_core::NodeKind::DartClass,
        );
        n.path = Some("lib/model.dart".to_string());
        n.start_line = Some(1);
        n.end_line = Some(5);
        store.upsert_node(&n).unwrap();

        let report = analyze_data_contract_with_store(
            &store,
            &DataContractOptions {
                repo_root: tmp.path().to_path_buf(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.json_keys.len(), 1);
        let k = &report.json_keys[0];
        assert_eq!(k.key, "count");
        assert_eq!(k.occurrences, 2); // the two `j['count']` reads
                                      // two different defaults observed: 0 and 1
        assert_eq!(k.defaults, vec!["0".to_string(), "1".to_string()]);
    }
}
