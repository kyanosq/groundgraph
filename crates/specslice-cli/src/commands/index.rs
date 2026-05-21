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
        if !python.resolver_used.is_empty() {
            writeln!(out, "  Resolver: {}", python.resolver_used).ok();
        }
        if !python.sidecar_skip_reason.is_empty() {
            writeln!(out, "  LSP skipped: {}", python.sidecar_skip_reason).ok();
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
    out
}

#[cfg(test)]
mod tests {
    use super::format_result;
    use specslice_engine::{GoIndexResult, IndexResult, PythonIndexResult, SwiftIndexResult};

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
    }

    #[test]
    fn render_includes_swift_section_with_files_and_symbols_when_indexed() {
        let result = IndexResult {
            swift: Some(SwiftIndexResult {
                files: 3,
                symbols: 12,
                resolver_used: "swift_lsp".into(),
                sidecar_skip_reason: String::new(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Swift index"), "missing Swift section: {out}");
        assert!(out.contains("3"), "missing Swift file count: {out}");
        assert!(out.contains("12"), "missing Swift symbol count: {out}");
        assert!(out.contains("swift_lsp"), "missing Swift resolver: {out}");
    }

    #[test]
    fn render_includes_swift_section_with_skip_reason_when_skipped() {
        let result = IndexResult {
            swift: Some(SwiftIndexResult {
                files: 0,
                symbols: 0,
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
                resolver_used: "go_lsp".into(),
                sidecar_skip_reason: String::new(),
            }),
            ..IndexResult::default()
        };
        let out = format_result(&result);
        assert!(out.contains("Go index"), "missing Go section: {out}");
        assert!(out.contains("4"), "missing Go file count: {out}");
        assert!(out.contains("18"), "missing Go symbol count: {out}");
        assert!(out.contains("go_lsp"), "missing Go resolver: {out}");
    }

    #[test]
    fn render_includes_python_section_with_lsp_resolver_when_indexed() {
        let result = IndexResult {
            python: Some(PythonIndexResult {
                files: 5,
                symbols: 20,
                tests: 3,
                imports: 7,
                framework_entrypoints: 4,
                resolver_used: "python_lsp".into(),
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
            out.contains("python_lsp"),
            "missing Python resolver label: {out}"
        );
        assert!(out.contains("TestCases: 3"));
        assert!(out.contains("Imports: 7"));
        assert!(
            out.contains("Framework entrypoints: 4"),
            "missing framework entrypoint count: {out}"
        );
    }

    #[test]
    fn render_includes_python_section_with_ast_fallback_reason_when_lsp_missing() {
        let result = IndexResult {
            python: Some(PythonIndexResult {
                files: 2,
                symbols: 4,
                tests: 0,
                imports: 1,
                framework_entrypoints: 0,
                resolver_used: "python_ast".into(),
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
            out.contains("python_ast"),
            "missing AST fallback resolver: {out}"
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
}
