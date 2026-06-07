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
    /// True when the table has no entity/DDL in the indexed sources and was
    /// synthesized purely from a SQL reference (junction/sequence/other-service
    /// table). Its schema is unknown; it exists so traces stay complete.
    #[serde(default)]
    pub external: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaIndexStats {
    pub files_scanned: usize,
    pub sql_tables: usize,
    pub orm_tables: usize,
    /// Subset of `orm_tables` whose name was *inferred* from the entity class
    /// name because no explicit `@TableName` was present (MyBatis-Plus
    /// convention). Surfaced separately so the recovered-table count is visible.
    pub implicit_orm_tables: usize,
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
    /// `HttpRoute` nodes indexed from Spring MVC controller mapping annotations
    /// (`@GetMapping`/`@PostMapping`/…/`@RequestMapping`). Lets a query for the
    /// *URL path* tailorx calls resolve to its handler method.
    pub http_routes: usize,
    /// `HttpRoute --references--> handler method` edges linked by matching the
    /// controller `Class.method` against Java method node id suffixes.
    pub route_method_edges: usize,
    /// Synthetic `DbTable` nodes for tables referenced by SQL but having no
    /// entity/DDL in the indexed sources (marked `external`, schema unknown).
    /// Keeps `trace` complete for junction/sequence/cross-service tables.
    pub external_tables: usize,
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

/// One HTTP endpoint route recovered from a Spring MVC controller annotation.
/// The handler `class`/`method` are kept so the route can be linked to its
/// already-indexed `JavaMethod` node; the full `path` is the URL tailorx calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRoute {
    /// HTTP verb: `GET`/`POST`/`PUT`/`DELETE`/`PATCH`, or `ANY` for a
    /// `@RequestMapping` without an explicit `method=`.
    pub verb: String,
    /// Full normalized route: class-level `@RequestMapping` prefix + method path.
    pub path: String,
    /// Declaring controller simple class name.
    pub class: String,
    /// Handler method name (often != the URL path segment).
    pub method: String,
    /// 1-based line of the mapping annotation.
    pub line: u32,
}

/// Serialized into [`Node::metadata_json`] for an `HttpRoute` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRouteMeta {
    pub verb: String,
    pub handler_class: String,
    pub handler_method: String,
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

