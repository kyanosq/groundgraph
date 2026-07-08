//! CLI e2e tests for `groundgraph candidate` 与 `groundgraph logic`。
//!
//! 使用 pixcraft_iap fixture，因为它附带 `business_logic.yaml` 候选。

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("pixcraft_iap")
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
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp_root)
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp_root)
        .arg("index")
        .assert()
        .success();
}

#[test]
fn candidate_list_shows_chinese_pending_review_items() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["candidate", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("GroundGraph 候选审阅清单"),
        "缺少标题: {stdout}",
    );
    assert!(stdout.contains("待审"), "缺少 待审 字样: {stdout}");
    assert!(
        stdout.contains("complete_purchase_unlocks_pro"),
        "缺少 fixture 中的候选 id: {stdout}",
    );
    assert!(
        stdout.contains("待确认问题"),
        "应该呈现 open_questions: {stdout}",
    );
}

#[test]
fn candidate_show_renders_full_chinese_detail() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["candidate", "show", "restore_purchases_is_incomplete"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("候选:"), "缺少候选标题: {stdout}");
    assert!(stdout.contains("业务描述:"), "缺少业务描述段落: {stdout}",);
    assert!(stdout.contains("证据"), "缺少证据段落: {stdout}");
    assert!(
        stdout.contains("待确认问题"),
        "缺少待确认问题段落: {stdout}",
    );
}

#[test]
fn candidate_review_writes_status_and_round_trips_through_list() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "candidate",
            "review",
            "restore_purchases_is_incomplete",
            "--needs-changes",
            "--note",
            "先补单元测试",
            "--reviewer",
            "qjs",
            "--answer",
            "Is restoration handled elsewhere (e.g. on app start by reading Hive)?",
        ])
        .assert()
        .success()
        .stdout(contains("状态 = needs_changes"));

    let yaml_path = tmp
        .path()
        .join(".groundgraph/candidates/business_logic.yaml");
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    assert!(
        yaml.contains("status: needs_changes"),
        "YAML 应该包含新的 status:\n{yaml}",
    );
    assert!(
        yaml.contains("先补单元测试"),
        "YAML 应该包含 reviewer note:\n{yaml}",
    );

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["candidate", "show", "restore_purchases_is_incomplete"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("状态: 需补充"),
        "show 应该显示中文 需补充: {stdout}",
    );
    assert!(
        stdout.contains("先补单元测试"),
        "show 应该呈现 note: {stdout}"
    );
}

#[test]
fn candidate_review_requires_a_verdict_flag() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["candidate", "review", "restore_purchases_is_incomplete"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("--accept") && stderr.contains("--reject"),
        "应该提示必须给出 verdict: {stderr}",
    );
}

#[test]
fn candidate_review_unknown_id_fails_with_helpful_message() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "candidate",
            "review",
            "no_such_candidate",
            "--accept",
            "--note",
            "x",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("no_such_candidate"),
        "stderr 应该提到未知候选: {stderr}",
    );
}

#[test]
fn logic_reports_candidates_as_candidate_only_then_confirmed_link_after_review() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["logic"])
        .assert();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("GroundGraph 逻辑可信度报告"),
        "缺少标题: {stdout}",
    );
    assert!(
        stdout.contains("complete_purchase_unlocks_pro"),
        "应该提及候选 id: {stdout}",
    );
    assert!(
        stdout.contains("AI 候选 (未审阅)") || stdout.contains("候选"),
        "应该标注为 AI 候选: {stdout}",
    );

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "candidate",
            "review",
            "complete_purchase_unlocks_pro",
            "--accept",
            "--note",
            "确认",
        ])
        .assert()
        .success();

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["logic"])
        .assert();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("已确认"),
        "review 后 logic 报告应该出现 已确认 字样: {stdout}",
    );
}

#[test]
fn logic_only_risks_filters_out_confirmed_link() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "candidate",
            "review",
            "complete_purchase_unlocks_pro",
            "--accept",
        ])
        .assert()
        .success();

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["logic", "--only-risks", "--json"])
        .assert();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let report: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json should produce parseable JSON");
    for item in report["items"].as_array().unwrap() {
        let verdict = item["verdict"].as_str().unwrap();
        assert_ne!(
            verdict, "confirmed_link",
            "--only-risks 不应包含 confirmed_link: {item:?}",
        );
    }
}

#[test]
fn graph_accepts_include_candidates_false_and_hides_candidates() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap(tmp.path());

    let assert = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "graph",
            "--format",
            "json",
            "--view",
            "business",
            "--include-candidates=false",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let view: serde_json::Value = serde_json::from_str(&stdout).expect("graph json should parse");
    let nodes = view["nodes"].as_array().expect("nodes should be an array");
    assert!(
        nodes
            .iter()
            .all(|node| node["kind"].as_str() != Some("business_candidate")),
        "--include-candidates=false should hide every business_candidate node: {stdout}",
    );
}
