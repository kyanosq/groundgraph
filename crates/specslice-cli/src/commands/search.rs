//! `specslice search` — `grep` replacement that returns code-graph
//! matches with explanations and a 1-hop subgraph.
//!
//! Three input forms (mutually exclusive):
//!
//! ```text
//! specslice search "login auth session"
//! specslice search --code "authService.signIn(email)"
//! specslice search --file lib/auth/auth_service.dart --line 42
//! ```
//!
//! Output mode: `--json` for machine consumption (default is a
//! human-friendly text rendering).

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use specslice_core::NodeKind;
use specslice_engine::search::{run_search, SearchOptions, SearchQuery, SearchResult};
use specslice_engine::{default_search_kinds, SEARCH_DEFAULT_DEPTH, SEARCH_DEFAULT_LIMIT};

#[derive(Debug, Clone)]
pub struct SearchRunArgs {
    pub repo_root: PathBuf,
    pub query: Option<String>,
    pub code: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub depth: usize,
    pub limit: usize,
    pub kinds: Vec<String>,
    pub json: bool,
    pub include_noise: bool,
}

pub fn run(args: SearchRunArgs) -> Result<()> {
    let query = pick_query(&args)?;
    let kinds = parse_kinds(&args.kinds)?;
    let options = SearchOptions {
        repo_root: args.repo_root.clone(),
        query,
        depth: args.depth,
        kinds,
        limit: args.limit.max(1),
        include_noise: args.include_noise,
    };
    let result = run_search(options).context("running search")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).context("serialising search result")?
        );
    } else {
        print_human(&result);
    }
    Ok(())
}

fn pick_query(args: &SearchRunArgs) -> Result<SearchQuery> {
    let count = [
        args.query.is_some(),
        args.code.is_some(),
        args.file.is_some(),
    ]
    .iter()
    .filter(|x| **x)
    .count();
    if count == 0 {
        bail!("provide a positional query, --code, or --file/--line");
    }
    if count > 1 {
        bail!("--code, --file and positional query are mutually exclusive");
    }
    if let Some(q) = &args.query {
        return Ok(SearchQuery::Keywords(q.clone()));
    }
    if let Some(c) = &args.code {
        return Ok(SearchQuery::Code(c.clone()));
    }
    let path = args.file.as_ref().unwrap().clone();
    let line = args
        .line
        .context("--file requires --line; pass --line <N> together with --file")?;
    Ok(SearchQuery::Position { path, line })
}

fn parse_kinds(raw: &[String]) -> Result<Vec<NodeKind>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut out: Vec<NodeKind> = Vec::new();
    for entry in raw {
        for piece in entry.split(',') {
            let trimmed = piece.trim();
            if trimmed.is_empty() {
                continue;
            }
            out.push(match_kind(trimmed)?);
        }
    }
    Ok(out)
}

fn match_kind(name: &str) -> Result<NodeKind> {
    // Operator-friendly short aliases (`method` for `dart_method`) so
    // `--kind method,class,test` works without the `dart_` prefix.
    let lower = name.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "file" => NodeKind::File,
        "doc" | "doc_section" => NodeKind::DocSection,
        "class" | "dart_class" => NodeKind::DartClass,
        "method" | "dart_method" => NodeKind::DartMethod,
        "function" | "dart_function" => NodeKind::DartFunction,
        "constructor" | "dart_constructor" => NodeKind::DartConstructor,
        "test" | "test_case" => NodeKind::TestCase,
        "group" | "test_group" => NodeKind::TestGroup,
        "provider" | "dart_provider" => NodeKind::DartProvider,
        "route" => NodeKind::Route,
        "storage" => NodeKind::Storage,
        "candidate" | "business_candidate" => NodeKind::BusinessCandidate,
        "requirement" => NodeKind::Requirement,
        other => {
            bail!(
                "unknown --kind `{other}`. valid: {}",
                default_search_kinds()
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    })
}

fn print_human(r: &SearchResult) {
    println!("SpecSlice search");
    println!("查询: {}", r.query);
    if !r.tokens.is_empty() {
        println!("分词: {}", r.tokens.join(", "));
    }
    println!();
    if r.matches.is_empty() {
        println!("(无命中)");
    } else {
        println!("== 命中 ({}) ==", r.matches.len());
        for (i, m) in r.matches.iter().enumerate() {
            let line = match m.line_range {
                Some((s, e)) => format!(":{s}-{e}"),
                None => String::new(),
            };
            let path = m.path.clone().unwrap_or_default();
            println!("[{:>3}] {} ({})  分数={}", i + 1, m.label, m.kind, m.score);
            println!("      id: {}", m.id);
            if !path.is_empty() {
                println!("      路径: {path}{line}");
            }
            if let Some(src) = &m.source {
                println!("      来源: {src}");
            }
            if !m.match_reasons.is_empty() {
                println!("      命中原因:");
                for reason in &m.match_reasons {
                    println!("        - {reason}");
                }
            }
        }
    }
    if !r.subgraph.nodes.is_empty() || !r.subgraph.edges.is_empty() {
        println!();
        println!(
            "== 子图 (节点 {} / 边 {}) ==",
            r.subgraph.nodes.len(),
            r.subgraph.edges.len()
        );
        // Show edges only — they're the interesting "why are these
        // connected" info. Nodes are summarised at the top.
        for e in r.subgraph.edges.iter().take(20) {
            println!("    {} --{}--> {}", e.from, e.kind, e.to);
        }
        if r.subgraph.edges.len() > 20 {
            println!("    ...");
        }
    }
    if !r.graph_commands.is_empty() {
        println!();
        println!("可视化命令:");
        for cmd in &r.graph_commands {
            println!("  $ {cmd}");
        }
    }
}

/// Surface the engine-side defaults so `main.rs` can wire `default_value_t`
/// without re-declaring constants.
#[allow(dead_code)]
pub fn default_depth() -> usize {
    SEARCH_DEFAULT_DEPTH
}

#[allow(dead_code)]
pub fn default_limit() -> usize {
    SEARCH_DEFAULT_LIMIT
}
