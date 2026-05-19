//! Minimal unified diff parser sufficient for `git diff --unified=0`.

use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub hunks: Vec<Hunk>,
    pub status: ChangeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    Modified,
    Added,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub new_start: u32,
    pub new_end: u32,
}

/// Run `git diff --unified=0 <base>..<head>` inside `repo_root` and return raw text.
pub fn git_diff(repo_root: &std::path::Path, base: &str, head: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--unified=0")
        .arg("--no-color")
        .arg(format!("{base}..{head}"))
        .output()
        .context("invoking `git diff`")?;
    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse the output of `git diff --unified=0`.
pub fn parse_unified_diff(text: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut current: Option<ChangedFile> = None;

    for line in text.lines() {
        if let Some(stripped) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file);
            }
            let path = parse_b_path(stripped).unwrap_or_default();
            current = Some(ChangedFile {
                path,
                hunks: Vec::new(),
                status: ChangeStatus::Modified,
            });
            continue;
        }
        if let Some(file) = current.as_mut() {
            if line.starts_with("new file mode") {
                file.status = ChangeStatus::Added;
            } else if line.starts_with("deleted file mode") {
                file.status = ChangeStatus::Deleted;
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                // Recover the new path; git uses `b/<path>` or `/dev/null`.
                if rest == "/dev/null" {
                    file.status = ChangeStatus::Deleted;
                } else if let Some(p) = rest.strip_prefix("b/") {
                    if file.path.is_empty() {
                        file.path = p.to_string();
                    }
                }
            } else if line.starts_with("--- /dev/null") {
                file.status = ChangeStatus::Added;
            } else if let Some(hunk) = parse_hunk_header(line) {
                file.hunks.push(hunk);
            }
        }
    }
    if let Some(file) = current.take() {
        files.push(file);
    }
    files
}

fn parse_b_path(diff_line: &str) -> Option<String> {
    // `a/<path> b/<path>`
    let mut parts = diff_line.split_whitespace();
    let _a = parts.next()?;
    let b = parts.next()?;
    let path = b.strip_prefix("b/")?;
    Some(path.to_string())
}

fn parse_hunk_header(line: &str) -> Option<Hunk> {
    // `@@ -a,b +c,d @@` or `@@ -a +c @@`. We only care about the `+` side.
    let rest = line.strip_prefix("@@")?;
    let plus_idx = rest.find('+')?;
    let after_plus = &rest[plus_idx + 1..];
    let end = after_plus.find(' ')?;
    let spec = &after_plus[..end];
    let mut iter = spec.split(',');
    let start: u32 = iter.next()?.parse().ok()?;
    let count: u32 = match iter.next() {
        Some(s) => s.parse().ok()?,
        None => 1,
    };
    let new_end = if count == 0 { start } else { start + count - 1 };
    Some(Hunk {
        new_start: start,
        new_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_modification_diff() {
        let raw = "diff --git a/lib/a.dart b/lib/a.dart\n--- a/lib/a.dart\n+++ b/lib/a.dart\n@@ -5,2 +5,3 @@\n-old\n+new\n+plus\n";
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "lib/a.dart");
        assert_eq!(f.status, ChangeStatus::Modified);
        assert_eq!(f.hunks.len(), 1);
        assert_eq!(f.hunks[0].new_start, 5);
        assert_eq!(f.hunks[0].new_end, 7);
    }

    #[test]
    fn parses_addition_and_deletion_status() {
        let added = "diff --git a/lib/b.dart b/lib/b.dart\nnew file mode 100644\n--- /dev/null\n+++ b/lib/b.dart\n@@ -0,0 +1,2 @@\n+a\n+b\n";
        let parsed = parse_unified_diff(added);
        assert_eq!(parsed[0].status, ChangeStatus::Added);

        let deleted = "diff --git a/lib/c.dart b/lib/c.dart\ndeleted file mode 100644\n--- a/lib/c.dart\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n";
        let parsed = parse_unified_diff(deleted);
        assert_eq!(parsed[0].status, ChangeStatus::Deleted);
    }

    #[test]
    fn handles_single_line_hunk_header() {
        let raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n";
        let parsed = parse_unified_diff(raw);
        assert_eq!(parsed[0].hunks[0].new_start, 1);
        assert_eq!(parsed[0].hunks[0].new_end, 1);
    }

    #[test]
    fn empty_diff_yields_empty_vec() {
        assert!(parse_unified_diff("").is_empty());
    }
}
