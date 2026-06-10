//! Integration tests for the SpecSlice MCP server.
//!
//! Two layers of coverage:
//!
//! 1. **In-process dispatcher** (`Server::dispatch`) — fast, no fixture
//!    bootstrap. Verifies `initialize`, `tools/list` and that an invalid
//!    tools/call shape returns the expected error envelope.
//! 2. **End-to-end binary** — pipe a small batch of JSON-RPC messages
//!    through the real `specslice-mcp` binary against the
//!    `flutter_watermark_app` fixture and assert tool results round-trip.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::{json, Value};
use specslice_mcp::server::Server;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn fixture_path() -> PathBuf {
    workspace_root()
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

fn bootstrap_fixture(tmp_root: &Path) {
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
fn dispatcher_initialize_reports_server_info() {
    let server = Server::new(PathBuf::from("."));
    let raw = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let response_line = server.dispatch(raw).expect("response expected");
    let value: Value = serde_json::from_str(&response_line).unwrap();
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], 1);
    let result = &value["result"];
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert_eq!(result["serverInfo"]["name"], "specslice-mcp");
    assert!(result["capabilities"]["tools"].is_object());
}

#[test]
fn dispatcher_notifications_initialized_returns_no_response() {
    let server = Server::new(PathBuf::from("."));
    let raw = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
    let resp = server.dispatch(raw);
    assert!(
        resp.is_none(),
        "notifications must not produce a response, got {resp:?}"
    );
}

#[test]
fn dispatcher_tools_list_advertises_seven_tools_with_input_schemas() {
    let server = Server::new(PathBuf::from("."));
    let raw = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}"#;
    let response_line = server.dispatch(raw).expect("response expected");
    let value: Value = serde_json::from_str(&response_line).unwrap();
    let tools = value["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    let expected = [
        "search_graph",
        "get_subgraph",
        "explain_symbol",
        "impact",
        "dead_code",
        "context_pack",
        "check_drift",
    ];
    assert_eq!(names, expected, "tools list must match the canonical order");
    for tool in tools {
        assert!(
            tool["inputSchema"]["type"].as_str() == Some("object"),
            "tool {} missing JSON Schema",
            tool["name"]
        );
    }
}

#[test]
fn packaged_skill_matches_mcp_launch_and_tool_contract() {
    let skill = std::fs::read_to_string(
        workspace_root()
            .join("packaging")
            .join("skills")
            .join("specslice")
            .join("SKILL.md"),
    )
    .expect("read packaged SpecSlice skill");

    assert!(
        skill.contains("specslice-mcp --repo-root /path/to/repo"),
        "Skill must document the real MCP binary launch command"
    );
    assert!(
        !skill.contains("specslice --repo-root /path/to/repo mcp serve"),
        "Skill must not document a non-existent `specslice mcp serve` CLI command"
    );
    assert!(
        !skill.contains("candidate_*"),
        "Skill must not advertise candidate_* MCP tools; candidates are exposed through context_pack/explain_symbol"
    );
    assert!(
        skill.contains(".specslice.yaml"),
        "Skill must point Swift/Go users at the root .specslice.yaml config"
    );
    assert!(
        !skill.contains(".specslice/config.yaml"),
        "Skill must not point users at a stale .specslice/config.yaml path"
    );

    let server = Server::new(PathBuf::from("."));
    let raw = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}"#;
    let response_line = server.dispatch(raw).expect("response expected");
    let value: Value = serde_json::from_str(&response_line).unwrap();
    for name in value["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
    {
        assert!(
            skill.contains(name),
            "Skill must mention advertised MCP tool `{name}`"
        );
    }
}

#[test]
fn dispatcher_method_not_found_returns_jsonrpc_error_envelope() {
    let server = Server::new(PathBuf::from("."));
    let raw = r#"{"jsonrpc":"2.0","id":42,"method":"does/not/exist","params":{}}"#;
    let response_line = server.dispatch(raw).expect("response expected");
    let value: Value = serde_json::from_str(&response_line).unwrap();
    assert_eq!(value["id"], 42);
    assert_eq!(value["error"]["code"], -32601);
}

#[test]
fn dispatcher_unknown_tool_returns_invalid_params() {
    let server = Server::new(PathBuf::from("."));
    let raw = r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#;
    let response_line = server.dispatch(raw).expect("response expected");
    let value: Value = serde_json::from_str(&response_line).unwrap();
    assert_eq!(value["id"], 11);
    assert_eq!(value["error"]["code"], -32602);
}

#[test]
fn dispatcher_tool_error_is_returned_as_is_error_content() {
    // Calling `search_graph` against a non-existent workspace should
    // wrap the failure as a tool result with `isError: true`, not as a
    // transport-level JSON-RPC error.
    let server = Server::new(PathBuf::from("/this/path/should/not/exist/specslice-mcp"));
    let raw = r#"{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"search_graph","arguments":{"query":"login"}}}"#;
    let response_line = server.dispatch(raw).expect("response expected");
    let value: Value = serde_json::from_str(&response_line).unwrap();
    assert_eq!(value["id"], 99);
    assert!(value["result"]["isError"].as_bool() == Some(true));
    let text = value["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    assert!(text.contains("specslice init") || text.contains("no SpecSlice workspace"));
}

#[test]
fn end_to_end_initialize_list_search_against_watermark_fixture() {
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap_fixture(tmp.path());

    let mut input = String::new();
    input.push_str(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    input.push('\n');
    input.push_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#);
    input.push('\n');
    input.push_str(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
    input.push('\n');
    let search_call = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "search_graph",
            "arguments": { "query": "watermark", "depth": 1, "limit": 5 }
        }
    });
    writeln!(&mut input, "{}", search_call).unwrap();

    let dead_code_call = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "dead_code",
            "arguments": { "min_confidence": "low" }
        }
    });
    writeln!(&mut input, "{}", dead_code_call).unwrap();

    let out = Command::cargo_bin("specslice-mcp")
        .unwrap()
        .arg("--repo-root")
        .arg(tmp.path())
        .write_stdin(input)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    // initialize, tools/list, tools/call ×2 → 4 responses.
    assert_eq!(lines.len(), 4, "expected 4 response lines, got: {stdout}");

    let init_resp: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["protocolVersion"], "2024-11-05");

    let list_resp: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(list_resp["id"], 2);
    let tool_names: Vec<&str> = list_resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(tool_names.contains(&"search_graph"));
    assert!(tool_names.contains(&"context_pack"));

    let search_resp: Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(search_resp["id"], 3);
    let content = &search_resp["result"]["content"][0];
    assert_eq!(content["type"], "text");
    assert_eq!(
        search_resp["result"]["isError"].as_bool(),
        Some(false),
        "search_graph should succeed against bootstrapped fixture"
    );
    let payload: Value = serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
    assert_eq!(payload["query"], "watermark");
    let matches = payload["matches"].as_array().expect("matches array");
    assert!(
        !matches.is_empty(),
        "watermark query must hit at least one symbol/doc in the fixture"
    );

    let dead_resp: Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(dead_resp["id"], 4);
    assert_eq!(
        dead_resp["result"]["isError"].as_bool(),
        Some(false),
        "dead_code tool must succeed against bootstrapped fixture"
    );
    let dead_payload: Value =
        serde_json::from_str(dead_resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(dead_payload["stats"].is_object());
    assert!(dead_payload["candidates"].is_array());
}
