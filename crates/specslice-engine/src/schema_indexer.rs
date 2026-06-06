//! P25 — DB schema indexer: turn the persistence contract into graph nodes.
//!
//! Two sources, one logical table name (so a Java service and its Go rewrite
//! line up by table):
//!
//! 1. **`CREATE TABLE`** in `.sql` files (the Go side's `schema.sql`).
//! 2. **ORM annotations** in `.java` entities — MyBatis-Plus `@TableName("x")`
//!    (+ `@TableId` / `@TableField`) and JPA `@Table(name = "x")`. Columns are
//!    the entity's persisted fields, camelCase → snake_case (MyBatis-Plus's
//!    default), with `@TableField(exist = false)` and `static` fields dropped.
//!
//! Each table becomes a [`NodeKind::DbTable`] node whose `metadata_json`
//! carries the columns, so `graph-equiv` can audit table/column parity.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use specslice_store::Store;
use walkdir::WalkDir;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::source_text::read_node_source;

/// Directory names never worth scanning for schema.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".specslice",
    "target",
    "build",
    "node_modules",
    "vendor",
    ".dart_tool",
    "Pods",
];

// ---------------------------------------------------------------------------
// Parsed model + node metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedColumn {
    pub name: String,
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTable {
    /// Logical table name (the key both repos share).
    pub name: String,
    pub columns: Vec<ParsedColumn>,
    /// `"sql"` | `"orm"`.
    pub source: &'static str,
    pub line: u32,
}

/// Serialized into [`Node::metadata_json`] for a `DbTable` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbColumnMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbTableMeta {
    pub columns: Vec<DbColumnMeta>,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaIndexStats {
    pub files_scanned: usize,
    pub sql_tables: usize,
    pub orm_tables: usize,
    pub columns: usize,
    /// MyBatis mapper `<select|insert|update|delete>` statements indexed.
    pub mapper_stmts: usize,
    /// `mapper-interface method --references--> SqlMapperStmt` edges linked.
    pub stmt_method_edges: usize,
    /// `SqlMapperStmt --persists_to--> DbTable` edges linked.
    pub stmt_table_edges: usize,
    /// `interface method --declares_implementation--> impl method` edges linked
    /// (Spring `I<Name>` ↔ `<Name>Impl` convention) so traversal descends
    /// through interface dispatch instead of dead-ending at the declaration.
    pub iface_impl_edges: usize,
    /// `callable --persists_to--> DbTable` edges linked from *inline* SQL string
    /// literals in non-Java method/function bodies (Go/Dart/TS/Python/Rust/…).
    /// This is what lets `trace` reach tables for repos that embed SQL directly
    /// instead of using MyBatis XML mappers.
    pub inline_sql_table_edges: usize,
}

/// One MyBatis mapper statement (`<select|insert|update|delete id="...">`).
/// The raw SQL body is the porting "bible": searching the graph for the mapper
/// method name now returns the actual SQL, not just the Java interface stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMapperStmt {
    /// The statement `id` attribute (matches the Java mapper-interface method).
    pub id: String,
    /// `"select" | "insert" | "update" | "delete"`.
    pub stmt_kind: String,
    /// Mapper `namespace` (the Java interface FQN), when present.
    pub namespace: Option<String>,
    /// Inner SQL text (tags stripped of attributes; entity refs kept verbatim).
    pub sql: String,
    /// 1-based line of the opening tag.
    pub line: u32,
}

/// Serialized into [`Node::metadata_json`] for a `SqlMapperStmt` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapperStmtMeta {
    pub stmt_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub sql: String,
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

/// Open the repo's configured graph.db and add/refresh its `DbTable` nodes by
/// scanning `.sql` + entity `.java` files. Idempotent (upsert).
pub fn index_schema(repo_root: &Path) -> Result<SchemaIndexStats> {
    let config = load_config(repo_root)?;
    let db_path = resolve_storage_path(repo_root, &config);
    let mut store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    store.migrate().context("migrating graph.db")?;
    index_schema_into(&mut store, repo_root)
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

pub fn index_schema_into(store: &mut Store, root: &Path) -> Result<SchemaIndexStats> {
    let mut stats = SchemaIndexStats::default();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e.file_name().to_str().unwrap_or("")))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        // MyBatis mapper XML: index each statement as a SqlMapperStmt node so
        // the SQL becomes searchable graph evidence (porting bible).
        if ext == "xml" {
            let Some(stmts) = read_and_xml(path) else {
                continue;
            };
            stats.files_scanned += 1;
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            for s in stmts {
                stats.mapper_stmts += 1;
                store
                    .upsert_node(&mapper_stmt_node(&rel, &s))
                    .with_context(|| format!("upserting mapper stmt {} from {rel}", s.id))?;
            }
            continue;
        }
        let tables = match ext.as_str() {
            "sql" => read_and(path, parse_sql_tables),
            "java" => read_and(path, parse_java_entity_tables),
            _ => continue,
        };
        let Some(tables) = tables else { continue };
        stats.files_scanned += 1;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for t in tables {
            match t.source {
                "sql" => stats.sql_tables += 1,
                _ => stats.orm_tables += 1,
            }
            stats.columns += t.columns.len();
            store
                .upsert_node(&db_table_node(&rel, &t))
                .with_context(|| format!("upserting table {} from {rel}", t.name))?;
        }
    }
    link_data_layer_edges(store, &mut stats)?;
    link_inline_sql_edges(store, root, &mut stats)?;
    Ok(stats)
}

