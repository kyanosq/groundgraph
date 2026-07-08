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
use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::config::EngineConfig;
use crate::source_text::read_node_source;

/// Build-output directories the schema walk skips *in addition* to the shared
/// [`crate::treesitter::ALWAYS_SKIP_DIRS`] noise set (VCS, agent worktrees,
/// vendored deps, Python virtualenvs/caches). Kept separate because these are
/// build artifacts rather than universal noise; the union is the single source
/// of truth shared with the tree-sitter symbol walk.
const SCHEMA_SKIP_DIRS: &[&str] = &["target", "build", "dist", ".build"];

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
    /// Client-consumed `HttpRoute` nodes recovered from a Dart endpoint-table
    /// class (`class ApiEndpoints { static const String x = '/path'; }`). These
    /// are the backend routes the client *calls* (verb `CONSUMED`), the mirror
    /// image of the server-side served routes — so a client repo's API surface
    /// becomes queryable by URL path and comparable to the server it ports.
    #[serde(default)]
    pub consumed_routes: usize,
    /// `caller method --references--> consumed HttpRoute` edges linked by finding
    /// `<EndpointsClass>.<NAME>` references in a callable's body.
    #[serde(default)]
    pub route_consumer_edges: usize,
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
    crate::config::load_config(repo_root)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    crate::config::resolve_storage_path(repo_root, config)
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
    // Client-consumed backend routes recovered from Dart endpoint-table classes,
    // accumulated across the walk and turned into nodes/edges after it (needs the
    // language indexers' callable nodes to already exist in the store).
    let mut dart_route_consts: Vec<DartRouteConst> = Vec::new();
    // Inline HTTP-client calls (Dart `_dio.get('/v1/me')`, TS `http.post('/x')`)
    // the client makes, with their file so the consumed route links to the
    // enclosing callable by line. Kept per-language so each links to its own
    // callable kinds.
    let mut dart_consumed_calls: Vec<(String, InlineConsumedCall)> = Vec::new();
    let mut ts_consumed_calls: Vec<(String, InlineConsumedCall)> = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_skipped_walk_entry(e))
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
        // Bound memory before any full-file `read_to_string` in this loop (XML
        // mapper, route scan, Dart/TS consumed-call scan, DDL `read_and`): an
        // 8 GB vendored/generated file, minified bundle or `.g.dart` would
        // otherwise be slurped whole into a String and OOM-kill the indexer.
        // Same capacity gate as the code/docs indexers (#67/#76/#186); an
        // oversized file simply contributes no schema/route evidence.
        if crate::source_text::is_oversized_source(path) {
            continue;
        }
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
        let is_go_test = ext == "go" && path.to_string_lossy().ends_with("_test.go");
        let is_route_lang = matches!(
            ext.as_str(),
            "java" | "go" | "py" | "ts" | "tsx" | "js" | "jsx" | "mjs"
        );
        if is_route_lang && !is_go_test {
            if let Ok(text) = std::fs::read_to_string(path) {
                // Spring MVC annotations (Java), net/http + Gin registrations
                // (Go), FastAPI/Flask decorators (Python) and Express/Hono
                // registrations (TS/JS) all land as HttpRoute nodes so the *URL
                // path* a client calls resolves to its handler regardless of
                // backend language. Go scans both net/http and Gin since a
                // service may mix them.
                let routes = match ext.as_str() {
                    "java" => parse_http_routes(&text),
                    "go" => {
                        let mut rs = parse_go_routes(&text);
                        rs.extend(parse_gin_routes(&text));
                        rs
                    }
                    "py" => parse_python_routes(&text),
                    _ => parse_ts_server_routes(&text),
                };
                if !routes.is_empty() {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    for r in &routes {
                        stats.http_routes += 1;
                        store
                            .upsert_node(&http_route_node(&rel, r))
                            .with_context(|| {
                                format!("upserting http route {} {} from {rel}", r.verb, r.path)
                            })?;
                    }
                }
            }
        }
        // Dart clients keep the backend routes they call in a constant table
        // (`class ApiEndpoints { static const String x = '/path'; }`). Collect
        // them now; nodes/edges are emitted after the walk so the consuming
        // callable nodes already exist.
        if ext == "dart" {
            if let Ok(text) = std::fs::read_to_string(path) {
                dart_route_consts.extend(parse_dart_route_constants(&text));
                let calls = parse_dart_consumed_calls(&text);
                if !calls.is_empty() {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    for c in calls {
                        dart_consumed_calls.push((rel.clone(), c));
                    }
                }
            }
        }
        // TS/JS clients (axios/fetch) consume backend routes the same inline way;
        // `.tsx` is included since hooks/components call the API directly too.
        if matches!(ext.as_str(), "ts" | "tsx" | "js" | "jsx" | "mjs") {
            if let Ok(text) = std::fs::read_to_string(path) {
                let calls = parse_ts_consumed_calls(&text);
                if !calls.is_empty() {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    for c in calls {
                        ts_consumed_calls.push((rel.clone(), c));
                    }
                }
            }
        }
        let tables = match ext.as_str() {
            "sql" => read_and(path, parse_sql_tables_from_sql_file),
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
    link_dart_consumed_routes(store, root, &dart_route_consts, &mut stats)?;
    link_inline_consumed_routes(store, &dart_consumed_calls, "dart", &mut stats)?;
    link_inline_consumed_routes(store, &ts_consumed_calls, "typescript", &mut stats)?;
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
    // Java (`JavaMethod`) and Go (`GoMethod`) handler nodes share the id suffix
    // shape `Type.method`; Python handlers are module-level functions whose
    // suffix is the bare function name (`list_strategies`). Indexing all of
    // them lets one map serve Spring, net/http and FastAPI/Flask alike.
    for kind in [
        NodeKind::JavaMethod,
        NodeKind::GoMethod,
        NodeKind::PythonFunction,
        NodeKind::PythonMethod,
    ] {
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
        // Client-consumed routes carry no server handler; they are linked from
        // the calling Dart method by `link_dart_consumed_routes`, not here.
        if meta.handler_class.is_empty() && meta.handler_method.is_empty() {
            continue;
        }
        // Classless handlers (Python module functions) match on the bare
        // function name; class-based handlers (Java/Go) on `Type.method`.
        let key = if meta.handler_class.is_empty() {
            meta.handler_method.to_ascii_lowercase()
        } else {
            format!("{}.{}", meta.handler_class, meta.handler_method).to_ascii_lowercase()
        };
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

/// Mirror image of [`link_http_route_edges`] for *client* repos: turn the routes
/// a Dart client consumes (recovered from its `ApiEndpoints`-style constant
/// table) into `HttpRoute` nodes (verb `CONSUMED`) and link each calling method
/// with a `method --references--> route` edge. The handler match is by source:
/// a callable references a route when its body mentions `<Class>.<const>`.
///
/// This makes a client's backend API surface queryable by URL path and directly
/// comparable to the server it ports — the same path string resolves on both
/// sides, so a `trace` of the served route and a `search` of the consumed route
/// line up. Idempotent (upsert); each `(callable, path)` pair links at most once.
fn link_dart_consumed_routes(
    store: &mut Store,
    root: &Path,
    consts: &[DartRouteConst],
    stats: &mut SchemaIndexStats,
) -> Result<()> {
    use std::collections::{HashMap, HashSet};
    if consts.is_empty() {
        return Ok(());
    }
    // Unique path -> consumed route node id (nodes created up front so the
    // edges below satisfy the nodes-before-edges invariant); `Class.const`
    // reference token -> path so a body mention resolves to the route.
    let mut path_to_id: HashMap<String, ArtifactId> = HashMap::new();
    let mut ref_to_path: HashMap<String, String> = HashMap::new();
    for c in consts {
        ref_to_path.insert(format!("{}.{}", c.class_name, c.name), c.path.clone());
        if !path_to_id.contains_key(&c.path) {
            let node = consumed_route_node(&c.path);
            let id = node.id.clone();
            store
                .upsert_node(&node)
                .with_context(|| format!("upserting consumed route {}", c.path))?;
            stats.consumed_routes += 1;
            path_to_id.insert(c.path.clone(), id);
        }
    }

    let mut edges: Vec<EdgeAssertion> = Vec::new();
    for &kind in NodeKind::ALL {
        if !kind.is_callable() || kind.language() != Some("dart") {
            continue;
        }
        for node in store.list_nodes_by_kind(kind)? {
            let Some(src) = read_node_source(root, &node) else {
                continue;
            };
            let mut linked: HashSet<String> = HashSet::new();
            for (token, path) in &ref_to_path {
                if !body_references_token(&src.raw, token) {
                    continue;
                }
                if !linked.insert(path.clone()) {
                    continue; // already linked this route for this callable
                }
                if let Some(route_id) = path_to_id.get(path) {
                    edges.push(EdgeAssertion::fact(
                        node.id.clone(),
                        route_id.clone(),
                        EdgeKind::References,
                        EdgeSource::LanguageAdapter,
                    ));
                    stats.route_consumer_edges += 1;
                }
            }
        }
    }
    for edge in &mut edges {
        edge.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
        store.upsert_edge(edge).with_context(|| {
            format!(
                "linking dart route-consumer edge {} -> {}",
                edge.from_id.as_str(),
                edge.to_id.as_str()
            )
        })?;
    }
    Ok(())
}

/// Sibling of [`link_dart_consumed_routes`] for the inline-call shape recovered
/// by [`parse_dart_consumed_calls`] / [`parse_ts_consumed_calls`]: turn each
/// `_dio.get('/v1/me')` into a consumed `HttpRoute` node and link the *enclosing*
/// `lang` callable to it with a `method --references--> route` edge. The consumer
/// is the innermost callable whose line range contains the call, so the edge
/// lands on the real API method rather than the file. Idempotent; each
/// `(callable, route)` links at most once.
fn link_inline_consumed_routes(
    store: &mut Store,
    calls: &[(String, InlineConsumedCall)],
    lang: &str,
    stats: &mut SchemaIndexStats,
) -> Result<()> {
    use std::collections::{HashMap, HashSet};
    if calls.is_empty() {
        return Ok(());
    }
    // Consumed-route nodes, keyed by path alone (verb-agnostic) so the served
    // and consumed sides collapse to the same node identity for route-coverage.
    let mut path_to_id: HashMap<String, ArtifactId> = HashMap::new();
    for (_rel, call) in calls {
        if !path_to_id.contains_key(&call.path) {
            let node = consumed_route_node(&call.path);
            let id = node.id.clone();
            store
                .upsert_node(&node)
                .with_context(|| format!("upserting consumed route {}", call.path))?;
            path_to_id.insert(call.path.clone(), id);
            stats.consumed_routes += 1;
        }
    }
    // file -> [(callable id, start, end)] for innermost-enclosing attribution.
    let mut by_file: HashMap<String, Vec<(ArtifactId, u32, u32)>> = HashMap::new();
    for &kind in NodeKind::ALL {
        if !kind.is_callable() || kind.language() != Some(lang) {
            continue;
        }
        for node in store.list_nodes_by_kind(kind)? {
            let (Some(path), Some(start)) = (node.path.clone(), node.start_line) else {
                continue;
            };
            let end = node.end_line.unwrap_or(start);
            by_file
                .entry(path)
                .or_default()
                .push((node.id.clone(), start, end));
        }
    }
    let mut edges: Vec<EdgeAssertion> = Vec::new();
    let mut seen: HashSet<(ArtifactId, ArtifactId)> = HashSet::new();
    for (rel, call) in calls {
        let Some(route_id) = path_to_id.get(&call.path) else {
            continue;
        };
        let Some(cands) = by_file.get(rel) else {
            continue;
        };
        let enclosing = cands
            .iter()
            .filter(|(_, start, end)| *start <= call.line && call.line <= *end)
            .min_by_key(|(_, start, end)| end.saturating_sub(*start));
        let Some((cid, _, _)) = enclosing else {
            continue;
        };
        if seen.insert((cid.clone(), route_id.clone())) {
            edges.push(EdgeAssertion::fact(
                cid.clone(),
                route_id.clone(),
                EdgeKind::References,
                EdgeSource::LanguageAdapter,
            ));
            stats.route_consumer_edges += 1;
        }
    }
    for edge in &mut edges {
        edge.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
        store.upsert_edge(edge).with_context(|| {
            format!(
                "linking inline route-consumer edge {} -> {}",
                edge.from_id.as_str(),
                edge.to_id.as_str()
            )
        })?;
    }
    Ok(())
}

/// True when `body` mentions `token` (e.g. `ApiEndpoints.styleDetail`) as a
/// whole reference — the char before must not extend the leading identifier
/// (nor be a `.`, which would make it a nested member access) and the char
/// after must not extend the trailing identifier. Prevents a constant name
/// from matching inside a longer identifier.
fn body_references_token(body: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let b = body.as_bytes();
    let t = token.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = body[i..].find(token) {
        let start = i + rel;
        let end = start + t.len();
        let before_ok = start == 0 || (!is_ident_byte(b[start - 1]) && b[start - 1] != b'.');
        let after_ok = end >= b.len() || !is_ident_byte(b[end]);
        if before_ok && after_ok {
            return true;
        }
        i = start + 1;
    }
    false
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

/// Build an `HttpRoute` node for a backend path a client *consumes* (verb
/// `CONSUMED`, no server handler). Id is keyed by path alone so the same path
/// referenced from several endpoint classes collapses to one node; `name` is
/// the last path segment so `search <segment>` matches it like the served side.
pub fn consumed_route_node(path: &str) -> Node {
    let id = ArtifactId::new(format!("http_route::consumed::{path}"));
    let mut node = Node::new(id, NodeKind::HttpRoute);
    node.name = Some(route_search_name(path));
    node.path = Some(path.to_string());
    node.indexer = Some(SCHEMA_INDEXER_NAME.to_string());
    let meta = HttpRouteMeta {
        verb: "CONSUMED".to_string(),
        handler_class: String::new(),
        handler_method: String::new(),
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
            frags.insert(
                id.to_ascii_lowercase(),
                text[open_end..body_end].trim().to_string(),
            );
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
                if lower[j..].starts_with("into") && bytes.get(j + 4).is_none_or(|c| !is_ident(*c))
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
    crate::treesitter::ALWAYS_SKIP_DIRS.contains(&name) || SCHEMA_SKIP_DIRS.contains(&name)
}

/// Whether the walk should prune this entry (and, for a directory, its whole
/// subtree). Two reasons:
///
/// 1. Its name is a never-scan directory ([`SKIP_DIRS`]).
/// 2. It is a **nested GroundGraph workspace** — a sub-directory (depth > 0) that
///    holds its own `.groundgraph.yaml`. Vendored/reference repos (e.g. tailorx
///    bundling the Java `platform` under `docs/references/source-repos/`) are
///    self-contained workspaces indexed by *their own* `index`; folding their
///    routes/tables/mappers into the parent graph creates thousands of phantom
///    nodes with no parent code node to link to. The root workspace (depth 0)
///    is exempt so its own config never prunes the entire walk.
fn is_skipped_walk_entry(e: &walkdir::DirEntry) -> bool {
    let name = e.file_name().to_str().unwrap_or("");
    // Hidden dirs below the root are tooling/build/cache output (DerivedData
    // variants, .build, .venv …), never first-party source — pruned like the
    // tree-sitter discovery and ripgrep/`ignore` default.
    if e.depth() > 0 && e.file_type().is_dir() && name.starts_with('.') {
        return true;
    }
    if is_skipped_dir(name) {
        return true;
    }
    // An embedded git repository (depth>0 dir with its own `.git/`) is a
    // different project — vendored upstream / reference clone — whose source
    // git does not track here; prune it like the tree-sitter discovery does so
    // its tables/routes never become phantom nodes in the parent graph.
    if e.depth() > 0 && e.file_type().is_dir() && e.path().join(".git").is_dir() {
        return true;
    }
    e.depth() > 0 && e.file_type().is_dir() && crate::config::has_config_file(e.path())
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
        "select", "from", "where", "join", "on", "as", "set", "values", "value", "into", "insert",
        "update", "delete", "dual", "and", "or", "by", "group", "order", "limit", "having",
        "union", "case", "when", "then", "else", "end",
    ];
    if name.len() < 2 {
        return false;
    }
    // A trailing `_` is the residue of a truncated dynamic name (`x_${var}`).
    if name.ends_with('_') {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
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

/// `.sql`-file entry point: strip SQL comments first so a commented-out
/// `-- CREATE TABLE old (…)` (or a `/* … */` block) is not minted as a phantom
/// table, and `-- column note` lines inside a table body don't become bogus
/// columns. Embedded SQL in *source* files keeps the raw scanner: there a `--`
/// is ambiguous (Go `i--`) and the DDL already lives inside a string literal.
fn parse_sql_tables_from_sql_file(text: &str) -> Vec<ParsedTable> {
    parse_sql_tables(&blank_sql_comments(text))
}

/// Blank SQL `--` line and `/* */` block comment contents with spaces (newlines
/// kept so offsets/line numbers are stable), leaving string literals (`'…'`) and
/// quoted identifiers (`"…"`) — where `''`/`""` are escaped quotes — intact, so a
/// `--` inside a literal is never read as a comment.
fn blank_sql_comments(text: &str) -> String {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = b.to_vec();
    let blank = |out: &mut [u8], from: usize, to: usize| {
        for byte in out.iter_mut().take(to).skip(from) {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let skip_quoted = |b: &[u8], start: usize, q: u8| -> usize {
        let mut i = start + 1;
        while i < n {
            if b[i] == q {
                if i + 1 < n && b[i + 1] == q {
                    i += 2; // doubled quote escape (`''` / `""`)
                    continue;
                }
                return i + 1;
            }
            i += 1;
        }
        n
    };
    let mut i = 0usize;
    while i < n {
        match b[i] {
            b'-' if i + 1 < n && b[i + 1] == b'-' => {
                let start = i;
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                let start = i;
                i += 2;
                while i < n && !(b[i] == b'*' && i + 1 < n && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                blank(&mut out, start, i);
            }
            b'\'' => i = skip_quoted(b, i, b'\''),
            b'"' => i = skip_quoted(b, i, b'"'),
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

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
        let j = skip_ws(bytes, after_name);
        if j >= bytes.len() || bytes[j] != b'(' {
            continue;
        }
        let Some((body, end)) = balanced_parens(bytes, j) else {
            continue;
        };
        let columns = parse_sql_columns(body);
        out.push(ParsedTable {
            name,
            columns,
            source: "sql",
            line: line_of(text, header),
        });
        // Advance past the whole `(...)` using the closing-paren index returned
        // by `balanced_parens`. Computing `j + body.len()` instead landed one
        // byte before `)` — inside the body's last char when it was multi-byte —
        // making the next `lower[search_from..]` slice mid-char and panic.
        search_from = search_from.max(end);
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
        // Braces inside string/char literals (`String p = "{";`) must not
        // shift the depth, or every later field is misread as method-body
        // noise (issues.md #10).
        let (opens, closes) = brace_counts_outside_strings(line);
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
                    table_line = u32::try_from(idx + 1).unwrap_or(u32::MAX);
                }
            } else if class_name.is_none() {
                if let Some(name) = class_name_from_decl(line) {
                    class_name = Some(name);
                    class_line = u32::try_from(idx + 1).unwrap_or(u32::MAX);
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
    let &name = tokens.last()?;
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
    // Strip comments so a commented-out `// @GetMapping("/x")` (or a block) in a
    // live controller isn't read as a route. Strings (mapping paths) are kept.
    let owned = blank_c_like_comments(text);
    let text = owned.as_str();
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
/// Blank the *contents* of `//` line and `/* */` block comments (their bytes
/// become spaces; newlines kept so byte offsets and line numbers are unchanged),
/// leaving code and string / char / raw literals intact. String-aware, so a `//`
/// inside a literal — e.g. a URL `"http://x"` — is never read as a comment.
/// Covers the lexical forms shared by Go and Java: `"…"` (and Java text blocks
/// `"""…"""`), `'…'`, and Go raw `` `…` ``. Keeps the route scanners from
/// resurrecting commented-out registrations as phantom endpoints.
fn blank_c_like_comments(text: &str) -> String {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = b.to_vec();
    let blank = |out: &mut [u8], from: usize, to: usize| {
        for byte in out.iter_mut().take(to).skip(from) {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0usize;
    while i < n {
        match b[i] {
            b'/' if i + 1 < n && b[i + 1] == b'/' => {
                let start = i;
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                let start = i;
                i += 2;
                while i < n && !(b[i] == b'*' && i + 1 < n && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                blank(&mut out, start, i);
            }
            b'"' if i + 2 < n && b[i + 1] == b'"' && b[i + 2] == b'"' => {
                i += 3;
                while i + 2 < n && !(b[i] == b'"' && b[i + 1] == b'"' && b[i + 2] == b'"') {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 3).min(n);
            }
            b'"' => i = skip_c_quote(b, i, b'"'),
            b'\'' => i = skip_c_quote(b, i, b'\''),
            b'`' => {
                i += 1;
                while i < n && b[i] != b'`' {
                    i += 1;
                }
                i = (i + 1).min(n);
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// Advance past a `"`/`'`-delimited literal opened at `start`, honouring `\`
/// escapes and stopping at an unescaped newline (defensive against an
/// unterminated literal swallowing the rest of the file).
fn skip_c_quote(b: &[u8], start: usize, q: u8) -> usize {
    let n = b.len();
    let mut i = start + 1;
    while i < n {
        match b[i] {
            b'\\' => i += 2,
            b'\n' => return i,
            c if c == q => return i + 1,
            _ => i += 1,
        }
    }
    n
}

pub fn parse_go_routes(text: &str) -> Vec<ParsedRoute> {
    if !text.contains("HandleFunc") && !text.contains(".Handle(") {
        return Vec::new();
    }
    // Strip comments so commented-out `mux.HandleFunc(...)` lines aren't routes.
    let owned = blank_c_like_comments(text);
    let text = owned.as_str();
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
        let handler = &args[args.len() - 1];
        let (class, method) = match parse_go_handler(handler) {
            Some((recv, m)) if !m.is_empty() => {
                let class = resolve_go_receiver_type(&receivers, open, &recv);
                // A named free function with no recoverable receiver type stays
                // unlinkable noise — keep dropping it.
                if class.is_empty() {
                    continue;
                }
                (class, m)
            }
            // Inline closure handler (`func(w, r){…}`): a real served path with
            // no named handler. Index it with an empty handler (no edge linked).
            _ if is_go_closure_literal(handler) => (String::new(), String::new()),
            _ => continue,
        };
        routes.push(ParsedRoute {
            verb,
            path: normalize_route("", &path),
            class,
            method,
            line: line_of(text, open),
        });
    }
    routes
}

/// Variable / parameter names bound to an Express (or Hono) app or router, so
/// `<name>.get("/p", …)` can be told apart from a same-shaped client call
/// (`http.get("/p")`). Recognises constructor bindings (`x = express()`,
/// `x = express.Router()`, `x = Router()`, `x = new Hono()`) and TypeScript type
/// annotations (`app: Express`, `r: Router`, …) — the latter matters because
/// route files usually receive the app as a parameter (`registerRoutes(app: Express)`).
fn collect_express_routers(text: &str) -> std::collections::HashSet<String> {
    let mut routers = std::collections::HashSet::new();
    for line in text.lines() {
        if let Some(eq) = line.find('=') {
            let rhs = line[eq + 1..].trim_start();
            let is_ctor = rhs.starts_with("express()")
                || rhs.starts_with("express.Router(")
                || rhs.starts_with("Router(")
                || rhs.starts_with("new Hono(")
                || rhs.starts_with("Hono(");
            if is_ctor {
                if let Some(name) = trailing_ident(&line[..eq]) {
                    routers.insert(name);
                }
            }
        }
        // Type annotations `name: Express | Application | Router | Hono`.
        let lb = line.as_bytes();
        for (c, _) in line.match_indices(':') {
            let after = line[c + 1..].trim_start();
            if after.starts_with("Express")
                || after.starts_with("Application")
                || after.starts_with("Router")
                || after.starts_with("Hono")
            {
                let name_end = {
                    let mut k = c;
                    while k > 0 && (lb[k - 1] as char).is_whitespace() {
                        k -= 1;
                    }
                    k
                };
                if let Some(name) = trailing_ident(&line[..name_end]) {
                    routers.insert(name);
                }
            }
        }
    }
    routers
}

/// The trailing identifier of `s` (e.g. `"const app"` → `"app"`), or `None`.
fn trailing_ident(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut end = b.len();
    while end > 0 && (b[end - 1] as char).is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident_byte(b[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    Some(s[start..end].to_string())
}

/// Recover HTTP routes from an Express / Hono server — the TS/JS analogue of
/// [`parse_gin_routes`]. Matches `<router>.<verb>("/path", …handler)` where the
/// receiver is a known app/router (see `collect_express_routers`) and the verb
/// is an HTTP method. Inline arrow / `function` handlers leave the handler empty
/// (the path is still indexed, like a Gin closure); a bare-identifier handler is
/// kept as the method name so the linker can resolve it. Comments are stripped
/// first so commented-out registrations don't become phantom routes.
pub fn parse_ts_server_routes(text: &str) -> Vec<ParsedRoute> {
    let routers = collect_express_routers(text);
    if routers.is_empty() {
        return Vec::new();
    }
    let owned = blank_c_like_comments(text);
    let text = owned.as_str();
    let b = text.as_bytes();
    const VERBS: &[&str] = &["get", "post", "put", "delete", "patch"];
    let mut routes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for verb in VERBS {
        let needle = format!(".{verb}(");
        for (pos, _) in text.match_indices(needle.as_str()) {
            let Some(recv) = read_ident_backwards(b, pos) else {
                continue;
            };
            if !routers.contains(&recv) {
                continue;
            }
            let open = pos + needle.len() - 1;
            let Some((body, _end)) = balanced_parens(b, open) else {
                continue;
            };
            let args = split_top_level_commas(body);
            if args.len() < 2 {
                continue; // a route needs a path *and* a handler
            }
            let path_lit = concat_string_literals(&args[0]);
            if !path_lit.starts_with('/') {
                continue; // rejects settings reads like `app.get("env")`
            }
            // Closure handler (`(req,res) => …` / `async … =>` / `function`) →
            // empty handler; a trailing bare identifier → named handler.
            let method = if is_js_closure_arg(&args[1]) {
                String::new()
            } else if is_plain_ident(args[args.len() - 1].trim()) {
                args[args.len() - 1].trim().to_string()
            } else {
                String::new()
            };
            let verb_up = verb.to_ascii_uppercase();
            let path = normalize_route("", &path_lit);
            if seen.insert((verb_up.clone(), path.clone(), method.clone())) {
                routes.push(ParsedRoute {
                    verb: verb_up,
                    path,
                    class: String::new(),
                    method,
                    line: line_of(text, pos),
                });
            }
        }
    }
    routes
}

/// A JS/TS handler argument that is an inline closure: an arrow function
/// (`(req, res) => …`), an `async` arrow / function, or a `function` literal.
fn is_js_closure_arg(arg: &str) -> bool {
    let t = arg.trim();
    t.contains("=>") || t.starts_with("function") || t.starts_with("async")
}

/// `true` when `s` is a single bare identifier (a named handler reference), i.e.
/// no dots, parens or whitespace — distinguishing `getUsers` from `(req)=>…`.
fn is_plain_ident(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(is_ident_byte)
}

/// A Python route decorator's verb kind.
enum PyVerb {
    /// An explicit HTTP method decorator (`@app.get`, `@router.post`).
    Method(String),
    /// `@app.websocket("/ws")` — modelled with the synthetic verb `WS`.
    Websocket,
    /// Flask's `@app.route("/p", methods=[...])` — verb read from `methods=`.
    Route,
}

/// Recover HTTP routes from Python web frameworks — the Python analogue of
/// [`parse_http_routes`] (Spring) and [`parse_go_routes`] (net/http). Handles
/// the decorator routing shared by FastAPI / Starlette / Flask:
///
/// ```python
/// @app.get("/api/strategies")              # FastAPI / Starlette
/// async def list_strategies(): ...
///
/// @router.post("/items", status_code=201)
/// def create_item(): ...
///
/// @app.route("/legacy", methods=["POST"])  # Flask
/// def legacy(): ...
/// ```
///
/// Any `@<recv>.<verb>(...)` whose `<verb>` is an HTTP method (or `websocket`,
/// or Flask's `route`) immediately preceding a `def` / `async def` is a route.
/// The path is the first string-literal argument; the handler is the decorated
/// function — matched later by its bare name, since FastAPI handlers are
/// module-level functions. Flask's verb comes from the `methods=[...]` kwarg
/// (default GET). A decorator whose path is not a string literal is skipped.
/// A receiver segment is required, so a bare `@get(...)` or `@property` is not
/// mistaken for a route.
pub fn parse_python_routes(text: &str) -> Vec<ParsedRoute> {
    if !text.contains('@') {
        return Vec::new();
    }
    let b = text.as_bytes();
    let mut routes = Vec::new();
    // Only treat `@` that sit at *code* level as decorators — a `@router.post`
    // printed inside a docstring usage example or a `#` comment is documentation,
    // not a route (atagent regression: phantom `POST /` with no real handler).
    let prefixes = collect_python_router_prefixes(text);
    for i in python_decorator_offsets(b) {
        let (dotted, after) = read_dotted_ident(b, i + 1);
        let Some(verb_kind) = python_decorator_verb(&dotted) else {
            continue;
        };
        let j = skip_ws(b, after);
        if j >= b.len() || b[j] != b'(' {
            continue;
        }
        let Some((body, end)) = balanced_parens(b, j) else {
            continue;
        };
        let Some(path) = python_first_string(body) else {
            continue;
        };
        let verb = match verb_kind {
            PyVerb::Method(v) => v,
            PyVerb::Websocket => "WS".to_string(),
            PyVerb::Route => python_methods_verb(body).unwrap_or_else(|| "GET".to_string()),
        };
        // `@router.get(...)` serves under the router's `prefix=` (FastAPI) /
        // `url_prefix=` (Flask Blueprint), like Java class-level
        // `@RequestMapping` (issues2.md #45).
        let recv = dotted.rsplit_once('.').map(|(r, _)| r).unwrap_or("");
        let base = prefixes.get(recv).map(String::as_str).unwrap_or("");
        if let Some(method) = python_handler_after(b, end) {
            routes.push(ParsedRoute {
                verb,
                path: normalize_route(base, &path),
                class: String::new(),
                method,
                line: line_of(text, i),
            });
        }
    }
    routes
}

/// Map of router variable → route prefix, recovered from
/// `name = APIRouter(prefix="/api/v1")` and
/// `name = Blueprint("x", __name__, url_prefix="/admin")` assignments
/// (issues2.md #45).
fn collect_python_router_prefixes(text: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let b = text.as_bytes();
    for (ctor, kwarg) in [("APIRouter", "prefix"), ("Blueprint", "url_prefix")] {
        let mut from = 0usize;
        while let Some(rel) = text[from..].find(ctor) {
            let at = from + rel;
            from = at + ctor.len();
            // Whole-identifier match only (`MyAPIRouterFactory` must not hit).
            if at > 0 && is_ident_byte(b[at - 1]) {
                continue;
            }
            let after = at + ctor.len();
            if after < b.len() && is_ident_byte(b[after]) {
                continue;
            }
            let j = skip_ws(b, after);
            if j >= b.len() || b[j] != b'(' {
                continue;
            }
            let Some((body, _)) = balanced_parens(b, j) else {
                continue;
            };
            let Some(target) = python_assign_target(b, at) else {
                continue;
            };
            let Some(prefix) = python_kwarg_string(body, kwarg) else {
                continue;
            };
            out.insert(target, prefix);
        }
    }
    out
}

/// The identifier being assigned when `call_at` points at the callee of
/// `name = Callee(...)`; `None` when the call is not a simple assignment.
fn python_assign_target(b: &[u8], call_at: usize) -> Option<String> {
    let mut i = call_at;
    while i > 0 && (b[i - 1] == b' ' || b[i - 1] == b'\t') {
        i -= 1;
    }
    if i == 0 || b[i - 1] != b'=' {
        return None;
    }
    i -= 1;
    // `==` is a comparison, not an assignment.
    if i > 0 && b[i - 1] == b'=' {
        return None;
    }
    while i > 0 && (b[i - 1] == b' ' || b[i - 1] == b'\t') {
        i -= 1;
    }
    let end = i;
    while i > 0 && is_ident_byte(b[i - 1]) {
        i -= 1;
    }
    if i == end {
        return None;
    }
    Some(String::from_utf8_lossy(&b[i..end]).into_owned())
}

/// The string value of keyword argument `name` inside a call body
/// (`prefix="/api/v1"`), or `None` when absent / non-literal.
fn python_kwarg_string(body: &str, name: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = body[from..].find(name) {
        let at = from + rel;
        from = at + name.len();
        let before_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
        let after = at + name.len();
        if !before_ok || (after < bytes.len() && is_ident_byte(bytes[after])) {
            continue;
        }
        let j = skip_ws(bytes, after);
        if j >= bytes.len() || bytes[j] != b'=' {
            continue;
        }
        // `==` comparison guard.
        if bytes.get(j + 1) == Some(&b'=') {
            continue;
        }
        let k = skip_ws(bytes, j + 1);
        let q = *bytes.get(k)?;
        if q != b'"' && q != b'\'' {
            continue;
        }
        let rest = &body[k + 1..];
        let close = rest.find(q as char)?;
        return Some(rest[..close].to_string());
    }
    None
}

/// Byte offsets of every `@` that sits at Python *code* level — i.e. not inside
/// a string literal (including triple-quoted docstrings) or a `#` comment. This
/// keeps route detection from tripping over decorators shown as usage examples
/// in documentation. Quote prefixes (`r"`, `f"`, `b"""`, …) need no special
/// handling: the prefix letters scan as ordinary bytes and the quote that
/// follows opens the string as usual.
fn python_decorator_offsets(b: &[u8]) -> Vec<usize> {
    let mut offs = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' | b'\'' => i = skip_python_string(b, i),
            b'@' => {
                offs.push(i);
                i += 1;
            }
            _ => i += 1,
        }
    }
    offs
}

/// Advance past a Python string literal whose opening quote is at `start`,
/// returning the offset just past its close (or end-of-input / newline for an
/// unterminated single-line string). Handles triple quotes and backslash
/// escapes; raw-string corner cases (a quote after a backslash) are irrelevant
/// to whether a `@` is code-level.
fn skip_python_string(b: &[u8], start: usize) -> usize {
    let q = b[start];
    let triple = start + 2 < b.len() && b[start + 1] == q && b[start + 2] == q;
    if triple {
        let mut i = start + 3;
        while i < b.len() {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == q && i + 2 < b.len() && b[i + 1] == q && b[i + 2] == q {
                return i + 3;
            }
            i += 1;
        }
        return b.len();
    }
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'\n' => return i, // unterminated single-line string
            c if c == q => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Read a dotted identifier (`app.get`, `router.post`) from `start`; returns
/// the text and the byte offset just past it.
fn read_dotted_ident(b: &[u8], start: usize) -> (String, usize) {
    let mut i = start;
    while i < b.len() && (is_ident_byte(b[i]) || b[i] == b'.') {
        i += 1;
    }
    (String::from_utf8_lossy(&b[start..i]).into_owned(), i)
}

/// Classify a decorator's dotted name into a route verb. Requires a receiver
/// segment (`app.get`, not `get`) so non-route decorators are ignored.
fn python_decorator_verb(dotted: &str) -> Option<PyVerb> {
    let (recv, last) = dotted.rsplit_once('.')?;
    if recv.is_empty() {
        return None;
    }
    match last.to_ascii_lowercase().as_str() {
        "get" | "post" | "put" | "delete" | "patch" | "head" | "options" | "trace" => {
            Some(PyVerb::Method(last.to_ascii_uppercase()))
        }
        "websocket" => Some(PyVerb::Websocket),
        "route" => Some(PyVerb::Route),
        _ => None,
    }
}

/// First single- or double-quoted string literal in a decorator argument list.
fn python_first_string(body: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != c {
                j += 1;
            }
            return Some(body[start..j.min(body.len())].to_string());
        }
        i += 1;
    }
    None
}

/// Flask verb from a `methods=["POST", ...]` kwarg — the first listed method,
/// upper-cased. `None` when no `methods=` is present (caller defaults to GET).
fn python_methods_verb(body: &str) -> Option<String> {
    let p = body.find("methods")?;
    let rest = &body[p + "methods".len()..];
    let eq = rest.find('=')?;
    let v = python_first_string(&rest[eq + 1..])?;
    (!v.is_empty()).then(|| v.to_ascii_uppercase())
}

/// From just past a route decorator's `)`, skip blank / comment / stacked
/// decorator lines and return the decorated function's name (`def NAME` /
/// `async def NAME`). Returns `None` if a non-decorator statement appears
/// before any `def`.
fn python_handler_after(b: &[u8], start: usize) -> Option<String> {
    let text = std::str::from_utf8(b).ok()?;
    let rest = &text[start.min(text.len())..];
    for line in rest.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with('@') {
            continue;
        }
        let sig = t.strip_prefix("async ").unwrap_or(t);
        if let Some(after_def) = sig.strip_prefix("def ") {
            let name: String = after_def
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            return (!name.is_empty()).then_some(name);
        }
        // Any other statement before a `def` → the decorator was not on a
        // function (e.g. a decorated assignment); not a route handler.
        return None;
    }
    None
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
    // Char-wise, not byte-wise: bytes zero-extended UTF-8 continuation
    // bytes into mojibake, and `\n` was "decoded" to the letter `n`,
    // silently rewriting route paths (issues2.md #49). Only `\"` and `\\`
    // decode; other escapes stay legible as written so the indexed path
    // still greps against the source.
    let chars: Vec<char> = expr.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '"' {
                    if chars[j] == '\\' && j + 1 < chars.len() {
                        match chars[j + 1] {
                            '"' => out.push('"'),
                            '\\' => out.push('\\'),
                            other => {
                                out.push('\\');
                                out.push(other);
                            }
                        }
                        j += 2;
                        continue;
                    }
                    out.push(chars[j]);
                    j += 1;
                }
                i = j + 1;
            }
            '`' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '`' {
                    out.push(chars[j]);
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

/// Recover HTTP routes registered with the Gin web framework
/// (`github.com/gin-gonic/gin`), the most common Go router — the framework
/// analogue of [`parse_go_routes`] (stdlib `net/http`). Handles:
///
/// ```go
/// func SetupRouter(appController *controller.AppController, …) *gin.Engine {
///     r := gin.New()
///     r.GET("/health", healthController.HealthCheck)
///     v1 := r.Group("/api/v1")          // prefix chain
///     apps := v1.Group("/apps")         // nested group → /api/v1/apps
///     apps.GET("/:id", appController.GetApp)   // → GET /api/v1/apps/:id
/// }
/// ```
///
/// Group prefixes are resolved by following `child := parent.Group("/p")`
/// chains from the `gin.New()` / `gin.Default()` root. The handler is the
/// *last* call argument (Gin appends per-route middleware before it);
/// `recv.Method` resolves its declaring type from the enclosing function's
/// typed parameters (`appController *controller.AppController` → `AppController`)
/// or a `controller.NewAppController(…)` constructor, so the route links to the
/// `Type.Method` suffix of the indexed `GoMethod` node.
pub fn parse_gin_routes(text: &str) -> Vec<ParsedRoute> {
    if !text.contains("gin.") && !text.contains("gin-gonic") {
        return Vec::new();
    }
    // Strip comments first so commented-out `r.GET(...)` lines (and any group /
    // router declarations inside them) don't register as phantom routes.
    let owned = blank_c_like_comments(text);
    let text = owned.as_str();
    let prefixes = collect_gin_group_prefixes(text);
    if prefixes.is_empty() {
        return Vec::new();
    }
    let var_types = collect_go_var_types(text);
    let struct_fields = collect_go_struct_field_types(text);
    let b = text.as_bytes();
    const VERBS: &[(&str, &str)] = &[
        ("GET", "GET"),
        ("POST", "POST"),
        ("PUT", "PUT"),
        ("DELETE", "DELETE"),
        ("PATCH", "PATCH"),
        ("HEAD", "HEAD"),
        ("OPTIONS", "OPTIONS"),
        ("Any", "ANY"),
    ];
    let mut routes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (token, verb) in VERBS {
        let needle = format!(".{token}(");
        for (pos, _) in text.match_indices(&needle) {
            // Receiver is the identifier immediately before the `.`.
            let Some(recv) = read_ident_backwards(b, pos) else {
                continue;
            };
            let Some(base) = prefixes.get(&recv) else {
                continue; // not a known router/group → not a Gin route call
            };
            let open = pos + needle.len() - 1;
            let Some((body, _end)) = balanced_parens(b, open) else {
                continue;
            };
            let args = split_top_level_commas(body);
            if args.len() < 2 {
                continue;
            }
            let path_lit = concat_string_literals(&args[0]);
            // A route path is an absolute path or an empty string (group root).
            if !path_lit.is_empty() && !path_lit.starts_with('/') {
                continue;
            }
            // Resolve the handler's declaring type through the receiver chain
            // (`d.Auth.Me`: var `d`→`Deps`, field `Deps.Auth`→`AuthHandler`); an
            // unresolved receiver leaves the class empty (route still indexed).
            let handler = &args[args.len() - 1];
            let (class, method) = match resolve_go_handler(handler, &var_types, &struct_fields) {
                Some((c, m)) if !m.is_empty() => (c, m),
                // Inline closure handler (`func(c *gin.Context){…}`): a real
                // served path (health/readiness probes) with no `Type.Method`
                // to link. Index it with an empty handler so route-coverage
                // sees the path; the route→method linker then emits no edge.
                _ if is_go_closure_literal(handler) => (String::new(), String::new()),
                _ => continue,
            };
            let path = normalize_route(base, &path_lit);
            if seen.insert((
                verb.to_string(),
                path.clone(),
                class.clone(),
                method.clone(),
            )) {
                routes.push(ParsedRoute {
                    verb: verb.to_string(),
                    path,
                    class,
                    method,
                    line: line_of(text, pos),
                });
            }
        }
    }
    routes
}

/// Map every Gin router / route-group variable to its accumulated path prefix.
/// `r := gin.New()` / `gin.Default()` seeds a root (empty prefix);
/// `child := parent.Group("/p")` extends its parent's prefix. Processed in
/// source order so a nested group sees its (already-declared) parent.
fn collect_gin_group_prefixes(text: &str) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut prefixes: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let t = line.trim();
        let Some((lhs, rhs)) = t.split_once(":=").or_else(|| t.split_once('=')) else {
            continue;
        };
        let var = lhs.trim().trim_end_matches(':').trim();
        if !is_ident(var) {
            continue;
        }
        let rhs = rhs.trim();
        if rhs.starts_with("gin.New(") || rhs.starts_with("gin.Default(") {
            prefixes.insert(var.to_string(), String::new());
            continue;
        }
        if let Some(gpos) = rhs.find(".Group(") {
            let parent = read_ident_backwards(rhs.as_bytes(), gpos).unwrap_or_default();
            let Some(base) = prefixes.get(&parent) else {
                continue;
            };
            let open = gpos + ".Group".len();
            if let Some((body, _)) = balanced_parens(rhs.as_bytes(), open) {
                let lit = concat_string_literals(body);
                let joined = normalize_route(base, &lit);
                prefixes.insert(var.to_string(), joined);
            }
        }
    }
    prefixes
}

/// Best-effort `variable → simple type name` map for a Go file, used to resolve
/// a Gin handler's declaring type. Covers (a) typed function parameters
/// (`name *pkg.Type` → `Type`, including the `a, b T` group form) and (b)
/// constructor locals (`name := pkg.NewType(…)` / `name := &pkg.Type{…}`).
fn collect_go_var_types(text: &str) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut map: HashMap<String, String> = HashMap::new();
    let b = text.as_bytes();
    // (a) Function parameter lists.
    for (idx, _) in text.match_indices("func") {
        let before_ok = idx == 0 || !is_ident_byte(b[idx.saturating_sub(1)]);
        let after = idx + 4;
        if !before_ok || after >= b.len() || !b[after].is_ascii_whitespace() {
            continue;
        }
        let mut p = skip_ws(b, after);
        // Skip an optional method receiver `(recv T)` so the parameter list —
        // not the receiver — is the `(` we parse.
        if p < b.len() && b[p] == b'(' {
            if let Some((_, end)) = balanced_parens(b, p) {
                p = end;
            }
        }
        // Advance to the parameter-list `(` (just past the function name).
        while p < b.len() && b[p] != b'(' && b[p] != b'{' {
            p += 1;
        }
        if p >= b.len() || b[p] != b'(' {
            continue;
        }
        let Some((body, _)) = balanced_parens(b, p) else {
            continue;
        };
        for frag in split_top_level_commas(body) {
            record_go_param_type(&frag, &mut map);
        }
    }
    // (b) Constructor / composite-literal locals.
    for line in text.lines() {
        let t = line.trim();
        let Some((lhs, rhs)) = t.split_once(":=") else {
            continue;
        };
        let var = lhs.trim();
        if !is_ident(var) {
            continue;
        }
        let rhs = rhs.trim();
        if let Some(ty) = go_constructor_type(rhs) {
            map.insert(var.to_string(), ty);
        }
    }
    map
}

/// Record `name [*][pkg.]Type` (and the `a, b T` group form) into `map`.
fn record_go_param_type(frag: &str, map: &mut std::collections::HashMap<String, String>) {
    let frag = frag.trim();
    let Some((names, ty)) = frag.rsplit_once(char::is_whitespace) else {
        return;
    };
    let simple = go_simple_type(ty);
    if simple.is_empty() {
        return;
    }
    for name in names.split(',') {
        let name = name.trim();
        if is_ident(name) {
            map.insert(name.to_string(), simple.clone());
        }
    }
}

/// `*controller.AppController` / `controller.AppController` / `AppController`
/// → `AppController`. Strips leading `*`/`&`, slices, and package qualifier.
fn go_simple_type(ty: &str) -> String {
    let ty = ty.trim().trim_start_matches(['*', '&']);
    let ty = ty
        .strip_prefix("[]")
        .unwrap_or(ty)
        .trim_start_matches(['*', '&']);
    let last = ty.rsplit('.').next().unwrap_or(ty).trim();
    if is_ident(last) {
        last.to_string()
    } else {
        String::new()
    }
}

/// Type produced by a constructor RHS: `pkg.NewType(…)` / `NewType(…)` →
/// `Type`; `&pkg.Type{…}` / `pkg.Type{…}` → `Type`.
fn go_constructor_type(rhs: &str) -> Option<String> {
    let rhs = rhs.trim_start_matches('&');
    if let Some(paren) = rhs.find('(') {
        let callee = rhs[..paren].rsplit('.').next().unwrap_or("");
        if let Some(ty) = callee.strip_prefix("New") {
            if is_ident(ty) {
                return Some(ty.to_string());
            }
        }
    }
    if let Some(brace) = rhs.find('{') {
        let head = rhs[..brace].trim();
        let ty = head.rsplit('.').next().unwrap_or(head);
        if is_ident(ty) {
            return Some(ty.to_string());
        }
    }
    None
}

/// Map `"StructName.FieldName" → simple field type` for every named struct in a
/// Go file. Powers dependency-injection handler resolution: routers idiomatically
/// take a `Deps`/`Handlers` struct and register handlers via a field selector
/// (`d.Auth.Me`), so linking the route needs the field's declaring type
/// (`Deps.Auth *AuthHandler` → `AuthHandler`). Single-file scope — covers the
/// common case where the dependency struct sits beside its router. Handles the
/// `A, B Type` group form and strips struct tags / pointers / package prefixes.
fn collect_go_struct_field_types(text: &str) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut map: HashMap<String, String> = HashMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        // `type <Name> struct {` opening the block on one line.
        let Some(rest) = t.strip_prefix("type ") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        if !is_ident(name) {
            continue;
        }
        let after_name = rest[name.len()..].trim_start();
        if !after_name.starts_with("struct") || !t.ends_with('{') {
            continue;
        }
        // Consume field lines until the matching close brace. Embedded
        // anonymous structs bump depth so their inner fields don't leak into
        // the outer type (they carry no usable named selector anyway).
        let mut depth = 1usize;
        for fl in lines.by_ref() {
            let ft = fl.trim();
            if ft.starts_with('}') {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                continue;
            }
            if ft.is_empty() || ft.starts_with("//") {
                continue;
            }
            // Drop a trailing struct tag (`Name Type \`json:"x"\``).
            let decl = ft.split('`').next().unwrap_or(ft).trim();
            if decl.ends_with('{') {
                depth += 1;
                continue;
            }
            let Some((names, ty)) = decl.rsplit_once(char::is_whitespace) else {
                continue; // embedded field (`io.Reader`) — no name to key on
            };
            let simple = go_simple_type(ty);
            if simple.is_empty() {
                continue;
            }
            for fname in names.split(',') {
                let fname = fname.trim();
                if is_ident(fname) {
                    map.insert(format!("{name}.{fname}"), simple.clone());
                }
            }
        }
    }
    map
}

/// Resolve a Gin / `net/http` handler argument to `(declaring_type, method)`.
/// Walks the receiver selector chain so both the plain receiver
/// (`h.Get` with `h *Handler`) and the injected dependency-struct field
/// (`d.Auth.Me` with `d Deps` and `Deps.Auth *AuthHandler`) resolve. A bare
/// identifier is a free function (classless). An unresolved receiver yields an
/// empty class — the route is still indexed, just left unlinked.
fn resolve_go_handler(
    arg: &str,
    var_types: &std::collections::HashMap<String, String>,
    struct_fields: &std::collections::HashMap<String, String>,
) -> Option<(String, String)> {
    let s = arg.trim_matches(|c: char| c == '(' || c == ')' || c == '&' || c.is_whitespace());
    let segs: Vec<&str> = s.split('.').map(str::trim).collect();
    if segs.iter().any(|seg| !is_ident(seg)) {
        return None;
    }
    match segs.as_slice() {
        [] => None,
        // Free function: `Handler` → classless, matched by bare method name.
        [method] => Some((String::new(), method.to_string())),
        [base, rest @ .., method] => {
            // Seed the type from the base variable, then walk each field hop.
            let mut ty = var_types.get(*base).cloned().unwrap_or_default();
            for field in rest {
                if ty.is_empty() {
                    break;
                }
                ty = struct_fields
                    .get(&format!("{ty}.{field}"))
                    .cloned()
                    .unwrap_or_default();
            }
            Some((ty, method.to_string()))
        }
    }
}

/// True when a Go route-handler argument is an inline function literal
/// (`func(c *gin.Context) { … }`) rather than a named handler. Such routes have
/// no `Type.Method` to link but are still real served paths — health/readiness
/// probes are the canonical case — so the caller indexes them with an empty
/// handler instead of dropping them. `func` must be the keyword (immediately
/// followed by the parameter-list `(`), never the prefix of a name like
/// `funcHandler`.
fn is_go_closure_literal(arg: &str) -> bool {
    arg.trim()
        .strip_prefix("func")
        .is_some_and(|rest| rest.trim_start().starts_with('('))
}

/// Read the identifier ending just before byte `end` (e.g. the receiver before
/// a `.GET(`). Returns `None` when the preceding byte is not an identifier.
fn read_ident_backwards(b: &[u8], end: usize) -> Option<String> {
    if end == 0 || end > b.len() {
        return None;
    }
    let mut s = end;
    while s > 0 && is_ident_byte(b[s - 1]) {
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

/// One backend route a (Dart) client *consumes*, recovered from a route-table
/// constant such as `class ApiEndpoints { static const String x = '/path'; }`.
/// `class_name`/`name` are kept so a caller body referencing `ApiEndpoints.x`
/// can be linked to the route node for `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DartRouteConst {
    pub class_name: String,
    pub name: String,
    pub path: String,
}

/// Recover client-consumed backend routes from Dart endpoint-table classes.
/// A constant counts only when (a) its enclosing class looks like a route table
/// (name contains `api`/`endpoint`/`route`/`uri`) and (b) its string value is an
/// absolute URL path (`/…`). This excludes base URLs (`https://…`), storage keys
/// and styling constants, which share the `static const` shape.
pub fn parse_dart_route_constants(text: &str) -> Vec<DartRouteConst> {
    if !text.contains("static const") {
        return Vec::new();
    }
    // A *client navigation* table (`AppRouter`, `AppRoutes`) holds the same
    // `static const String x = '/path'` shape as a backend endpoint table and
    // matches the name heuristic ("route"), but its paths are Flutter screen
    // routes consumed by `Navigator`, not backend APIs. The reliable
    // discriminator is the file's navigation machinery — `onGenerateRoute`,
    // `Navigator`, a `*PageRoute`, `RouteObserver`, `NavigatorState` — which a
    // pure API-endpoint constants file never carries. Skip such files so a
    // client's own UI flow is not mis-reported as backend surface it consumes.
    if is_navigation_router_source(text) {
        return Vec::new();
    }
    let classes = collect_dart_classes(text);
    let mut out = Vec::new();
    for (idx, _) in text.match_indices("static const") {
        let k = idx + "static const".len();
        // The declaration runs to the first `=`; bail if a statement boundary
        // intervenes (defensive — class-field consts never contain one here).
        let Some(eq_rel) = text[k..].find('=') else {
            continue;
        };
        let eq = k + eq_rel;
        if text[k..eq].contains([';', '{', '}']) {
            continue;
        }
        // The identifier just before `=` is the constant name; any preceding
        // tokens are its (optional) type.
        let name = text[k..eq]
            .split_whitespace()
            .last()
            .unwrap_or("")
            .to_string();
        if !is_ident(&name) {
            continue;
        }
        let Some(raw_path) = first_dart_string_literal(&text[eq + 1..]) else {
            continue;
        };
        if !raw_path.starts_with('/') || raw_path.len() < 2 {
            continue;
        }
        let class_name = enclosing_dart_class(&classes, idx);
        if !is_route_table_class(&class_name) {
            continue;
        }
        // Same placeholder normalization as the inline-call pipeline, so
        // `/users/$id` and `/users/${id}` collapse onto one consumed node
        // (issues2.md #31/#44).
        out.push(DartRouteConst {
            class_name,
            name,
            path: normalize_consumed_route_path(&raw_path),
        });
    }
    out
}

/// One inline HTTP call a client makes — `verb` (upper-cased method name), the
/// normalized route `path`, and the source `line` (1-based) used to attribute the
/// call to its enclosing callable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineConsumedCall {
    pub verb: String,
    pub path: String,
    pub line: u32,
}

/// HTTP verbs called as a method (Dio `dio.get(path)`, axios `http.post(path)`,
/// the `http` package `client.post(Uri.parse(path))`).
const INLINE_HTTP_VERBS: &[&str] = &["get", "post", "put", "delete", "patch", "head"];

/// Recover the backend routes a Dart client consumes by calling an HTTP client
/// *inline* — `_dio.get<T>('/v1/me')`, `_dio.post('/v1/teams/$id/sync', …)` —
/// the dominant real-world shape, which carries no `ApiEndpoints` constant table
/// for [`parse_dart_route_constants`] to mine.
pub fn parse_dart_consumed_calls(text: &str) -> Vec<InlineConsumedCall> {
    scan_inline_consumed_calls(text, false)
}

/// TypeScript/JS sibling of [`parse_dart_consumed_calls`] for axios/fetch-style
/// clients (`http.post<T>('/admin/login', …)`, `` http.get(`/admin/users/${id}`) ``).
/// Same shape, plus backtick template-literal paths with `${…}` interpolation.
pub fn parse_ts_consumed_calls(text: &str) -> Vec<InlineConsumedCall> {
    scan_inline_consumed_calls(text, true)
}

/// Scan `<recv>.<verb>(<path-literal>, …)` HTTP calls. A call qualifies only when
/// the method is an HTTP verb and its first argument yields a string literal that
/// is an absolute path (`/…`), which rejects collection/cache `.get('key')`
/// calls. Path interpolation (`$id`, `${x}`) normalizes to a `:param` placeholder
/// so the consumed path matches the server's `:id` under
/// [`crate::route_coverage::route_key`]. `allow_backtick` admits TS/JS template
/// literals; Dart has none so it is disabled there.
fn scan_inline_consumed_calls(text: &str, allow_backtick: bool) -> Vec<InlineConsumedCall> {
    // Strip comments so a commented-out `// http.get('/x')` isn't read as a
    // consumed route. Backtick template paths are kept (preserved as raw strings).
    let owned = blank_c_like_comments(text);
    let text = owned.as_str();
    let b = text.as_bytes();
    // A server route definition (`app.get("/p", handler)`) has the same
    // `recv.verb("/p")` shape as a client call; exclude Express/Hono routers so
    // those are counted as *served* routes, not *consumed* ones. (TS/JS only.)
    let routers = if allow_backtick {
        collect_express_routers(text)
    } else {
        std::collections::HashSet::new()
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for verb in INLINE_HTTP_VERBS {
        let needle = format!(".{verb}");
        for (pos, _) in text.match_indices(needle.as_str()) {
            // A receiver must precede the dot (`recv.get`), and the verb must be
            // a whole method name, not the prefix of a longer one (`.getter`).
            if pos == 0 {
                continue;
            }
            let prev = b[pos - 1];
            // `$` is a valid JS/TS/Dart identifier char (ECMAScript
            // IdentifierStart), so receivers like jQuery `$` or an RxJS-style
            // `http$` end in `$`; without it `$.get('/p')` / `http$.post('/p')`
            // would be dropped as non-calls.
            if !(is_ident_byte(prev)
                || prev == b'$'
                || prev == b')'
                || prev == b']'
                || prev == b'>')
            {
                continue;
            }
            // `app.get("/p", handler)` is a *server* route, not a client call.
            if !routers.is_empty() {
                if let Some(recv) = read_ident_backwards(b, pos) {
                    if routers.contains(&recv) {
                        continue;
                    }
                }
            }
            let after = pos + needle.len();
            if after < b.len() && is_ident_byte(b[after]) {
                continue;
            }
            // Skip optional `<T>` type arguments between the method and the `(`.
            let mut p = skip_ws(b, after);
            if p < b.len() && b[p] == b'<' {
                match skip_angle_generics(b, p) {
                    Some(end) => p = skip_ws(b, end),
                    None => continue,
                }
            }
            if p >= b.len() || b[p] != b'(' {
                continue;
            }
            let Some((body, _)) = balanced_parens(b, p) else {
                continue;
            };
            let args = split_top_level_commas(body);
            let Some(first) = args.first() else {
                continue;
            };
            let Some(raw) = first_inline_path_literal(first, allow_backtick) else {
                continue;
            };
            if !raw.starts_with('/') || raw.len() < 2 {
                continue;
            }
            let path = normalize_consumed_route_path(&raw);
            let line = line_of(text, pos);
            if seen.insert((verb.to_string(), path.clone(), line)) {
                out.push(InlineConsumedCall {
                    verb: verb.to_ascii_uppercase(),
                    path,
                    line,
                });
            }
        }
    }
    out
}

/// The leading string-literal content of an argument: a `'…'` / `"…"` quote and,
/// when `allow_backtick`, a `` `…` `` template literal (TS/JS). Interpolation is
/// kept verbatim for [`normalize_consumed_route_path`] to placeholder.
fn first_inline_path_literal(arg: &str, allow_backtick: bool) -> Option<String> {
    let s = arg.trim();
    let bytes = s.as_bytes();
    let q = *bytes.first()?;
    let is_quote = q == b'\'' || q == b'"' || (allow_backtick && q == b'`');
    if !is_quote {
        return None;
    }
    let rest = &s[1..];
    let close = rest.find(q as char)?;
    Some(rest[..close].to_string())
}

/// Index just past the `>` matching the `<` at `open`, tracking nesting
/// (`Map<String, List<int>>`). Bails (`None`) on a token that cannot appear in a
/// type-argument list, so a stray `<` comparison is not mistaken for generics.
fn skip_angle_generics(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b';' | b'{' | b'}' | b'(' | b')' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Normalize a consumed route path: drop any query/fragment and map each
/// interpolated segment (Dart `$id`/`${x}`, TS `${x}`) to a `:param` placeholder
/// so the path param drops out of the cross-graph match key the same way a server
/// `:id` does.
fn normalize_consumed_route_path(raw: &str) -> String {
    let path = raw.split(['?', '#']).next().unwrap_or(raw);
    path.split('/')
        .map(|seg| if seg.contains('$') { ":param" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

/// `(byte offset, class name)` for every `class <Name>` declaration, in source
/// order — used to attribute a `static const` to its enclosing class.
fn collect_dart_classes(text: &str) -> Vec<(usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    for (idx, _) in text.match_indices("class") {
        let before_ok = idx == 0 || !is_ident_byte(b[idx - 1]);
        let after = idx + 5;
        if !before_ok || after >= b.len() || !b[after].is_ascii_whitespace() {
            continue;
        }
        let ns = skip_ws(b, after);
        let (name, _) = read_ident(b, ns);
        if !name.is_empty() {
            out.push((idx, name));
        }
    }
    out
}

/// Name of the class whose declaration most closely precedes `offset`.
fn enclosing_dart_class(classes: &[(usize, String)], offset: usize) -> String {
    let mut best = "";
    for (at, name) in classes {
        if *at < offset {
            best = name;
        } else {
            break;
        }
    }
    best.to_string()
}

/// True when a class name looks like a table of API routes.
fn is_route_table_class(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["endpoint", "api", "route", "uri"]
        .iter()
        .any(|kw| lower.contains(kw))
}

/// True when a Dart source defines *client navigation* rather than a backend
/// API endpoint table. Presence of any Flutter routing machinery is the tell:
/// an API endpoint constants file is plain `static const String` declarations
/// with none of these. Used to keep `AppRouter`-style screen routes out of the
/// consumed-backend-route set.
fn is_navigation_router_source(text: &str) -> bool {
    [
        "onGenerateRoute",
        "Navigator",
        "PageRoute", // MaterialPageRoute / CupertinoPageRoute / PageRouteBuilder
        "RouteObserver",
        "NavigatorState",
        "GoRoute",
        "GoRouter",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// Content of the first single- or double-quoted string literal in `s` (Dart
/// path constants use either). Stops at the matching quote; no escape handling
/// is needed for URL paths.
fn first_dart_string_literal(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'"' || b[i] == b'\'' {
            let quote = b[i];
            let from = i + 1;
            let mut j = from;
            while j < b.len() && b[j] != quote {
                j += 1;
            }
            if j <= b.len() {
                return Some(String::from_utf8_lossy(&b[from..j.min(b.len())]).to_string());
            }
        }
        // Stop scanning at a statement end so we never wander into the next decl.
        if b[i] == b';' {
            break;
        }
        i += 1;
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
        .rfind(|s| !s.is_empty() && !s.starts_with('{'))
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
///
/// Parens inside string literals — `@GetMapping("/smile(")`, SQL
/// `DEFAULT '('` — are content, not nesting (issues.md #11).
fn balanced_parens(b: &[u8], open: usize) -> Option<(&str, usize)> {
    let mut depth = 0i32;
    let mut i = open;
    let mut in_string = 0u8;
    let mut escaped = false;
    while i < b.len() {
        let c = b[i];
        if in_string != 0 {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == in_string {
                in_string = 0;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' | b'\'' => in_string = c,
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

/// Count `{` / `}` on a Java source line, ignoring any inside string or
/// char literals and anything behind a `//` comment.
fn brace_counts_outside_strings(line: &str) -> (i32, i32) {
    let mut opens = 0i32;
    let mut closes = 0i32;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_string = Some(c),
            '/' if chars.peek() == Some(&'/') => break,
            '{' => opens += 1,
            '}' => closes += 1,
            _ => {}
        }
    }
    (opens, closes)
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
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

fn line_of(text: &str, byte_idx: usize) -> u32 {
    let newlines = text[..byte_idx.min(text.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    u32::try_from(newlines)
        .unwrap_or(u32::MAX)
        .saturating_add(1)
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
    fn parse_http_routes_ignores_commented_out_mappings() {
        // Commented-out mappings — a `//` line and a `/* */` block — in a live
        // controller must not mint phantom routes; only the real mapping counts.
        let java = r#"
@RestController
@RequestMapping("/style-info")
public class StyleInfoController {

    // @GetMapping("/old")
    // public RS<Void> old() { return null; }

    /*
    @PostMapping("/legacy")
    public RS<Void> legacy() { return null; }
    */

    @GetMapping("/live")
    public RS<Void> live() { return null; }
}
"#;
        let routes = parse_http_routes(java);
        assert_eq!(
            routes.len(),
            1,
            "only the live mapping counts; commented-out ones are ignored, got {routes:?}"
        );
        assert_eq!(routes[0].method, "live");
        assert_eq!(routes[0].path, "/style-info/live");
        assert_eq!(routes[0].verb, "GET");
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
        assert_eq!(
            routes[0].path.as_deref(),
            Some("/style-info/getMeasuresInfo")
        );

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
    fn parse_gin_routes_resolves_group_prefixes_and_handler_types() {
        // Gin: nested route groups build a path prefix, the handler is the last
        // call argument, and the handler's declaring type is recovered from the
        // typed function parameters so the route can link to `Type.Method`.
        let go = r#"
package router

import "github.com/gin-gonic/gin"

func SetupRouter(appController *controller.AppController, healthController *controller.HealthController) *gin.Engine {
	r := gin.New()
	r.Use(middleware.Recovery())
	r.GET("/health", healthController.HealthCheck)
	v1 := r.Group("/api/v1")
	{
		apps := v1.Group("/apps")
		{
			apps.POST("", appController.CreateApp)
			apps.GET("/:id", appController.GetApp)
			apps.DELETE("/:id", appController.DeleteApp)
		}
	}
	return r
}
"#;
        let routes = parse_gin_routes(go);
        assert_eq!(routes.len(), 4, "four routes parsed, got {routes:?}");

        let health = routes
            .iter()
            .find(|r| r.method == "HealthCheck")
            .expect("health route");
        assert_eq!(health.verb, "GET");
        assert_eq!(health.path, "/health");
        assert_eq!(health.class, "HealthController");

        let create = routes
            .iter()
            .find(|r| r.method == "CreateApp")
            .expect("create route");
        assert_eq!(create.verb, "POST");
        assert_eq!(create.path, "/api/v1/apps", "group prefix + empty path");
        assert_eq!(create.class, "AppController");

        let get = routes.iter().find(|r| r.method == "GetApp").expect("get");
        assert_eq!(get.verb, "GET");
        assert_eq!(get.path, "/api/v1/apps/:id", "nested group + param path");
        assert_eq!(get.class, "AppController");
    }

    #[test]
    fn parse_gin_routes_ignores_non_gin_go() {
        let go = r#"
package service
type S struct{}
func (s *S) GET(id int) any { return s.repo.GET(id) }
"#;
        assert!(parse_gin_routes(go).is_empty());
    }

    #[test]
    fn parse_gin_routes_ignores_commented_out_routes() {
        // Commented-out registrations — a `//` line and a `/* */` block — are
        // dead code, not routes: scanning them would mint phantom endpoints.
        // The string-awareness guard: a URL literal containing `//` must NOT be
        // read as a comment, so the live route after it survives.
        let go = r#"
package main
import "github.com/gin-gonic/gin"
func main() {
    r := gin.Default()
    _ = "http://example.com/x" // base url, then a real trailing comment
    // r.GET("/debug", h.Debug)
    /* r.POST("/legacy", h.Legacy) */
    r.GET("/health", h.Health)
}
"#;
        let routes = parse_gin_routes(go);
        assert_eq!(
            routes.len(),
            1,
            "only the live route counts; commented-out ones are ignored, got {routes:?}"
        );
        assert_eq!(routes[0].verb, "GET");
        assert_eq!(routes[0].path, "/health");
        assert_eq!(routes[0].method, "Health");
    }

    #[test]
    fn parse_gin_routes_resolves_handler_via_dependency_struct_field() {
        // Idiomatic Go DI: a `Deps` struct is injected into the router
        // constructor and every handler is reached through a field selector
        // (`d.Auth.Me`). Recovering the declaring type needs BOTH the param
        // type (`d Deps`) and the struct's field types (`Deps.Auth
        // *AuthHandler`); a 2-hop walk the old `recv_last` lookup missed,
        // leaving `class` empty so all routes failed to link. Real repo: Shift
        // backend `internal/api/router.go` (25 routes, every one via `d.X.M`,
        // 0 linked). Also exercises the empty-prefix group `v1.Group("")`
        // inheriting `/v1`.
        let go = r#"
package api

import "github.com/gin-gonic/gin"

type Deps struct {
	Cfg  *config.Config
	Auth *AuthHandler
	Team *TeamHandler
}

func NewRouter(d Deps) *gin.Engine {
	r := gin.New()
	v1 := r.Group("/v1")
	v1.POST("/auth/apple", d.Auth.SignInWithApple)
	authed := v1.Group("")
	authed.GET("/me", d.Auth.Me)
	authed.GET("/teams/:id", d.Team.Get)
	return r
}
"#;
        let routes = parse_gin_routes(go);

        let apple = routes
            .iter()
            .find(|r| r.method == "SignInWithApple")
            .expect("apple route");
        assert_eq!(apple.path, "/v1/auth/apple");
        assert_eq!(apple.class, "AuthHandler", "d.Auth → Deps.Auth field type");

        let me = routes.iter().find(|r| r.method == "Me").expect("me route");
        assert_eq!(me.path, "/v1/me", "empty-prefix group inherits /v1");
        assert_eq!(me.class, "AuthHandler");

        let get = routes
            .iter()
            .find(|r| r.method == "Get")
            .expect("get route");
        assert_eq!(get.path, "/v1/teams/:id");
        assert_eq!(get.class, "TeamHandler", "d.Team → Deps.Team field type");
    }

    #[test]
    fn parse_gin_routes_indexes_inline_closure_handlers() {
        // Health / readiness probes are routinely registered with an inline
        // function literal instead of a named handler
        // (`r.GET("/healthz", func(c *gin.Context){…})`). The path is still a
        // real served route that `route-coverage` must account for, so it is
        // indexed with an *empty* handler (there is no `Type.Method` to link).
        // Real repo: Shift backend `internal/api/router.go` (/healthz, /readyz)
        // — previously dropped, making the served-route set under-count by 2.
        let go = r#"
package api

import (
	"net/http"

	"github.com/gin-gonic/gin"
)

func NewRouter() *gin.Engine {
	r := gin.New()
	r.GET("/healthz", func(c *gin.Context) { c.JSON(http.StatusOK, gin.H{"status": "ok"}) })
	r.GET("/readyz", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{"status": "ok"})
	})
	return r
}
"#;
        let routes = parse_gin_routes(go);

        let healthz = routes
            .iter()
            .find(|r| r.path == "/healthz")
            .expect("/healthz indexed despite closure handler");
        assert_eq!(healthz.verb, "GET");
        assert!(
            healthz.class.is_empty() && healthz.method.is_empty(),
            "closure handler has no Type.Method to link, got {healthz:?}"
        );

        assert!(
            routes.iter().any(|r| r.path == "/readyz"),
            "multi-line closure route indexed too, got {routes:?}"
        );

        // Named-handler routes must still resolve normally (no regression).
        assert!(
            !routes
                .iter()
                .any(|r| r.path == "/healthz" && !r.method.is_empty()),
            "closure route must not invent a handler method"
        );
    }

    #[test]
    fn parse_go_routes_indexes_inline_closure_handlers() {
        // net/http analogue: a closure registered on a ServeMux is a served
        // path even though there is no named handler method to link.
        let go = r#"
package main

import "net/http"

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	})
}
"#;
        let routes = parse_go_routes(go);
        let healthz = routes
            .iter()
            .find(|r| r.path == "/healthz")
            .expect("/healthz indexed despite closure handler");
        assert_eq!(healthz.verb, "GET");
        assert!(
            healthz.class.is_empty() && healthz.method.is_empty(),
            "closure handler has no Type.Method to link, got {healthz:?}"
        );
    }

    #[test]
    fn links_gin_http_route_to_handler_method() {
        // A Gin router file + a pre-seeded handler GoMethod node must yield an
        // HttpRoute node and a route--references-->method edge, matched by the
        // `Type.Method` suffix recovered from the typed parameter.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("internal/router")).unwrap();
        std::fs::write(
            root.join("internal/router/router.go"),
            r#"
package router
import "github.com/gin-gonic/gin"
func SetupRouter(appController *controller.AppController) *gin.Engine {
	r := gin.New()
	v1 := r.Group("/api/v1")
	apps := v1.Group("/apps")
	apps.GET("/:id", appController.GetApp)
	return r
}
"#,
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let method_id =
            ArtifactId::new("go::internal/controller/app_controller.go::AppController.GetApp");
        store
            .upsert_node(&Node::new(method_id.clone(), NodeKind::GoMethod))
            .unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.http_routes, 1, "one Gin route indexed");
        assert_eq!(stats.route_method_edges, 1, "route->method edge linked");

        let routes = store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path.as_deref(), Some("/api/v1/apps/:id"));

        let from_route = store.list_edges_from(&routes[0].id).unwrap();
        assert!(
            from_route
                .iter()
                .any(|e| e.kind == EdgeKind::References && e.to_id == method_id),
            "expected Gin route->method References edge, got {from_route:?}"
        );
    }

    #[test]
    fn schema_index_skips_python_venv_and_worktree_copies() {
        // Installed dependencies (`.venv/.../site-packages/`) and tool worktree
        // copies (`.claude/worktrees/`) must never be mined: a vendored FastAPI's
        // docstring examples (`@app.get("/items") def read_items()`) would
        // otherwise pollute the graph with phantom routes that link nowhere.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (rel, route) in [
            ("src/api.py", "/real"),
            (
                ".venv/lib/python3.11/site-packages/fastapi/applications.py",
                "/items",
            ),
            (".claude/worktrees/exp/src/dup.py", "/dup"),
            ("venv/site-packages/flask/app.py", "/vendored"),
            ("__pycache__/cached.py", "/cached"),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(
                &p,
                format!("@app.get(\"{route}\")\ndef handler():\n    return 1\n"),
            )
            .unwrap();
        }

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let stats = index_schema_into(&mut store, root).unwrap();

        assert_eq!(
            stats.http_routes, 1,
            "only the first-party src/api.py route"
        );
        let routes = store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap();
        let paths: Vec<_> = routes.iter().filter_map(|n| n.path.clone()).collect();
        assert_eq!(paths, vec!["/real".to_string()], "venv/worktree pruned");
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
    fn parse_python_routes_fastapi_and_flask() {
        // FastAPI / Starlette method decorators, a websocket, and a Flask
        // `route(..., methods=[...])`. Lifecycle decorators (`on_event`),
        // non-route decorators (`@property`) and non-literal paths are ignored.
        let py = r#"
from fastapi import FastAPI, APIRouter

app = FastAPI()
router = APIRouter()

@app.get("/api/strategies")
async def list_strategies():
    return []

@router.post("/items", status_code=201)
def create_item(payload):
    return payload

@app.websocket("/ws/feed")
async def feed(ws):
    await ws.accept()

@app.route("/legacy", methods=["POST", "GET"])
def legacy():
    return "ok"

@app.on_event("startup")
async def startup():
    pass

@property
def name(self):
    return self._name

@app.get(DYNAMIC_PATH)
def dynamic():
    return 1
"#;
        let routes = parse_python_routes(py);
        assert_eq!(routes.len(), 4, "four real routes, got {routes:?}");

        let ls = routes
            .iter()
            .find(|r| r.method == "list_strategies")
            .expect("list_strategies route");
        assert_eq!(ls.verb, "GET");
        assert_eq!(ls.path, "/api/strategies");
        assert!(ls.class.is_empty(), "module-level handler has no class");

        let ci = routes
            .iter()
            .find(|r| r.method == "create_item")
            .expect("create_item route");
        assert_eq!(ci.verb, "POST");
        assert_eq!(ci.path, "/items");

        let feed = routes
            .iter()
            .find(|r| r.method == "feed")
            .expect("ws route");
        assert_eq!(feed.verb, "WS");
        assert_eq!(feed.path, "/ws/feed");

        let legacy = routes
            .iter()
            .find(|r| r.method == "legacy")
            .expect("flask route");
        assert_eq!(legacy.verb, "POST", "first verb in methods=[...]");
        assert_eq!(legacy.path, "/legacy");
    }

    /// issues2.md #49: the old byte-wise literal concat zero-extended UTF-8
    /// continuation bytes into mojibake and "decoded" `\n` to `n` / `\x2f`
    /// to `x`, silently corrupting route paths.
    #[test]
    fn concat_string_literals_is_utf8_safe_and_keeps_escapes_legible() {
        // CJK path survives intact.
        assert_eq!(concat_string_literals(r#""/api/用户""#), "/api/用户");
        // Adjacent literals still concatenate.
        assert_eq!(
            concat_string_literals(r#""/api/" + "v1/users""#),
            "/api/v1/users"
        );
        // Escaped quote and backslash decode…
        assert_eq!(concat_string_literals(r#""a\"b""#), "a\"b");
        assert_eq!(concat_string_literals(r#""a\\b""#), "a\\b");
        // …while unknown escapes stay legible as written instead of being
        // mangled into a different path (`\n` is NOT the letter n).
        assert_eq!(concat_string_literals(r#""a\nb""#), "a\\nb");
        assert_eq!(concat_string_literals(r#""v\x2f1""#), "v\\x2f1");
        // Backtick raw literal keeps everything verbatim.
        assert_eq!(concat_string_literals("`/raw/路径`"), "/raw/路径");
    }

    /// issues2.md #45: `APIRouter(prefix=...)` / `Blueprint(url_prefix=...)`
    /// must propagate onto the decorated paths, the same way Java class-level
    /// `@RequestMapping` and Go `Gin.Group()` prefixes already do.
    #[test]
    fn parse_python_routes_propagates_router_prefixes() {
        let py = r#"
from fastapi import APIRouter
from flask import Blueprint

router = APIRouter(prefix="/api/v1", tags=["users"])
bp = Blueprint("admin", __name__, url_prefix="/admin")
plain = APIRouter()

@router.get("/users")
def list_users():
    return []

@bp.route("/dashboard", methods=["GET"])
def dashboard():
    return "ok"

@plain.get("/health")
def health():
    return "up"
"#;
        let routes = parse_python_routes(py);
        let paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths.contains(&"/api/v1/users"),
            "APIRouter prefix must apply, got {paths:?}"
        );
        assert!(
            paths.contains(&"/admin/dashboard"),
            "Blueprint url_prefix must apply, got {paths:?}"
        );
        assert!(
            paths.contains(&"/health"),
            "prefix-less router stays bare, got {paths:?}"
        );
    }

    /// issues2.md #46 claimed `skip_python_string` misses a closing `"""`
    /// at end-of-file; the bound is actually correct. Pin it.
    #[test]
    fn python_decorators_after_eof_adjacent_docstring_are_found() {
        // The module docstring closes at the very last byte before the
        // decorator block — and in the second sample the file *ends* with
        // the closing quotes.
        let py = "\"\"\"module doc\"\"\"\n@app.get(\"/x\")\ndef x():\n    return 1\n";
        let routes = parse_python_routes(py);
        assert_eq!(routes.len(), 1, "decorator after docstring: {routes:?}");

        let trailing = "@app.get(\"/y\")\ndef y():\n    \"\"\"doc ends file\"\"\"";
        let routes2 = parse_python_routes(trailing);
        assert_eq!(routes2.len(), 1, "docstring at EOF: {routes2:?}");
    }

    #[test]
    fn parse_python_routes_ignores_non_web_python() {
        let py = r#"
import dataclasses

@dataclasses.dataclass
class Strategy:
    name: str

@functools.cache
def compute():
    return 1
"#;
        assert!(parse_python_routes(py).is_empty());
    }

    #[test]
    fn parse_python_routes_ignores_decorators_in_docstrings_and_comments() {
        // Regression (atagent backend/app/utils/http_dependencies.py): a
        // `@router.post("/")` shown as a *usage example* inside a docstring —
        // and one inside a `#` comment — were mistaken for real routes, minting
        // a phantom `POST /` HttpRoute with no resolvable handler. Only genuine
        // code-level decorators are routes.
        let py = r#"
from fastapi import APIRouter

router = APIRouter()


def get_request_id_header():
    """FastAPI dependency: read or mint a Request ID.

    Usage:
        @router.post("/")
        async def my_endpoint(request_id: str = Depends(get_request_id_header)):
            pass
    """
    return "x"


# Legacy example kept for docs: @app.get("/old") def old(): ...


@router.get("/real")
async def real_endpoint():
    return {}
"#;
        let routes = parse_python_routes(py);
        assert_eq!(
            routes.len(),
            1,
            "only the code-level route counts; docstring/comment examples are ignored, got {routes:?}"
        );
        assert_eq!(routes[0].method, "real_endpoint");
        assert_eq!(routes[0].path, "/real");
        assert_eq!(routes[0].verb, "GET");
    }

    #[test]
    fn links_python_http_route_to_handler_function() {
        // A FastAPI route file + a pre-seeded handler PythonFunction node must
        // yield an HttpRoute node and a route--references-->function edge,
        // matched by the bare function-name suffix (classless handler).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/api")).unwrap();
        std::fs::write(
            root.join("src/api/main.py"),
            r#"
from fastapi import FastAPI

app = FastAPI()

@app.get("/api/strategies")
async def list_strategies():
    return []
"#,
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        // Pre-seed the handler node (normally from the Python code indexer).
        let fn_id = ArtifactId::new("python::src/api/main.py::list_strategies");
        store
            .upsert_node(&Node::new(fn_id.clone(), NodeKind::PythonFunction))
            .unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.http_routes, 1, "one Python HTTP route indexed");
        assert_eq!(stats.route_method_edges, 1, "route->function edge linked");

        let routes = store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path.as_deref(), Some("/api/strategies"));

        let from_route = store.list_edges_from(&routes[0].id).unwrap();
        assert!(
            from_route
                .iter()
                .any(|e| e.kind == EdgeKind::References && e.to_id == fn_id),
            "expected Python route->function References edge, got {from_route:?}"
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
        for m in [
            "DesignHandler.GetDesignInfo",
            "DesignHandler.SelectCraftTree",
        ] {
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
        assert_eq!(
            store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap().len(),
            2
        );

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
    fn parse_dart_route_constants_extracts_api_paths() {
        // A route-table class (`ApiEndpoints`) exposes the backend paths the
        // client consumes; sibling non-route constants must be ignored.
        let dart = r#"
class AppConstants {
  static const String baseUrl = 'https://platform.kutetailor.com/api';
  static const String accessToken = 'access_token';
  static const int pageSize = 20;
}

class ApiEndpoints {
  ApiEndpoints._();
  static const String login = '/token/oauth/token';
  static const String styleDetail =
      '/style/app/style-info/getDesignDetail';
  static const String markRead = '/member/member/markRead';
}
"#;
        let routes = parse_dart_route_constants(dart);
        let paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths.contains(&"/token/oauth/token"),
            "login path, got {paths:?}"
        );
        assert!(paths.contains(&"/style/app/style-info/getDesignDetail"));
        assert!(paths.contains(&"/member/member/markRead"));
        // base URL (https), storage key (no slash), numeric, and the whole
        // non-route AppConstants class are excluded.
        assert!(!paths.iter().any(|p| p.starts_with("https")));
        assert!(!paths.contains(&"access_token"));
        assert_eq!(
            routes.len(),
            3,
            "only the 3 ApiEndpoints paths, got {routes:?}"
        );

        let sd = routes
            .iter()
            .find(|r| r.path == "/style/app/style-info/getDesignDetail")
            .unwrap();
        assert_eq!(sd.class_name, "ApiEndpoints");
        assert_eq!(sd.name, "styleDetail");
    }

    /// Interpolated path params in endpoint-table constants must normalize
    /// to `:param` like the inline-call pipeline does, so `/users/$id` and
    /// `/users/${id}` collapse onto one consumed node and the placeholder
    /// drops out of cross-graph match keys (issues2.md #31/#44).
    #[test]
    fn parse_dart_route_constants_normalizes_interpolated_params() {
        let dart = r#"
class ApiEndpoints {
  static const String userDetail = '/member/users/$userId';
  static const String orderItem = '/order/orders/${orderId}';
}
"#;
        let routes = parse_dart_route_constants(dart);
        let paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths.contains(&"/member/users/:param"),
            "dollar-identifier must placeholder, got {paths:?}"
        );
        assert!(
            paths.contains(&"/order/orders/:param"),
            "braced interpolation must placeholder, got {paths:?}"
        );
    }

    #[test]
    fn parse_dart_route_constants_ignores_non_route_class() {
        // Same `static const String = '/...'` shape, but the class name does not
        // look like a route table → not treated as consumed routes.
        let dart = r#"
class AssetPaths {
  static const String logo = '/assets/logo.png';
}
"#;
        assert!(parse_dart_route_constants(dart).is_empty());
    }

    #[test]
    fn parse_dart_consumed_calls_extracts_dio_verbs_and_normalizes_interpolation() {
        // The dominant real Dart client shape (Shift app `sync_api.dart`): a Dio
        // client called inline with a path *literal*, no `ApiEndpoints` table.
        // The verb is the method name, the path the first string argument (the
        // `<T>` type args between method and `(` must be skipped), and `$id`
        // interpolation must normalize to a `:param` placeholder so it matches
        // the server's `:id` under `route_key`. Non-path `.get('key')` calls on
        // maps/caches (arg without a leading `/`) must be ignored.
        let dart = r#"
class BackendClient {
  Future<void> me() async {
    final r = await _dio.get<Map<String, dynamic>>('/v1/me');
  }
  Future<void> deleteAccount() async {
    await _dio.delete<void>('/v1/account');
  }
  Future<void> teamSync(String teamId) async {
    await _dio.post<Map<String, dynamic>>('/v1/teams/$teamId/sync', data: {});
  }
  String local() => cache.get('local-key');
  String header() => options.headers['x'];
}
"#;
        let calls = parse_dart_consumed_calls(dart);
        let got: Vec<(&str, &str)> = calls
            .iter()
            .map(|c| (c.verb.as_str(), c.path.as_str()))
            .collect();
        assert!(got.contains(&("GET", "/v1/me")), "GET /v1/me, got {got:?}");
        assert!(
            got.contains(&("DELETE", "/v1/account")),
            "DELETE /v1/account, got {got:?}"
        );
        assert!(
            got.contains(&("POST", "/v1/teams/:param/sync")),
            "interpolation → :param, got {got:?}"
        );
        assert!(
            !got.iter().any(|(_, p)| p.contains("local-key")),
            "non-path .get('local-key') ignored, got {got:?}"
        );
    }

    #[test]
    fn parse_ts_consumed_calls_handles_axios_template_literals() {
        // Shift admin `src/api.ts`: axios with `<T>` generics and a backtick
        // template-literal path carrying `${...}` interpolation, which must
        // normalize to `:param` so it matches the server's `:id`. The same
        // inline shape Dart uses, but TS adds backtick strings.
        let ts = r#"
const http = axios.create({ baseURL });
export async function login(username: string) {
  const { data } = await http.post<LoginResponse>('/admin/login', { username });
}
export async function getUser(id: string) {
  const { data } = await http.get<AdminUserDetail>(`/admin/users/${encodeURIComponent(id)}`);
}
export async function grant(id: string, body: GrantBody) {
  const { data } = await http.post<UserDTO>(`/admin/users/${encodeURIComponent(id)}/grant`, body);
}
"#;
        let calls = parse_ts_consumed_calls(ts);
        let got: Vec<(&str, &str)> = calls
            .iter()
            .map(|c| (c.verb.as_str(), c.path.as_str()))
            .collect();
        assert!(
            got.contains(&("POST", "/admin/login")),
            "POST /admin/login, got {got:?}"
        );
        assert!(
            got.contains(&("GET", "/admin/users/:param")),
            "template `${{…}}` → :param, got {got:?}"
        );
        assert!(
            got.contains(&("POST", "/admin/users/:param/grant")),
            "POST grant, got {got:?}"
        );
    }

    #[test]
    fn parse_ts_consumed_calls_ignores_commented_out_calls() {
        // Commented-out client calls (a `//` line and a `/* */` block) are dead
        // code, not consumed routes. The live call — even on a backtick template
        // path — still resolves.
        let ts = r#"
const http = axios.create({ baseURL });
export async function f(id: string) {
  // const a = await http.get('/admin/legacy');
  /* const b = await http.post('/admin/old', body); */
  const { data } = await http.get(`/admin/users/${id}`);
}
"#;
        let calls = parse_ts_consumed_calls(ts);
        let got: Vec<(&str, &str)> = calls
            .iter()
            .map(|c| (c.verb.as_str(), c.path.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![("GET", "/admin/users/:param")],
            "only the live call counts; commented-out ones are ignored, got {got:?}"
        );
    }

    #[test]
    fn parse_ts_consumed_calls_handles_dollar_receivers() {
        // jQuery `$.get(...)` and RxJS-style `http$.post(...)` receivers both end
        // in `$`, a valid JS/TS identifier char (ECMAScript IdentifierStart). They
        // consume backend routes like any other client call, so the `$` preceding
        // the verb must not drop the call. Real vub admin JS does exactly this:
        // `$.get("/admin/attachment/" + id, ...)`.
        let js = r#"
function load() {
  $.get("/admin/attachment/list", function (r) { vm.x = r; });
  this.http$.post("/api/v2/orders", body);
}
"#;
        let calls = parse_ts_consumed_calls(js);
        let mut got: Vec<(&str, &str)> = calls
            .iter()
            .map(|c| (c.verb.as_str(), c.path.as_str()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("GET", "/admin/attachment/list"),
                ("POST", "/api/v2/orders"),
            ],
            "$-receivers ($.get, http$.post) must be detected, got {got:?}"
        );
    }

    #[test]
    fn parse_ts_server_routes_express_app_param_and_router() {
        // CraftAI shape: routes registered on an `app: Express` *parameter* with
        // inline arrow handlers, plus a `router = express.Router()` with a named
        // handler. `app.get("env")` (a settings read) and a client `http.get`
        // must NOT be treated as served routes.
        let ts = r#"
import type { Express } from "express";
export async function registerRoutes(app: Express) {
  if (app.get("env") === "development") {}
  app.post("/api/auth/login", async (req, res) => { res.json({ ok: true, n: 1 }); });
  app.get("/api/screens/:id", async (req, res) => { res.json({}); });
  // app.delete("/api/screens/:id", async (req, res) => {});
}
const router = express.Router();
router.get("/api/health", healthHandler);
const http = axios.create({ baseURL });
async function client() { await http.get("/api/remote"); }
"#;
        let routes = parse_ts_server_routes(ts);
        let got: Vec<(&str, &str, &str)> = routes
            .iter()
            .map(|r| (r.verb.as_str(), r.path.as_str(), r.method.as_str()))
            .collect();
        assert_eq!(
            routes.len(),
            3,
            "login + screens/:id + health; settings read, client call & commented route excluded, got {got:?}"
        );
        assert!(
            got.contains(&("POST", "/api/auth/login", "")),
            "closure handler → empty: {got:?}"
        );
        assert!(got.contains(&("GET", "/api/screens/:id", "")), "{got:?}");
        assert!(
            got.contains(&("GET", "/api/health", "healthHandler")),
            "named handler kept for linking: {got:?}"
        );
    }

    #[test]
    fn ts_consumed_scanner_excludes_express_server_routes() {
        // The disambiguation: `app.get("/p", handler)` (server) and
        // `http.get("/p")` (client) share a shape; only the client call is a
        // consumed route.
        let ts = r#"
import type { Express } from "express";
export function reg(app: Express) {
  app.get("/api/users", async (req, res) => { res.json([]); });
}
const http = axios.create({ baseURL });
export async function fetchUsers() { return await http.get("/api/users"); }
"#;
        let calls = parse_ts_consumed_calls(ts);
        let consumed: Vec<(&str, &str)> = calls
            .iter()
            .map(|c| (c.verb.as_str(), c.path.as_str()))
            .collect();
        assert_eq!(
            consumed,
            vec![("GET", "/api/users")],
            "only the client http.get is consumed; the express app.get is a server route, got {consumed:?}"
        );
    }

    #[test]
    fn links_dart_inline_dio_consumer_to_consumed_route() {
        // End-to-end of the inline-call path (the Shift-app shape): a Dart method
        // calls `_dio.get('/v1/me')` directly — no constant table — and the full
        // schema pass must emit a consumed HttpRoute node and a
        // method--references-->route edge on the *enclosing* callable.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("lib/sync")).unwrap();
        std::fs::write(
            root.join("lib/sync/sync_api.dart"),
            "class BackendClient {\n  Future<void> me() async {\n    await _dio.get<Map<String, dynamic>>('/v1/me');\n  }\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        // Pre-seed the consumer method node (normally from the Dart indexer).
        let method_id = ArtifactId::new("dart_method::lib/sync/sync_api.dart#BackendClient.me");
        let mut mnode = Node::new(method_id.clone(), NodeKind::DartMethod);
        mnode.path = Some("lib/sync/sync_api.dart".to_string());
        mnode.source_file = Some("lib/sync/sync_api.dart".to_string());
        mnode.start_line = Some(2);
        mnode.end_line = Some(4);
        store.upsert_node(&mnode).unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.consumed_routes, 1, "inline dio route indexed");
        assert_eq!(stats.route_consumer_edges, 1, "one consumer->route edge");

        let routes = store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap();
        assert!(
            routes.iter().any(|r| r.path.as_deref() == Some("/v1/me")),
            "consumed /v1/me node, got {routes:?}"
        );
        let edges = store.list_edges_from(&method_id).unwrap();
        assert!(
            edges.iter().any(|e| e.kind == EdgeKind::References),
            "me() references the consumed route"
        );
    }

    #[test]
    fn links_ts_inline_axios_consumer_to_consumed_route() {
        // The TS/JS arm of the inline-call path: an axios call with `<T>`
        // generics inside a typescript_function must yield a consumed route node
        // and a function--references-->route edge (lang filter = "typescript").
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/api.ts"),
            "export async function login() {\n  const { data } = await http.post<R>('/admin/login', body);\n  return data;\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let fn_id = ArtifactId::new("typescript_function::src/api.ts#login");
        let mut fnode = Node::new(fn_id.clone(), NodeKind::TypescriptFunction);
        fnode.path = Some("src/api.ts".to_string());
        fnode.source_file = Some("src/api.ts".to_string());
        fnode.start_line = Some(1);
        fnode.end_line = Some(4);
        store.upsert_node(&fnode).unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.consumed_routes, 1, "inline axios route indexed");
        assert_eq!(stats.route_consumer_edges, 1, "one consumer->route edge");
        let edges = store.list_edges_from(&fn_id).unwrap();
        assert!(
            edges.iter().any(|e| e.kind == EdgeKind::References),
            "login() references the consumed route"
        );
    }

    #[test]
    fn parse_dart_route_constants_ignores_client_navigation_router() {
        // `AppRouter` matches the name heuristic ("route"), but it is a *client*
        // navigation table (named Flutter routes consumed by Navigator), not a
        // backend API endpoint table. Its file carries navigation machinery
        // (`onGenerateRoute` / `Navigator` / `*PageRoute`), the discriminator:
        // an API endpoint constants file never does. Without this guard the
        // client's own screen routes (`/login`, `/home`) get mis-emitted as
        // backend routes it "consumes", conflating UI flow with API surface.
        let dart = r#"
class AppRouter {
  const AppRouter._();
  static const String login = '/login';
  static const String home = '/home';
  static const String productDetail = '/product-detail';

  static Route<dynamic>? onGenerateRoute(RouteSettings settings) {
    switch (settings.name) {
      case login:
        return CupertinoPageRoute<void>(builder: (_) => const LoginPage());
    }
    return null;
  }

  static void toHome(BuildContext context) {
    Navigator.pushNamed(context, home);
  }
}
"#;
        assert!(
            parse_dart_route_constants(dart).is_empty(),
            "navigation router must not be treated as a backend endpoint table"
        );
    }

    #[test]
    fn links_dart_consumer_to_consumed_route() {
        // An api-client method whose body references `ApiEndpoints.styleDetail`
        // must yield a consumed HttpRoute node for that path and a
        // method--references-->route edge, so the client's backend API surface
        // is queryable by URL path (symmetric with server-side HttpRoute).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("lib/core")).unwrap();
        std::fs::write(
            root.join("lib/core/endpoints.dart"),
            r#"
class ApiEndpoints {
  static const String styleDetail = '/style/app/style-info/getDesignDetail';
}
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("lib/core/client.dart"),
            "class StyleApiClient {\n  Future<dynamic> getDetail() {\n    return _http.get(ApiEndpoints.styleDetail);\n  }\n}\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        // Pre-seed the consumer method node (normally from the Dart indexer).
        let method_id =
            ArtifactId::new("dart_method::lib/core/client.dart#StyleApiClient.getDetail");
        let mut mnode = Node::new(method_id.clone(), NodeKind::DartMethod);
        mnode.path = Some("lib/core/client.dart".to_string());
        mnode.source_file = Some("lib/core/client.dart".to_string());
        mnode.start_line = Some(2);
        mnode.end_line = Some(4);
        store.upsert_node(&mnode).unwrap();

        let stats = index_schema_into(&mut store, root).unwrap();
        assert_eq!(stats.consumed_routes, 1, "one consumed route indexed");
        assert_eq!(stats.route_consumer_edges, 1, "one consumer->route edge");

        let routes = store.list_nodes_by_kind(NodeKind::HttpRoute).unwrap();
        let route = routes
            .iter()
            .find(|r| r.path.as_deref() == Some("/style/app/style-info/getDesignDetail"))
            .expect("consumed route node");
        assert_eq!(route.name.as_deref(), Some("getDesignDetail"));

        let edges = store.list_edges_from(&method_id).unwrap();
        assert!(
            edges
                .iter()
                .any(|e| e.kind == EdgeKind::References && e.to_id == route.id),
            "expected consumer->route References edge, got {edges:?}"
        );
    }

    #[test]
    fn schema_index_skips_nested_groundgraph_workspaces() {
        // A vendored reference repo carries its *own* `.groundgraph.yaml`; it is a
        // separate GroundGraph workspace, indexed by its own `index`, never folded
        // into the parent graph. Without this, a repo that vendors reference
        // source (e.g. tailorx bundling the Java `platform` under
        // `docs/references/source-repos/`) gets thousands of phantom
        // routes/tables/mappers with no parent code node to link to.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // The parent workspace's own schema + its own config.
        std::fs::write(root.join(".groundgraph.yaml"), "repo:\n  root: .\n").unwrap();
        std::fs::write(
            root.join("schema.sql"),
            "CREATE TABLE own_table (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        // A nested, self-contained workspace (vendored reference) with schema.
        let nested = root.join("docs/references/vendored");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(".groundgraph.yaml"), "repo:\n  root: .\n").unwrap();
        std::fs::write(
            nested.join("vendored_schema.sql"),
            "CREATE TABLE vendored_table (id INTEGER PRIMARY KEY);",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let stats = index_schema_into(&mut store, root).unwrap();

        let names: Vec<String> = store
            .list_nodes_by_kind(NodeKind::DbTable)
            .unwrap()
            .iter()
            .filter_map(|t| t.name.clone())
            .collect();
        assert!(
            names.iter().any(|n| n == "own_table"),
            "parent workspace table indexed, got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "vendored_table"),
            "nested workspace table must be skipped, got {names:?}"
        );
        assert_eq!(stats.sql_tables, 1, "only the parent's own table counts");
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

    /// Regression: `parse_sql_tables` must not panic when a column body ends on
    /// a multi-byte UTF-8 char. The loop advanced `search_from` by `body.len()`,
    /// landing one byte *before* the closing paren — inside the last char — so
    /// the next `lower[search_from..]` sliced mid-char and panicked
    /// (`byte index N is not a char boundary`). Indexing GroundGraph's own source
    /// (a doc string containing '…') tripped this in the wild.
    #[test]
    fn parse_sql_tables_does_not_panic_on_multibyte_char_at_body_end() {
        // 3-byte '…' immediately before ')': the exact shape that crashed index.
        let tables = parse_sql_tables("create table a(note …)");
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "a");
        // A trailing table forces the re-slice that actually panicked.
        let two = parse_sql_tables("create table a(x é) and create table b(id int)");
        assert!(two.iter().any(|t| t.name == "b"), "second table must parse");
    }

    #[test]
    fn sql_file_scanner_skips_commented_out_tables_and_inline_notes() {
        // A `.sql` file: a `--` and a `/* */` commented-out CREATE TABLE must not
        // mint phantom tables, and a `-- note` inside the live table body must not
        // become a bogus column.
        let sql = r#"
-- CREATE TABLE old_users (id BIGINT);
/* CREATE TABLE legacy (
     id BIGINT
   ); */
CREATE TABLE users (
    id BIGINT PRIMARY KEY, -- internal id, do not expose
    email TEXT NOT NULL
);
"#;
        let tables = parse_sql_tables_from_sql_file(sql);
        assert_eq!(
            tables.len(),
            1,
            "only the live table; commented-out DDL is ignored, got {tables:?}"
        );
        assert_eq!(tables[0].name, "users");
        let cols: Vec<&str> = tables[0].columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, vec!["id", "email"], "the `-- note` is not a column");
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
    fn entity_fields_after_a_braced_string_literal_are_still_columns() {
        // `"{"` inside a field initialiser used to be counted as a real
        // brace, so the depth drifted to 2 and every later field was
        // misread as method-body noise (issues.md #10).
        let java = r#"
import com.baomidou.mybatisplus.annotation.TableId;

@TableName("demo")
public class Demo implements Serializable {

    @TableId
    private Integer id;

    private String jsonPrefix = "{";

    private Integer afterBrace;
}
"#;
        let tables = parse_java_entity_tables(java);
        assert_eq!(tables.len(), 1);
        let names: Vec<&str> = tables[0].columns.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"after_brace"),
            "field after the braced literal must still be a column: {names:?}"
        );
    }

    #[test]
    fn balanced_parens_skips_parens_inside_string_literals() {
        // Route annotations regularly carry parens in their path strings —
        // `@GetMapping("/smile(")` — the old counter treated them as real
        // nesting and never found the close (issues.md #11).
        let b = br#"("/smile(" , handler)"#;
        let (body, end) = balanced_parens(b, 0).expect("must close at the real `)`");
        assert_eq!(body, r#""/smile(" , handler"#);
        assert_eq!(end, b.len());

        // Single-quoted SQL strings too: `DEFAULT '('` inside a column body.
        let b = b"(x INT DEFAULT '(', y INT)";
        let (body, _) = balanced_parens(b, 0).expect("close");
        assert_eq!(body, "x INT DEFAULT '(', y INT");

        // Balanced input without strings keeps working.
        let b = b"(a, (b, c), d)";
        let (body, end) = balanced_parens(b, 0).expect("close");
        assert_eq!(body, "a, (b, c), d");
        assert_eq!(end, b.len());
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
        assert_eq!(
            stmts.len(),
            1,
            "only the <select> is a statement: {stmts:?}"
        );
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
        assert!(
            extract_sql_table_refs("select date_from, transform_id from t")
                .contains(&"t".to_string())
        );
        assert_eq!(
            extract_sql_table_refs("select date_from, transform_id from t").len(),
            1
        );
    }

    #[test]
    fn extract_table_refs_handles_insert_without_into() {
        // MySQL allows `INSERT <table>` (no INTO); MyBatis batch inserts use it.
        let t1 =
            extract_sql_table_refs("insert style_package_info ( category, style_code ) values");
        assert!(
            t1.contains(&"style_package_info".to_string()),
            "insert-without-into missed the table: {t1:?}"
        );
        // Classic `insert into x` still resolves to x (and never to `into`).
        let t2 =
            extract_sql_table_refs("insert into v_image_post(string, time) value(#{s}, now())");
        assert_eq!(t2, vec!["v_image_post"]);
    }

    #[test]
    fn extract_table_refs_excludes_cte_names() {
        // `WITH RECURSIVE cte AS (…) … FROM cte`: `cte` is a CTE alias, not a
        // base table. The real table (`craft`) is kept; the CTE name is dropped
        // so it isn't mistaken for (or synthesized as) a table.
        let sql =
            "WITH RECURSIVE cte AS ( SELECT id, pid FROM craft WHERE id = 1 ) SELECT * FROM cte";
        let t = extract_sql_table_refs(sql);
        assert!(
            t.contains(&"craft".to_string()),
            "real base table kept: {t:?}"
        );
        assert!(!t.contains(&"cte".to_string()), "CTE alias excluded: {t:?}");

        // Multiple CTEs in one WITH.
        let multi = "with a as (select 1 from t1), b as (select 2 from t2) select * from a join b";
        let mut m = extract_sql_table_refs(multi);
        m.sort();
        assert_eq!(
            m,
            vec!["t1", "t2"],
            "only base tables, no CTE aliases: {m:?}"
        );
    }

    #[test]
    fn extract_table_refs_skips_on_duplicate_key_update_columns() {
        // `ON DUPLICATE KEY UPDATE col=…` — the token after UPDATE is a column,
        // not a table, so it must not be mistaken for one (would otherwise
        // synthesize a bogus external table named after the column).
        let sql = "insert into t (a, b) values (1, 2) on duplicate key update a = values(a)";
        let t = extract_sql_table_refs(sql);
        assert!(t.contains(&"t".to_string()), "{t:?}");
        assert!(
            !t.contains(&"a".to_string()),
            "SET column must not be a table: {t:?}"
        );
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
        // 2-char minimum boundary (the `name.len() < 2` guard above the
        // `chars.next()` extraction): a valid 2-char name passes.
        assert!(is_plausible_table_name("ab"));
    }

    #[test]
    fn is_ident_handles_degenerate_inputs() {
        // Pins the contract around the `!s.is_empty()` guard that protects the
        // leading-char inspection (#206): empty and leading-digit reject
        // without panicking even if the guard is later refactored.
        assert!(is_ident("name"));
        assert!(is_ident("_private"));
        assert!(is_ident("col_1"));
        assert!(!is_ident(""));
        assert!(!is_ident("1col"));
        assert!(!is_ident("a-b"));
    }

    #[test]
    fn parse_java_field_rejects_degenerate_lines() {
        // Pins the contract around the `tokens.len() < 2` guard that protects
        // the `tokens.last()` extraction (#206).
        assert_eq!(
            parse_java_field("private int count;"),
            Some("count".to_string())
        );
        assert_eq!(parse_java_field(";"), None);
        assert_eq!(parse_java_field("   ;"), None);
        assert_eq!(parse_java_field("void run();"), None);
        assert_eq!(parse_java_field("static final int X = 1;"), None);
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
        // groundgraph must synthesize an `external` DbTable node so the trace from
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
        assert_eq!(
            stats2.external_tables, 1,
            "external count stable on re-index"
        );
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
