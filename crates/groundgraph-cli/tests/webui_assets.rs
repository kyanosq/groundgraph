//! Drift guard: the crate-local copies of the webui assets (embedded via
//! `include_str!` in `commands/graph.rs` so `cargo package` works) must stay
//! byte-identical to the `webui/` sources of truth. CI enforces this with
//! `scripts/sync_webui_assets.sh --check`; this test gives the same signal in
//! the dev loop (`cargo test`) with an actionable message.
//!
//! Skips silently when the repo-level `webui/` directory is absent (e.g. if
//! the test were ever compiled from the packaged crate, where only the
//! crate-local copies exist).

use std::path::{Path, PathBuf};

fn repo_webui() -> Option<PathBuf> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../webui");
    repo.is_dir().then_some(repo)
}

fn assert_copy_matches_source(rel: &str) {
    let Some(repo) = repo_webui() else {
        eprintln!("repo webui/ not present; skipping drift check for {rel}");
        return;
    };
    let source = std::fs::read(repo.join(rel)).expect("read webui source");
    let copy = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("webui")
            .join(rel),
    )
    .expect("read crate-local copy");
    assert_eq!(
        source, copy,
        "crate-local webui copy drifted from webui/{rel}; run `scripts/sync_webui_assets.sh`"
    );
}

#[test]
fn crate_local_viewer_template_matches_webui_source() {
    assert_copy_matches_source("index.html");
}

#[test]
fn crate_local_viewer_bundle_matches_webui_source() {
    assert_copy_matches_source("vendor/groundgraph-viewer.bundle.js");
}
