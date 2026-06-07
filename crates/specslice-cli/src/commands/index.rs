use std::fmt::Write;
use std::path::Path;

use anyhow::Result;
use specslice_engine::{index_repository, IndexOptions, IndexResult};

pub fn run(repo_root: &Path, docs_only: bool) -> Result<()> {
    let options = if docs_only {
        IndexOptions::docs_only(repo_root.to_path_buf())
    } else {
        IndexOptions::all(repo_root.to_path_buf())
    };
    let result = index_repository(options)?;
    print!("{}", format_result(&result));
    record_index_metrics(&result);
    // P25: fold the persistence contract into the same graph. Best-effort —
    // a schema scan failure must not abort a successful code/docs index.
    if !docs_only {
        match specslice_engine::schema_indexer::index_schema(repo_root) {
            Ok(s) => {
                println!("Schema index:");
                println!("  Tables (SQL {} + ORM {})", s.sql_tables, s.orm_tables);
                println!("  Columns: {}", s.columns);
                println!("  Mapper statements: {}", s.mapper_stmts);
                println!(
                    "  Data-layer edges: method→SQL {} + SQL→table {} + interface→impl {} + inline-SQL→table {}",
                    s.stmt_method_edges,
                    s.stmt_table_edges,
                    s.iface_impl_edges,
                    s.inline_sql_table_edges
                );
                specslice_engine::stats::set_metric("tables", (s.sql_tables + s.orm_tables) as i64);
                specslice_engine::stats::set_metric("columns", s.columns as i64);
                specslice_engine::stats::set_metric("mapper_stmts", s.mapper_stmts as i64);
                specslice_engine::stats::set_metric(
                    "data_layer_edges",
                    (s.stmt_method_edges + s.stmt_table_edges) as i64,
                );
                specslice_engine::stats::set_metric("iface_impl_edges", s.iface_impl_edges as i64);
                specslice_engine::stats::set_metric(
                    "inline_sql_table_edges",
                    s.inline_sql_table_edges as i64,
                );
            }
            Err(e) => eprintln!("Schema index skipped: {e:#}"),
        }
    }
    Ok(())
}

/// Push aggregate index counts into the per-command stats collector so
/// `specslice stats` can report how much each `index` run produced.
fn record_index_metrics(result: &IndexResult) {
    use specslice_engine::stats::set_metric;
    let mut files = 0i64;
    let mut symbols = 0i64;
    if let Some(d) = &result.docs {
        set_metric("doc_sections", d.doc_sections as i64);
    }
    if let Some(c) = &result.code {
        files += c.files as i64;
        symbols += c.symbols as i64;
    }
    if let Some(s) = &result.swift {
        files += s.files as i64;
        symbols += s.symbols as i64;
    }
    if let Some(s) = &result.go {
        files += s.files as i64;
        symbols += s.symbols as i64;
    }
    if let Some(s) = &result.python {
        files += s.files as i64;
        symbols += s.symbols as i64;
    }
    if let Some(s) = &result.typescript {
        files += s.files as i64;
        symbols += s.symbols as i64;
    }
    if let Some(s) = &result.java {
        files += s.files as i64;
        symbols += s.symbols as i64;
    }
    if let Some(r) = &result.rust {
        files += r.files as i64;
        symbols += r.symbols as i64;
    }
    for lang in &result.treesitter {
        files += lang.files as i64;
        symbols += lang.symbols as i64;
    }
    set_metric("files", files);
    set_metric("symbols", symbols);
}

