//! CLI plumbing for `groundgraph graph`.
//!
//! Format dispatch:
//!
//! - `json`    — print or write the [`GraphViewModel`].
//! - `mermaid` — emit a Mermaid `flowchart LR` for docs/PR embeds.
//! - `html`    — write a fully self-contained HTML file under
//!   `.groundgraph/export/graph.html` by default. No CDN, no network, no
//!   bundler.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use groundgraph_engine::graph::{build_graph_view, GraphOptions, GraphView, GraphViewModel};
use groundgraph_engine::network::{build_network_graph, NetworkOptions};

use super::graph_html::render_html;
use super::graph_mermaid::render_mermaid;

const DEFAULT_HTML_OUT: &str = ".groundgraph/export/graph.html";
const DEFAULT_WEB_OUT: &str = ".groundgraph/export/graph-web.html";

/// The `webui` viewer, the single source of truth for both the standalone dev
/// page and the embedded CLI export. The `web` format inlines the graph into a
/// copy of this template via the `SS_DATA_SLOT` marker.
///
/// This reads the crate-local copy (`crates/groundgraph-cli/webui/`) so
/// `cargo package` / `cargo publish` work — `include_str!` cannot reach outside
/// the crate root. `scripts/sync_webui_assets.sh` (with `--check` in CI) keeps
/// the copy byte-identical to the `webui/` source of truth.
const VIEWER_TEMPLATE: &str = include_str!("../../webui/index.html");

const VIEWER_STYLE: &str = include_str!("../../webui/workspace.css");
const VIEWER_MODEL: &str = include_str!("../../webui/model.js");
const VIEWER_SCRIPT: &str = include_str!("../../webui/workspace.js");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFormat {
    Json,
    Html,
    Mermaid,
    Web,
}

#[derive(Debug, Clone)]
pub struct GraphRunArgs {
    pub repo_root: PathBuf,
    pub format: GraphFormat,
    pub view: GraphView,
    pub out: Option<PathBuf>,
    pub focus: Option<String>,
    pub include_risks: bool,
    pub include_candidates: bool,
    pub max_nodes: Option<usize>,
    pub pretty: bool,
    /// `--include-noise` — surface framework noise (toString / dispose
    /// / initState / build / …) instead of hiding it by default.
    pub include_noise: bool,
}

pub fn run(args: GraphRunArgs) -> Result<()> {
    // Without explicit selectors web exposes every stored node. Selectors use
    // the same curated pipeline as JSON/HTML rather than being silently ignored.
    if args.format == GraphFormat::Web && args.view == GraphView::Overview && args.focus.is_none() && args.max_nodes.is_none() && args.include_candidates && args.include_risks {
        return emit_web(&args.repo_root, args.out.as_deref());
    }

    // Curated selectors define the exported scope; the workspace separately
    // caps the currently drawn neighborhood and reports its omissions.
    let options = GraphOptions {
        view: args.view,
        focus: args.focus.clone(),
        include_risks: args.include_risks,
        include_candidates: args.include_candidates,
        max_nodes: args.max_nodes,
        include_noise: args.include_noise,
    };
    let view = build_graph_view(&args.repo_root, options)
        .with_context(|| format!("building graph view at {}", args.repo_root.display()))?;

    // A typo'd `--focus` otherwise produces a silent empty export with a
    // `wrote …` success line — indistinguishable from an empty repo.
    if let Some(w) = focus_miss_warning(args.focus.as_deref(), view.nodes.len()) {
        tracing::warn!("{w}");
    }

    match args.format {
        GraphFormat::Json => emit_json(&view, args.out.as_deref(), args.pretty)?,
        GraphFormat::Mermaid => emit_mermaid(&view, args.out.as_deref())?,
        GraphFormat::Html => emit_html(&view, &args.repo_root, args.out.as_deref())?,
        GraphFormat::Web => {
            let target = args.out.clone().unwrap_or_else(|| args.repo_root.join(DEFAULT_WEB_OUT));
            emit_html(&view, &args.repo_root, Some(&target))?;
        }
    }
    Ok(())
}

/// Warn when `--focus <id>` yielded an empty view: the id almost certainly
/// didn't match any business id / module path / artifact id. Returns `None`
/// (stay silent) when no focus was requested — an empty overview is a
/// legitimate result for a freshly-initialised repo.
fn focus_miss_warning(focus: Option<&str>, node_count: usize) -> Option<String> {
    match focus {
        Some(f) if node_count == 0 => Some(format!(
            "warning: --focus `{f}` matched no nodes — check the id (expected a REQ-… business id, a module path, or a full artifact id)"
        )),
        _ => None,
    }
}

