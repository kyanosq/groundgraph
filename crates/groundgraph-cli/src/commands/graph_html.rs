//! Curated graph views and raw networks share one evidence workspace.
use anyhow::Result;
use groundgraph_engine::graph::GraphViewModel;

pub fn render_html(view: &GraphViewModel) -> Result<String> {
    super::graph::render_web_html(&serde_json::to_string(view)?)
}
