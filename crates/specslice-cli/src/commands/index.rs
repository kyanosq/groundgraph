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
    Ok(())
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
            writeln!(out, "  References (LSP): {}", go.references).ok();
        }
        if !go.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", go.resolver_used).ok();
        }
        if !go.sidecar_skip_reason.is_empty() {
            writeln!(out, "  LSP skipped: {}", go.sidecar_skip_reason).ok();
        }
    }
    // P16 — Python adapter status. The resolver field disambiguates
    // between `python_lsp` (pyright/basedpyright/pylsp ran) and
    // `python_ast` (AST-only fallback). The `LSP skipped` line surfaces
    // exactly *why* the LSP layer was bypassed so operators see whether
    // their venv was discovered or not.
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
        // P23.1: structure always comes from tree-sitter; the LSP server
        // (when present) only contributes Calls/References edges. Surface
        // that overlay count so operators can tell enrichment ran.
        if python.references > 0 {
            writeln!(out, "  References (LSP): {}", python.references).ok();
        }
        if !python.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", python.resolver_used).ok();
        }
        if !python.sidecar_skip_reason.is_empty() {
            writeln!(out, "  LSP skipped: {}", python.sidecar_skip_reason).ok();
        }
    }
    // P20/P23 — TypeScript / Java adapter status. Structure comes from the
    // tree-sitter driver (`Resolver: typescript_treesitter`); an optional LSP
    // only overlays `Calls`/`References`. `References (LSP)` surfaces that
    // overlay count, and `LSP skipped` surfaces why the LSP pass was bypassed
    // (binary missing, broken shebang, etc).
    if let Some(ts) = &result.typescript {
        writeln!(out, "TypeScript index:").ok();
        writeln!(out, "  TypeScript files: {}", ts.files).ok();
        writeln!(out, "  Symbols: {}", ts.symbols).ok();
        writeln!(out, "  TestCases: {}", ts.tests).ok();
        writeln!(out, "  Imports: {}", ts.imports).ok();
        if ts.heuristic_references > 0 {
            writeln!(out, "  References (heuristic): {}", ts.heuristic_references).ok();
        }
        if ts.references > 0 {
            writeln!(out, "  References (LSP): {}", ts.references).ok();
        }
        if !ts.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", ts.resolver_used).ok();
        }
        if !ts.sidecar_skip_reason.is_empty() {
            writeln!(out, "  LSP skipped: {}", ts.sidecar_skip_reason).ok();
        }
    }
    if let Some(java) = &result.java {
        writeln!(out, "Java index:").ok();
        writeln!(out, "  Java files: {}", java.files).ok();
        writeln!(out, "  Symbols: {}", java.symbols).ok();
        writeln!(out, "  TestCases: {}", java.tests).ok();
        writeln!(out, "  Imports: {}", java.imports).ok();
        if java.references > 0 {
            writeln!(out, "  References (LSP): {}", java.references).ok();
        }
        if !java.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", java.resolver_used).ok();
        }
        if !java.sidecar_skip_reason.is_empty() {
            writeln!(out, "  LSP skipped: {}", java.sidecar_skip_reason).ok();
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
                sidecar_skip_reason: String::new(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Go index"), "missing Go section: {out}");
        assert!(out.contains("4"), "missing Go file count: {out}");
        assert!(out.contains("18"), "missing Go symbol count: {out}");
        assert!(out.contains("go_treesitter"), "missing Go resolver: {out}");
        assert!(
            out.contains("References (LSP): 5"),
            "missing LSP reference overlay count: {out}"
        );
    }

    #[test]
    fn render_includes_python_section_with_treesitter_resolver_and_lsp_references() {
        let result = IndexResult {
            python: Some(PythonIndexResult {
                files: 5,
                symbols: 20,
                tests: 3,
                imports: 7,
                framework_entrypoints: 4,
                references: 12,
                resolver_used: "python_treesitter".into(),
                sidecar_skip_reason: String::new(),
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
            out.contains("References (LSP): 12"),
            "missing LSP reference overlay count: {out}"
        );
    }

    #[test]
    fn render_includes_python_section_with_skip_reason_when_lsp_enrichment_missing() {
        let result = IndexResult {
            python: Some(PythonIndexResult {
                files: 2,
                symbols: 4,
                tests: 0,
                imports: 1,
                framework_entrypoints: 0,
                references: 0,
                resolver_used: "python_treesitter".into(),
                sidecar_skip_reason: "未在 PATH / .venv 中找到 pyright/basedpyright/pylsp".into(),
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
            "structure resolver should be tree-sitter: {out}"
        );
        // No LSP server → no reference overlay line at all.
        assert!(
            !out.contains("References (LSP):"),
            "should not show reference line when enrichment skipped: {out}"
        );
        assert!(
            out.contains("LSP skipped"),
            "missing skip reason in Python section: {out}"
        );
    }

    #[test]
    fn render_includes_go_section_with_skip_reason_when_skipped() {
        let result = IndexResult {
            go: Some(GoIndexResult {
                files: 0,
                symbols: 0,
                tests: 0,
                imports: 0,
                references: 0,
                resolver_used: String::new(),
                sidecar_skip_reason: "未在 PATH 中找到 `gopls`".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Go index"), "missing Go section: {out}");
        assert!(
            out.contains("PATH"),
            "missing skip reason in Go section: {out}"
        );
    }

    #[test]
    fn render_includes_typescript_section_with_lsp_references_when_enriched() {
        let result = IndexResult {
            typescript: Some(TypescriptIndexResult {
                files: 6,
                symbols: 19,
                tests: 4,
                imports: 11,
                references: 7,
                heuristic_references: 0,
                resolver_used: "typescript_treesitter".into(),
                sidecar_skip_reason: String::new(),
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
            "missing TypeScript resolver label: {out}"
        );
        assert!(
            out.contains("References (LSP): 7"),
            "missing LSP reference overlay count: {out}"
        );
        assert!(out.contains("TestCases: 4"));
        assert!(out.contains("Imports: 11"));
    }

    #[test]
    fn render_includes_typescript_section_with_treesitter_resolver_when_lsp_missing() {
        let result = IndexResult {
            typescript: Some(TypescriptIndexResult {
                files: 3,
                symbols: 8,
                tests: 2,
                imports: 5,
                references: 0,
                heuristic_references: 6,
                resolver_used: "typescript_treesitter".into(),
                sidecar_skip_reason:
                    "未在 PATH / node_modules/.bin 找到 typescript-language-server，跳过 Calls/References 富化"
                        .into(),
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
        assert!(
            !out.contains("References (LSP)"),
            "no LSP overlay → no reference line: {out}"
        );
        assert!(
            out.contains("References (heuristic): 6"),
            "heuristic resolver count should surface even without LSP: {out}"
        );
        assert!(
            out.contains("LSP skipped"),
            "missing skip reason in TypeScript section: {out}"
        );
    }

    #[test]
    fn render_includes_java_section_with_lsp_references_when_enriched() {
        let result = IndexResult {
            java: Some(JavaIndexResult {
                files: 7,
                symbols: 22,
                tests: 5,
                imports: 9,
                references: 4,
                resolver_used: "java_treesitter".into(),
                sidecar_skip_reason: String::new(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Java index"), "missing Java section: {out}");
        assert!(
            out.contains("java_treesitter"),
            "missing Java resolver label: {out}"
        );
        assert!(
            out.contains("References (LSP): 4"),
            "missing LSP reference overlay count: {out}"
        );
        assert!(out.contains("TestCases: 5"));
        assert!(out.contains("Imports: 9"));
    }

    #[test]
    fn render_includes_java_section_with_treesitter_resolver_when_lsp_missing() {
        let result = IndexResult {
            java: Some(JavaIndexResult {
                files: 2,
                symbols: 4,
                tests: 1,
                imports: 3,
                references: 0,
                resolver_used: "java_treesitter".into(),
                sidecar_skip_reason: "未在 PATH 找到 jdtls，跳过 Calls/References 富化".into(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Java index"), "missing Java section: {out}");
        assert!(
            out.contains("java_treesitter"),
            "structure should always come from the tree-sitter driver: {out}"
        );
        assert!(
            !out.contains("References (LSP)"),
            "no LSP overlay → no reference line: {out}"
        );
        assert!(
            out.contains("LSP skipped"),
            "missing skip reason in Java section: {out}"
        );
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
