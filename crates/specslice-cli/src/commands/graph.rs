//! CLI plumbing for `specslice graph`.
//!
//! Format dispatch:
//!
//! - `json`    — print or write the [`GraphViewModel`].
//! - `mermaid` — emit a Mermaid `flowchart LR` for docs/PR embeds.
//! - `html`    — write a fully self-contained HTML file under
//!   `.specslice/export/graph.html` by default. No CDN, no network, no
//!   bundler.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use specslice_engine::graph::{build_graph_view, GraphOptions, GraphView, GraphViewModel};

use super::graph_html::render_html;
use super::graph_mermaid::render_mermaid;

const DEFAULT_HTML_OUT: &str = ".specslice/export/graph.html";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFormat {
    Json,
    Html,
    Mermaid,
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
    // HTML embeds the full graph; the renderer enforces an 80-visible-node
    // cap on top of `default_visible` so users see manageable starts even on
    // giant repos. Engine-level `max_nodes` remains an explicit opt-in cap.
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

    match args.format {
        GraphFormat::Json => emit_json(&view, args.out.as_deref(), args.pretty)?,
        GraphFormat::Mermaid => emit_mermaid(&view, args.out.as_deref())?,
        GraphFormat::Html => emit_html(&view, &args.repo_root, args.out.as_deref())?,
    }
    Ok(())
}

fn emit_json(view: &GraphViewModel, out: Option<&Path>, pretty: bool) -> Result<()> {
    let json = if pretty {
        serde_json::to_string_pretty(view)
    } else {
        serde_json::to_string(view)
    }
    .context("serialising graph view to JSON")?;
    match out {
        Some(path) => write_to(path, &json),
        None => {
            println!("{json}");
            Ok(())
        }
    }
}

fn emit_mermaid(view: &GraphViewModel, out: Option<&Path>) -> Result<()> {
    let body = render_mermaid(view);
    match out {
        Some(path) => write_to(path, &body),
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
    let body = render_html(view);
    write_to(&target, &body)?;
    println!("wrote {}", target.display());
    Ok(())
}

fn write_to(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent of {}", path.display()))?;
        }
    }
    std::fs::write(path, body)
        .with_context(|| format!("writing graph output to {}", path.display()))?;
    Ok(())
}
