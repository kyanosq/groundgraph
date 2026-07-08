//! Atomic file writer for engine-authored artifacts (`.groundgraph/links.yaml`,
//! `business_logic.yaml` review results).
//!
//! These are user-facing products of expensive flows (AI candidate review,
//! manual link manifests). A plain `std::fs::write` truncates the target
//! before writing, so a crash / full disk / power loss mid-write leaves a
//! half-written YAML that fails to parse on the next `groundgraph index` —
//! and, for a review result, destroys work that took a full AI round to
//! produce. Write to a sibling temp file then `rename` into place so the
//! target is only ever replaced as a whole (issues2.md #71; mirrors the CLI
//! `commands::output::write_atomic` introduced for issues.md #17).

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Write `body` to `path` atomically (temp file beside the target + rename).
/// Parent directories are created if missing.
pub(crate) fn write_atomic(path: &Path, body: &str) -> Result<()> {
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
    // Durability: flush the temp file's data to disk *before* the rename. The
    // module promises power-loss safety, but rename without a prior fsync can,
    // after a crash, surface the renamed target pointing at unwritten blocks
    // (zero-length / truncated file). `sync_all` closes that window.
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("fsync temp file for {}", path.display()))?;
    tmp.persist(path)
        .with_context(|| format!("moving temp file into {}", path.display()))?;
    // fsync the parent directory so the rename (a directory metadata change) is
    // itself durable. Best-effort: some platforms refuse to open a directory as
    // a file, and a missing dir-fsync only weakens durability, never content.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_atomic;

    #[test]
    fn writes_content_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/links.yaml");
        write_atomic(&target, "requirements: {}\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "requirements: {}\n"
        );
    }

    #[test]
    fn overwrites_without_leaving_temp_residue() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("business_logic.yaml");
        write_atomic(&target, "v1").unwrap();
        write_atomic(&target, "v2").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "no temp residue: {entries:?}");
    }
}
