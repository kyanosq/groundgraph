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
use specslice_engine::network::{build_network_graph, NetworkOptions};

use super::graph_html::render_html;
use super::graph_mermaid::render_mermaid;

const DEFAULT_HTML_OUT: &str = ".specslice/export/graph.html";
const DEFAULT_WEB_OUT: &str = ".specslice/export/graph-web.html";

/// The `webui` viewer, the single source of truth for both the standalone dev
/// page and the embedded CLI export. The `web` format inlines the graph into a
/// copy of this template via the `SS_DATA_SLOT` marker.
const VIEWER_TEMPLATE: &str = include_str!("../../../../webui/index.html");

/// The offline viewer bundle (three + 3d-force-graph + UnrealBloomPass as one
/// classic IIFE). The dev page loads it via `<script src>`; the `web` export
/// inlines it so the result is a single portable file with no network at all.
const VIEWER_BUNDLE: &str = include_str!("../../../../webui/vendor/specslice-viewer.bundle.js");

/// The dev page's `<script src>` for the bundle; the export replaces it with the
/// inlined bundle so a single file works straight from `file://`.
const VENDOR_TAG: &str = "<script src=\"./vendor/specslice-viewer.bundle.js\"></script>";

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
    // `web` renders the *full* raw topology (the viewer degrades it at render
    // time), so it bypasses the curated/capped business view entirely.
    if args.format == GraphFormat::Web {
        return emit_web(&args.repo_root, args.out.as_deref());
    }

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
        GraphFormat::Web => unreachable!("web handled above"),
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
        Some(path) => {
            write_to(path, &json)?;
            println!("wrote {}", path.display());
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
            write_to(path, &body)?;
            println!("wrote {}", path.display());
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
    let body = render_html(view);
    write_to(&target, &body)?;
    println!("wrote {}", target.display());
    Ok(())
}

fn emit_web(repo_root: &Path, out: Option<&Path>) -> Result<()> {
    let net = build_network_graph(NetworkOptions {
        repo_root: repo_root.to_path_buf(),
        keep_isolated: false,
    })
    .with_context(|| format!("building network graph at {}", repo_root.display()))?;
    let json = serde_json::to_string(&net).context("serialising network graph")?;
    let html = render_web_html(&json);
    let target = match out {
        Some(p) => p.to_path_buf(),
        None => repo_root.join(DEFAULT_WEB_OUT),
    };
    write_to(&target, &html)?;
    println!(
        "wrote {} ({} nodes, {} links)",
        target.display(),
        net.meta.nodes,
        net.meta.links
    );
    Ok(())
}

/// Inline `data_json` into a copy of the viewer template at the `SS_DATA_SLOT`
/// marker as `window.__SS_DATA__`, so the result is a single self-contained
/// file that boots with no fetch (works from `file://`).
fn render_web_html(data_json: &str) -> String {
    // Neutralise any `</script>` (or other `</…`) hiding inside a string value
    // so the embedded data can never close the host <script> tag early. `\/`
    // is a valid JSON escape for `/`, so the payload still parses.
    let safe = data_json.replace("</", "<\\/");
    let data_script = format!("<script>window.__SS_DATA__ = {safe};</script>");
    let with_data = inline_at_data_slot(VIEWER_TEMPLATE, &data_script);
    inline_vendor_bundle(&with_data)
}

/// Replace the `SS_DATA_SLOT` marker line with the inlined data `<script>`.
fn inline_at_data_slot(template: &str, data_script: &str) -> String {
    if let Some(start) = template.find("<!-- SS_DATA_SLOT") {
        if let Some(rel_end) = template[start..].find("-->") {
            let end = start + rel_end + "-->".len();
            let mut s = String::with_capacity(template.len() + data_script.len());
            s.push_str(&template[..start]);
            s.push_str(data_script);
            s.push_str(&template[end..]);
            return s;
        }
    }
    // Marker missing (template drift): inject just before the vendor bundle so it
    // still runs before the viewer reads `window.__SS_DATA__`.
    template.replacen(VENDOR_TAG, &format!("{data_script}\n{VENDOR_TAG}"), 1)
}

/// Swap the dev page's `<script src=…bundle…>` for the inlined bundle so the
/// export is a single file (the `./vendor/…` path would 404 from `/tmp`).
fn inline_vendor_bundle(html: &str) -> String {
    // Targeted neutralisation: `</script` can only appear inside a JS string or
    // regex here, where `<\/script` is identical, so this never changes behaviour
    // while guaranteeing the inlined bundle cannot close the host tag early.
    let safe_bundle = VIEWER_BUNDLE.replace("</script", "<\\/script");
    html.replacen(VENDOR_TAG, &format!("<script>{safe_bundle}</script>"), 1)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_template_has_the_data_slot_marker() {
        // The CLI export depends on this marker living in webui/index.html.
        assert!(
            VIEWER_TEMPLATE.contains("<!-- SS_DATA_SLOT"),
            "webui/index.html must keep the SS_DATA_SLOT marker for `graph --format web`"
        );
        assert!(VIEWER_TEMPLATE.contains("window.__SS_DATA__"));
    }

    #[test]
    fn viewer_template_references_the_vendor_bundle() {
        // The export swaps this exact tag for the inlined bundle; if the dev page
        // ever renames the bundle the export would silently keep a dead CDN-less
        // <script src> and break offline. Pin both ends.
        assert!(
            VIEWER_TEMPLATE.contains(VENDOR_TAG),
            "webui/index.html must load the vendor bundle via the exact VENDOR_TAG"
        );
        assert!(
            VIEWER_BUNDLE.contains("globalThis.THREE"),
            "bundle must expose THREE as a global for the classic viewer script"
        );
    }

    #[test]
    fn render_web_html_inlines_data_and_keeps_viewer() {
        let json =
            r#"{"meta":{"repo":"demo","nodes":1,"links":0},"nodes":[{"id":"a"}],"links":[]}"#;
        let html = render_web_html(json);
        assert!(
            html.contains("window.__SS_DATA__ = {\"meta\":{\"repo\":\"demo\""),
            "data must be inlined as the global the viewer reads"
        );
        // Viewer code survives and the placeholder marker is consumed.
        assert!(
            html.contains("ForceGraph3D({ controlType: 'orbit' })"),
            "viewer code preserved"
        );
        assert!(
            !html.contains("SS_DATA_SLOT"),
            "marker replaced, not left behind"
        );
    }

    #[test]
    fn render_web_html_inlines_the_vendor_bundle_for_a_single_file() {
        let html = render_web_html(r#"{"nodes":[],"links":[]}"#);
        assert!(
            !html.contains(VENDOR_TAG),
            "the ./vendor/ <script src> must be replaced (it 404s from a /tmp export)"
        );
        assert!(
            html.contains("globalThis.THREE"),
            "the bundle itself must be inlined so the file is offline-portable"
        );
    }

    #[test]
    fn render_web_html_neutralises_script_close_in_data() {
        // A node name containing `</script>` must not be able to close the host
        // tag; it is escaped to the JSON-valid `<\/script>`.
        let json = r#"{"nodes":[{"id":"x","name":"</script><b>"}]}"#;
        let html = render_web_html(json);
        assert!(
            html.contains("<\\/script><b>"),
            "`</` escaped inside payload"
        );
        assert!(
            !html.contains("\"name\":\"</script>"),
            "raw </script> from data must not survive"
        );
    }
}
