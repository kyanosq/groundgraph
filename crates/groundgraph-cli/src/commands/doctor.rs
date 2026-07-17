//! #116 — `groundgraph doctor`: probe the external tools and artefacts
//! GroundGraph relies on (git, SCIP indexers, Dart, the graph.db, the config)
//! and report which are present, so "language X has 0 symbols" can be
//! attributed to a missing tool rather than empty code.
//!
//! Required probes (git / config / graph.db) flip to ✗ and drive the exit
//! code (2 under the #233 contract when any fails). Optional probes (SCIP
//! indexers, Dart SDK) are informational — they report what is on PATH but
//! never fail the check, because structure-only indexing works without them.

use std::path::Path;

use anyhow::Result;

use crate::exit_code::EXIT_USER_ERROR;

/// One probe result.
struct DoctorCheck {
    name: String,
    ok: bool,
    /// Empty when `ok`; otherwise the actionable hint shown under the ✗ line.
    detail: String,
}

pub fn run(repo_root: &Path) -> Result<i32> {
    // Required probes first (a missing one is a real failure), then optional
    // ones (reported but never fail the run).
    let checks = vec![
        check_required_tool("git", "git diff for impact / select-tests"),
        check_config(repo_root),
        check_graph_db(repo_root),
        check_scip_indexers(),
        check_optional_tool("dart", "Dart SDK for the analyzer sidecar"),
        check_optional_tool("sourcekit-lsp", "Swift LSP adapter"),
    ];

    for c in &checks {
        let icon = if c.ok { "✓" } else { "✗" };
        println!("{icon} {}", c.name);
        if !c.detail.is_empty() {
            println!("    → {}", c.detail);
        }
    }
    let failures = checks.iter().filter(|c| !c.ok).count();
    println!();
    println!("Doctor: {} check(s), {} failed.", checks.len(), failures);
    Ok(if failures == 0 {
        0
    } else {
        i32::from(EXIT_USER_ERROR)
    })
}

fn check_required_tool(tool: &str, why: &str) -> DoctorCheck {
    let name = format!("{tool} — {why}");
    if on_path(tool) {
        DoctorCheck {
            name,
            ok: true,
            detail: String::new(),
        }
    } else {
        DoctorCheck {
            name,
            ok: false,
            detail: format!(
                "`{tool}` not found on PATH — install it, or avoid the commands that need it (impact / select-tests)"
            ),
        }
    }
}

fn check_optional_tool(tool: &str, why: &str) -> DoctorCheck {
    // Always `ok`: the adapter is opt-in, so a missing tool is a hint, not a
    // failure. The detail line tells the user whether it is available.
    let on = on_path(tool);
    DoctorCheck {
        name: format!("{tool} — {why} (optional)"),
        ok: true,
        detail: if on {
            String::new()
        } else {
            format!("not on PATH — the matching adapter stays structure-only until {tool} is installed")
        },
    }
}

fn check_scip_indexers() -> DoctorCheck {
    let tools = ["scip-typescript", "scip-java", "scip-go", "scip-dart"];
    let found: Vec<&str> = tools.iter().copied().filter(|t| on_path(t)).collect();
    DoctorCheck {
        name: "SCIP indexers — precision overlay (optional)".into(),
        ok: true,
        detail: if found.is_empty() {
            "none on PATH — precision overlay disabled; structure-only indexing remains".into()
        } else {
            format!("on PATH: {}", found.join(", "))
        },
    }
}

fn check_graph_db(repo_root: &Path) -> DoctorCheck {
    let db = repo_root.join(".groundgraph").join("graph.db");
    if db.is_file() {
        DoctorCheck {
            name: "graph.db — indexed graph store".into(),
            ok: true,
            detail: String::new(),
        }
    } else {
        DoctorCheck {
            name: "graph.db — indexed graph store".into(),
            ok: false,
            detail: "no .groundgraph/graph.db — run `groundgraph index`".into(),
        }
    }
}

fn check_config(repo_root: &Path) -> DoctorCheck {
    let cfg = repo_root.join(".groundgraph.yaml");
    if !cfg.is_file() {
        return DoctorCheck {
            name: ".groundgraph.yaml — workspace config".into(),
            ok: false,
            detail: "no .groundgraph.yaml — run `groundgraph init`".into(),
        };
    }
    match groundgraph_engine::config::load_config(repo_root) {
        Ok(_) => DoctorCheck {
            name: ".groundgraph.yaml — workspace config".into(),
            ok: true,
            detail: String::new(),
        },
        Err(e) => DoctorCheck {
            name: ".groundgraph.yaml — workspace config".into(),
            ok: false,
            detail: format!("failed to parse: {e}"),
        },
    }
}

/// Whether `tool` is reachable on `PATH`. Best-effort: checks file presence
/// (Unix) / `.exe` presence (Windows) without spawning, so doctor stays fast
/// and side-effect-free.
fn on_path(tool: &str) -> bool {
    let candidates: Vec<String> = if cfg!(windows) {
        vec![format!("{tool}.exe"), tool.to_string()]
    } else {
        vec![tool.to_string()]
    };
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths)
        .any(|dir| candidates.iter().any(|c| dir.join(c).is_file()))
}