/// Stitch the data layer into the call graph so an endpoint subgraph reaches
/// all the way to the tables it touches:
///
/// * `mapper-interface method --references--> SqlMapperStmt` — matched by the
///   statement's `namespace` simple-name + `id` against the Java method node's
///   id suffix (`...::CraftConflictMapper.selectConflictListTreeByCloth`).
/// * `SqlMapperStmt --persists_to--> DbTable` — matched by table names parsed
///   from the statement SQL against `DbTable` node names.
///
/// Both edge kinds are in [`crate::search::EXPANSION_EDGE_KINDS`], so the
/// existing controller→service→impl→mapper traversal now extends to the SQL
/// and the tables. Idempotent (upsert). Java methods have no `name` field, so
/// they are keyed by their id suffix; tables/stmts carry `name`.
fn link_data_layer_edges(store: &mut Store, stats: &mut SchemaIndexStats) -> Result<()> {
    use std::collections::HashMap;

    // id-suffix (`SimpleClass.method`, lower-cased) -> method node ids, plus a
    // flat list keeping original case for the interface→impl convention match.
    let mut method_by_suffix: HashMap<String, Vec<ArtifactId>> = HashMap::new();
    let mut java_methods: Vec<(String, ArtifactId)> = Vec::new();
    for m in store.list_nodes_by_kind(NodeKind::JavaMethod)? {
        let id = m.id.as_str();
        if let Some(suffix) = id.rsplit("::").next() {
            method_by_suffix
                .entry(suffix.to_ascii_lowercase())
                .or_default()
                .push(m.id.clone());
            java_methods.push((suffix.to_string(), m.id.clone()));
        }
    }

    let mut edges: Vec<EdgeAssertion> = Vec::new();

    // interface → impl edges, two Java/Spring conventions (both require the
    // paired method to actually exist, so no bogus edge is emitted; a dedup set
    // prevents double-counting when the two conventions ever overlap):
    //   A) C#/legacy `I<Core>` interface ⇒ `<Core>Impl`   (ICraftService→CraftServiceImpl)
    //   B) dominant Spring `<Name>` interface ⇒ `<Name>Impl` (DictSystemService→DictSystemServiceImpl)
    // Convention B is what real Spring services overwhelmingly use; only relying
    // on A made polyglot/Spring repos report near-zero interface→impl coverage.
    let mut iface_impl_seen: std::collections::HashSet<(ArtifactId, ArtifactId)> =
        std::collections::HashSet::new();
    for (suffix, node_id) in &java_methods {
        let Some((class, method)) = suffix.rsplit_once('.') else {
            continue;
        };
        // A: this node is the `I<Core>` interface; pair with `<Core>Impl`.
        if is_interface_class_name(class) {
            let impl_key = format!("{}Impl.{method}", &class[1..]).to_ascii_lowercase();
            if let Some(impl_ids) = method_by_suffix.get(&impl_key) {
                for impl_id in impl_ids {
                    if node_id != impl_id
                        && iface_impl_seen.insert((node_id.clone(), impl_id.clone()))
                    {
                        edges.push(EdgeAssertion::fact(
                            node_id.clone(),
                            impl_id.clone(),
                            EdgeKind::DeclaresImplementation,
                            EdgeSource::LanguageAdapter,
                        ));
                        stats.iface_impl_edges += 1;
                    }
                }
            }
        }
        // B: this node is the `<Name>Impl` impl; pair with interface `<Name>`.
        if let Some(core) = class.strip_suffix("Impl").filter(|c| !c.is_empty()) {
            let iface_key = format!("{core}.{method}").to_ascii_lowercase();
            if let Some(iface_ids) = method_by_suffix.get(&iface_key) {
                for iface_id in iface_ids {
                    if iface_id != node_id
                        && iface_impl_seen.insert((iface_id.clone(), node_id.clone()))
                    {
                        edges.push(EdgeAssertion::fact(
                            iface_id.clone(),
                            node_id.clone(),
                            EdgeKind::DeclaresImplementation,
                            EdgeSource::LanguageAdapter,
                        ));
                        stats.iface_impl_edges += 1;
                    }
                }
            }
        }
    }
    // table name (lower-cased) -> table node ids.
    let mut table_by_name: HashMap<String, Vec<ArtifactId>> = HashMap::new();
    for t in store.list_nodes_by_kind(NodeKind::DbTable)? {
        if let Some(name) = &t.name {
            table_by_name
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(t.id.clone());
        }
    }

    for stmt in store.list_nodes_by_kind(NodeKind::SqlMapperStmt)? {
        let Some(meta_json) = &stmt.metadata_json else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<MapperStmtMeta>(meta_json) else {
            continue;
        };
        // method link: <namespace-simple-name>.<stmt-id>
        if let (Some(ns), Some(stmt_id)) = (&meta.namespace, &stmt.name) {
            let simple = ns.rsplit('.').next().unwrap_or(ns);
            let key = format!("{simple}.{stmt_id}").to_ascii_lowercase();
            if let Some(method_ids) = method_by_suffix.get(&key) {
                for mid in method_ids {
                    edges.push(EdgeAssertion::fact(
                        mid.clone(),
                        stmt.id.clone(),
                        EdgeKind::References,
                        EdgeSource::LanguageAdapter,
                    ));
                    stats.stmt_method_edges += 1;
                }
            }
        }
        // table links: each table parsed out of the SQL.
        for table in extract_sql_table_refs(&meta.sql) {
            if let Some(table_ids) = table_by_name.get(&table) {
                for tid in table_ids {
                    edges.push(EdgeAssertion::fact(
                        stmt.id.clone(),
                        tid.clone(),
                        EdgeKind::PersistsTo,
                        EdgeSource::LanguageAdapter,
                    ));
                    stats.stmt_table_edges += 1;
                }
            }
        }
    }
    for edge in &edges {
        store.upsert_edge(edge).with_context(|| {
            format!(
                "linking data-layer edge {} -> {}",
                edge.from_id.as_str(),
                edge.to_id.as_str()
            )
        })?;
    }
    Ok(())
}

