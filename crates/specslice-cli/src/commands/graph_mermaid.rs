//! Mermaid serializer for `specslice graph --format mermaid`.
//!
//! Output is a `flowchart LR` diagram with stable `n0`, `n1`, … aliases so
//! that ASCII-only artifact IDs do not leak into rendered diagrams.

use std::collections::BTreeMap;

use specslice_engine::graph::{GraphLayer, GraphViewModel};

pub fn render_mermaid(view: &GraphViewModel) -> String {
    let mut out = String::from("flowchart LR\n");

    let mut alias_for: BTreeMap<&str, String> = BTreeMap::new();
    for (idx, node) in view.nodes.iter().enumerate() {
        let alias = format!("n{idx}");
        let shape = node_shape(node.layer);
        let label = escape_label(&format_label(&node.label, node.path.as_deref()));
        let line = format!("  {alias}{}\"{label}\"{}\n", shape.0, shape.1);
        out.push_str(&line);
        alias_for.insert(node.id.as_str(), alias);
    }

    for edge in &view.edges {
        let (Some(from), Some(to)) = (
            alias_for.get(edge.from.as_str()),
            alias_for.get(edge.to.as_str()),
        ) else {
            continue;
        };
        let label = escape_label(&edge.kind);
        let arrow = match edge.layer {
            GraphLayer::Confirmed => "-->",
            GraphLayer::Candidate => "-.->",
            _ => "---",
        };
        out.push_str(&format!("  {from} {arrow}|{label}| {to}\n"));
    }

    if view.findings.iter().any(|f| f.code == "graph_truncated") {
        out.push_str("  %% graph truncated by --max-nodes\n");
    }
    if view.findings.iter().any(|f| f.code == "focus_not_found") {
        out.push_str("  %% focus id not found\n");
    }
    out
}

fn node_shape(layer: GraphLayer) -> (&'static str, &'static str) {
    match layer {
        GraphLayer::Confirmed => ("(", ")"),
        GraphLayer::Candidate => ("[/", "/]"),
        GraphLayer::Risk => ("{{", "}}"),
        GraphLayer::Fact => ("[", "]"),
    }
}

fn format_label(label: &str, path: Option<&str>) -> String {
    match path {
        Some(p) if !p.is_empty() => format!("{label} ({p})"),
        _ => label.to_string(),
    }
}

fn escape_label(text: &str) -> String {
    text.replace('"', "\\\"").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_engine::graph::{
        GraphEdge, GraphLayer, GraphNode, GraphStats, GraphStatus, GraphViewModel,
    };

    fn view() -> GraphViewModel {
        GraphViewModel {
            schema_version: 2,
            view: "overview".into(),
            repo_root: "/tmp".into(),
            generated_at: "now".into(),
            focus: None,
            stats: GraphStats::default(),
            nodes: vec![
                GraphNode {
                    id: "docsec::a#x".into(),
                    kind: "doc_section".into(),
                    column: specslice_engine::graph::GraphColumn::Documents,
                    layer: GraphLayer::Fact,
                    label: r#"Quote " and newline\n"#.into(),
                    path: Some("docs/a.md".into()),
                    line_range: None,
                    status: GraphStatus::Confirmed,
                    parent_id: None,
                    child_count: 0,
                    default_visible: true,
                    confidence: None,
                    source: None,
                    badges: vec![],
                },
                GraphNode {
                    id: "req::REQ-1".into(),
                    kind: "requirement".into(),
                    column: specslice_engine::graph::GraphColumn::Business,
                    layer: GraphLayer::Confirmed,
                    label: "REQ-1".into(),
                    path: None,
                    line_range: None,
                    status: GraphStatus::Confirmed,
                    parent_id: None,
                    child_count: 0,
                    default_visible: true,
                    confidence: None,
                    source: None,
                    badges: vec![],
                },
            ],
            edges: vec![GraphEdge {
                id: "edge1".into(),
                from: "docsec::a#x".into(),
                to: "req::REQ-1".into(),
                kind: "documents".into(),
                layer: GraphLayer::Confirmed,
                status: GraphStatus::Confirmed,
                confidence: None,
                source: None,
                rationale: None,
            }],
            findings: vec![],
        }
    }

    #[test]
    fn renders_flowchart_with_aliases_and_layer_arrows() {
        let out = render_mermaid(&view());
        assert!(out.starts_with("flowchart LR\n"));
        assert!(out.contains("n0["));
        assert!(out.contains("n1("));
        assert!(out.contains("n0 -->|documents| n1"));
        assert!(
            out.contains("\\\""),
            "label quotes should be escaped: {out}"
        );
        assert!(!out.contains("docsec::"), "raw ids leaked: {out}");
    }

    #[test]
    fn truncation_note_appears_when_finding_present() {
        let mut v = view();
        v.findings.push(specslice_engine::graph::GraphFinding {
            code: "graph_truncated".into(),
            severity: "warning".into(),
            message: "trunc".into(),
            target_id: None,
        });
        let out = render_mermaid(&v);
        assert!(out.contains("graph truncated"));
    }
}
