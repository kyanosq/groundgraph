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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use specslice_core::NodeKind;
use specslice_engine::graph::GraphLayer;
use specslice_engine::search::{
    compute_search_html_payload, run_search, SearchOptions, SearchQuery, SearchResult,
    HTML_DEFAULT_FOCUS_BUDGET,
};
use specslice_engine::{default_search_kinds, SEARCH_DEFAULT_DEPTH, SEARCH_DEFAULT_LIMIT};

use crate::commands::graph_mermaid::{render_parts, MermaidEdge, MermaidNode};
use crate::commands::search_html;

/// Output mode selected on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFormat {
    /// Human-readable Chinese text (default).
    Text,
    /// JSON for agents / scripts.
    Json,
    /// Self-contained search-driven HTML reader.
    Html,
    /// P14 — local Mermaid `flowchart LR` of the search subgraph.
    Mermaid,
}

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
    pub format: SearchFormat,
    /// File to write `Html` output to. When `None`, HTML lands in
    /// `<repo_root>/.specslice/export/search-<slug>.html`. JSON / Text
    /// always go to stdout.
    pub output: Option<PathBuf>,
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
    match args.format {
        SearchFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).context("serialising search result")?
            );
        }
        SearchFormat::Text => print_human(&result),
        SearchFormat::Html => {
            let payload =
                compute_search_html_payload(&result, &args.repo_root, HTML_DEFAULT_FOCUS_BUDGET);
            let html = search_html::render_html(&payload).context("rendering search HTML")?;
            let out_path = resolve_html_output(&args.repo_root, &args.output, &result)?;
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating output directory {}", parent.display()))?;
            }
            std::fs::write(&out_path, html)
                .with_context(|| format!("writing HTML to {}", out_path.display()))?;
            println!("HTML 已生成: {}", out_path.display());
        }
        SearchFormat::Mermaid => {
            let mermaid = render_search_mermaid(&result);
            write_or_stdout(
                &args.repo_root,
                &args.output,
                "search",
                &result.query,
                &mermaid,
            )?;
        }
    }
    Ok(())
}

/// Build a Mermaid `flowchart LR` from a `SearchResult`. Matches are
/// rendered as `Confirmed` (rounded) nodes so reviewers can spot them
/// against expansion-only `Fact` (rectangular) neighbours at a glance.
pub fn render_search_mermaid(result: &SearchResult) -> String {
    let match_ids: BTreeSet<String> = result.matches.iter().map(|m| m.id.clone()).collect();
    let nodes: Vec<MermaidNode> = result
        .subgraph
        .nodes
        .iter()
        .map(|node| {
            let layer = if match_ids.contains(&node.id) {
                GraphLayer::Confirmed
            } else {
                GraphLayer::Fact
            };
            MermaidNode {
                id: node.id.clone(),
                label: node.label.clone(),
                layer,
                path: node.path.clone(),
            }
        })
        .collect();
    let edges: Vec<MermaidEdge> = result
        .subgraph
        .edges
        .iter()
        .map(|edge| MermaidEdge {
            from: edge.from.clone(),
            to: edge.to.clone(),
            kind: edge.kind.clone(),
            layer: GraphLayer::Fact,
        })
        .collect();
    let notes = vec![format!(
        "specslice search \"{}\" matches={} subgraph_nodes={} edges={}",
        result.query.replace('"', "'"),
        result.matches.len(),
        result.subgraph.nodes.len(),
        result.subgraph.edges.len()
    )];
    render_parts(&nodes, &edges, &notes)
}

