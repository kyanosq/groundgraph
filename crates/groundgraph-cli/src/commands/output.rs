//! Shared atomic writer for command outputs (reports, HTML, packs).

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Output format for commands that emit either human-readable text or JSON
/// (#115). Replaces the per-command bare-`String` + runtime `bail!` with a
/// single clap `ValueEnum`, so an invalid value is rejected at parse time
/// (exit 2 under the #233 contract) instead of reaching a per-command
/// runtime `bail!` (the legacy exit 1 / 70 with an ad-hoc message).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextJsonFormat {
    /// Human-readable text (the default for every command that uses this).
    Text,
    /// JSON for agents / scripts.
    Json,
}

/// Write `body` to `path` atomically: parent directories are created,
/// content goes to a sibling temp file first, then an OS `rename` swaps
/// it into place. A crash (or full disk) mid-write can therefore never
/// leave a truncated report where a previous good one stood
/// (issues.md #17).
pub fn write_atomic(path: &Path, body: &str) -> Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => {
            std::fs::create_dir_all(p)
                .with_context(|| format!("creating parent of {}", path.display()))?;
            p
        }
        _ => Path::new("."),
    };
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temp file beside {}", path.display()))?;
    tmp.write_all(body.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    tmp.persist(path)
        .with_context(|| format!("moving temp file into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_atomic;

    #[test]
    fn writes_content_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/report.html");
        write_atomic(&target, "<html>中文</html>").unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "<html>中文</html>"
        );
    }

    #[test]
    fn overwrites_existing_file_and_leaves_no_temp_residue() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.json");
        write_atomic(&target, "v1").unwrap();
        write_atomic(&target, "v2").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "no temp files may linger after a successful write: {entries:?}"
        );
    }
}
