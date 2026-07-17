//! Path confinement for operator- and repo-configured paths (#145/#242/#263).
//!
//! One shared policy for resolving a configured path against a workspace
//! root, used by both the engine (CLI) and the MCP server so the two can
//! never drift apart again:
//!
//! - an **absolute** path is the operator explicitly naming a location
//!   outside the workspace — honoured verbatim (by design);
//! - a **relative** path joins under the root;
//! - a relative path containing `..` is **refused**: a poisoned
//!   `.groundgraph.yaml` must not relocate the SQLite store (or any other
//!   GroundGraph-owned file) outside the analysed repository (#242).

use std::path::{Component, Path, PathBuf};

/// A configured relative path tried to escape its confined root via `..`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "path `{configured}` must not contain `..`; use an absolute path to point outside the root"
)]
pub struct PathEscapeError {
    /// The configured path that was refused, verbatim.
    pub configured: String,
}

/// Resolve `configured` against `root` under the shared confinement policy
/// (see module docs). `configured` keeps plain `join` semantics: no
/// normalization beyond what [`Path::join`] does, so `"."` / `"./x"` behave
/// exactly as they did at every historical call site (component-equal to the
/// root / to `root/x`). An empty string resolves to the root itself.
pub fn confine_under_root(root: &Path, configured: &str) -> Result<PathBuf, PathEscapeError> {
    let candidate = Path::new(configured);
    // Absolute is an explicit operator override — honoured verbatim.
    if candidate.is_absolute() {
        return Ok(candidate.to_path_buf());
    }
    // Confine a *relative* path: any `..` component escapes the root.
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(PathEscapeError {
            configured: configured.to_string(),
        });
    }
    Ok(root.join(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole resolution contract, table-driven: relative joins, empty
    /// string, `.`/`./` normalization, absolute passthrough.
    #[test]
    fn confine_under_root_resolves_ok_cases() {
        let root = Path::new("/work/repo");
        let cases: &[(&str, &str)] = &[
            // Plain relative paths join under the root.
            ("graph.db", "/work/repo/graph.db"),
            (".groundgraph/graph.db", "/work/repo/.groundgraph/graph.db"),
            ("sub/dir/file.db", "/work/repo/sub/dir/file.db"),
            // Empty string resolves to the root itself (`join("")`).
            ("", "/work/repo"),
            // `.` / `./` keep historical `join` semantics: the results are
            // component-equal to the plain forms (Path equality normalizes
            // away mid-path `.`).
            (".", "/work/repo"),
            ("./graph.db", "/work/repo/graph.db"),
            ("a/./b.db", "/work/repo/a/b.db"),
            // Absolute paths pass through verbatim — an explicit operator
            // override, allowed by design on both engine and MCP sides.
            (
                "/var/lib/groundgraph/graph.db",
                "/var/lib/groundgraph/graph.db",
            ),
            // An absolute path already under the root is likewise verbatim.
            (
                "/work/repo/.groundgraph/graph.db",
                "/work/repo/.groundgraph/graph.db",
            ),
        ];
        for (configured, expected) in cases {
            assert_eq!(
                confine_under_root(root, configured).unwrap(),
                PathBuf::from(expected),
                "configured={configured:?}"
            );
        }
    }

    #[test]
    fn confine_under_root_rejects_parent_dir_escape() {
        let root = Path::new("/work/repo");
        let cases = [
            "..",
            "../evil.db",
            "../../etc/evil.db",
            ".groundgraph/../../escape.db",
            "a/../../b.db",
        ];
        for configured in cases {
            let err = confine_under_root(root, configured).unwrap_err();
            assert_eq!(err.configured, configured, "configured={configured:?}");
            assert!(err.to_string().contains(".."), "{err}");
            assert!(err.to_string().contains(configured), "{err}");
        }
    }

    #[test]
    fn confine_under_root_works_with_a_relative_root() {
        // The policy does not require an absolute root — confinement is
        // purely lexical, matching every historical call site.
        assert_eq!(
            confine_under_root(Path::new("repo"), "graph.db").unwrap(),
            PathBuf::from("repo/graph.db")
        );
        assert!(confine_under_root(Path::new("repo"), "../x.db").is_err());
    }
}