/// Resolve `--output` (or default to stdout) for plain-text Mermaid /
/// other future text exports. `kind` and `slug_basis` only matter when
/// the caller wants the default-path behaviour applied — for now, both
/// `search` and `impact` go to stdout when `--output` is omitted, so we
/// keep the surface intentionally narrow.
fn write_or_stdout(
    _repo_root: &Path,
    output: &Option<PathBuf>,
    _kind: &str,
    _slug_basis: &str,
    contents: &str,
) -> Result<()> {
    match output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("creating output directory {}", parent.display())
                    })?;
                }
            }
            std::fs::write(path, contents)
                .with_context(|| format!("writing output to {}", path.display()))?;
            println!("已写入: {}", path.display());
        }
        None => {
            print!("{contents}");
        }
    }
    Ok(())
}

fn resolve_html_output(
    repo_root: &Path,
    requested: &Option<PathBuf>,
    result: &SearchResult,
) -> Result<PathBuf> {
    if let Some(p) = requested {
        if p.is_absolute() {
            return Ok(p.clone());
        }
        return Ok(repo_root.join(p));
    }
    let slug = slugify(&result.query);
    let name = if slug.is_empty() {
        "search.html".to_string()
    } else {
        format!("search-{slug}.html")
    };
    Ok(repo_root.join(".specslice/export").join(name))
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 40 {
        out.truncate(40);
    }
    out
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
    // Canonical snake_case names resolve via the single source of truth in
    // `specslice-core` (`dart_class`, `typescript_interface`, `cpp_method`,
    // …) — no need to re-list all ~58 here. Below we only keep the *extra*
    // short / legacy aliases the canonical scheme does not cover (kept in
    // lockstep with the MCP `parse_node_kind` alias table).
    if let Some(kind) = NodeKind::from_str(&lower) {
        return Ok(kind);
    }
    Ok(match lower.as_str() {
        // Bare aliases bound to Dart (`class`/`method` mean Dart unprefixed).
        "doc" => NodeKind::DocSection,
        "class" => NodeKind::DartClass,
        "method" => NodeKind::DartMethod,
        "function" => NodeKind::DartFunction,
        "constructor" => NodeKind::DartConstructor,
        "test" => NodeKind::TestCase,
        "group" => NodeKind::TestGroup,
        "provider" => NodeKind::DartProvider,
        "candidate" => NodeKind::BusinessCandidate,
        // Swift / Go short aliases.
        "swift_init" => NodeKind::SwiftInitializer,
        "gostruct" => NodeKind::GoStruct,
        "gointerface" => NodeKind::GoInterface,
        "gofunc" => NodeKind::GoFunction,
        // Python `py_` aliases.
        "py_module" => NodeKind::PythonModule,
        "py_class" => NodeKind::PythonClass,
        "py_function" | "pyfunc" => NodeKind::PythonFunction,
        "py_method" => NodeKind::PythonMethod,
        // TypeScript `ts_` aliases.
        "ts_module" => NodeKind::TypescriptModule,
        "ts_class" => NodeKind::TypescriptClass,
        "ts_interface" => NodeKind::TypescriptInterface,
        "ts_enum" => NodeKind::TypescriptEnum,
        "ts_function" | "tsfunc" => NodeKind::TypescriptFunction,
        "ts_method" => NodeKind::TypescriptMethod,
        // Rust `rs_` aliases.
        "rs_module" | "rs_mod" => NodeKind::RustModule,
        "rs_struct" => NodeKind::RustStruct,
        "rs_enum" => NodeKind::RustEnum,
        "rs_trait" => NodeKind::RustTrait,
        "rs_function" | "rs_fn" => NodeKind::RustFunction,
        "rs_method" => NodeKind::RustMethod,
        // C / C++ short + `cxx_` aliases.
        "cfn" => NodeKind::CFunction,
        "cxx_namespace" | "cpp_ns" => NodeKind::CppNamespace,
        "cxx_class" => NodeKind::CppClass,
        "cxx_struct" => NodeKind::CppStruct,
        "cxx_enum" => NodeKind::CppEnum,
        "cxx_function" | "cpp_fn" => NodeKind::CppFunction,
        "cxx_method" => NodeKind::CppMethod,
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
            if let Some(role) = &m.framework_role {
                println!("      框架角色: {role}");
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
    let block = render_warnings_block(&r.warnings);
    if !block.is_empty() {
        print!("{block}");
    }
}

/// Build the human-readable warnings tail (`== Warnings ==`) so test
/// code can assert it without capturing stdout.
///
/// Empty `warnings` yields an empty string — the caller must check
/// before printing so we don't emit a blank trailing section.
fn render_warnings_block(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push('\n');
    s.push_str("== Warnings ==\n");
    for w in warnings {
        s.push_str(&format!("  - {w}\n"));
    }
    s
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

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_engine::search::{SearchEdge, SearchMatch, SearchNode, SearchSubgraph};

    /// Regression: `parse_kinds` shipped Dart/Swift/Go/Python kinds but
    /// silently rejected every TypeScript/Java kind, so
    /// `--kind typescript_function` / `--kind java_method` errored out
    /// even though the P20 indexer emits exactly those node kinds.
    #[test]
    fn match_kind_accepts_typescript_and_java_aliases() {
        assert_eq!(
            match_kind("typescript_function").unwrap(),
            NodeKind::TypescriptFunction
        );
        assert_eq!(match_kind("ts_class").unwrap(), NodeKind::TypescriptClass);
        assert_eq!(
            match_kind("ts_interface").unwrap(),
            NodeKind::TypescriptInterface
        );
        assert_eq!(match_kind("ts_enum").unwrap(), NodeKind::TypescriptEnum);
        assert_eq!(
            match_kind("typescript_module").unwrap(),
            NodeKind::TypescriptModule
        );
        assert_eq!(match_kind("java_method").unwrap(), NodeKind::JavaMethod);
        assert_eq!(match_kind("java_class").unwrap(), NodeKind::JavaClass);
        assert_eq!(
            match_kind("java_constructor").unwrap(),
            NodeKind::JavaConstructor
        );
        assert_eq!(match_kind("java_package").unwrap(), NodeKind::JavaPackage);
    }

    /// P21 regression — `default_search_kinds()` advertises the Rust
    /// kinds, so the parser must accept both their full names and the
    /// `rs_` short aliases.
    #[test]
    fn match_kind_accepts_rust_aliases() {
        assert_eq!(match_kind("rust_struct").unwrap(), NodeKind::RustStruct);
        assert_eq!(match_kind("rs_struct").unwrap(), NodeKind::RustStruct);
        assert_eq!(match_kind("rust_enum").unwrap(), NodeKind::RustEnum);
        assert_eq!(match_kind("rust_trait").unwrap(), NodeKind::RustTrait);
        assert_eq!(match_kind("rs_trait").unwrap(), NodeKind::RustTrait);
        assert_eq!(match_kind("rust_function").unwrap(), NodeKind::RustFunction);
        assert_eq!(match_kind("rs_fn").unwrap(), NodeKind::RustFunction);
        assert_eq!(match_kind("rust_method").unwrap(), NodeKind::RustMethod);
        assert_eq!(match_kind("rust_module").unwrap(), NodeKind::RustModule);
        assert_eq!(match_kind("rs_mod").unwrap(), NodeKind::RustModule);
    }

    #[test]
    fn match_kind_accepts_c_and_cpp_aliases() {
        assert_eq!(match_kind("c_function").unwrap(), NodeKind::CFunction);
        assert_eq!(match_kind("c_struct").unwrap(), NodeKind::CStruct);
        assert_eq!(match_kind("c_enum").unwrap(), NodeKind::CEnum);
        assert_eq!(match_kind("cpp_namespace").unwrap(), NodeKind::CppNamespace);
        assert_eq!(match_kind("cxx_class").unwrap(), NodeKind::CppClass);
        assert_eq!(match_kind("cpp_struct").unwrap(), NodeKind::CppStruct);
        assert_eq!(match_kind("cpp_enum").unwrap(), NodeKind::CppEnum);
        assert_eq!(match_kind("cpp_fn").unwrap(), NodeKind::CppFunction);
        assert_eq!(match_kind("cpp_method").unwrap(), NodeKind::CppMethod);
    }

    fn mk_result() -> SearchResult {
        SearchResult {
            query: "login".into(),
            tokens: vec!["login".into()],
            matches: vec![SearchMatch {
                id: "dart_method::lib/auth.dart#A.signIn".into(),
                kind: "dart_method".into(),
                label: "A.signIn".into(),
                path: Some("lib/auth.dart".into()),
                line_range: Some((10, 20)),
                score: 100,
                source: None,
                match_reasons: vec![],
                framework_role: None,
            }],
            subgraph: SearchSubgraph {
                nodes: vec![
                    SearchNode {
                        id: "dart_method::lib/auth.dart#A.signIn".into(),
                        kind: "dart_method".into(),
                        label: "A.signIn".into(),
                        path: Some("lib/auth.dart".into()),
                        line_range: Some((10, 20)),
                    },
                    SearchNode {
                        id: "dart_method::lib/auth.dart#B.callee".into(),
                        kind: "dart_method".into(),
                        label: "B.callee".into(),
                        path: Some("lib/auth.dart".into()),
                        line_range: Some((30, 35)),
                    },
                ],
                edges: vec![SearchEdge {
                    id: "edge1".into(),
                    from: "dart_method::lib/auth.dart#A.signIn".into(),
                    to: "dart_method::lib/auth.dart#B.callee".into(),
                    kind: "calls".into(),
                    source_file: None,
                    line_range: None,
                    snippet: None,
                }],
            },
            graph_commands: vec![],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn search_mermaid_highlights_matches_as_confirmed_nodes_and_uses_aliases() {
        let out = render_search_mermaid(&mk_result());
        assert!(out.starts_with("flowchart LR\n"), "missing header: {out}");
        // Match (A.signIn) → Confirmed → rounded shape `(...)`.
        assert!(
            out.contains("n0(\"A.signIn (lib/auth.dart)\")"),
            "expected rounded match node, got: {out}"
        );
        // Expansion node (B.callee) → Fact → rectangle `[...]`.
        assert!(
            out.contains("n1[\"B.callee (lib/auth.dart)\"]"),
            "expected rectangular expansion node, got: {out}"
        );
        // Edge uses Fact arrow `---` and `calls` label.
        assert!(
            out.contains("n0 ---|calls| n1"),
            "expected `---|calls|` arrow, got: {out}"
        );
        // No raw artifact ids leak through.
        assert!(
            !out.contains("dart_method::"),
            "raw ids leaked into Mermaid: {out}"
        );
        // Note line includes search context for human readers.
        assert!(
            out.contains("specslice search \"login\""),
            "expected search context comment, got: {out}"
        );
    }

    // -----------------------------------------------------------------------
    // v0.3.0-A Phase 4 — CLI human renderer surfaces engine warnings.
    // -----------------------------------------------------------------------

    #[test]
    fn render_warnings_block_empty_returns_empty_string() {
        assert_eq!(render_warnings_block(&[]), "");
    }

    #[test]
    fn render_warnings_block_lists_each_warning_with_header_and_dash() {
        let block = render_warnings_block(&[
            "warn: 节点 X 的出边质量查询失败：sqlite locked".to_string(),
            "warn: 节点 Y 的邻接查询失败：io error".to_string(),
        ]);
        assert!(
            block.contains("== Warnings =="),
            "expected `== Warnings ==` header, got: {block}",
        );
        assert!(
            block.contains("- warn: 节点 X 的出边质量查询失败"),
            "expected first warning rendered with dash, got: {block}",
        );
        assert!(
            block.contains("- warn: 节点 Y 的邻接查询失败"),
            "expected second warning rendered with dash, got: {block}",
        );
        assert!(
            block.starts_with('\n'),
            "warnings block must start with a blank line so it's visually \
             separated from the previous section, got: {block:?}",
        );
    }
}
