//! Mermaid serializer for `specslice graph --format mermaid` and the
//! P14 local-subgraph exports (`search`, `impact`, `candidate show`).
//!
//! Output is a `flowchart LR` diagram with stable `n0`, `n1`, … aliases
//! so that ASCII-only artifact IDs do not leak into rendered diagrams.
//! The core renderer ([`render_parts`]) accepts lighter `MermaidNode` /
//! `MermaidEdge` tuples so each command can map its own report shape
//! onto Mermaid without first building a full [`GraphViewModel`].

use std::collections::BTreeMap;

use specslice_engine::graph::{GraphLayer, GraphViewModel};

/// Compact node representation shared by `graph`, `search`, `impact`
/// and `candidate show` Mermaid emitters. Keep this struct small —
/// adding fields means each emitter must thread them through.
#[derive(Debug, Clone)]
pub struct MermaidNode {
    pub id: String,
    pub label: String,
    pub layer: GraphLayer,
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MermaidEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub layer: GraphLayer,
}

/// Render a minimal `flowchart LR` diagram from light node/edge
/// tuples, optionally followed by comment lines (used for truncation
/// notes etc.). Dangling edges — edges that reference an id not
/// present in `nodes` — are silently dropped so callers don't have to
/// pre-filter.
pub fn render_parts(nodes: &[MermaidNode], edges: &[MermaidEdge], notes: &[String]) -> String {
    let mut out = String::from("flowchart LR\n");
    if nodes.is_empty() {
        out.push_str("  %% empty subgraph\n");
        for note in notes {
            out.push_str(&format!("  %% {}\n", escape_comment(note)));
        }
        return out;
    }

    let mut alias_for: BTreeMap<&str, String> = BTreeMap::new();
    for (idx, node) in nodes.iter().enumerate() {
        let alias = format!("n{idx}");
        let shape = node_shape(node.layer);
        let label = escape_label(&format_label(&node.label, node.path.as_deref()));
        out.push_str(&format!("  {alias}{}\"{label}\"{}\n", shape.0, shape.1));
        alias_for.insert(node.id.as_str(), alias);
    }

    for edge in edges {
        let (Some(from), Some(to)) = (
            alias_for.get(edge.from.as_str()),
            alias_for.get(edge.to.as_str()),
        ) else {
            continue;
        };
        let label = escape_label(&edge.kind);
        let arrow = arrow_for(edge.layer);
        out.push_str(&format!("  {from} {arrow}|{label}| {to}\n"));
    }

    for note in notes {
        out.push_str(&format!("  %% {}\n", escape_comment(note)));
    }
    out
}

pub fn render_mermaid(view: &GraphViewModel) -> String {
    // Honour the view's `default_visible` surface. `--view overview/business/code`
    // only *toggle* per-node visibility — they do not prune `view.nodes` — so a
    // raw dump would emit the entire graph (a real dogfood bug: `--view business`
    // produced an 8k-line diagram with zero requirements in view). Mermaid is for
    // docs/PR embeds, so render only the visible surface and the edges between
    // visible nodes. `focus` already narrowed `nodes` and marks them all visible.
    let visible: std::collections::HashSet<&str> = view
        .nodes
        .iter()
        .filter(|n| n.default_visible)
        .map(|n| n.id.as_str())
        .collect();
    let nodes: Vec<MermaidNode> = view
        .nodes
        .iter()
        .filter(|n| n.default_visible)
        .map(|n| MermaidNode {
            id: n.id.clone(),
            label: n.label.clone(),
            layer: n.layer,
            path: n.path.clone(),
        })
        .collect();
    let edges: Vec<MermaidEdge> = view
        .edges
        .iter()
        .filter(|e| visible.contains(e.from.as_str()) && visible.contains(e.to.as_str()))
        .map(|e| MermaidEdge {
            from: e.from.clone(),
            to: e.to.clone(),
            kind: e.kind.clone(),
            layer: e.layer,
        })
        .collect();
    let mut notes = Vec::new();
    if view.findings.iter().any(|f| f.code == "graph_truncated") {
        notes.push("graph truncated by --max-nodes".to_string());
    }
    if view.findings.iter().any(|f| f.code == "focus_not_found") {
        notes.push("focus id not found".to_string());
    }
    render_parts(&nodes, &edges, &notes)
}

fn node_shape(layer: GraphLayer) -> (&'static str, &'static str) {
    match layer {
        GraphLayer::Confirmed => ("(", ")"),
        GraphLayer::Candidate => ("[/", "/]"),
        GraphLayer::Risk => ("{{", "}}"),
        GraphLayer::Fact => ("[", "]"),
    }
}

fn arrow_for(layer: GraphLayer) -> &'static str {
    match layer {
        GraphLayer::Confirmed => "-->",
        GraphLayer::Candidate => "-.->",
        _ => "---",
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

fn escape_comment(text: &str) -> String {
    text.replace('\n', " ")
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
                source_file: None,
                line_range: None,
                snippet: None,
                resolver: None,
                evidence_quality: None,
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
    fn render_mermaid_only_emits_default_visible_nodes_and_their_edges() {
        // Simulates `--view overview/business`: the engine keeps every node in
        // the model but toggles `default_visible`. Mermaid must render only the
        // visible surface, dropping hidden nodes and edges touching them.
        let mut v = view();
        v.nodes[0].default_visible = false; // hide the doc_section
        v.nodes[1].default_visible = true; // keep the requirement
        let out = render_mermaid(&v);
        assert!(
            !out.contains("Quote"),
            "hidden doc_section must not render: {out}"
        );
        assert!(
            out.contains("REQ-1"),
            "visible requirement should render: {out}"
        );
        assert!(
            !out.contains("|documents|"),
            "edge touching a hidden node must be dropped: {out}"
        );
        // Exactly one node alias should exist.
        assert!(out.contains("n0"), "{out}");
        assert!(!out.contains("n1"), "only one visible node expected: {out}");
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

    #[test]
    fn render_parts_renders_minimal_diagram_and_drops_dangling_edges() {
        let nodes = vec![
            MermaidNode {
                id: "a".into(),
                label: "Alpha".into(),
                layer: GraphLayer::Fact,
                path: Some("src/a.rs".into()),
            },
            MermaidNode {
                id: "b".into(),
                label: "Beta".into(),
                layer: GraphLayer::Confirmed,
                path: None,
            },
        ];
        let edges = vec![
            MermaidEdge {
                from: "a".into(),
                to: "b".into(),
                kind: "calls".into(),
                layer: GraphLayer::Fact,
            },
            // Dangling — `c` is not in `nodes`, must be ignored.
            MermaidEdge {
                from: "a".into(),
                to: "c".into(),
                kind: "calls".into(),
                layer: GraphLayer::Fact,
            },
        ];
        let out = render_parts(&nodes, &edges, &["truncated".into()]);
        assert!(out.starts_with("flowchart LR\n"));
        assert!(out.contains("n0[\"Alpha (src/a.rs)\"]"));
        assert!(out.contains("n1(\"Beta\")"));
        assert!(out.contains("n0 ---|calls| n1"));
        assert!(out.contains("%% truncated"));
        // Only one edge survived.
        assert_eq!(out.matches("---|calls|").count(), 1);
    }

    #[test]
    fn render_parts_emits_empty_subgraph_comment_when_no_nodes() {
        let out = render_parts(&[], &[], &["no nodes".into()]);
        assert!(out.contains("%% empty subgraph"));
        assert!(out.contains("%% no nodes"));
    }
}
