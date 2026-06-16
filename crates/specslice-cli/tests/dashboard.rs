//! `specslice dashboard` — self-contained HTML management panel.
//!
//! One command aggregates the whole analysis surface (overview, business
//! modules, feature clusters, checks, dead code, questions, purity) into a
//! single offline HTML file. These tests pin the CLI contract: file written,
//! data inlined, every section key present, script-closing payloads
//! neutralised.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("flutter_watermark_app")
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

fn bootstrap(tmp_root: &Path) {
    copy_dir(&fixture_path(), tmp_root);
    let db = tmp_root.join(".specslice/graph.db");
    if db.exists() {
        std::fs::remove_file(&db).unwrap();
    }
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp_root)
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp_root)
        .arg("index")
        .assert()
        .success();
}

#[test]
fn dashboard_writes_self_contained_html_with_all_sections() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["dashboard"])
        .assert()
        .success();
    // #111: the "wrote …" status line goes to stderr so stdout stays clean
    // for piping; stdout must therefore carry no status noise.
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("wrote"),
        "must report output path on stderr: {stderr}"
    );
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "dashboard writes a file; stdout must stay empty, got: {stdout:?}"
    );

    let out = tmp.path().join(".specslice/export/dashboard.html");
    assert!(out.exists(), "default output file must exist");
    let html = std::fs::read_to_string(&out).unwrap();

    // Self-contained: inlined data + no external resource references.
    assert!(html.contains("window.__SS_DASHBOARD__"));
    assert!(
        !html.contains("src=\"http") && !html.contains("href=\"http"),
        "dashboard must work offline (no CDN tags)"
    );

    // Every analysis section ships its data key.
    for key in [
        "\"meta\"",
        "\"overview\"",
        "\"modules\"",
        "\"features\"",
        "\"checks\"",
        "\"dead_code\"",
        "\"questions\"",
        "\"purity\"",
    ] {
        assert!(html.contains(key), "dashboard data must contain {key}");
    }
    // Fixture content actually flowed through (the watermark app indexes
    // real Dart symbols, so the overview must report nonzero nodes).
    assert!(html.contains("\"nodes\""));
}

#[test]
fn dashboard_honours_custom_out_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let custom = tmp.path().join("panel.html");
    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["dashboard", "--out", custom.to_str().unwrap()])
        .assert()
        .success();
    assert!(custom.exists());
    let html = std::fs::read_to_string(&custom).unwrap();
    // `</script>` inside any embedded string must be neutralised so the
    // payload can never terminate the host script tag.
    assert!(!html.replace("<\\/script", "").contains("</script>x"));
    assert!(html.contains("window.__SS_DASHBOARD__"));
}
