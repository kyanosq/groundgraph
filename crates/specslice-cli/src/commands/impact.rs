use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use specslice_engine::graph::GraphLayer;
use specslice_engine::slice::SliceItem;
use specslice_engine::{run_impact, ImpactOptions, ImpactReport};

use crate::commands::graph_mermaid::{render_parts, MermaidEdge, MermaidNode};

/// P14 — output format for `specslice impact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactFormat {
    Text,
    Json,
    Mermaid,
}

pub fn run(
    repo_root: &Path,
    base: &str,
    head: &str,
    format: ImpactFormat,
    output: Option<PathBuf>,
) -> Result<()> {
    let report = run_impact(ImpactOptions {
        repo_root: repo_root.to_path_buf(),
        base_ref: base.to_string(),
        head_ref: head.to_string(),
        reindex: true,
    })?;
    match format {
        ImpactFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ImpactFormat::Text => print_human(&report),
        ImpactFormat::Mermaid => {
            let mermaid = render_impact_mermaid(&report);
            match output {
                Some(path) => {
                    super::output::write_atomic(&path, &mermaid)
                        .with_context(|| format!("writing impact mermaid to {}", path.display()))?;
                    println!("已写入: {}", path.display());
                }
                None => print!("{mermaid}"),
            }
        }
    }
    Ok(())
}

/// Build an impact-focused Mermaid graph:
///
/// * `changed_files` → root rectangles (synthetic `file::{path}` ids)
/// * `changed_symbols` / `linked_implementations` / `propagated_symbols` → Fact
/// * `affected_requirements` → Confirmed (rounded)
/// * `affected_confirmed_candidates` → Candidate (parallelogram)
/// * `linked_tests` → Confirmed (tests are the "should-run" answer)
///
/// P15 — edges come from `report.impact_edges`, the *real* set of
/// graph edges traversed while computing the report. The previous
/// implementation synthesised cross-product approximations
/// (changed_symbols × requirements, all propagated_symbols hung onto
/// a single changed_symbol) which silently lied about provenance.
///
/// The only exceptions are `affected_confirmed_candidates`: those
/// arrive from the candidate manifest YAML, not from the store, so
/// we draw a single `evidence` edge per candidate from the first
/// available anchor (changed symbol, then changed file). That edge
/// is labelled `evidence` rather than a graph kind so the reader
/// understands it is a manifest link, not a store edge.
pub fn render_impact_mermaid(report: &ImpactReport) -> String {
    let mut nodes: Vec<MermaidNode> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let push_node = |nodes: &mut Vec<MermaidNode>,
                     seen: &mut BTreeSet<String>,
                     id: &str,
                     label: String,
                     layer: GraphLayer,
                     path: Option<String>| {
        if seen.insert(id.to_string()) {
            nodes.push(MermaidNode {
                id: id.to_string(),
                label,
                layer,
                path,
            });
        }
    };

    // Anchor: changed files as Fact rectangles. Synthetic id prefix
    // so they never collide with artifact ids in the graph.
    let changed_file_set: BTreeSet<&str> =
        report.changed_files.iter().map(String::as_str).collect();
    for file in &report.changed_files {
        push_node(
            &mut nodes,
            &mut seen,
            &format!("file::{file}"),
            file.clone(),
            GraphLayer::Fact,
            None,
        );
    }

    for item in &report.changed_symbols {
        push_node(
            &mut nodes,
            &mut seen,
            &item.id,
            label_for(item),
            GraphLayer::Fact,
            item.path.clone(),
        );
    }
    for item in &report.affected_requirements {
        push_node(
            &mut nodes,
            &mut seen,
            &item.id,
            label_for(item),
            GraphLayer::Confirmed,
            item.path.clone(),
        );
    }
    for item in &report.linked_implementations {
        push_node(
            &mut nodes,
            &mut seen,
            &item.id,
            label_for(item),
            GraphLayer::Fact,
            item.path.clone(),
        );
    }
    for item in &report.linked_tests {
        push_node(
            &mut nodes,
            &mut seen,
            &item.id,
            label_for(item),
            GraphLayer::Confirmed,
            item.path.clone(),
        );
    }
    for item in &report.affected_confirmed_candidates {
        push_node(
            &mut nodes,
            &mut seen,
            &item.id,
            label_for(item),
            GraphLayer::Candidate,
            item.path.clone(),
        );
    }
    for item in &report.propagated_symbols {
        push_node(
            &mut nodes,
            &mut seen,
            &item.id,
            label_for(item),
            GraphLayer::Fact,
            item.path.clone(),
        );
    }
    for item in &report.changed_doc_sections {
        push_node(
            &mut nodes,
            &mut seen,
            &item.id,
            label_for(item),
            GraphLayer::Fact,
            item.path.clone(),
        );
    }
    for item in &report.affected_docs {
        push_node(
            &mut nodes,
            &mut seen,
            &item.id,
            label_for(item),
            GraphLayer::Fact,
            item.path.clone(),
        );
    }

    // Real edges from impact_edges. We translate `from`/`to` to the
    // diagram's id space:
    //   * if it matches a changed-file path, use `file::{path}`;
    //   * otherwise keep the artifact id as-is.
    let mut edges: Vec<MermaidEdge> = Vec::new();
    let mut edge_seen: BTreeSet<(String, String, String)> = BTreeSet::new();
    for edge in &report.impact_edges {
        let from_id = if changed_file_set.contains(edge.from.as_str()) {
            format!("file::{}", edge.from)
        } else {
            edge.from.clone()
        };
        let to_id = if changed_file_set.contains(edge.to.as_str()) {
            format!("file::{}", edge.to)
        } else {
            edge.to.clone()
        };
        let key = (from_id.clone(), to_id.clone(), edge.kind.clone());
        if !edge_seen.insert(key) {
            continue;
        }
        let layer = match edge.kind.as_str() {
            "declares_implementation" | "declares_verification" | "documents" => {
                GraphLayer::Confirmed
            }
            _ => GraphLayer::Fact,
        };
        edges.push(MermaidEdge {
            from: from_id,
            to: to_id,
            kind: edge.kind.clone(),
            layer,
        });
    }

    // Confirmed-candidate edges live in the manifest YAML, not the
    // store. Surface a single synthetic `evidence` edge per candidate
    // from the most specific available anchor so the reader can spot
    // which candidates are touched.
    for cand in &report.affected_confirmed_candidates {
        if let Some(anchor) = sym_anchor(&report.changed_symbols) {
            let key = (anchor.clone(), cand.id.clone(), "evidence".to_string());
            if edge_seen.insert(key) {
                edges.push(MermaidEdge {
                    from: anchor,
                    to: cand.id.clone(),
                    kind: "evidence".into(),
                    layer: GraphLayer::Candidate,
                });
            }
        } else if let Some(first_file) = report.changed_files.first() {
            let from = format!("file::{first_file}");
            let key = (from.clone(), cand.id.clone(), "evidence".to_string());
            if edge_seen.insert(key) {
                edges.push(MermaidEdge {
                    from,
                    to: cand.id.clone(),
                    kind: "evidence".into(),
                    layer: GraphLayer::Candidate,
                });
            }
        }
    }

    let notes = vec![format!(
        "specslice impact — changed_files={} changed_symbols={} affected_requirements={} \
         linked_tests={} candidates={} propagated_symbols={} impact_edges={}",
        report.changed_files.len(),
        report.changed_symbols.len(),
        report.affected_requirements.len(),
        report.linked_tests.len(),
        report.affected_confirmed_candidates.len(),
        report.propagated_symbols.len(),
        report.impact_edges.len(),
    )];
    render_parts(&nodes, &edges, &notes)
}