/// Language-agnostic data-layer linker for **inline SQL**: any callable code
/// symbol whose *body* embeds SQL referencing a known table gets a
/// `callable --persists_to--> DbTable` edge. This is the Go/Dart/TS/Python/Rust
/// counterpart to the MyBatis mapper path above — those languages keep their
/// SQL as string literals in the method body rather than in XML, so the only
/// way to reach the tables is to read the body span and parse the SQL.
///
/// Java is intentionally skipped (the MyBatis `SqlMapperStmt` path already links
/// it, and re-scanning every Java method body would be wasted work).
///
/// Safety: a table edge is only emitted when the parsed table name matches an
/// existing `DbTable` node, so the deliberately-tolerant SQL scanner cannot
/// invent tables out of ordinary identifiers. Idempotent (upsert), and each
/// `(callable, table)` pair is emitted at most once.
fn link_inline_sql_edges(
    store: &mut Store,
    root: &Path,
    stats: &mut SchemaIndexStats,
) -> Result<()> {
    use std::collections::{HashMap, HashSet};

    // table name (lower-cased) -> table node ids. No tables ⇒ nothing to link.
    let mut table_by_name: HashMap<String, Vec<ArtifactId>> = HashMap::new();
    for t in store.list_nodes_by_kind(NodeKind::DbTable)? {
        if let Some(name) = &t.name {
            table_by_name
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(t.id.clone());
        }
    }
    if table_by_name.is_empty() {
        return Ok(());
    }

    // Cache file contents so a file with many callables is read once.
    let mut edges: Vec<EdgeAssertion> = Vec::new();
    for &kind in NodeKind::ALL {
        if !kind.is_callable() || kind.language() == Some("java") {
            continue;
        }
        for node in store.list_nodes_by_kind(kind)? {
            let Some(src) = read_node_source(root, &node) else {
                continue;
            };
            let mut linked: HashSet<String> = HashSet::new();
            for table in extract_sql_table_refs(&src.raw) {
                if !linked.insert(table.clone()) {
                    continue; // already linked this table for this callable
                }
                if let Some(table_ids) = table_by_name.get(&table) {
                    for tid in table_ids {
                        edges.push(EdgeAssertion::fact(
                            node.id.clone(),
                            tid.clone(),
                            EdgeKind::PersistsTo,
                            EdgeSource::LanguageAdapter,
                        ));
                        stats.inline_sql_table_edges += 1;
                    }
                }
            }
        }
    }
    for edge in &edges {
        store.upsert_edge(edge).with_context(|| {
            format!(
                "linking inline-sql edge {} -> {}",
                edge.from_id.as_str(),
                edge.to_id.as_str()
            )
        })?;
    }
    Ok(())
}

fn read_and_xml(path: &Path) -> Option<Vec<ParsedMapperStmt>> {
    let text = std::fs::read_to_string(path).ok()?;
    let stmts = parse_mapper_stmts(&text);
    if stmts.is_empty() {
        None
    } else {
        Some(stmts)
    }
}

/// Build a `SqlMapperStmt` node for one mapper statement. The id is namespaced
/// by file + statement id so the same method name in different mappers stays
/// distinct; `name` is the bare statement id so `search` matches it like a
/// method name.
pub fn mapper_stmt_node(rel_path: &str, stmt: &ParsedMapperStmt) -> Node {
    let id = ArtifactId::new(format!("sql_mapper::{rel_path}::{}", stmt.id));
    let mut node = Node::new(id, NodeKind::SqlMapperStmt);
    node.name = Some(stmt.id.clone());
    node.path = Some(rel_path.to_string());
    node.source_file = Some(rel_path.to_string());
    node.start_line = Some(stmt.line);
    node.indexer = Some("schema".to_string());
    let meta = MapperStmtMeta {
        stmt_kind: stmt.stmt_kind.clone(),
        namespace: stmt.namespace.clone(),
        sql: stmt.sql.clone(),
    };
    node.metadata_json = serde_json::to_string(&meta).ok();
    node
}