fn emit_json(view: &GraphViewModel, out: Option<&Path>, pretty: bool) -> Result<()> {
    let json = if pretty {
        serde_json::to_string_pretty(view)
    } else {
        serde_json::to_string(view)
    }
    .context("serialising graph view to JSON")?;
    match out {
        Some(path) => {
            super::output::write_atomic(path, &json)?;
            eprintln!("wrote {}", path.display());
            Ok(())
        }
        None => {
            println!("{json}");
            Ok(())
        }
    }
}

fn emit_mermaid(view: &GraphViewModel, out: Option<&Path>) -> Result<()> {
    let body = render_mermaid(view);
    match out {
        Some(path) => {
            super::output::write_atomic(path, &body)?;
            eprintln!("wrote {}", path.display());
            Ok(())
        }
        None => {
            println!("{body}");
            Ok(())
        }
    }
}

fn emit_html(view: &GraphViewModel, repo_root: &Path, out: Option<&Path>) -> Result<()> {
    let target = match out {
        Some(p) => p.to_path_buf(),
        None => repo_root.join(DEFAULT_HTML_OUT),
    };
    let body = render_html(view)?;
    super::output::write_atomic(&target, &body)?;
    eprintln!("wrote {}", target.display());
    Ok(())
}

fn emit_web(repo_root: &Path, out: Option<&Path>) -> Result<()> {
    let net = build_network_graph(NetworkOptions {
        repo_root: repo_root.to_path_buf(),
        keep_isolated: true,
    })
    .with_context(|| format!("building network graph at {}", repo_root.display()))?;
    let json = serde_json::to_string(&net).context("serialising network graph")?;
    let html = render_web_html(&json)?;
    let target = match out {
        Some(p) => p.to_path_buf(),
        None => repo_root.join(DEFAULT_WEB_OUT),
    };
    super::output::write_atomic(&target, &html)?;
    eprintln!(
        "wrote {} ({} nodes, {} links)",
        target.display(),
        net.meta.nodes,
        net.meta.links
    );
    Ok(())
}

/// All HTML formats use the same offline workspace; asset drift fails explicitly.
pub(super) fn render_web_html(data_json: &str) -> Result<String> {
    let _: serde_json::Value = serde_json::from_str(data_json).context("invalid viewer data")?;
    let safe = data_json.replace('<', "\\u003c");
    let data = format!("<script id=\"groundgraph-data\" type=\"application/json\">{safe}</script>");
    let mut html = replace_asset(VIEWER_TEMPLATE, "<!-- SS_DATA_SLOT -->", &data)?;
    html = replace_asset(&html, "<link rel=\"stylesheet\" href=\"./workspace.css\" />", &format!("<style>{VIEWER_STYLE}</style>"))?;
    for (tag, script) in [("<script src=\"./model.js\"></script>", VIEWER_MODEL), ("<script src=\"./workspace.js\"></script>", VIEWER_SCRIPT)] {
        let safe_script = script.replace("</script", "<\\/script");
        html = replace_asset(&html, tag, &format!("<script>{safe_script}</script>"))?;
    }
    Ok(html)
}

fn replace_asset(template: &str, marker: &str, value: &str) -> Result<String> {
    anyhow::ensure!(template.matches(marker).count() == 1, "viewer template must contain exactly one {marker}");
    Ok(template.replacen(marker, value, 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn focus_miss_warns_only_when_focus_set_and_view_empty() {
        assert!(focus_miss_warning(Some("missing"), 0).unwrap().contains("missing"));
        assert!(focus_miss_warning(Some("exists"), 5).is_none());
        assert!(focus_miss_warning(None, 0).is_none());
    }
    #[test]
    fn viewer_is_self_contained_and_uses_inert_escaped_data() {
        let html = render_web_html(r#"{"nodes":[{"id":"中文</script><!--<script>"}],"links":[]}"#).unwrap();
        assert!(html.contains(r#"中文\u003c/script>\u003c!--\u003cscript>"#));
        assert!(html.contains("application/json"));
        assert!(html.contains("GroundGraphModel"));
        assert!(html.contains("id=\"migration\""));
        assert!(!html.contains("<script src="));
        assert!(!html.contains("<link rel=\"stylesheet\""));
        assert!(!html.contains("unsafe-eval"));
        assert!(!html.contains("SS_DATA_SLOT"));
    }
    #[test]
    fn template_drift_and_bad_json_are_errors() {
        assert!(replace_asset("no marker", "MARKER", "value").is_err());
        assert!(replace_asset("MARKER MARKER", "MARKER", "value").is_err());
        assert!(render_web_html("not JSON").is_err());
    }
}
