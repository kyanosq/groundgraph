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
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent).with_context(|| {
                                format!("creating output directory {}", parent.display())
                            })?;
                        }
                    }
                    std::fs::write(&path, &mermaid)
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
/// * `changed_files` → root rectangles
/// * `changed_symbols` / `linked_implementations` / `propagated_symbols` → Fact
/// * `affected_requirements` → Confirmed (rounded)
/// * `affected_confirmed_candidates` → Candidate (parallelogram)
/// * `linked_tests` → Confirmed (tests are the "should-run" answer)
///
/// Edges are synthesised from the report rather than the store so the
/// diagram always matches the JSON `linked_tests` / `propagated_symbols`
/// arrays exactly, even when the graph store later evolves.
pub fn render_impact_mermaid(report: &ImpactReport) -> String {
    use std::collections::BTreeSet;

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

    // Anchor: changed files as Fact rectangles. Use a synthetic id
    // prefix so it never collides with an artifact id.
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

    let mut edges: Vec<MermaidEdge> = Vec::new();

    // changed_files → changed_symbols (via path match)
    for sym in &report.changed_symbols {
        if let Some(path) = &sym.path {
            edges.push(MermaidEdge {
                from: format!("file::{path}"),
                to: sym.id.clone(),
                kind: "contains".into(),
                layer: GraphLayer::Fact,
            });
        }
    }

    // changed_symbols → propagated_symbols (synthetic "calls/refs" edge,
    // since the underlying edge could be either kind).
    for prop in &report.propagated_symbols {
        edges.push(MermaidEdge {
            from: sym_anchor(&report.changed_symbols).unwrap_or_default(),
            to: prop.id.clone(),
            kind: "calls/refs".into(),
            layer: GraphLayer::Fact,
        });
    }

    // changed_symbols → affected_requirements (declares_implementation).
    for sym in &report.changed_symbols {
        for req in &report.affected_requirements {
            edges.push(MermaidEdge {
                from: sym.id.clone(),
                to: req.id.clone(),
                kind: "declares_implementation".into(),
                layer: GraphLayer::Confirmed,
            });
        }
    }

    // affected_requirements → linked_implementations (declares_implementation).
    for req in &report.affected_requirements {
        for impl_item in &report.linked_implementations {
            edges.push(MermaidEdge {
                from: impl_item.id.clone(),
                to: req.id.clone(),
                kind: "declares_implementation".into(),
                layer: GraphLayer::Confirmed,
            });
        }
    }

    // affected_requirements → linked_tests (declares_verification).
    for req in &report.affected_requirements {
        for test in &report.linked_tests {
            edges.push(MermaidEdge {
                from: test.id.clone(),
                to: req.id.clone(),
                kind: "declares_verification".into(),
                layer: GraphLayer::Confirmed,
            });
        }
    }

    // changed_symbols / changed_files → affected_confirmed_candidates.
    for cand in &report.affected_confirmed_candidates {
        if let Some(anchor) = sym_anchor(&report.changed_symbols) {
            edges.push(MermaidEdge {
                from: anchor,
                to: cand.id.clone(),
                kind: "evidence".into(),
                layer: GraphLayer::Candidate,
            });
        } else if let Some(first_file) = report.changed_files.first() {
            edges.push(MermaidEdge {
                from: format!("file::{first_file}"),
                to: cand.id.clone(),
                kind: "evidence".into(),
                layer: GraphLayer::Candidate,
            });
        }
    }

    let notes = vec![format!(
        "specslice impact — changed_files={} changed_symbols={} affected_requirements={} \
         linked_tests={} candidates={} propagated_symbols={}",
        report.changed_files.len(),
        report.changed_symbols.len(),
        report.affected_requirements.len(),
        report.linked_tests.len(),
        report.affected_confirmed_candidates.len(),
        report.propagated_symbols.len(),
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
        // Propagated symbol appears with calls/refs label.
        assert!(
            out.contains("---|calls/refs|"),
            "missing propagated arrow: {out}"
        );
        // Note line carries the summary so reviewers can sanity-check.
        assert!(
            out.contains("specslice impact"),
            "missing summary comment: {out}"
        );
        // No raw artifact ids leak through.
        assert!(!out.contains("dart_method::"));
        assert!(!out.contains("test_case::"));
        assert!(!out.contains("business_candidate::"));
    }
}