/// Parse MyBatis mapper XML into its CRUD statements. Deliberately tolerant
/// (no full XML parser): finds `<mapper namespace="...">` then each
/// `<select|insert|update|delete ... id="x" ...> BODY </tag>`. Non-mapper XML
/// (pom.xml, layouts) yields nothing because it has no such tags with an `id`.
pub fn parse_mapper_stmts(text: &str) -> Vec<ParsedMapperStmt> {
    let lower = text.to_ascii_lowercase();
    // Only treat files that look like a MyBatis mapper.
    if !lower.contains("<mapper") {
        return Vec::new();
    }
    let namespace = extract_attr(text, &lower, lower.find("<mapper").unwrap_or(0), "namespace");
    let mut out = Vec::new();
    for tag in ["select", "insert", "update", "delete"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(&open) {
            let start = from + rel;
            // Ensure it's a tag boundary (next char is space, newline, or '>').
            let after = start + open.len();
            let boundary = lower.as_bytes().get(after).copied();
            if !matches!(boundary, Some(b' ') | Some(b'\n') | Some(b'\r') | Some(b'\t') | Some(b'>'))
            {
                from = after;
                continue;
            }
            let Some(gt_rel) = lower[start..].find('>') else {
                break;
            };
            let open_end = start + gt_rel + 1;
            let id = extract_attr(text, &lower, start, "id");
            let Some(close_rel) = lower[open_end..].find(&close) else {
                from = open_end;
                continue;
            };
            let body_end = open_end + close_rel;
            from = body_end + close.len();
            let Some(id) = id else { continue };
            out.push(ParsedMapperStmt {
                id,
                stmt_kind: tag.to_string(),
                namespace: namespace.clone(),
                sql: text[open_end..body_end].trim().to_string(),
                line: line_of(text, start),
            });
        }
    }
    out.sort_by_key(|s| s.line);
    out
}

/// True for a Java interface class name following the `I<Upper>…` convention
/// (`ICraftService`, `IOrderService`) — used to pair interfaces with their
/// `<Name>Impl` implementations. Requires the 2nd char to be uppercase so
/// ordinary names like `Image` / `Item` are not mistaken for interfaces.
fn is_interface_class_name(class: &str) -> bool {
    let mut chars = class.chars();
    match (chars.next(), chars.next()) {
        (Some('I'), Some(second)) => second.is_ascii_uppercase(),
        _ => false,
    }
}

