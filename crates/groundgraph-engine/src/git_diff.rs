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
    /// A pure rename / copy (`git diff` rename detection). Carries no content
    /// hunks; `path` is the *new* path.
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub new_start: u32,
    pub new_end: u32,
}

/// Build the argument list for `git diff`.
///
/// * `head` non-empty → committed range `git diff <base>..<head>`.
/// * `head` empty → diff `<base>` against the **working tree**
///   (`git diff <base>`), so `impact` can run on uncommitted edits without a
///   throwaway commit. Note: this picks up tracked modifications (staged and
///   unstaged); brand-new *untracked* files are not part of `git diff` until
///   they are added.
fn diff_args(base: &str, head: &str) -> Vec<String> {
    let mut args = vec![
        "diff".to_string(),
        "--unified=0".to_string(),
        "--no-color".to_string(),
    ];
    if head.is_empty() {
        args.push(base.to_string());
    } else {
        args.push(format!("{base}..{head}"));
    }
    args
}

/// Reject a git ref that could be misread as a `git` option (`--output=…`,
/// `-O<orderfile>`, …). `base`/`head` reach here from `impact`'s arguments,
/// which on the MCP path come straight from a remote client — an unvalidated
/// leading `-` turns `git diff` into an arbitrary file write/read primitive.
fn ensure_safe_ref(label: &str, value: &str) -> Result<()> {
    if value.starts_with('-') {
        anyhow::bail!("invalid git {label} ref `{value}`: must not start with '-'");
    }
    Ok(())
}

/// Run `git diff --unified=0` for the given refs inside `repo_root` and return
/// raw text. See [`diff_args`] for the committed-range vs working-tree modes.
pub fn git_diff(repo_root: &std::path::Path, base: &str, head: &str) -> Result<String> {
    ensure_safe_ref("base", base)?;
    ensure_safe_ref("head", head)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(diff_args(base, head))
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
            } else if let Some(rest) = line
                .strip_prefix("rename to ")
                .or_else(|| line.strip_prefix("copy to "))
            {
                // Pure rename/copy: no `+++`/`@@` follow, and `parse_b_path`'s
                // whitespace split mangles paths with spaces. The `… to` line
                // carries the new path verbatim — trust it. (#270)
                file.path = rest.to_string();
                file.status = ChangeStatus::Renamed;
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
    // `start + count - 1` can overflow u32 on a hostile diff header (diffs come
    // from `git diff`/`git show`/remote PRs — untrusted). Use checked math so a
    // bogus hunk is skipped rather than panicking (debug) or wrapping to a wrong
    // line number (release).
    let new_end = if count == 0 {
        start
    } else {
        start.checked_add(count - 1)?
    };
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

    #[test]
    fn hostile_hunk_header_does_not_overflow() {
        // `@@ -1 +2,4294967295 @@`: start=2, count=u32::MAX → `start + count - 1`
        // exceeds u32::MAX. Must not panic (debug) or wrap to 0 (release): the
        // hunk is skipped instead. Diffs are untrusted input (remote PRs,
        // `git show`).
        assert_eq!(parse_hunk_header("@@ -1 +2,4294967295 @@"), None);
        // A normal large-but-valid hunk still parses.
        assert_eq!(
            parse_hunk_header("@@ -1 +10,5 @@"),
            Some(Hunk {
                new_start: 10,
                new_end: 14,
            })
        );
    }

    #[test]
    fn diff_args_use_committed_range_when_head_present() {
        assert_eq!(
            diff_args("origin/main", "HEAD"),
            vec!["diff", "--unified=0", "--no-color", "origin/main..HEAD"],
        );
    }

    #[test]
    fn diff_args_target_working_tree_when_head_empty() {
        // `git diff <base>` (no `..head`) compares base against the working
        // tree, so `impact` can run on uncommitted changes.
        assert_eq!(
            diff_args("HEAD", ""),
            vec!["diff", "--unified=0", "--no-color", "HEAD"],
        );
    }

    #[test]
    fn rejects_refs_that_look_like_git_options() {
        // #241 option-injection guard: a ref starting with `-` must be refused
        // before it reaches `git diff` — `--output=<path>` / `-O<orderfile>`
        // would otherwise write/read arbitrary files via the diff subprocess.
        assert!(ensure_safe_ref("base", "--output=/tmp/pwn").is_err());
        assert!(ensure_safe_ref("base", "-O/tmp/order").is_err());
        assert!(ensure_safe_ref("head", "--upload-pack=evil").is_err());
        // Legitimate refs (including the empty head = working-tree mode) pass.
        assert!(ensure_safe_ref("base", "origin/main").is_ok());
        assert!(ensure_safe_ref("base", "HEAD~3").is_ok());
        assert!(ensure_safe_ref("base", "v1.2.3").is_ok());
        assert!(ensure_safe_ref("head", "").is_ok());
        assert!(ensure_safe_ref("base", "@{upstream}").is_ok());
    }

    #[test]
    fn rename_with_spaces_recovers_new_path() {
        // #270: a pure rename (100% similarity) carries no `+++`/`@@`; a path
        // with spaces breaks `parse_b_path`'s whitespace split, so the
        // `rename to` line must recover the new path.
        let raw = "diff --git a/old name.rs b/new name.rs\nsimilarity index 100%\nrename from old name.rs\nrename to new name.rs\n";
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new name.rs");
        assert_eq!(files[0].status, ChangeStatus::Renamed);
    }
}