/// Render the human-readable summary for `specslice index`. Factored
/// out from [`run`] so unit tests can exercise the formatting without
/// touching the filesystem or running the indexer.
pub(crate) fn format_result(result: &IndexResult) -> String {
    let mut out = String::new();
    if let Some(docs) = &result.docs {
        writeln!(out, "Docs index:").ok();
        writeln!(out, "  Files: {}", docs.files).ok();
        writeln!(out, "  Requirements: {}", docs.requirements).ok();
        writeln!(out, "  DocSections: {}", docs.doc_sections).ok();
        writeln!(out, "  Edges: {}", docs.edges).ok();
    }
    if let Some(code) = &result.code {
        writeln!(out, "Code index:").ok();
        writeln!(out, "  Dart files: {}", code.files).ok();
        writeln!(out, "  Symbols: {}", code.symbols).ok();
        writeln!(out, "  TestCases: {}", code.tests).ok();
        if !code.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", code.resolver_used).ok();
        }
        if !code.sidecar_skip_reason.is_empty() {
            writeln!(out, "  Sidecar skipped: {}", code.sidecar_skip_reason).ok();
        }
    }
    // P12 复核 [P2]: surface the Swift / Go adapter status. Operators
    // need to see whether the adapter actually ran, how many files /
    // symbols landed, and the skip reason when the LSP binary or cache
    // is missing — otherwise enabling `swift.enabled` looks like a no-op.
    if let Some(swift) = &result.swift {
        writeln!(out, "Swift index:").ok();
        writeln!(out, "  Swift files: {}", swift.files).ok();
        writeln!(out, "  Symbols: {}", swift.symbols).ok();
        writeln!(out, "  TestCases: {}", swift.tests).ok();
        writeln!(out, "  Imports: {}", swift.imports).ok();
        if swift.references > 0 {
            writeln!(out, "  References (LSP): {}", swift.references).ok();
        }
        if !swift.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", swift.resolver_used).ok();
        }
        if !swift.sidecar_skip_reason.is_empty() {
            writeln!(out, "  LSP skipped: {}", swift.sidecar_skip_reason).ok();
        }
    }
    if let Some(go) = &result.go {
        writeln!(out, "Go index:").ok();
        writeln!(out, "  Go files: {}", go.files).ok();
        writeln!(out, "  Symbols: {}", go.symbols).ok();
        writeln!(out, "  TestCases: {}", go.tests).ok();
        writeln!(out, "  Imports: {}", go.imports).ok();
        if go.references > 0 {
            writeln!(out, "  References (heuristic): {}", go.references).ok();
        }
        if !go.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", go.resolver_used).ok();
        }
    }
    // P16 — Python adapter status. Structure + heuristic edges come from the
    // tree-sitter driver (`Resolver: python_treesitter`); precision comes from
    // a SCIP overlay when present.
    if let Some(python) = &result.python {
        writeln!(out, "Python index:").ok();
        writeln!(out, "  Python files: {}", python.files).ok();
        writeln!(out, "  Symbols: {}", python.symbols).ok();
        writeln!(out, "  TestCases: {}", python.tests).ok();
        writeln!(out, "  Imports: {}", python.imports).ok();
        // P17: framework entry points are surfaced here even when
        // they are 0 so operators can see whether the classifier
        // wired up against their codebase at all.
        writeln!(
            out,
            "  Framework entrypoints: {}",
            python.framework_entrypoints
        )
        .ok();
        if python.references > 0 {
            writeln!(out, "  References (heuristic): {}", python.references).ok();
        }
        if !python.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", python.resolver_used).ok();
        }
    }
    // P20/P23 — TypeScript / Java adapter status. Structure + heuristic edges
    // come from the tree-sitter driver (`Resolver: typescript_treesitter`);
    // precision comes from the SCIP overlay (`scip-typescript` / `scip-java`).
    if let Some(ts) = &result.typescript {
        writeln!(out, "TypeScript index:").ok();
        writeln!(out, "  TypeScript files: {}", ts.files).ok();
        writeln!(out, "  Symbols: {}", ts.symbols).ok();
        writeln!(out, "  TestCases: {}", ts.tests).ok();
        writeln!(out, "  Imports: {}", ts.imports).ok();
        if ts.heuristic_references > 0 {
            writeln!(out, "  References (heuristic): {}", ts.heuristic_references).ok();
        }
        if !ts.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", ts.resolver_used).ok();
        }
    }
    if let Some(java) = &result.java {
        writeln!(out, "Java index:").ok();
        writeln!(out, "  Java files: {}", java.files).ok();
        writeln!(out, "  Symbols: {}", java.symbols).ok();
        writeln!(out, "  TestCases: {}", java.tests).ok();
        writeln!(out, "  Imports: {}", java.imports).ok();
        if java.references > 0 {
            writeln!(out, "  References (heuristic): {}", java.references).ok();
        }
        if !java.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", java.resolver_used).ok();
        }
    }
    if let Some(rust) = &result.rust {
        writeln!(out, "Rust index:").ok();
        writeln!(out, "  Rust files: {}", rust.files).ok();
        writeln!(out, "  Symbols: {}", rust.symbols).ok();
        writeln!(out, "  Imports: {}", rust.imports).ok();
        if !rust.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", rust.resolver_used).ok();
        }
    }
    if !result.treesitter.is_empty() {
        writeln!(out, "Tree-sitter index:").ok();
        for lang in &result.treesitter {
            writeln!(
                out,
                "  {}: {} files, {} symbols, {} imports ({})",
                lang.language, lang.files, lang.symbols, lang.imports, lang.resolver_used
            )
            .ok();
        }
    }
    {
        use specslice_engine::scip_runner::ScipRunStatus;
        // Auto-invocation outcomes. `Unsupported` (no indexer spec) is not
        // actionable, so the section only appears when at least one language
        // generated / was skipped (binary absent) / failed.
        let actionable: Vec<_> = result
            .scip_runs
            .iter()
            .filter(|r| !matches!(r.status, ScipRunStatus::Unsupported))
            .collect();
        if !actionable.is_empty() {
            writeln!(out, "SCIP indexers:").ok();
            for run in actionable {
                match &run.status {
                    ScipRunStatus::Generated => {
                        writeln!(out, "  {}: generated", run.language).ok();
                    }
                    ScipRunStatus::Skipped(reason) => {
                        writeln!(out, "  {}: skipped ({reason})", run.language).ok();
                    }
                    ScipRunStatus::Failed(reason) => {
                        writeln!(out, "  {}: failed ({reason})", run.language).ok();
                    }
                    ScipRunStatus::Unsupported => {}
                }
            }
        }
    }
    if let Some(scip) = &result.scip {
        // Only surfaced when a `.scip` file was actually ingested; an enabled
        // overlay that found nothing on disk reports zero and stays silent.
        if scip.scip_files > 0 {
            writeln!(out, "SCIP overlay:").ok();
            writeln!(out, "  Files: {}", scip.scip_files).ok();
            writeln!(out, "  Documents: {}", scip.documents).ok();
            writeln!(out, "  Edges: {}", scip.edges).ok();
            // Heuristic precision SCIP displaced on its covered files (dedup →
            // single source of truth). Silent when nothing was suppressed.
            if scip.suppressed > 0 {
                writeln!(out, "  Suppressed (heuristic): {}", scip.suppressed).ok();
            }
        }
    }
    if let Some(links) = &result.links {
        writeln!(out, "Links index:").ok();
        writeln!(out, "  Requirements: {}", links.requirements).ok();
        writeln!(out, "  Docs: {}", links.docs).ok();
        writeln!(out, "  Implementations: {}", links.implementations).ok();
        writeln!(out, "  Tests: {}", links.tests).ok();
        writeln!(out, "  Edges: {}", links.edges).ok();
    }
    if let Some(reqs) = &result.requirements_md {
        if reqs.files > 0 || reqs.requirements > 0 {
            writeln!(out, "Requirements (markdown):").ok();
            writeln!(out, "  Files: {}", reqs.files).ok();
            writeln!(out, "  Requirements: {}", reqs.requirements).ok();
            writeln!(out, "  Docs: {}", reqs.documents).ok();
            writeln!(out, "  Implementations: {}", reqs.implementations).ok();
            writeln!(out, "  Tests: {}", reqs.verifications).ok();
            writeln!(out, "  Edges: {}", reqs.edges).ok();
            if reqs.unresolved > 0 {
                writeln!(out, "  Unresolved refs: {}", reqs.unresolved).ok();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format_result;
    use specslice_engine::{
        GoIndexResult, IndexResult, JavaIndexResult, PythonIndexResult, SwiftIndexResult,
        TypescriptIndexResult,
    };

    #[test]
    fn render_omits_swift_section_when_adapter_is_disabled() {
        let result = IndexResult::default();
        let out = format_result(&result);
        assert!(
            !out.contains("Swift index"),
            "Swift section should not appear when swift adapter disabled: {out}"
        );
        assert!(
            !out.contains("Go index"),
            "Go section should not appear when go adapter disabled: {out}"
        );
        assert!(
            !out.contains("TypeScript index"),
            "TypeScript section should not appear when ts adapter disabled: {out}"
        );
        assert!(
            !out.contains("Java index"),
            "Java section should not appear when java adapter disabled: {out}"
        );
    }

    #[test]
    fn render_includes_swift_section_with_files_and_symbols_when_indexed() {
        let result = IndexResult {
            swift: Some(SwiftIndexResult {
                files: 3,
                symbols: 12,
                tests: 4,
                imports: 0,
                references: 7,
                resolver_used: "swift_treesitter".into(),
                sidecar_skip_reason: String::new(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Swift index"), "missing Swift section: {out}");
        assert!(out.contains("3"), "missing Swift file count: {out}");
        assert!(out.contains("12"), "missing Swift symbol count: {out}");
        assert!(
            out.contains("swift_treesitter"),
            "missing Swift resolver: {out}"
        );
        assert!(
            out.contains("References (LSP): 7"),
            "missing LSP reference overlay count: {out}"
        );
    }

    #[test]
    fn render_includes_swift_section_with_skip_reason_when_skipped() {
        let result = IndexResult {
            swift: Some(SwiftIndexResult {
                files: 0,
                symbols: 0,
                tests: 0,
                imports: 0,
                references: 0,
                resolver_used: String::new(),
                sidecar_skip_reason: "未在 PATH 中找到 `sourcekit-lsp`".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Swift index"), "missing Swift section: {out}");
        assert!(
            out.contains("PATH"),
            "missing skip reason in Swift section: {out}"
        );
    }

    #[test]
    fn render_includes_go_section_with_files_and_symbols_when_indexed() {
        let result = IndexResult {
            go: Some(GoIndexResult {
                files: 4,
                symbols: 18,
                tests: 2,
                imports: 6,
                references: 5,
                resolver_used: "go_treesitter".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Go index"), "missing Go section: {out}");
        assert!(out.contains("4"), "missing Go file count: {out}");
        assert!(out.contains("18"), "missing Go symbol count: {out}");
        assert!(out.contains("go_treesitter"), "missing Go resolver: {out}");
        assert!(
            out.contains("References (heuristic): 5"),
            "missing heuristic reference count: {out}"
        );
    }

    #[test]
    fn render_includes_python_section_with_treesitter_resolver_and_heuristic_references() {
        let result = IndexResult {
            python: Some(PythonIndexResult {
                files: 5,
                symbols: 20,
                tests: 3,
                imports: 7,
                framework_entrypoints: 4,
                references: 12,
                resolver_used: "python_treesitter".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(
            out.contains("Python index"),
            "missing Python section: {out}"
        );
        assert!(
            out.contains("python_treesitter"),
            "missing Python resolver label: {out}"
        );
        assert!(out.contains("TestCases: 3"));
        assert!(out.contains("Imports: 7"));
        assert!(
            out.contains("Framework entrypoints: 4"),
            "missing framework entrypoint count: {out}"
        );
        assert!(
            out.contains("References (heuristic): 12"),
            "missing heuristic reference count: {out}"
        );
    }

    #[test]
    fn render_omits_go_reference_line_when_zero_and_keeps_no_lsp_label() {
        let result = IndexResult {
            go: Some(GoIndexResult {
                files: 0,
                symbols: 0,
                tests: 0,
                imports: 0,
                references: 0,
                resolver_used: "go_treesitter".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Go index"), "missing Go section: {out}");
        // LSP overlay was retired for Go: no LSP labels should ever appear.
        assert!(
            !out.contains("References (LSP)") && !out.contains("LSP skipped"),
            "Go must not surface any LSP labels: {out}"
        );
        assert!(
            !out.contains("References (heuristic)"),
            "zero references → no reference line: {out}"
        );
    }

    #[test]
    fn render_includes_typescript_section_with_heuristic_references() {
        let result = IndexResult {
            typescript: Some(TypescriptIndexResult {
                files: 3,
                symbols: 8,
                tests: 2,
                imports: 5,
                heuristic_references: 6,
                resolver_used: "typescript_treesitter".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(
            out.contains("TypeScript index"),
            "missing TypeScript section: {out}"
        );
        assert!(
            out.contains("typescript_treesitter"),
            "structure should always come from the tree-sitter driver: {out}"
        );
        // LSP overlay was retired for TypeScript: no LSP labels should appear.
        assert!(
            !out.contains("References (LSP)") && !out.contains("LSP skipped"),
            "TypeScript must not surface any LSP labels: {out}"
        );
        assert!(
            out.contains("References (heuristic): 6"),
            "heuristic resolver count should surface: {out}"
        );
        assert!(out.contains("TestCases: 2"));
        assert!(out.contains("Imports: 5"));
    }

    #[test]
    fn render_includes_java_section_with_treesitter_resolver_and_heuristic_references() {
        let result = IndexResult {
            java: Some(JavaIndexResult {
                files: 7,
                symbols: 22,
                tests: 5,
                imports: 9,
                references: 4,
                resolver_used: "java_treesitter".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Java index"), "missing Java section: {out}");
        assert!(
            out.contains("java_treesitter"),
            "missing Java resolver label: {out}"
        );
        // LSP overlay was retired for Java: no LSP labels should appear.
        assert!(
            !out.contains("References (LSP)") && !out.contains("LSP skipped"),
            "Java must not surface any LSP labels: {out}"
        );
        assert!(
            out.contains("References (heuristic): 4"),
            "missing heuristic reference count: {out}"
        );
        assert!(out.contains("TestCases: 5"));
        assert!(out.contains("Imports: 9"));
    }

    #[test]
    fn render_includes_rust_section_with_treesitter_resolver_when_indexed() {
        let result = IndexResult {
            rust: Some(specslice_engine::RustIndexResult {
                files: 12,
                symbols: 140,
                imports: 60,
                references: 85,
                resolver_used: "rust_treesitter".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Rust index"), "missing Rust section: {out}");
        assert!(
            out.contains("rust_treesitter"),
            "missing Rust resolver label: {out}"
        );
        assert!(out.contains("Symbols: 140"));
        assert!(out.contains("Imports: 60"));
    }

    #[test]
    fn render_includes_scip_indexers_section_for_generated_and_skipped() {
        use specslice_engine::scip_runner::{ScipRunOutcome, ScipRunStatus};
        let result = IndexResult {
            scip_runs: vec![
                ScipRunOutcome {
                    language: "rust".into(),
                    status: ScipRunStatus::Generated,
                    output: Some(std::path::PathBuf::from(".specslice/scip/rust.scip")),
                },
                ScipRunOutcome {
                    language: "go".into(),
                    status: ScipRunStatus::Skipped("未在 PATH 找到 `scip-go`".into()),
                    output: None,
                },
            ],
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(
            out.contains("SCIP indexers"),
            "missing SCIP indexers section: {out}"
        );
        assert!(out.contains("rust"), "missing generated lang: {out}");
        assert!(out.contains("scip-go"), "missing skip reason: {out}");
    }

    #[test]
    fn render_omits_unsupported_only_scip_indexers_section() {
        use specslice_engine::scip_runner::{ScipRunOutcome, ScipRunStatus};
        // Languages with no auto-invoke spec (java/swift/c/cpp) report
        // Unsupported; that is not actionable, so the section stays hidden.
        let result = IndexResult {
            scip_runs: vec![ScipRunOutcome {
                language: "java".into(),
                status: ScipRunStatus::Unsupported,
                output: None,
            }],
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(
            !out.contains("SCIP indexers"),
            "section should hide when only Unsupported entries: {out}"
        );
    }

    #[test]
    fn render_includes_scip_section_when_overlay_ingested_files() {
        let result = IndexResult {
            scip: Some(specslice_engine::scip_overlay::ScipOverlayResult {
                scip_files: 2,
                documents: 318,
                edges: 9681,
                suppressed: 142,
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("SCIP overlay"), "missing SCIP section: {out}");
        assert!(out.contains("Files: 2"), "missing SCIP file count: {out}");
        assert!(
            out.contains("Documents: 318"),
            "missing SCIP document count: {out}"
        );
        assert!(
            out.contains("Edges: 9681"),
            "missing SCIP edge count: {out}"
        );
        assert!(
            out.contains("Suppressed (heuristic): 142"),
            "missing SCIP suppression count: {out}"
        );
    }

    #[test]
    fn render_omits_scip_section_when_no_scip_file_on_disk() {
        // Enrichment ran (`Some`) but found no `.scip` file → zero counts.
        // A noisy "SCIP overlay: 0 files" line would only confuse, so suppress.
        let result = IndexResult {
            scip: Some(specslice_engine::scip_overlay::ScipOverlayResult::default()),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(
            !out.contains("SCIP overlay"),
            "SCIP section should be hidden when no .scip ingested: {out}"
        );
    }

    #[test]
    fn render_includes_unified_treesitter_section_per_language() {
        let result = IndexResult {
            treesitter: vec![
                specslice_engine::TreeSitterLangResult {
                    language: "typescript".into(),
                    files: 8,
                    symbols: 64,
                    imports: 30,
                    resolver_used: "typescript_treesitter".into(),
                },
                specslice_engine::TreeSitterLangResult {
                    language: "cpp".into(),
                    files: 3,
                    symbols: 21,
                    imports: 5,
                    resolver_used: "cpp_treesitter".into(),
                },
            ],
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Tree-sitter index"), "missing section: {out}");
        assert!(
            out.contains("typescript: 8 files, 64 symbols, 30 imports (typescript_treesitter)"),
            "missing typescript line: {out}"
        );
        assert!(
            out.contains("cpp: 3 files, 21 symbols, 5 imports (cpp_treesitter)"),
            "missing cpp line: {out}"
        );
    }
}