/// Indexer name stamped on every node and edge the schema pass produces, so a
/// re-index can wholesale-clear its prior outputs (like the language indexers)
/// instead of leaving stale routes/tables/edges behind.
pub const SCHEMA_INDEXER_NAME: &str = "schema";

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
    // Re-index = clean slate for this pass. Without this, upsert-only writes leave
    // orphaned nodes/edges when a route/table/mapper statement is deleted or
    // renamed in source (forcing a full graph rebuild). Mirrors how the language
    // indexers clear their own prior generation before re-emitting.
    store
        .clear_indexer_outputs(SCHEMA_INDEXER_NAME)
        .context("clearing prior schema-indexer outputs")?;
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
        // Spring MVC controllers: index each @GetMapping/@PostMapping/… as an
        // HttpRoute node so a query for the *URL path* tailorx calls resolves to
        // the handler method, even when the path segment != the Java method
        // name. Done before the table early-continue below because controllers
        // usually declare no `@TableName`, so they would otherwise be skipped.
        if ext == "java" || ext == "go" {
            if let Ok(text) = std::fs::read_to_string(path) {
                // Spring MVC annotations (Java) and net/http ServeMux registrations
                // (Go) both land as HttpRoute nodes so the *URL path* tailorx calls
                // resolves to its handler regardless of backend language.
                let routes = if ext == "java" {
                    parse_http_routes(&text)
                } else {
                    parse_go_routes(&text)
                };
                if !routes.is_empty() {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    for r in &routes {
                        stats.http_routes += 1;
                        store.upsert_node(&http_route_node(&rel, r)).with_context(|| {
                            format!("upserting http route {} {} from {rel}", r.verb, r.path)
                        })?;
                    }
                }
            }
        }
        let tables = match ext.as_str() {
            "sql" => read_and(path, parse_sql_tables),
            "java" => read_and(path, parse_java_entity_tables),
            // Backends that keep their schema as an embedded string literal
            // (Go `migrations.go`, Rust/Python/TS migration modules, …) define
            // `CREATE TABLE` inside source code, never a `.sql` file. The same
            // tolerant DDL scanner finds it there too, so the inline-SQL linker
            // below can reach those tables. Java is excluded — it uses ORM
            // annotations (handled above) and the MyBatis XML path.
            _ if is_embedded_sql_source_ext(&ext) => read_and(path, parse_sql_tables),
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
                "orm-implicit" => {
                    stats.orm_tables += 1;
                    stats.implicit_orm_tables += 1;
                }
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
    link_http_route_edges(store, &mut stats)?;
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
    // table name (lower-cased) -> table node ids, plus a registry of existing
    // synthetic "external" tables (so re-index reuses them instead of dupes).
    let mut table_by_name: HashMap<String, Vec<ArtifactId>> = HashMap::new();
    let mut external_registry: HashMap<String, ArtifactId> = HashMap::new();
    for t in store.list_nodes_by_kind(NodeKind::DbTable)? {
        if let Some(name) = &t.name {
            let key = name.to_ascii_lowercase();
            if t.id.as_str().starts_with("db_table::<external>::") {
                external_registry.insert(key.clone(), t.id.clone());
            }
            table_by_name.entry(key).or_default().push(t.id.clone());
        }
    }
    let mut new_external_nodes: Vec<Node> = Vec::new();

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
            } else if is_plausible_table_name(&table) {
                // No entity/DDL for this table — synthesize a single external
                // node (schema unknown) so the trace still reaches it.
                let tid = external_registry
                    .entry(table.clone())
                    .or_insert_with(|| {
                        let node = external_db_table_node(&table);
                        let id = node.id.clone();
                        new_external_nodes.push(node);
                        id
                    })
                    .clone();
                table_by_name
                    .entry(table.clone())
                    .or_default()
                    .push(tid.clone());
                edges.push(EdgeAssertion::fact(
                    stmt.id.clone(),
                    tid,
                    EdgeKind::PersistsTo,
                    EdgeSource::LanguageAdapter,
                ));
                stats.stmt_table_edges += 1;
            }
        }
    }
    stats.external_tables = external_registry.len();
    // Persist synthetic nodes before their edges (nodes-before-edges invariant).
    for node in &new_external_nodes {
        store
            .upsert_node(node)
            .with_context(|| format!("upserting external table {}", node.id.as_str()))?;
    }
    for edge in &mut edges {
        edge.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
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
    for edge in &mut edges {
        edge.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
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

/// Stitch each `HttpRoute` into the call graph with a `route --references-->
/// handler method` edge, matched by the route's `handler_class.handler_method`
/// against Java method node id suffixes (`...::StyleInfoController.measuresInfo`).
/// `References` is in [`crate::search::EXPANSION_EDGE_KINDS`], so `trace` started
/// from the URL path descends straight into the handler and its downstream
/// service → mapper → SQL → tables. Idempotent (upsert).
fn link_http_route_edges(store: &mut Store, stats: &mut SchemaIndexStats) -> Result<()> {
    use std::collections::HashMap;

    let routes = store.list_nodes_by_kind(NodeKind::HttpRoute)?;
    if routes.is_empty() {
        return Ok(());
    }
    let mut method_by_suffix: HashMap<String, Vec<ArtifactId>> = HashMap::new();
    // Both Java (`JavaMethod`) and Go (`GoMethod`) handler nodes share the same
    // id suffix shape `Type.method`, so one map serves Spring and net/http alike.
    for kind in [NodeKind::JavaMethod, NodeKind::GoMethod] {
        for m in store.list_nodes_by_kind(kind)? {
            if let Some(suffix) = m.id.as_str().rsplit("::").next() {
                method_by_suffix
                    .entry(suffix.to_ascii_lowercase())
                    .or_default()
                    .push(m.id.clone());
            }
        }
    }

    let mut edges: Vec<EdgeAssertion> = Vec::new();
    for route in routes {
        let Some(meta_json) = &route.metadata_json else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<HttpRouteMeta>(meta_json) else {
            continue;
        };
        let key = format!("{}.{}", meta.handler_class, meta.handler_method).to_ascii_lowercase();
        if let Some(method_ids) = method_by_suffix.get(&key) {
            for mid in method_ids {
                edges.push(EdgeAssertion::fact(
                    route.id.clone(),
                    mid.clone(),
                    EdgeKind::References,
                    EdgeSource::LanguageAdapter,
                ));
                stats.route_method_edges += 1;
            }
        }
    }
    for edge in &mut edges {
        edge.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
        store.upsert_edge(edge).with_context(|| {
            format!(
                "linking http-route edge {} -> {}",
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
    node.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
    let meta = MapperStmtMeta {
        stmt_kind: stmt.stmt_kind.clone(),
        namespace: stmt.namespace.clone(),
        sql: stmt.sql.clone(),
    };
    node.metadata_json = serde_json::to_string(&meta).ok();
    node
}

/// Build an `HttpRoute` node for one Spring MVC route. The id is namespaced by
/// file + verb + full path so distinct endpoints stay separate; `name` is the
/// method-level URL segment so `search <segment>` matches it like a symbol, and
/// `path` carries the full route so a query for any segment can substring-hit.
pub fn http_route_node(rel_path: &str, r: &ParsedRoute) -> Node {
    let id = ArtifactId::new(format!("http_route::{rel_path}::{} {}", r.verb, r.path));
    let mut node = Node::new(id, NodeKind::HttpRoute);
    node.name = Some(route_search_name(&r.path));
    node.path = Some(r.path.clone());
    node.source_file = Some(rel_path.to_string());
    node.start_line = Some(r.line);
    node.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
    let meta = HttpRouteMeta {
        verb: r.verb.clone(),
        handler_class: r.class.clone(),
        handler_method: r.method.clone(),
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
    let namespace = extract_attr(
        text,
        &lower,
        lower.find("<mapper").unwrap_or(0),
        "namespace",
    );
    // Reusable `<sql id="x">…</sql>` fragments, so `<include refid="x"/>` inside
    // a statement can be inlined and its FROM/JOIN tables recovered.
    let fragments = collect_sql_fragments(text, &lower);
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
            if !matches!(
                boundary,
                Some(b' ') | Some(b'\n') | Some(b'\r') | Some(b'\t') | Some(b'>')
            ) {
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
            let body = text[open_end..body_end].trim();
            out.push(ParsedMapperStmt {
                id,
                stmt_kind: tag.to_string(),
                namespace: namespace.clone(),
                sql: expand_includes(body, &fragments, 0),
                line: line_of(text, start),
            });
        }
    }
    out.sort_by_key(|s| s.line);
    out
}

/// Collect MyBatis `<sql id="x">BODY</sql>` reusable fragments, keyed by the
/// lower-cased `id`. Tolerant scan mirroring [`parse_mapper_stmts`].
fn collect_sql_fragments(text: &str, lower: &str) -> std::collections::HashMap<String, String> {
    let mut frags = std::collections::HashMap::new();
    let open = "<sql";
    let close = "</sql>";
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(open) {
        let start = from + rel;
        let after = start + open.len();
        // Tag boundary so `<sqlMap>`/`<sqlSession…>` don't match.
        let boundary = lower.as_bytes().get(after).copied();
        if !matches!(
            boundary,
            Some(b' ') | Some(b'\n') | Some(b'\r') | Some(b'\t') | Some(b'>')
        ) {
            from = after;
            continue;
        }
        let Some(gt_rel) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + gt_rel + 1;
        let id = extract_attr(text, lower, start, "id");
        let Some(c_rel) = lower[open_end..].find(close) else {
            from = open_end;
            continue;
        };
        let body_end = open_end + c_rel;
        from = body_end + close.len();
        if let Some(id) = id {
            frags.insert(id.to_ascii_lowercase(), text[open_end..body_end].trim().to_string());
        }
    }
    frags
}

/// Inline `<include refid="x"/>` (and `<include refid="x">…</include>`) with the
/// body of fragment `x`, recursively (bounded). Unknown refids are dropped.
fn expand_includes(
    body: &str,
    fragments: &std::collections::HashMap<String, String>,
    depth: u8,
) -> String {
    if depth >= 8 || !body.contains("<include") {
        return body.to_string();
    }
    let lower = body.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(rel) = lower[i..].find("<include") {
        let start = i + rel;
        out.push_str(&body[i..start]);
        let Some(gt_rel) = lower[start..].find('>') else {
            // Malformed tag: keep the rest verbatim.
            out.push_str(&body[start..]);
            return out;
        };
        let open_end = start + gt_rel + 1;
        let self_closing = gt_rel > 0 && bytes[start + gt_rel - 1] == b'/';
        let refid = extract_attr(body, &lower, start, "refid");
        // Span of the whole <include> element to drop.
        let drop_end = if self_closing {
            open_end
        } else if let Some(c_rel) = lower[open_end..].find("</include>") {
            open_end + c_rel + "</include>".len()
        } else {
            open_end
        };
        if let Some(rid) = refid {
            if let Some(frag) = fragments.get(&rid.to_ascii_lowercase()) {
                out.push(' ');
                out.push_str(&expand_includes(frag, fragments, depth + 1));
                out.push(' ');
            }
        }
        i = drop_end;
    }
    out.push_str(&body[i..]);
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
    let lower = sql.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    // Read a (possibly backtick / schema-qualified) table identifier starting at
    // `i` (after skipping any whitespace). Returns the bare table name, or
    // `None` for a subquery/expression `(...)`.
    let read_table = |mut i: usize| -> Option<String> {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'(' {
            return None;
        }
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
            return None;
        }
        let raw = &lower[start..i];
        // Keep only the table part of `schema.table`, drop backticks.
        let name = raw
            .rsplit('.')
            .next()
            .unwrap_or(raw)
            .trim_matches('`')
            .to_string();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    };
    // Collect CTE / derived-table names defined as `<name> AS ( … )` so a later
    // `FROM <name>` is not mistaken for (or synthesized as) a base table.
    let mut cte_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut s = 0usize;
        while let Some(rel) = lower[s..].find("as") {
            let at = s + rel;
            s = at + 2;
            let before_ok = at == 0 || !is_ident(bytes[at - 1]);
            let after_idx = at + 2;
            let after_ok = after_idx >= bytes.len() || !is_ident(bytes[after_idx]);
            if !before_ok || !after_ok {
                continue;
            }
            // The CTE body opens with `(` right after `as`.
            let mut j = after_idx;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b'(' {
                continue;
            }
            // The identifier immediately before `as` is the CTE name.
            let mut k = at;
            while k > 0 && bytes[k - 1].is_ascii_whitespace() {
                k -= 1;
            }
            let end = k;
            while k > 0 && is_ident(bytes[k - 1]) {
                k -= 1;
            }
            if k < end {
                cte_names.insert(lower[k..end].to_string());
            }
        }
    }
    // Match each keyword as a *whole word* — boundaries are any non-identifier
    // byte — so `FROM\n`, `JOIN\t`, `(select … from x)` and a leading `update x`
    // all resolve, while `date_from` / `transform` / `into_x` do not.
    for kw in ["from", "join", "update", "into", "insert"] {
        let mut search = 0usize;
        while let Some(rel) = lower[search..].find(kw) {
            let at = search + rel;
            search = at + kw.len();
            let before_ok = at == 0 || !is_ident(bytes[at - 1]);
            let after_idx = at + kw.len();
            let after_ok = after_idx >= bytes.len() || !is_ident(bytes[after_idx]);
            if !before_ok || !after_ok {
                continue;
            }
            // `ON DUPLICATE KEY UPDATE col=…`: the token after UPDATE is a SET
            // column, not a table. Skip when the preceding word is `key`.
            if kw == "update" {
                let mut k = at;
                while k > 0 && bytes[k - 1].is_ascii_whitespace() {
                    k -= 1;
                }
                let word_end = k;
                while k > 0 && is_ident(bytes[k - 1]) {
                    k -= 1;
                }
                if &lower[k..word_end] == "key" {
                    continue;
                }
            }
            let mut i = after_idx;
            // `INSERT [INTO] table`: MySQL allows omitting INTO. Skip an optional
            // `into` so both `insert into x` and `insert x` reach the table.
            if kw == "insert" {
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if lower[j..].starts_with("into")
                    && bytes.get(j + 4).map_or(true, |c| !is_ident(*c))
                {
                    i = j + 4;
                }
            }
            if let Some(name) = read_table(i) {
                if !cte_names.contains(&name) && !out.contains(&name) {
                    out.push(name);
                }
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

/// Source-code extensions whose files may embed `CREATE TABLE` DDL as a string
/// literal (Go migrations, Rust/Python/TS migration modules, …). Java is
/// deliberately omitted: its tables come from `@TableName`/`@Table` ORM
/// annotations (parsed via [`parse_java_entity_tables`]) and MyBatis XML, and
/// re-scanning every `.java` body for DDL would invite false positives.
fn is_embedded_sql_source_ext(ext: &str) -> bool {
    matches!(
        ext,
        "go" | "rs"
            | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "dart"
            | "kt"
            | "kts"
            | "rb"
            | "php"
            | "cs"
            | "scala"
            | "swift"
    )
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
    node.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
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
        external: false,
    };
    node.metadata_json = serde_json::to_string(&meta).ok();
    node
}

/// Build a synthetic `DbTable` node for a table that is referenced by SQL but
/// has no entity/DDL in the indexed sources. Keyed globally by name (no source
/// file), so every reference to the same table shares one node, and marked
/// `external` so consumers can show it as "schema unknown".
pub fn external_db_table_node(name: &str) -> Node {
    let id = ArtifactId::new(format!("db_table::<external>::{name}"));
    let mut node = Node::new(id, NodeKind::DbTable);
    node.name = Some(name.to_string());
    node.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
    let meta = DbTableMeta {
        columns: Vec::new(),
        source: "external".to_string(),
        external: true,
    };
    node.metadata_json = serde_json::to_string(&meta).ok();
    node
}

/// Whether a SQL-parsed identifier is plausibly a real table name worth
/// synthesizing an external node for. Rejects SQL keywords that a tolerant
/// scanner might mis-read as a table, blank/numeric tokens, and 1-char names.
pub fn is_plausible_table_name(name: &str) -> bool {
    const NON_TABLES: &[&str] = &[
        "select", "from", "where", "join", "on", "as", "set", "values", "value",
        "into", "insert", "update", "delete", "dual", "and", "or", "by", "group",
        "order", "limit", "having", "union", "case", "when", "then", "else", "end",
    ];
    if name.len() < 2 {
        return false;
    }
    // A trailing `_` is the residue of a truncated dynamic name (`x_${var}`).
    if name.ends_with('_') {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    !NON_TABLES.contains(&name)
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
        "PRIMARY" | "FOREIGN" | "UNIQUE" | "KEY" | "CONSTRAINT" | "INDEX" | "CHECK" | "EXCLUDE"
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
    // First top-level `class` name + line, used to infer the table name when no
    // explicit @TableName is present (MyBatis-Plus class-name convention).
    let mut class_name: Option<String> = None;
    let mut class_line = 1u32;
    // Whether the class declares a @TableId/@TableField field — the tell that it
    // is a persisted MyBatis-Plus entity (vs. a VO/DTO/ServiceImpl that merely
    // imports the library). Gates the class-name inference to avoid false tables.
    let mut saw_mp_field_anno = false;
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
            if (line.starts_with("@TableName") || line.starts_with("@Table("))
                && table_name.is_none()
            {
                if let Some(v) = first_quoted(line) {
                    table_name = Some(v);
                    table_line = (idx + 1) as u32;
                }
            } else if class_name.is_none() {
                if let Some(name) = class_name_from_decl(line) {
                    class_name = Some(name);
                    class_line = (idx + 1) as u32;
                }
            }
        } else if depth == 1 {
            // Class body: field annotations + field declarations.
            if line.starts_with("@TableId") {
                saw_mp_field_anno = true;
                explicit = first_quoted(line);
            } else if line.starts_with("@TableField") {
                saw_mp_field_anno = true;
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

    if let Some(name) = table_name {
        return vec![ParsedTable {
            name,
            columns,
            source: "orm",
            line: table_line,
        }];
    }
    // No explicit @TableName: fall back to the MyBatis-Plus default, where the
    // table is the snake_case of the entity class name (`SizeSys` -> `size_sys`).
    // Gated on a @TableId/@TableField annotation so only real persisted entities
    // qualify — a ServiceImpl/Controller/VO that imports the library does not.
    if saw_mp_field_anno {
        if let Some(cls) = class_name {
            if !is_non_entity_class_name(&cls) {
                return vec![ParsedTable {
                    name: to_snake_case(&cls),
                    columns,
                    source: "orm-implicit",
                    line: class_line,
                }];
            }
        }
    }
    Vec::new()
}

/// Extract the declared class name from a class-declaration line, e.g.
/// `public class SizeSys implements Serializable {` → `SizeSys`. Returns
/// `None` for `interface`/`enum`/`record` declarations (no `class` keyword) or
/// any line without a `class Name` token pair.
fn class_name_from_decl(line: &str) -> Option<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let pos = tokens.iter().position(|t| *t == "class")?;
    let raw = tokens.get(pos + 1)?;
    // Strip generic params / attached brace: `Foo<T>{` → `Foo`.
    let name: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let first = name.chars().next()?;
    if first.is_ascii_uppercase() {
        Some(name)
    } else {
        None
    }
}

/// Reject class names that obviously are not persisted entities even if they
/// carry MyBatis-Plus annotations for some other reason. Belt-and-suspenders on
/// top of the @TableId/@TableField gate.
fn is_non_entity_class_name(name: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "Controller",
        "ServiceImpl",
        "Service",
        "Mapper",
        "Application",
        "Config",
        "Configuration",
        "Handler",
        "Interceptor",
        "Aspect",
        "Exception",
        "Test",
    ];
    SUFFIXES.iter().any(|s| name.ends_with(s))
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
// Spring MVC HTTP route parsing
// ---------------------------------------------------------------------------

/// Parse Spring MVC controller mapping annotations into [`ParsedRoute`]s.
///
/// Deliberately tolerant (no Java parser): gated on the class-level stereotype
/// (`@RestController`/`@Controller`) so Feign *clients* and other
/// `@RequestMapping` users are excluded. For each method-level
/// `@GetMapping`/`@PostMapping`/`@PutMapping`/`@DeleteMapping`/`@PatchMapping`
/// (and method-level `@RequestMapping`), it joins the class-level prefix with
/// the annotation's path, reads the HTTP verb, and recovers the handler method
/// name by skipping any intervening annotations/comments to the signature.
pub fn parse_http_routes(text: &str) -> Vec<ParsedRoute> {
    if !(text.contains("@RestController") || text.contains("@Controller")) {
        return Vec::new();
    }
    let Some((class_name, class_decl_at)) = find_controller_class(text) else {
        return Vec::new();
    };
    let base = class_level_base_path(text, class_decl_at);
    let b = text.as_bytes();
    let mut routes = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'@' {
            i += 1;
            continue;
        }
        let (annot, after_name) = read_ident(b, i + 1);
        let (verb, is_mapping) = mapping_kind(&annot);
        if !is_mapping {
            i = after_name;
            continue;
        }
        // The class-level @RequestMapping is the prefix, not a route itself.
        if annot == "RequestMapping" && i < class_decl_at {
            i = after_name;
            continue;
        }
        let j = skip_ws(b, after_name);
        let (method_path, verb_final, args_end) = if j < b.len() && b[j] == b'(' {
            let (body, end) = balanced_parens(b, j).unwrap_or(("", j + 1));
            let v = if annot == "RequestMapping" {
                request_method_verb(body).unwrap_or_else(|| "ANY".to_string())
            } else {
                verb.to_string()
            };
            (annotation_path(body), v, end)
        } else {
            (String::new(), verb.to_string(), j)
        };
        if let Some(method) = find_handler_method(b, args_end) {
            routes.push(ParsedRoute {
                verb: verb_final,
                path: normalize_route(&base, &method_path),
                class: class_name.clone(),
                method,
                line: line_of(text, i),
            });
        }
        i = args_end;
    }
    routes
}

/// Recover HTTP routes registered on a Go `net/http` `ServeMux` (Go 1.22+
/// method-aware patterns) — the Go analogue of [`parse_http_routes`] for Spring.
/// Handles `mux.HandleFunc("VERB /path", recv.Method)` / `mux.Handle(...)`,
/// including a pattern split across string concatenation with a gateway-prefix
/// variable (`"GET "+prefix+"/p"` → path `/p`). The handler's declaring type is
/// taken from the enclosing method receiver (`func (recv *Type) Register(...)`),
/// so the route links to the `Type.Method` suffix of the indexed `GoMethod` node.
pub fn parse_go_routes(text: &str) -> Vec<ParsedRoute> {
    if !text.contains("HandleFunc") && !text.contains(".Handle(") {
        return Vec::new();
    }
    let b = text.as_bytes();
    let receivers = collect_go_receivers(b);
    let mut routes = Vec::new();
    // Call sites: byte index of the `(` opening each `.HandleFunc(`/`.Handle(`.
    let mut calls: Vec<usize> = Vec::new();
    for (idx, _) in text.match_indices(".HandleFunc(") {
        calls.push(idx + ".HandleFunc".len());
    }
    for (idx, _) in text.match_indices(".Handle(") {
        calls.push(idx + ".Handle".len());
    }
    calls.sort_unstable();
    for open in calls {
        let Some((body, _end)) = balanced_parens(b, open) else {
            continue;
        };
        let args = split_top_level_commas(body);
        if args.len() < 2 {
            continue;
        }
        let Some((verb, path)) = parse_go_route_pattern(&args[0]) else {
            continue;
        };
        let Some((handler_recv, handler_method)) = parse_go_handler(&args[args.len() - 1]) else {
            continue;
        };
        let class = resolve_go_receiver_type(&receivers, open, &handler_recv);
        if class.is_empty() || handler_method.is_empty() {
            continue;
        }
        routes.push(ParsedRoute {
            verb,
            path: normalize_route("", &path),
            class,
            method: handler_method,
            line: line_of(text, open),
        });
    }
    routes
}

/// All method receivers in source order: `(byte offset of `func`, recv var,
/// recv type)`. Plain functions (no `(recv ...)` clause) are skipped.
fn collect_go_receivers(b: &[u8]) -> Vec<(usize, String, String)> {
    let text = std::str::from_utf8(b).unwrap_or("");
    let mut out = Vec::new();
    for (idx, _) in text.match_indices("func") {
        let before_ok = idx == 0 || !is_ident_byte(b[idx - 1]);
        let after = idx + 4;
        if !before_ok || after >= b.len() || !b[after].is_ascii_whitespace() {
            continue;
        }
        let j = skip_ws(b, after);
        if j >= b.len() || b[j] != b'(' {
            continue; // a plain function, not a method
        }
        let mut k = skip_ws(b, j + 1);
        let (recv_var, k2) = read_ident(b, k);
        k = skip_ws(b, k2);
        if k < b.len() && b[k] == b'*' {
            k = skip_ws(b, k + 1);
        }
        let (recv_type, _k3) = read_ident(b, k);
        if !recv_var.is_empty() && !recv_type.is_empty() {
            out.push((idx, recv_var, recv_type));
        }
    }
    out
}

/// The receiver type whose method body encloses the call at `call_offset`, but
/// only when its receiver variable matches the handler selector's receiver
/// (`h.Method` → `h`). Empty when the handler isn't a method on that receiver.
fn resolve_go_receiver_type(
    receivers: &[(usize, String, String)],
    call_offset: usize,
    handler_recv: &str,
) -> String {
    let mut best: Option<&(usize, String, String)> = None;
    for r in receivers {
        if r.0 < call_offset {
            best = Some(r);
        } else {
            break;
        }
    }
    match best {
        Some((_, var, ty)) if var == handler_recv => ty.clone(),
        _ => String::new(),
    }
}

/// Parse a Go ServeMux pattern argument into `(verb, path)`. Joins every string
/// literal in the (possibly concatenated) expression, then splits an optional
/// leading HTTP-method token. Returns `None` when the result isn't a `/`-path.
fn parse_go_route_pattern(arg: &str) -> Option<(String, String)> {
    let lit = concat_string_literals(arg);
    let lit = lit.trim();
    if lit.is_empty() {
        return None;
    }
    let (head, rest) = match lit.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim_start()),
        None => ("", lit),
    };
    const VERBS: &[&str] = &[
        "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "CONNECT", "TRACE",
    ];
    let upper = head.to_ascii_uppercase();
    let (verb, path) = if VERBS.contains(&upper.as_str()) && !rest.is_empty() {
        (upper, rest.to_string())
    } else {
        ("ANY".to_string(), lit.to_string())
    };
    if !path.starts_with('/') {
        return None;
    }
    Some((verb, path))
}

/// Concatenate the contents of every double-quoted or backtick-raw string
/// literal in an expression, ignoring `+` operators and variable identifiers.
fn concat_string_literals(expr: &str) -> String {
    let b = expr.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' && j + 1 < b.len() {
                        out.push(b[j + 1] as char);
                        j += 2;
                        continue;
                    }
                    out.push(b[j] as char);
                    j += 1;
                }
                i = j + 1;
            }
            b'`' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'`' {
                    out.push(b[j] as char);
                    j += 1;
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Parse a handler argument expression into `(receiver var, method)`. Recognizes
/// a method value `recv.Method` (returns the trailing receiver segment + method)
/// and a bare function `Fn` (returns an empty receiver).
fn parse_go_handler(arg: &str) -> Option<(String, String)> {
    let s = arg.trim_matches(|c: char| c == '(' || c == ')' || c == '&' || c.is_whitespace());
    if let Some(dot) = s.rfind('.') {
        let recv_last = s[..dot].rsplit('.').next().unwrap_or("");
        let method = &s[dot + 1..];
        if is_ident(method) && is_ident(recv_last) {
            return Some((recv_last.to_string(), method.to_string()));
        }
        if is_ident(method) {
            return Some((String::new(), method.to_string()));
        }
        return None;
    }
    if is_ident(s) {
        return Some((String::new(), s.to_string()));
    }
    None
}

/// `(simple class name, byte index of the `class` keyword)` for the controller —
/// the first `class <Ident>` after the `@RestController`/`@Controller`
/// stereotype, so a comment mentioning "class" before the annotations can't
/// derail detection.
fn find_controller_class(text: &str) -> Option<(String, usize)> {
    let stereotype = text
        .find("@RestController")
        .or_else(|| text.find("@Controller"))?;
    let b = text.as_bytes();
    let mut i = stereotype;
    while let Some(rel) = text[i..].find("class") {
        let at = i + rel;
        let before_ok = at == 0 || !is_ident_byte(b[at - 1]);
        let after = at + 5;
        let after_ok = after < b.len() && b[after].is_ascii_whitespace();
        if before_ok && after_ok {
            let ns = skip_ws(b, after);
            let (name, _) = read_ident(b, ns);
            if !name.is_empty() {
                return Some((name, at));
            }
        }
        i = at + 5;
    }
    None
}

/// The class-level `@RequestMapping` path (the route prefix), or empty.
fn class_level_base_path(text: &str, class_decl_at: usize) -> String {
    let region = &text[..class_decl_at];
    let Some(at) = region.rfind("@RequestMapping") else {
        return String::new();
    };
    let b = text.as_bytes();
    let after = at + "@RequestMapping".len();
    let j = skip_ws(b, after);
    if j < b.len() && b[j] == b'(' {
        if let Some((body, _)) = balanced_parens(b, j) {
            return annotation_path(body);
        }
    }
    String::new()
}

/// `(verb, is_mapping_annotation)` for a Spring mapping annotation simple name.
fn mapping_kind(annot: &str) -> (&'static str, bool) {
    match annot {
        "GetMapping" => ("GET", true),
        "PostMapping" => ("POST", true),
        "PutMapping" => ("PUT", true),
        "DeleteMapping" => ("DELETE", true),
        "PatchMapping" => ("PATCH", true),
        "RequestMapping" => ("ANY", true),
        _ => ("", false),
    }
}

/// The mapping path inside an annotation's `(...)` body. Prefers an explicit
/// `value=`/`path=` attribute, else the first string literal; arrays like
/// `{"/a","/b"}` take the first entry.
fn annotation_path(body: &str) -> String {
    for key in ["value", "path"] {
        if let Some(p) = body.find(key) {
            let rest = body[p + key.len()..].trim_start();
            if let Some(stripped) = rest.strip_prefix('=') {
                if let Some(q) = first_quoted(stripped) {
                    return q;
                }
            }
        }
    }
    first_quoted(body).unwrap_or_default()
}

/// The verb of a method-level `@RequestMapping`, read from
/// `method = RequestMethod.<VERB>`.
fn request_method_verb(body: &str) -> Option<String> {
    let p = body.find("RequestMethod.")?;
    let (v, _) = read_ident(body.as_bytes(), p + "RequestMethod.".len());
    (!v.is_empty()).then(|| v.to_ascii_uppercase())
}

/// From just past a mapping annotation, skip any further annotations + comments,
/// then return the handler method name (the identifier right before the first
/// `(` of the signature). Tolerates Swagger annotations, `@Override`/
/// `@ResponseBody`, Javadoc, and generic return types containing `<...>`.
fn find_handler_method(b: &[u8], start: usize) -> Option<String> {
    let mut i = start;
    loop {
        i = skip_ws_and_comments(b, i);
        if i < b.len() && b[i] == b'@' {
            let (_annot, after) = read_ident(b, i + 1);
            let mut k = skip_ws(b, after);
            if k < b.len() && b[k] == b'(' {
                if let Some((_body, end)) = balanced_parens(b, k) {
                    k = end;
                }
            }
            i = k;
            continue;
        }
        break;
    }
    // At the signature: find the first '(' (the param list). The method name is
    // the identifier immediately before it (generics carry no '(').
    let mut j = i;
    while j < b.len() && b[j] != b'(' {
        // Hitting a class/block boundary first means this wasn't a method.
        if matches!(b[j], b'{' | b'}' | b';' | b'=') {
            return None;
        }
        j += 1;
    }
    if j >= b.len() {
        return None;
    }
    let mut end = j;
    while end > i && b[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut s = end;
    while s > i && is_ident_byte(b[s - 1]) {
        s -= 1;
    }
    if s == end {
        return None;
    }
    let name = std::str::from_utf8(&b[s..end]).ok()?;
    if name.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    Some(name.to_string())
}

/// Join a class-level prefix and a method path into one normalized route:
/// single leading slash, no empty/duplicate segments, no trailing slash.
fn normalize_route(base: &str, method: &str) -> String {
    let mut segs: Vec<&str> = Vec::new();
    for part in [base, method] {
        for seg in part.split('/') {
            let s = seg.trim();
            if !s.is_empty() {
                segs.push(s);
            }
        }
    }
    if segs.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segs.join("/"))
    }
}

/// The searchable `name` for a route node: the last static (non-`{var}`) path
/// segment, so `search getMeasuresInfo` resolves `/style-info/getMeasuresInfo`.
fn route_search_name(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .next_back()
        .unwrap_or(path)
        .to_string()
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Skip whitespace and `//` / `/* */` comments.
fn skip_ws_and_comments(b: &[u8], mut i: usize) -> usize {
    loop {
        i = skip_ws(b, i);
        if i + 1 < b.len() && b[i] == b'/' && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if i + 1 < b.len() && b[i] == b'/' && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
        } else {
            break;
        }
    }
    i
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
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

fn line_of(text: &str, byte_idx: usize) -> u32 {
    text[..byte_idx.min(text.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count() as u32
        + 1
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
    fn parses_spring_controller_routes_with_class_prefix_and_verbs() {
        // The crux: the URL path segment (`getMeasuresInfo`) is NOT the Java
        // method name (`measuresInfo`). The parser must recover both, joined to
        // the class-level `@RequestMapping` prefix, with the right HTTP verb,
        // even with Swagger/`@ApiOperation` + Javadoc noise between annotation
        // and signature, and a generic return type containing `<...>`.
        let java = r#"
@RestController
@RequestMapping("/style-info")
public class StyleInfoController {

    @ApiOperation("标准号尺码")
    /** javadoc noise */
    @GetMapping("/getMeasuresInfo")
    @ResponseBody
    public RS<List<SizeVO>> measuresInfo(@RequestParam Integer id) { return null; }

    @PostMapping(value = "/save", produces = "application/json")
    public RS<Void> doSave(@RequestBody Foo f) { return null; }

    @RequestMapping(value = "/legacy", method = RequestMethod.PUT)
    public RS<Void> legacyUpdate() { return null; }

    @GetMapping
    public RS<String> root() { return null; }
}
"#;
        let routes = parse_http_routes(java);
        let find = |m: &str| routes.iter().find(|r| r.method == m).cloned();

        let measures = find("measuresInfo").expect("measuresInfo route");
        assert_eq!(measures.path, "/style-info/getMeasuresInfo");
        assert_eq!(measures.verb, "GET");
        assert_eq!(measures.class, "StyleInfoController");

        let save = find("doSave").expect("doSave route");
        assert_eq!(save.path, "/style-info/save");
        assert_eq!(save.verb, "POST");

        let legacy = find("legacyUpdate").expect("legacyUpdate route");
        assert_eq!(legacy.path, "/style-info/legacy");
        assert_eq!(legacy.verb, "PUT");

        // A bare @GetMapping with no path falls back to the class prefix.
        let root = find("root").expect("root route");
        assert_eq!(root.path, "/style-info");
        assert_eq!(root.verb, "GET");
    }

    #[test]
    fn ignores_feign_clients_and_non_controllers() {
        // Feign *clients* use @GetMapping on interface methods but are NOT HTTP
        // servers — indexing them as routes would be wrong. The class-level
        // stereotype gate (@RestController/@Controller) must exclude them.
        let feign = r#"
@FeignClient(name = "craft", path = "/craft")
public interface CraftFeign {
    @GetMapping("/getById")
    RS<CraftVO> getCraftById(@RequestParam Integer id);
}
"#;
        assert!(parse_http_routes(feign).is_empty());
    }

    #[test]
    fn links_http_route_to_handler_method_by_class_and_method() {
        // End-to-end: a controller file + a pre-seeded handler JavaMethod node
        // must yield an HttpRoute node whose `name` is the URL segment and a
        // `route --references--> method` edge so `trace <url-path>` descends
        // into the handler even though path segment != method name.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("StyleInfoController.java"),
            r#"
package com.kutesmart.cloud.style.controller;
@RestController
@RequestMapping("/style-info")
public class StyleInfoController {
    @GetMapping("/getMeasuresInfo")
    public RS<List<SizeVO>> measuresInfo(@RequestParam Integer id) { return null; }
}
"#,
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        // Pre-seed the handler method node (normally from the Java code indexer).
        let method_id = ArtifactId::new(
            "java::src/main/java/com/kutesmart/cloud/style/controller/StyleInfoController.java::StyleInfoController.measuresInfo",
        );
        store
            .upsert_node(&Node::new(method_id.clone(), NodeKind::JavaMethod))
            .unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.http_routes, 1, "one HTTP route indexed");
        assert_eq!(stats.route_method_edges, 1, "route->method edge linked");

        // The route node is searchable by the URL segment.
        let routes = store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].name.as_deref(), Some("getMeasuresInfo"));
        assert_eq!(routes[0].path.as_deref(), Some("/style-info/getMeasuresInfo"));

        // route --references--> handler method.
        let from_route = store.list_edges_from(&routes[0].id).unwrap();
        assert!(
            from_route
                .iter()
                .any(|e| e.kind == EdgeKind::References && e.to_id == method_id),
            "expected route->method References edge, got {from_route:?}"
        );
    }

    #[test]
    fn parse_go_routes_direct_and_concat_pattern() {
        // Go 1.22 `net/http` ServeMux: a method-aware pattern string, where the
        // handler is a method value on the enclosing receiver. The pattern may be
        // split across string concatenation with a gateway-prefix variable.
        let go = r#"
package handler

import "net/http"

type CustomerSizeHandler struct{ Svc any }

func (h *CustomerSizeHandler) Register(mux *http.ServeMux) {
	mux.HandleFunc("POST /customer/customer/size/getCustomerPosition", h.GetCustomerPosition)
}

type DesignHandler struct{ Svc any }

func (h *DesignHandler) Register(mux *http.ServeMux, gatewayPrefix string) {
	mux.HandleFunc("GET "+gatewayPrefix+"/app/craft/getDesignInfo", h.GetDesignInfo)
}
"#;
        let routes = parse_go_routes(go);
        assert_eq!(routes.len(), 2, "two routes parsed, got {routes:?}");

        let pos = routes
            .iter()
            .find(|r| r.method == "GetCustomerPosition")
            .expect("getCustomerPosition route");
        assert_eq!(pos.verb, "POST");
        assert_eq!(pos.path, "/customer/customer/size/getCustomerPosition");
        assert_eq!(pos.class, "CustomerSizeHandler");

        let design = routes
            .iter()
            .find(|r| r.method == "GetDesignInfo")
            .expect("getDesignInfo route");
        assert_eq!(design.verb, "GET");
        // The gateway-prefix variable is opaque at index time; the stable static
        // path segments are still recovered from the literal concatenation parts.
        assert_eq!(design.path, "/app/craft/getDesignInfo");
        assert_eq!(design.class, "DesignHandler");
    }

    #[test]
    fn parse_go_routes_ignores_non_routing_go() {
        let go = r#"
package service

type DesignService struct{}

func (s *DesignService) GetDesignInfo(id int) (any, error) {
	return s.repo.GetDesignInfo(id)
}
"#;
        assert!(parse_go_routes(go).is_empty());
    }

    #[test]
    fn links_go_http_route_to_handler_method() {
        // A Go handler file registering a route + a pre-seeded handler GoMethod
        // node must yield an HttpRoute node and a route--references-->method edge,
        // matched by the `Type.Method` suffix shared with `JavaMethod`.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("internal/craft/handler")).unwrap();
        std::fs::write(
            root.join("internal/craft/handler/design_handler.go"),
            r#"
package handler

import "net/http"

type DesignHandler struct{ Svc any }

func (h *DesignHandler) Register(mux *http.ServeMux, gatewayPrefix string) {
	mux.HandleFunc("GET "+gatewayPrefix+"/app/craft/getDesignInfo", h.GetDesignInfo)
}
"#,
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        // Pre-seed the handler method node (normally from the Go code indexer).
        let method_id = ArtifactId::new(
            "go::internal/craft/handler/design_handler.go::DesignHandler.GetDesignInfo",
        );
        store
            .upsert_node(&Node::new(method_id.clone(), NodeKind::GoMethod))
            .unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.http_routes, 1, "one Go HTTP route indexed");
        assert_eq!(stats.route_method_edges, 1, "route->method edge linked");

        let routes = store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].name.as_deref(), Some("getDesignInfo"));
        assert_eq!(routes[0].path.as_deref(), Some("/app/craft/getDesignInfo"));

        let from_route = store.list_edges_from(&routes[0].id).unwrap();
        assert!(
            from_route
                .iter()
                .any(|e| e.kind == EdgeKind::References && e.to_id == method_id),
            "expected Go route->method References edge, got {from_route:?}"
        );
    }

    #[test]
    fn reindex_prunes_stale_schema_nodes_and_edges() {
        // Re-indexing must not leave behind nodes/edges for source that no longer
        // exists. Like the language indexers, the schema pass must clear its own
        // prior outputs first, so a deleted route/table disappears without a full
        // rebuild.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("internal/craft/handler")).unwrap();
        let p = root.join("internal/craft/handler/h.go");

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        for m in ["DesignHandler.GetDesignInfo", "DesignHandler.SelectCraftTree"] {
            let id = ArtifactId::new(format!("go::internal/craft/handler/h.go::{m}"));
            store
                .upsert_node(&Node::new(id, NodeKind::GoMethod))
                .unwrap();
        }

        std::fs::write(
            &p,
            r#"
package handler
import "net/http"
type DesignHandler struct{}
func (h *DesignHandler) Register(mux *http.ServeMux) {
	mux.HandleFunc("GET /app/craft/getDesignInfo", h.GetDesignInfo)
	mux.HandleFunc("GET /app/craft/selectCraftTree", h.SelectCraftTree)
}
"#,
        )
        .unwrap();
        let s1 = index_schema_into(&mut store, root).unwrap();
        assert_eq!(s1.http_routes, 2, "first pass indexes both routes");
        assert_eq!(store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap().len(), 2);

        // Drop the second route, then re-index the same store.
        std::fs::write(
            &p,
            r#"
package handler
import "net/http"
type DesignHandler struct{}
func (h *DesignHandler) Register(mux *http.ServeMux) {
	mux.HandleFunc("GET /app/craft/getDesignInfo", h.GetDesignInfo)
}
"#,
        )
        .unwrap();
        let _s2 = index_schema_into(&mut store, root).unwrap();

        let routes = store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap();
        assert_eq!(
            routes.len(),
            1,
            "stale route node must be pruned on re-index, got {routes:?}"
        );
        assert_eq!(routes[0].name.as_deref(), Some("getDesignInfo"));
        // The surviving route keeps exactly one handler edge; no dangling edge
        // for the removed route remains.
        let all_route_edges: usize = store
            .list_nodes_by_kind(NodeKind::HttpRoute)
            .unwrap()
            .iter()
            .map(|r| {
                store
                    .list_edges_from(&r.id)
                    .unwrap()
                    .into_iter()
                    .filter(|e| e.kind == EdgeKind::References)
                    .count()
            })
            .sum();
        assert_eq!(all_route_edges, 1, "exactly one live route->method edge");
    }

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
    fn infers_table_name_from_class_when_tablename_absent() {
        // MyBatis-Plus convention: an entity without an explicit @TableName maps
        // to the snake_case of its class name (`SizeSys` -> `size_sys`). Such
        // entities were previously dropped, so mapper SQL like
        // `from size_sys ss` had no DbTable node to persist_to and the table
        // silently vanished from traces. The @TableId / @TableField field
        // annotations are the tell that this class IS a persisted entity.
        let java = r#"
import com.baomidou.mybatisplus.annotation.IdType;
import com.baomidou.mybatisplus.annotation.TableField;
import com.baomidou.mybatisplus.annotation.TableId;

@Data
@ApiModel(value="SizeSys对象", description="尺寸主表")
public class SizeSys implements Serializable {

    private static final long serialVersionUID = 1L;

    @TableId(value = "id", type = IdType.AUTO)
    private Integer id;

    @ApiModelProperty(value = "尺码名称")
    private String name;

    private Integer isDefault;

    @TableField(exist = false)
    private String extra;
}
"#;
        let tables = parse_java_entity_tables(java);
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.name, "size_sys");
        assert_eq!(t.source, "orm-implicit");
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "is_default"]);
        assert!(!names.contains(&"extra")); // @TableField(exist=false) skipped
    }

    #[test]
    fn does_not_infer_table_for_non_entity_classes() {
        // A service impl imports MyBatis-Plus (ServiceImpl/BaseMapper) yet has no
        // @TableId/@TableField fields — it must NOT be mistaken for a table
        // named `size_sys_service_impl`.
        let svc = r#"
import com.baomidou.mybatisplus.extension.service.impl.ServiceImpl;
public class SizeSysServiceImpl extends ServiceImpl<SizeSysMapper, SizeSys>
        implements ISizeSysService {
    private Integer cacheSize;
    public java.util.List<SizeSys> all() { return null; }
}
"#;
        assert!(parse_java_entity_tables(svc).is_empty());

        // A plain VO/DTO with no MyBatis-Plus field annotations is not a table.
        let vo = r#"
@Data
public class SizeSysVO implements Serializable {
    private Integer id;
    private String name;
}
"#;
        assert!(parse_java_entity_tables(vo).is_empty());
    }

    #[test]
    fn normalize_column_aligns_dialects() {
        assert_eq!(
            normalize_column("categoryId"),
            normalize_column("category_id")
        );
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
        assert_eq!(
            s0.namespace.as_deref(),
            Some("com.kutesmart.cloud.craft.mapper.CraftConflictMapper")
        );
        assert!(
            s0.sql.contains("from craft_conflict cc"),
            "sql body kept: {}",
            s0.sql
        );
        assert!(
            s0.sql.contains("category_id in"),
            "sql body kept: {}",
            s0.sql
        );
        assert!(
            s0.line >= 4 && s0.line <= 5,
            "line points at the <select>: {}",
            s0.line
        );
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
    fn mapper_include_refid_inlines_sql_fragment() {
        // The FROM clause lives in a reusable `<sql>` fragment pulled in via
        // `<include refid>`; the statement body alone has no table. The parser
        // must inline the fragment so the SQL — and its table refs — are whole.
        let xml = r#"<?xml version="1.0"?>
<mapper namespace="com.x.OrdenCraftMapper">
  <sql id="selectOrdenCraftVO">
      select oc.id, oc.orden_detail_id from orden_craft oc
  </sql>
  <select id="selectOrdenCraftByDetailId" resultType="x">
      <include refid="selectOrdenCraftVO" />
      where oc.orden_detail_id = #{detailId}
      order by oc.id
  </select>
</mapper>"#;
        let stmts = parse_mapper_stmts(xml);
        // The <sql> fragment is not itself a CRUD statement.
        assert_eq!(stmts.len(), 1, "only the <select> is a statement: {stmts:?}");
        let s = &stmts[0];
        assert_eq!(s.id, "selectOrdenCraftByDetailId");
        assert!(
            s.sql.to_ascii_lowercase().contains("from orden_craft"),
            "include not inlined: {}",
            s.sql
        );
        assert!(
            extract_sql_table_refs(&s.sql).contains(&"orden_craft".to_string()),
            "table not recovered after inlining: {}",
            s.sql
        );
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
    fn extract_table_refs_handles_newline_and_tab_delimited_keywords() {
        // Real mappers put `FROM` at end-of-line with the table on the next:
        // keyword matching must treat any whitespace (newline/tab), not only a
        // literal space, as the delimiter.
        let sql = "SELECT GROUP_CONCAT(DISTINCT img_id)\n        FROM\n        style_finish_stock\n        WHERE x = 1";
        assert_eq!(extract_sql_table_refs(sql), vec!["style_finish_stock"]);

        // Newline before JOIN, tab after it.
        let j = "select * from a\nleft join\tb on a.id = b.id";
        let mut t = extract_sql_table_refs(j);
        t.sort();
        assert_eq!(t, vec!["a", "b"]);

        // A `from`/`join` embedded in an identifier or column name is not a hit.
        assert!(extract_sql_table_refs("select date_from, transform_id from t").contains(&"t".to_string()));
        assert_eq!(extract_sql_table_refs("select date_from, transform_id from t").len(), 1);
    }

    #[test]
    fn extract_table_refs_handles_insert_without_into() {
        // MySQL allows `INSERT <table>` (no INTO); MyBatis batch inserts use it.
        let t1 = extract_sql_table_refs("insert style_package_info ( category, style_code ) values");
        assert!(
            t1.contains(&"style_package_info".to_string()),
            "insert-without-into missed the table: {t1:?}"
        );
        // Classic `insert into x` still resolves to x (and never to `into`).
        let t2 = extract_sql_table_refs("insert into v_image_post(string, time) value(#{s}, now())");
        assert_eq!(t2, vec!["v_image_post"]);
    }

    #[test]
    fn extract_table_refs_excludes_cte_names() {
        // `WITH RECURSIVE cte AS (…) … FROM cte`: `cte` is a CTE alias, not a
        // base table. The real table (`craft`) is kept; the CTE name is dropped
        // so it isn't mistaken for (or synthesized as) a table.
        let sql = "WITH RECURSIVE cte AS ( SELECT id, pid FROM craft WHERE id = 1 ) SELECT * FROM cte";
        let t = extract_sql_table_refs(sql);
        assert!(t.contains(&"craft".to_string()), "real base table kept: {t:?}");
        assert!(!t.contains(&"cte".to_string()), "CTE alias excluded: {t:?}");

        // Multiple CTEs in one WITH.
        let multi = "with a as (select 1 from t1), b as (select 2 from t2) select * from a join b";
        let mut m = extract_sql_table_refs(multi);
        m.sort();
        assert_eq!(m, vec!["t1", "t2"], "only base tables, no CTE aliases: {m:?}");
    }

    #[test]
    fn extract_table_refs_skips_on_duplicate_key_update_columns() {
        // `ON DUPLICATE KEY UPDATE col=…` — the token after UPDATE is a column,
        // not a table, so it must not be mistaken for one (would otherwise
        // synthesize a bogus external table named after the column).
        let sql = "insert into t (a, b) values (1, 2) on duplicate key update a = values(a)";
        let t = extract_sql_table_refs(sql);
        assert!(t.contains(&"t".to_string()), "{t:?}");
        assert!(!t.contains(&"a".to_string()), "SET column must not be a table: {t:?}");
    }

    #[test]
    fn external_table_name_plausibility() {
        assert!(is_plausible_table_name("member_role"));
        assert!(is_plausible_table_name("sys_user_role"));
        // SQL keywords / junk are rejected.
        assert!(!is_plausible_table_name("select"));
        assert!(!is_plausible_table_name("dual"));
        assert!(!is_plausible_table_name("1table"));
        assert!(!is_plausible_table_name("x"));
        assert!(!is_plausible_table_name(""));
        // Trailing `_` is the residue of a dynamic name `express_dict_${x}`.
        assert!(!is_plausible_table_name("express_dict_"));
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

        let stmt_id =
            ArtifactId::new("sql_mapper::CraftConflictMapper.xml::selectConflictListTreeByCloth");
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
            from_stmt.iter().any(|e| e.kind == EdgeKind::PersistsTo
                && e.to_id.as_str().ends_with("::craft_conflict")),
            "expected stmt->table PersistsTo edge, got {from_stmt:?}"
        );
    }

    #[test]
    fn synthesizes_external_table_node_for_entityless_sql_ref() {
        // A junction table touched only via raw mapper SQL — no entity, no DDL.
        // specslice must synthesize an `external` DbTable node so the trace from
        // the mapper still reaches a table instead of dead-ending.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("MemberRoleMapper.xml"),
            r#"<?xml version="1.0"?>
<mapper namespace="com.x.MemberRoleMapper">
  <delete id="deleteMemberRole">delete from member_role where user_id = #{userId}</delete>
</mapper>"#,
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.external_tables, 1, "one external table synthesized");

        let tables = store.list_nodes_by_kind(NodeKind::DbTable).unwrap();
        let ext = tables
            .iter()
            .find(|n| n.id.as_str() == "db_table::<external>::member_role")
            .expect("external table node present");
        assert_eq!(ext.name.as_deref(), Some("member_role"));
        let meta: DbTableMeta = serde_json::from_str(ext.metadata_json.as_ref().unwrap()).unwrap();
        assert!(meta.external, "node marked external");
        assert_eq!(meta.source, "external");
        assert!(meta.columns.is_empty(), "schema unknown -> no columns");

        // stmt --persists_to--> external table.
        let stmt_id = ArtifactId::new("sql_mapper::MemberRoleMapper.xml::deleteMemberRole");
        let from_stmt = store.list_edges_from(&stmt_id).unwrap();
        assert!(
            from_stmt
                .iter()
                .any(|e| e.kind == EdgeKind::PersistsTo && e.to_id == ext.id),
            "expected stmt->external table PersistsTo edge, got {from_stmt:?}"
        );

        // Idempotent: a second pass synthesizes nothing new (node reused).
        let stats2 = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats2.external_tables, 1, "external count stable on re-index");
        let after: Vec<_> = store
            .list_nodes_by_kind(NodeKind::DbTable)
            .unwrap()
            .into_iter()
            .filter(|n| n.id.as_str() == "db_table::<external>::member_role")
            .collect();
        assert_eq!(after.len(), 1, "no duplicate external node on re-index");
    }

    #[test]
    fn links_spring_service_impl_without_i_prefix() {
        // Dominant Spring convention: `FooService` (interface) ⇒ `FooServiceImpl`
        // (impl), with NO `I` prefix. The linker must pair them so traversal
        // descends through interface dispatch instead of dead-ending at the
        // declaration. Reproduces the vub/yolan miss (DictSystemService).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("schema.sql"),
            "CREATE TABLE dict_system (id BIGINT);",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let iface = ArtifactId::new(
            "java::src/com/vhub/yolan/service/DictSystemService.java::DictSystemService.getDictSystem",
        );
        let impl_id = ArtifactId::new(
            "java::src/com/vhub/yolan/service/impl/DictSystemServiceImpl.java::DictSystemServiceImpl.getDictSystem",
        );
        store
            .upsert_node(&Node::new(iface.clone(), NodeKind::JavaMethod))
            .unwrap();
        store
            .upsert_node(&Node::new(impl_id.clone(), NodeKind::JavaMethod))
            .unwrap();

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
    fn discovers_create_table_embedded_in_go_migrations_and_links_repo_method() {
        // Real Go pattern (Shift backend): the schema is defined as a Go raw
        // string literal in `migrations.go` (no `.sql` file at all), and repo
        // methods embed SQL string literals. The schema indexer must (1) parse
        // the embedded `CREATE TABLE` into a DbTable node, and (2) link the repo
        // method body's inline `... FROM referrals ...` to it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("migrations.go"),
            "package storage\nvar migrations = []migration{{\n  version: 1,\n  sql: `\nCREATE TABLE referrals (\n    id          TEXT PRIMARY KEY,  -- internal uuid (v7)\n    referrer_id TEXT NOT NULL,\n    referee_id  TEXT NOT NULL\n);\nCREATE INDEX idx_ref ON referrals(referrer_id);\n`,\n}}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("referral.go"),
            "package repo\nfunc (r *ReferralRepo) CountByReferrer() {\n\t_ = `SELECT COUNT(*) FROM referrals WHERE referrer_id = ?`\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let method_id = ArtifactId::new("go::referral.go::ReferralRepo.CountByReferrer");
        let mut m = Node::new(method_id.clone(), NodeKind::GoMethod);
        m.path = Some("referral.go".to_string());
        m.start_line = Some(2);
        m.end_line = Some(4);
        store.upsert_node(&m).unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(
            stats.sql_tables, 1,
            "embedded CREATE TABLE in a .go file must be discovered: {stats:?}"
        );
        let tables = store.list_nodes_by_kind(NodeKind::DbTable).unwrap();
        assert!(
            tables
                .iter()
                .any(|t| t.name.as_deref() == Some("referrals")),
            "expected a `referrals` DbTable node, got {tables:?}"
        );
        assert!(
            stats.inline_sql_table_edges >= 1,
            "repo method inline SQL must link to the embedded table: {stats:?}"
        );
        let from_method = store.list_edges_from(&method_id).unwrap();
        assert!(
            from_method.iter().any(
                |e| e.kind == EdgeKind::PersistsTo && e.to_id.as_str().ends_with("::referrals")
            ),
            "expected repo-method->table PersistsTo edge, got {from_method:?}"
        );
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
                .any(|e| e.kind == EdgeKind::PersistsTo && e.to_id.as_str().ends_with("::craft")),
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
        store
            .upsert_node(&Node::new(iface.clone(), NodeKind::JavaMethod))
            .unwrap();
        store
            .upsert_node(&Node::new(imp.clone(), NodeKind::JavaMethod))
            .unwrap();

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
        assert_eq!(
            extract_sql_table_refs("update craft_default set sort=1 where id=#{id}"),
            vec!["craft_default"]
        );
        assert_eq!(
            extract_sql_table_refs("insert into craft_recommend(id) values(#{id})"),
            vec!["craft_recommend"]
        );
        // a subquery after FROM (next token is '(') must not yield a bogus table.
        assert!(
            extract_sql_table_refs("select * from (select id from craft) t")
                .contains(&"craft".to_string())
        );
        assert!(
            !extract_sql_table_refs("select * from (select id from craft) t")
                .contains(&"(select".to_string())
        );
    }
}
