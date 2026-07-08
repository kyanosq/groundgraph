//! `groundgraph schema-index` — index DB schema (P25).
//!
//! Scans the repo for `CREATE TABLE` (`.sql`) and ORM entity annotations
//! (`@TableName` / `@Table` in `.java`) and writes each table (with columns)
//! into the graph as a `DbTable` node, so `graph-equiv` can audit data-contract
//! parity between a service and its rewrite. Also indexes MyBatis mapper XML
//! statements (`<select|insert|update|delete>`) as `SqlMapperStmt` nodes so the
//! query SQL becomes searchable graph evidence for porting.

use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_engine::schema_indexer::{index_schema, SchemaIndexStats};

#[derive(Debug, Clone)]
pub struct SchemaIndexRunArgs {
    pub repo_root: PathBuf,
    pub json: bool,
}

pub fn run(args: SchemaIndexRunArgs) -> Result<()> {
    let stats: SchemaIndexStats =
        index_schema(&args.repo_root).context("索引数据库表结构 (schema-index)")?;
    groundgraph_engine::stats::set_metric("tables", (stats.sql_tables + stats.orm_tables) as i64);
    groundgraph_engine::stats::set_metric("implicit_orm_tables", stats.implicit_orm_tables as i64);
    groundgraph_engine::stats::set_metric("external_tables", stats.external_tables as i64);
    groundgraph_engine::stats::set_metric("columns", stats.columns as i64);
    groundgraph_engine::stats::set_metric("mapper_stmts", stats.mapper_stmts as i64);
    groundgraph_engine::stats::set_metric(
        "data_layer_edges",
        (stats.stmt_method_edges + stats.stmt_table_edges) as i64,
    );
    groundgraph_engine::stats::set_metric("iface_impl_edges", stats.iface_impl_edges as i64);
    groundgraph_engine::stats::set_metric(
        "inline_sql_table_edges",
        stats.inline_sql_table_edges as i64,
    );
    groundgraph_engine::stats::set_metric("http_routes", stats.http_routes as i64);
    groundgraph_engine::stats::set_metric("route_method_edges", stats.route_method_edges as i64);
    groundgraph_engine::stats::set_metric("consumed_routes", stats.consumed_routes as i64);
    groundgraph_engine::stats::set_metric("route_consumer_edges", stats.route_consumer_edges as i64);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        println!("GroundGraph 表结构索引完成 (DbTable / SqlMapperStmt 节点)");
        println!(
            "扫描文件 {} · SQL 表 {} · ORM 表 {} (含类名约定推断 {}) · 外部表(无 schema) {} · 列合计 {} · Mapper 语句 {}",
            stats.files_scanned,
            stats.sql_tables,
            stats.orm_tables,
            stats.implicit_orm_tables,
            stats.external_tables,
            stats.columns,
            stats.mapper_stmts,
        );
        println!(
            "数据层链路: method→SQL {} 条 · SQL→table {} 条 · interface→impl {} 条 · 内联SQL→table {} 条 (接口可直达表)",
            stats.stmt_method_edges,
            stats.stmt_table_edges,
            stats.iface_impl_edges,
            stats.inline_sql_table_edges,
        );
        println!(
            "HTTP 路由: {} 个 · 路由→方法 {} 条 (按 URL 路径反查 Spring 处理方法)",
            stats.http_routes, stats.route_method_edges,
        );
        println!(
            "客户端消费路由: {} 个 · 调用方→路由 {} 条 (按 URL 路径反查 Dart 调用方,与服务端对齐)",
            stats.consumed_routes, stats.route_consumer_edges,
        );
    }
    Ok(())
}