/// Extract the table names a SQL statement reads/writes, best-effort: the
/// identifier following `from` / `join` / `update` / `into`. Aliases, backticks,
/// and `schema.` prefixes are stripped; subqueries (`from (`) are skipped.
/// Lower-cased for matching `DbTable` node names. Deliberately tolerant — this
/// is evidence linking, not a SQL parser.
pub fn extract_sql_table_refs(sql: &str) -> Vec<String> {
    // Pad with a leading space so a statement-initial keyword (`update x`,
    // `insert into x`) still matches the ` <kw> ` patterns.
    let lower = format!(" {}", sql.to_ascii_lowercase());
    let bytes = lower.as_bytes();
    let mut out: Vec<String> = Vec::new();
    for kw in [" from ", " join ", " update ", " into "] {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(kw) {
            let mut i = from + rel + kw.len();
            from = i;
            // Skip whitespace.
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Subquery / expression — not a table name.
            if i >= bytes.len() || bytes[i] == b'(' {
                continue;
            }
            // Read a (possibly backtick / schema-qualified) identifier.
            let start = i;
            while i < bytes.len() {
                let c = bytes[i];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'`' {
                    i += 1;
                } else {
                    break;
                }
            }
            if i == start {
                continue;
            }
            let raw = &lower[start..i];
            // Keep only the table part of `schema.table`, drop backticks.
            let name = raw
                .rsplit('.')
                .next()
                .unwrap_or(raw)
                .trim_matches('`')
                .to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Read `name="value"` (single or double quoted) from the tag that begins at
/// `tag_start`, scanning only up to the tag's closing `>`.
fn extract_attr(text: &str, lower: &str, tag_start: usize, name: &str) -> Option<String> {
    let tag_end = lower[tag_start..].find('>').map(|r| tag_start + r)?;
    let window = &lower[tag_start..tag_end];
    let key = format!("{name}=");
    let rel = window.find(&key)?;
    let mut i = tag_start + rel + key.len();
    let bytes = text.as_bytes();
    let quote = bytes.get(i).copied()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    i += 1;
    let start = i;
    while i < tag_end && bytes[i] != quote {
        i += 1;
    }
    Some(text[start..i].to_string())
}

fn is_skipped_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

fn read_and(path: &Path, f: fn(&str) -> Vec<ParsedTable>) -> Option<Vec<ParsedTable>> {
    let text = std::fs::read_to_string(path).ok()?;
    let tables = f(&text);
    if tables.is_empty() {
        None
    } else {
        Some(tables)
    }
}

pub fn db_table_node(rel_path: &str, table: &ParsedTable) -> Node {
    let id = ArtifactId::new(format!("db_table::{rel_path}::{}", table.name));
    let mut node = Node::new(id, NodeKind::DbTable);
    node.name = Some(table.name.clone());
    node.path = Some(rel_path.to_string());
    node.source_file = Some(rel_path.to_string());
    node.start_line = Some(table.line);
    node.indexer = Some("schema".to_string());
    let meta = DbTableMeta {
        columns: table
            .columns
            .iter()
            .map(|c| DbColumnMeta {
                name: c.name.clone(),
                definition: c.definition.clone(),
            })
            .collect(),
        source: table.source.to_string(),
    };
    node.metadata_json = serde_json::to_string(&meta).ok();
    node
}

// ---------------------------------------------------------------------------
// SQL: CREATE TABLE
// ---------------------------------------------------------------------------

pub fn parse_sql_tables(text: &str) -> Vec<ParsedTable> {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find("create table") {
        let header = search_from + rel;
        search_from = header + "create table".len();
        // Identifier: skip "if not exists", whitespace, then read name.
        let mut i = search_from;
        i = skip_ws(bytes, i);
        if let Some(after) = match_kw(&lower, i, "if not exists") {
            i = skip_ws(bytes, after);
        }
        let (name, after_name) = read_ident(bytes, i);
        if name.is_empty() {
            continue;
        }
        // Find the opening paren of the column list.
        let mut j = skip_ws(bytes, after_name);
        if j >= bytes.len() || bytes[j] != b'(' {
            continue;
        }
        let Some((body, _end)) = balanced_parens(bytes, j) else {
            continue;
        };
        let columns = parse_sql_columns(body);
        out.push(ParsedTable {
            name,
            columns,
            source: "sql",
            line: line_of(text, header),
        });
        j += body.len();
        search_from = search_from.max(j);
    }
    out
}

fn parse_sql_columns(body: &str) -> Vec<ParsedColumn> {
    let mut cols = Vec::new();
    for piece in split_top_level_commas(body) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let (first, rest) = split_first_token(piece);
        let bare = strip_quotes(&first);
        if bare.is_empty() || is_sql_constraint_kw(&bare) {
            continue;
        }
        cols.push(ParsedColumn {
            name: bare,
            definition: rest.trim().to_string(),
        });
    }
    cols
}

fn is_sql_constraint_kw(token: &str) -> bool {
    matches!(
        token.to_ascii_uppercase().as_str(),
        "PRIMARY"
            | "FOREIGN"
            | "UNIQUE"
            | "KEY"
            | "CONSTRAINT"
            | "INDEX"
            | "CHECK"
            | "EXCLUDE"
    )
}

// ---------------------------------------------------------------------------
// Java entities: @TableName / @Table + fields
// ---------------------------------------------------------------------------

pub fn parse_java_entity_tables(text: &str) -> Vec<ParsedTable> {
    let mut table_name: Option<String> = None;
    let mut table_line = 1u32;
    let mut columns: Vec<ParsedColumn> = Vec::new();
    let mut explicit: Option<String> = None;
    let mut skip_field = false;
    // Brace depth: 0 = file/class-annotation level, 1 = class body (fields),
    // ≥2 = method bodies (ignored, so `return null;` is never a "field").
    let mut depth: i32 = 0;

    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;
        if line.is_empty() || line.starts_with("//") || line.starts_with('*') {
            depth += opens - closes;
            continue;
        }

        if depth == 0 {
            // Class-level annotations: @TableName("x") / @Table(name="x").
            if (line.starts_with("@TableName") || line.starts_with("@Table(")) && table_name.is_none()
            {
                if let Some(v) = first_quoted(line) {
                    table_name = Some(v);
                    table_line = (idx + 1) as u32;
                }
            }
        } else if depth == 1 {
            // Class body: field annotations + field declarations.
            if line.starts_with("@TableId") {
                explicit = first_quoted(line);
            } else if line.starts_with("@TableField") {
                if line.contains("exist") && line.contains("false") {
                    skip_field = true;
                } else if let Some(v) = first_quoted(line) {
                    explicit = Some(v);
                }
            } else if line.starts_with('@') {
                // unrelated annotation; preserve pending state
            } else if opens == 0 && closes == 0 {
                if let Some(field) = parse_java_field(line) {
                    if skip_field {
                        skip_field = false;
                        explicit = None;
                    } else {
                        let col = explicit.take().unwrap_or_else(|| to_snake_case(&field));
                        columns.push(ParsedColumn {
                            name: col,
                            definition: String::new(),
                        });
                    }
                } else {
                    explicit = None;
                    skip_field = false;
                }
            }
            // Lines that open a new scope (e.g. a getter) fall through to the
            // depth update below and are handled at depth ≥ 2 (ignored).
        }

        depth += opens - closes;
    }

    match table_name {
        Some(name) => vec![ParsedTable {
            name,
            columns,
            source: "orm",
            line: table_line,
        }],
        None => Vec::new(),
    }
}

/// Extract a persisted field's name, or `None` if the line is not a simple
/// field declaration (method, constant, brace, …).
fn parse_java_field(line: &str) -> Option<String> {
    if line.contains('(') || !line.contains(';') {
        return None; // method / non-field
    }
    let head = line.trim_end_matches(';').trim();
    let head = head.split('=').next().unwrap_or(head).trim();
    let tokens: Vec<&str> = head.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    if tokens.iter().any(|t| *t == "static" || *t == "class") {
        return None; // serialVersionUID and friends
    }
    let name = *tokens.last().unwrap();
    if name == "serialVersionUID" || !is_ident(name) {
        return None;
    }
    Some(name.to_string())
}

// ---------------------------------------------------------------------------
// Small text helpers (no regex dependency)
// ---------------------------------------------------------------------------

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn match_kw(lower: &str, i: usize, kw: &str) -> Option<usize> {
    lower[i..].starts_with(kw).then_some(i + kw.len())
}

fn read_ident(b: &[u8], start: usize) -> (String, usize) {
    let mut i = start;
    // Allow a leading quote/backtick/bracket.
    let mut quote = 0u8;
    if i < b.len() && matches!(b[i], b'`' | b'"' | b'[') {
        quote = b[i];
        i += 1;
    }
    let from = i;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            let close = if quote == b'[' { b']' } else { quote };
            if c == close {
                break;
            }
            i += 1;
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' {
            i += 1;
        } else {
            break;
        }
    }
    let name = String::from_utf8_lossy(&b[from..i])
        .trim_matches(|c| c == '`' || c == '"' || c == '[' || c == ']')
        .to_string();
    // The table name may be schema-qualified (db.table) — keep last segment.
    let name = name.rsplit('.').next().unwrap_or(&name).to_string();
    if quote != 0 && i < b.len() {
        i += 1; // consume closing quote
    }
    (name, i)
}

