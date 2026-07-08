use std::path::Path;

use anyhow::Result;
use groundgraph_engine::{build_context, ContextOptions};

pub fn run(repo_root: &Path, requirement: &str, include_snippets: bool, json: bool) -> Result<()> {
    let pack = build_context(ContextOptions {
        repo_root: repo_root.to_path_buf(),
        requirement: requirement.to_string(),
        include_snippets,
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&pack)?);
    } else {
        print!("{}", render_human(&pack));
    }
    Ok(())
}

/// Render one slice item as `name (path)`. Printing the path alone (the old
/// behaviour) collapsed two distinct symbols living in the same file into two
/// identical-looking lines, so a reader could neither tell them apart nor see
/// which symbol the snippet belongs to. Mirrors `slice`'s `print_items`.
fn item_line(item: &groundgraph_engine::SliceItem) -> String {
    let where_ = item.path.clone().unwrap_or_else(|| item.id.clone());
    let label = item.name.clone().unwrap_or_else(|| item.id.clone());
    format!("- {label} ({where_})")
}

fn render_human(pack: &groundgraph_engine::ContextPack) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Context Pack: {} {}",
        pack.requirement_id,
        pack.title.clone().unwrap_or_default()
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Docs:");
    for d in &pack.slice.docs {
        let _ = writeln!(out, "{}", item_line(d));
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Implementation:");
    for d in &pack.slice.implementation {
        let _ = writeln!(out, "{}", item_line(d));
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Linked tests:");
    for d in &pack.slice.linked_tests {
        let _ = writeln!(out, "{}", item_line(d));
    }
    if !pack.files_to_read.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Files to read:");
        for f in &pack.files_to_read {
            let _ = writeln!(out, "- {f}");
        }
    }
    if !pack.tests_to_run.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Tests to run:");
        for t in &pack.tests_to_run {
            let _ = writeln!(out, "- {t}");
        }
    }
    if !pack.docs_snippets.is_empty()
        || !pack.impl_snippets.is_empty()
        || !pack.test_snippets.is_empty()
    {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Snippets included: docs={}, impl={}, test={}",
            pack.docs_snippets.len(),
            pack.impl_snippets.len(),
            pack.test_snippets.len(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_engine::{ContextPack, FeatureSlice, SliceItem};

    fn item(id: &str, name: &str, path: &str) -> SliceItem {
        SliceItem {
            id: id.into(),
            kind: "python_function".into(),
            path: Some(path.into()),
            name: Some(name.into()),
            line_range: None,
        }
    }

    /// Two distinct implementation symbols in the *same file* must each show
    /// their own name; the old path-only formatter rendered both as the bare
    /// file path, which read as a meaningless duplicate and hid which symbol
    /// the reader was looking at.
    #[test]
    fn implementation_items_show_symbol_name_not_just_path() {
        let pack = ContextPack {
            requirement_id: "REQ-PRICE".into(),
            title: Some("订单计价".into()),
            slice: FeatureSlice {
                requirement_id: "REQ-PRICE".into(),
                title: Some("订单计价".into()),
                docs: vec![],
                implementation: vec![
                    item(
                        "py::src/orders.py::price_order",
                        "price_order",
                        "src/orders.py",
                    ),
                    item(
                        "py::src/orders.py::apply_coupon",
                        "apply_coupon",
                        "src/orders.py",
                    ),
                ],
                linked_tests: vec![],
                code_fanout: vec![],
                risks: vec![],
            },
            files_to_read: vec!["src/orders.py".into()],
            tests_to_run: vec![],
            docs_snippets: vec![],
            impl_snippets: vec![],
            test_snippets: vec![],
            edges: vec![],
        };
        let out = render_human(&pack);
        assert!(
            out.contains("- price_order (src/orders.py)"),
            "missing named impl line: {out}"
        );
        assert!(
            out.contains("- apply_coupon (src/orders.py)"),
            "missing named impl line: {out}"
        );
        // The two impl lines must be distinguishable, not two identical
        // `- src/orders.py` rows.
        assert!(
            !out.contains("Implementation:\n- src/orders.py\n- src/orders.py"),
            "impl lines collapsed to duplicate paths: {out}"
        );
    }
}