fn label_for(item: &SliceItem) -> String {
    item.name
        .clone()
        .unwrap_or_else(|| item.id.rsplit("::").next().unwrap_or(&item.id).to_string())
}

/// Pick the first changed symbol id as the "fan-out anchor" so the
/// Mermaid graph remains a tree (not a forest of disconnected edges)
/// even when there are many changed symbols. Falls back to `None` if
/// no changed symbol exists.
fn sym_anchor(items: &[SliceItem]) -> Option<String> {
    items.first().map(|i| i.id.clone())
}

fn print_human(report: &ImpactReport) {
    println!("SpecSlice Impact Report");
    println!();
    println!("Changed files:");
    if report.changed_files.is_empty() {
        println!("- (none)");
    } else {
        for f in &report.changed_files {
            println!("- {f}");
        }
    }
    println!();
    println!("Changed symbols:");
    if report.changed_symbols.is_empty() {
        println!("- (none)");
    } else {
        for s in &report.changed_symbols {
            println!(
                "- {} ({})",
                s.name.clone().unwrap_or_else(|| s.id.clone()),
                s.path.clone().unwrap_or_else(|| s.id.clone())
            );
        }
    }
    if !report.changed_doc_sections.is_empty() {
        println!();
        println!("Changed doc sections:");
        for d in &report.changed_doc_sections {
            println!(
                "- {} ({})",
                d.name.clone().unwrap_or_else(|| d.id.clone()),
                d.path.clone().unwrap_or_default()
            );
        }
    }
    println!();
    println!("Affected requirements:");
    if report.affected_requirements.is_empty() {
        println!("- (none)");
    } else {
        for r in &report.affected_requirements {
            println!("- {} {}", r.id, r.name.clone().unwrap_or_default());
        }
    }
    if !report.linked_implementations.is_empty() {
        println!();
        println!("Linked implementation:");
        for i in &report.linked_implementations {
            println!(
                "- {} ({})",
                i.name.clone().unwrap_or_else(|| i.id.clone()),
                i.path.clone().unwrap_or_else(|| i.id.clone())
            );
        }
    }
    if !report.linked_tests.is_empty() {
        println!();
        println!("Linked tests:");
        for t in &report.linked_tests {
            println!("- {}", t.path.clone().unwrap_or_else(|| t.id.clone()));
        }
    }
    if !report.affected_confirmed_candidates.is_empty() {
        println!();
        println!("受影响的已确认业务候选 (需重新审阅):");
        for c in &report.affected_confirmed_candidates {
            println!("- {} {}", c.id, c.name.clone().unwrap_or_default(),);
        }
    }
    if !report.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for w in &report.warnings {
            println!("- {w}");
        }
    }
    if !report.info.is_empty() {
        println!();
        println!("Info:");
        for i in &report.info {
            println!("- {i}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, kind: &str, name: &str, path: Option<&str>) -> SliceItem {
        SliceItem {
            id: id.into(),
            kind: kind.into(),
            path: path.map(String::from),
            name: Some(name.into()),
            line_range: None,
        }
    }

    fn sample_report() -> ImpactReport {
        use specslice_engine::impact::ImpactEdge;
        ImpactReport {
            changed_files: vec!["lib/foo.dart".into()],
            changed_symbols: vec![item(
                "dart_method::lib/foo.dart#Foo.bar",
                "dart_method",
                "Foo.bar",
                Some("lib/foo.dart"),
            )],
            changed_doc_sections: vec![],
            affected_requirements: vec![item("req::REQ-X", "requirement", "REQ-X", None)],
            affected_docs: vec![],
            linked_tests: vec![item(
                "test_case::test/foo_test.dart#bar works",
                "test_case",
                "bar works",
                Some("test/foo_test.dart"),
            )],
            linked_implementations: vec![],
            affected_confirmed_candidates: vec![item(
                "business_candidate::login",
                "business_candidate",
                "Login candidate",
                None,
            )],
            propagated_symbols: vec![item(
                "dart_method::lib/bar.dart#Bar.baz",
                "dart_method",
                "Bar.baz",
                Some("lib/bar.dart"),
            )],
            impact_edges: vec![
                // file → changed_symbol (real structural edge)
                ImpactEdge {
                    from: "lib/foo.dart".into(),
                    to: "dart_method::lib/foo.dart#Foo.bar".into(),
                    kind: "contains".into(),
                },
                // changed_symbol declares_implementation REQ-X
                ImpactEdge {
                    from: "dart_method::lib/foo.dart#Foo.bar".into(),
                    to: "req::REQ-X".into(),
                    kind: "declares_implementation".into(),
                },
                // test → REQ-X (declares_verification, reverse direction)
                ImpactEdge {
                    from: "test_case::test/foo_test.dart#bar works".into(),
                    to: "req::REQ-X".into(),
                    kind: "declares_verification".into(),
                },
                // caller → callee (propagation)
                ImpactEdge {
                    from: "dart_method::lib/bar.dart#Bar.baz".into(),
                    to: "dart_method::lib/foo.dart#Foo.bar".into(),
                    kind: "calls".into(),
                },
            ],
            warnings: vec![],
            info: vec![],
        }
    }

    #[test]
    fn impact_mermaid_renders_changed_files_requirements_tests_and_candidates() {
        let out = render_impact_mermaid(&sample_report());
        assert!(out.starts_with("flowchart LR\n"));
        // Changed file appears as Fact rectangle.
        assert!(
            out.contains("[\"lib/foo.dart\"]"),
            "missing changed file node: {out}"
        );
        // Requirement → Confirmed rounded shape.
        assert!(
            out.contains("(\"REQ-X\")"),
            "missing requirement node: {out}"
        );
        // Linked test → Confirmed rounded shape with `declares_verification` arrow.
        assert!(
            out.contains("-->|declares_verification|"),
            "missing verification arrow: {out}"
        );
        // Business candidate → Candidate parallelogram with `-.->` arrow.
        assert!(out.contains("[/"), "missing candidate parallelogram: {out}");
        assert!(
            out.contains("-.->|evidence|"),
            "missing candidate evidence arrow: {out}"
        );
        // Propagated symbol appears via real `calls` edge from the
        // caller, not a synthesised "calls/refs" approximation.
        assert!(
            out.contains("---|calls|"),
            "missing real calls arrow: {out}"
        );
        assert!(
            !out.contains("calls/refs"),
            "synthesised `calls/refs` label leaked through: {out}"
        );
        // The contains edge anchors the changed file → changed symbol.
        assert!(
            out.contains("---|contains|"),
            "missing structural contains arrow: {out}"
        );
        // The declares_implementation edge runs from the changed
        // symbol to the requirement, not from every changed symbol.
        assert!(
            out.contains("-->|declares_implementation|"),
            "missing declares_implementation arrow: {out}"
        );
        // Note line carries the summary including impact_edges count
        // so reviewers can sanity-check provenance.
        assert!(
            out.contains("specslice impact"),
            "missing summary comment: {out}"
        );
        assert!(
            out.contains("impact_edges=4"),
            "summary missing impact_edges count: {out}"
        );
        // No raw artifact ids leak through.
        assert!(!out.contains("dart_method::"));
        assert!(!out.contains("test_case::"));
        assert!(!out.contains("business_candidate::"));
    }
}
