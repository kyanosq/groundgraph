//! P6 — `specslice search --format html` CLI integration test.
//!
//! Uses the watermark fixture (no Dart sidecar required) to exercise
//! the search-driven HTML reader end-to-end through the real binary so
//! the flag parsing, file writing and template wiring stay covered.

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
fn search_html_writes_self_contained_reader_to_default_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "watermark", "--format", "html"])
        .assert()
        .success();

    let html_path = tmp.path().join(".specslice/export/search-watermark.html");
    assert!(
        html_path.exists(),
        "default HTML output path must exist: {}",
        html_path.display()
    );
    let html = std::fs::read_to_string(&html_path).unwrap();

    // Self-contained: no remote URLs. The W3C SVG namespace
    // (`http://www.w3.org/2000/svg`) is an *identifier* required by
    // `document.createElementNS` — browsers never fetch it — so we
    // strip it out before scanning for live URLs.
    let scan = html
        .replace("http://www.w3.org/2000/svg", "")
        .replace("http://www.w3.org/1999/xhtml", "");
    assert!(
        !scan.contains("https://") && !scan.contains("http://") && !scan.contains("//cdn."),
        "rendered HTML must not reference remote URLs"
    );
    // Reader scaffolding present.
    assert!(html.contains("SpecSlice Search"));
    assert!(html.contains("specslice-search-data"));
    // The query and at least one default-Chinese badge must be embedded.
    assert!(html.contains("watermark"));
    assert!(
        html.contains("命中") || html.contains("代码") || html.contains("文档"),
        "Chinese reader chrome must be present"
    );
}

#[test]
fn search_html_respects_explicit_output_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());
    let out = tmp.path().join("custom/search.html");

    Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "search",
            "watermark",
            "--format",
            "html",
            "--output",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(out.exists());
    assert!(std::fs::read_to_string(&out)
        .unwrap()
        .contains("SpecSlice Search"));
}

#[test]
fn search_text_format_default_keeps_human_output_compatible() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let out = Command::cargo_bin("specslice")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "watermark"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SpecSlice search"));
    assert!(stdout.contains("查询: watermark"));
}