/// Given `bytes[open] == '('`, return the inner body (excluding the parens)
/// and the index just past the matching `)`.
fn balanced_parens(b: &[u8], open: usize) -> Option<(&str, usize)> {
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    let body = std::str::from_utf8(&b[open + 1..i]).ok()?;
                    return Some((body, i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_top_level_commas(body: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in body.chars() {
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
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    parts
}

fn split_first_token(s: &str) -> (String, String) {
    let s = s.trim_start();
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(s.len());
    (s[..end].to_string(), s[end..].to_string())
}

fn strip_quotes(s: &str) -> String {
    s.trim_matches(|c| c == '`' || c == '"' || c == '[' || c == ']' || c == '\'')
        .to_string()
}

fn first_quoted(s: &str) -> Option<String> {
    let start = s.find('"')?;
    let rest = &s[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

fn line_of(text: &str, byte_idx: usize) -> u32 {
    text[..byte_idx.min(text.len())].bytes().filter(|b| *b == b'\n').count() as u32 + 1
}

fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Normalise a column name for cross-dialect comparison: drop non-alphanumerics
/// and lowercase, so Java `categoryId` ≍ SQL `category_id`.
pub fn normalize_column(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_create_table_skipping_constraints() {
        let sql = r#"
        CREATE TABLE IF NOT EXISTS craft_conflict (
            id INTEGER PRIMARY KEY,
            category_id INTEGER NOT NULL,
            craft_id INTEGER,
            "type" INTEGER,
            PRIMARY KEY (id),
            FOREIGN KEY (craft_id) REFERENCES craft(id),
            UNIQUE (category_id, craft_id)
        );
        "#;
        let tables = parse_sql_tables(sql);
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.name, "craft_conflict");
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "category_id", "craft_id", "type"]);
    }

    #[test]
    fn parses_mybatis_entity_camel_to_snake_and_skips_non_persisted() {
        let java = r#"
@Data
@TableName("craft_conflict")
@ApiModel(value="CraftConflict对象", description="工艺冲突表")
public class CraftConflict implements Serializable {

    private static final long serialVersionUID = 1L;

    @TableId(value = "id", type = IdType.AUTO)
    private Integer id;

    @ApiModelProperty(value = "品类id")
    private Integer categoryId;

    private Integer craftId;
    private Integer conflictPid;
    private BigDecimal sizeMin;

    @TableField(exist = false)
    private String conflictAdviseTranslation;

    public String getConflictAdviseTranslation() {
        return null;
    }
}
"#;
        let tables = parse_java_entity_tables(java);
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.name, "craft_conflict");
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["id", "category_id", "craft_id", "conflict_pid", "size_min"]
        );
        // exist=false and serialVersionUID are not columns.
        assert!(!names.contains(&"conflict_advise_translation"));
        assert!(!names.iter().any(|n| n.contains("serial")));
    }

    #[test]
    fn non_entity_java_yields_no_tables() {
        let java = "public class CraftConflictController { public void foo() {} }";
        assert!(parse_java_entity_tables(java).is_empty());
    }

    #[test]
    fn normalize_column_aligns_dialects() {
        assert_eq!(normalize_column("categoryId"), normalize_column("category_id"));
        assert_eq!(normalize_column("conflictPid"), "conflictpid");
    }

    #[test]
    fn parses_mybatis_mapper_statements() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE mapper PUBLIC "-//mybatis.org//DTD Mapper 3.0//EN" "http://mybatis.org/dtd/mybatis-3-mapper.dtd">
<mapper namespace="com.kutesmart.cloud.craft.mapper.CraftConflictMapper">
    <select id="selectConflictListTreeByCloth"
            resultType="com.kutesmart.cloud.craft.vo.CraftConflictListTreeVO">
        select
            cc.craft_id as craftId,
            cc.`type`,
            cc.conflict_id as conflictId
        from craft_conflict cc
        where cc.allow = 0 and cc.type in (1,2,3,4,5,6,7) and cc.category_id in (${cloth})
    </select>
    <insert id="batchInsertConflicts">
        insert into craft_conflict(craft_id, conflict_id) values (#{c}, #{x})
    </insert>
</mapper>
"#;
        let stmts = parse_mapper_stmts(xml);
        assert_eq!(stmts.len(), 2, "want 2 statements, got {stmts:?}");
        let s0 = &stmts[0];
        assert_eq!(s0.id, "selectConflictListTreeByCloth");
        assert_eq!(s0.stmt_kind, "select");
        assert_eq!(s0.namespace.as_deref(), Some("com.kutesmart.cloud.craft.mapper.CraftConflictMapper"));
        assert!(s0.sql.contains("from craft_conflict cc"), "sql body kept: {}", s0.sql);
        assert!(s0.sql.contains("category_id in"), "sql body kept: {}", s0.sql);
        assert!(s0.line >= 4 && s0.line <= 5, "line points at the <select>: {}", s0.line);
        let s1 = &stmts[1];
        assert_eq!(s1.id, "batchInsertConflicts");
        assert_eq!(s1.stmt_kind, "insert");
    }

    #[test]
    fn non_mapper_xml_yields_no_statements() {
        let pom = r#"<project><dependencies><dependency><groupId>x</groupId></dependency></dependencies></project>"#;
        assert!(parse_mapper_stmts(pom).is_empty());
    }

    #[test]
    fn extracts_table_refs_from_sql() {
        let sql = "select cc.craft_id, a.img_path from craft_conflict cc \
                   left join attachment a on a.id = cc.image_id \
                   inner join `craft` c on c.id = cc.craft_id \
                   where cc.category_id in (${cloth})";
        let mut t = extract_sql_table_refs(sql);
        t.sort();
        assert_eq!(t, vec!["attachment", "craft", "craft_conflict"]);
    }

    #[test]
    fn links_mapper_method_to_stmt_to_table() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("schema.sql"),
            "CREATE TABLE craft_conflict (id BIGINT, craft_id BIGINT);",
        )
        .unwrap();
        std::fs::write(
            root.join("CraftConflictMapper.xml"),
            r#"<?xml version="1.0"?>
<mapper namespace="com.kutesmart.cloud.craft.mapper.CraftConflictMapper">
  <select id="selectConflictListTreeByCloth" resultType="x">
    select cc.craft_id from craft_conflict cc where cc.cloth = #{cloth}
  </select>
</mapper>"#,
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        // Pre-seed the Java mapper-interface method node (normally from the code
        // indexer). Its `name` is None; identity is the id suffix.
        let method_id = ArtifactId::new(
            "java::rcmtm-cloud-craft/src/main/java/com/kutesmart/cloud/craft/mapper/CraftConflictMapper.java::CraftConflictMapper.selectConflictListTreeByCloth",
        );
        store
            .upsert_node(&Node::new(method_id.clone(), NodeKind::JavaMethod))
            .unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.mapper_stmts, 1, "one mapper stmt indexed");
        assert_eq!(stats.stmt_method_edges, 1, "method->stmt edge linked");
        assert_eq!(stats.stmt_table_edges, 1, "stmt->table edge linked");

        let stmt_id = ArtifactId::new("sql_mapper::CraftConflictMapper.xml::selectConflictListTreeByCloth");
        // method --references--> stmt
        let from_method = store.list_edges_from(&method_id).unwrap();
        assert!(
            from_method
                .iter()
                .any(|e| e.kind == EdgeKind::References && e.to_id == stmt_id),
            "expected method->stmt References edge, got {from_method:?}"
        );
        // stmt --persists_to--> craft_conflict table
        let from_stmt = store.list_edges_from(&stmt_id).unwrap();
        assert!(
            from_stmt
                .iter()
                .any(|e| e.kind == EdgeKind::PersistsTo
                    && e.to_id.as_str().ends_with("::craft_conflict")),
            "expected stmt->table PersistsTo edge, got {from_stmt:?}"
        );
    }

    #[test]
    fn links_spring_service_impl_without_i_prefix() {
        // Dominant Spring convention: `FooService` (interface) ⇒ `FooServiceImpl`
        // (impl), with NO `I` prefix. The linker must pair them so traversal
        // descends through interface dispatch instead of dead-ending at the
        // declaration. Reproduces the vub/yolan miss (DictSystemService).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("schema.sql"), "CREATE TABLE dict_system (id BIGINT);").unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let iface = ArtifactId::new(
            "java::src/com/vhub/yolan/service/DictSystemService.java::DictSystemService.getDictSystem",
        );
        let impl_id = ArtifactId::new(
            "java::src/com/vhub/yolan/service/impl/DictSystemServiceImpl.java::DictSystemServiceImpl.getDictSystem",
        );
        store.upsert_node(&Node::new(iface.clone(), NodeKind::JavaMethod)).unwrap();
        store.upsert_node(&Node::new(impl_id.clone(), NodeKind::JavaMethod)).unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert!(
            stats.iface_impl_edges >= 1,
            "expected >=1 interface->impl edge (Spring <Name>Service convention), got {}",
            stats.iface_impl_edges
        );
        let from_iface = store.list_edges_from(&iface).unwrap();
        assert!(
            from_iface
                .iter()
                .any(|e| e.kind == EdgeKind::DeclaresImplementation && e.to_id == impl_id),
            "expected DictSystemService->DictSystemServiceImpl DeclaresImplementation edge, got {from_iface:?}"
        );
    }

    #[test]
    fn iface_impl_no_edge_without_matching_impl() {
        // A plain class ending in `Impl` with no same-named interface, and an
        // interface with no `Impl`, must NOT fabricate an edge.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("schema.sql"), "CREATE TABLE t (id BIGINT);").unwrap();
        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        store
            .upsert_node(&Node::new(
                ArtifactId::new("java::a/LonelyServiceImpl.java::LonelyServiceImpl.run"),
                NodeKind::JavaMethod,
            ))
            .unwrap();
        store
            .upsert_node(&Node::new(
                ArtifactId::new("java::a/OtherService.java::OtherService.ping"),
                NodeKind::JavaMethod,
            ))
            .unwrap();
        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.iface_impl_edges, 0, "no matching pair -> no edge");
    }

    #[test]
    fn links_go_method_inline_sql_to_table() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("schema.sql"),
            "CREATE TABLE craft (id INTEGER, ecode TEXT);",
        )
        .unwrap();
        // A real Go source file whose repo method embeds SQL as a string literal
        // (no MyBatis XML). The linker must read the body span and reach `craft`.
        std::fs::write(
            root.join("repo.go"),
            "package repo\nfunc (r *CraftRepo) ListByEcode() {\n\tq := `SELECT id, ecode FROM craft WHERE ecode = ?`\n\t_ = q\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        // Pre-seed the Go method node (normally produced by the code indexer),
        // pointing at the body span that carries the SQL.
        let method_id = ArtifactId::new("go::repo.go::CraftRepo.ListByEcode");
        let mut m = Node::new(method_id.clone(), NodeKind::GoMethod);
        m.path = Some("repo.go".to_string());
        m.start_line = Some(2);
        m.end_line = Some(5);
        store.upsert_node(&m).unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.sql_tables, 1, "one sql table indexed");
        assert!(
            stats.inline_sql_table_edges >= 1,
            "go method->table edge linked, got {}",
            stats.inline_sql_table_edges
        );

        let from_method = store.list_edges_from(&method_id).unwrap();
        assert!(
            from_method
                .iter()
                .any(|e| e.kind == EdgeKind::PersistsTo
                    && e.to_id.as_str().ends_with("::craft")),
            "expected go-method->table PersistsTo edge, got {from_method:?}"
        );
    }

    #[test]
    fn classifies_interface_class_names() {
        assert!(is_interface_class_name("ICraftService"));
        assert!(is_interface_class_name("IOrderService"));
        assert!(!is_interface_class_name("Image"));
        assert!(!is_interface_class_name("Item"));
        assert!(!is_interface_class_name("CraftServiceImpl"));
        assert!(!is_interface_class_name("I"));
    }

    #[test]
    fn links_interface_methods_to_impls() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // schema-index needs at least one scanned file to run; an empty mapper
        // is fine — we only care about the interface→impl post-pass here.
        std::fs::write(root.join("schema.sql"), "CREATE TABLE t (id BIGINT);").unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let iface = ArtifactId::new(
            "java::a/service/ICraftConflictService.java::ICraftConflictService.selectStyleConflictById",
        );
        let imp = ArtifactId::new(
            "java::a/service/impl/CraftConflictServiceImpl.java::CraftConflictServiceImpl.selectStyleConflictById",
        );
        store.upsert_node(&Node::new(iface.clone(), NodeKind::JavaMethod)).unwrap();
        store.upsert_node(&Node::new(imp.clone(), NodeKind::JavaMethod)).unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.iface_impl_edges, 1, "one interface->impl edge");

        let from_iface = store.list_edges_from(&iface).unwrap();
        assert!(
            from_iface
                .iter()
                .any(|e| e.kind == EdgeKind::DeclaresImplementation && e.to_id == imp),
            "expected interface->impl DeclaresImplementation edge, got {from_iface:?}"
        );
    }

    #[test]
    fn extracts_table_refs_for_write_stmts_and_skips_subquery() {
        assert_eq!(extract_sql_table_refs("update craft_default set sort=1 where id=#{id}"), vec!["craft_default"]);
        assert_eq!(extract_sql_table_refs("insert into craft_recommend(id) values(#{id})"), vec!["craft_recommend"]);
        // a subquery after FROM (next token is '(') must not yield a bogus table.
        assert!(extract_sql_table_refs("select * from (select id from craft) t").contains(&"craft".to_string()));
        assert!(!extract_sql_table_refs("select * from (select id from craft) t").contains(&"(select".to_string()));
    }
}
