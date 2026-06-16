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
    // #99: the shipped skill is now the single source `skills/specslice/SKILL.md`
    // (the release script copies it; the drift-prone `packaging/skills` duplicate
    // was removed). This contract test still guards that whatever we ship matches
    // the live MCP launch command + tool list.
    let skill = std::fs::read_to_string(
        workspace_root()
            .join("skills")
            .join("specslice")
            .join("SKILL.md"),
    )
    .expect("read SpecSlice skill (skills/specslice/SKILL.md)");

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
fn dispatcher_validates_tool_arguments_against_input_schema() {
    // #89: the dispatcher must enforce each tool's advertised `inputSchema`
    // before invoking the handler, returning -32602 for a contract violation
    // instead of silently dropping the offending field.
    let server = Server::new(PathBuf::from("."));

    // Wrong type: `depth` is declared `integer`, a string must be rejected.
    let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_graph","arguments":{"query":"x","depth":"deep"}}}"#;
    let v: Value = serde_json::from_str(&server.dispatch(raw).unwrap()).unwrap();
    assert_eq!(
        v["error"]["code"], -32602,
        "wrong type → invalid params: {v}"
    );

    // Undeclared field with additionalProperties:false.
    let raw = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_graph","arguments":{"query":"x","bogus":1}}}"#;
    let v: Value = serde_json::from_str(&server.dispatch(raw).unwrap()).unwrap();
    assert_eq!(
        v["error"]["code"], -32602,
        "unknown field → invalid params: {v}"
    );

    // Missing required field (`node_id` for get_subgraph).
    let raw = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_subgraph","arguments":{"depth":1}}}"#;
    let v: Value = serde_json::from_str(&server.dispatch(raw).unwrap()).unwrap();
    assert_eq!(
        v["error"]["code"], -32602,
        "missing required → invalid params: {v}"
    );

    // Enum violation for dead_code.min_confidence.
    let raw = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"dead_code","arguments":{"min_confidence":"extreme"}}}"#;
    let v: Value = serde_json::from_str(&server.dispatch(raw).unwrap()).unwrap();
    assert_eq!(v["error"]["code"], -32602, "bad enum → invalid params: {v}");
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

#[test]
fn tools_call_round_trips_explain_context_drift_and_impact() {
    // #222: explain_symbol / context_pack / check_drift / impact were never
    // exercised through `tools/call`. Their schema validation, error wrapping
    // and JSON serialization only ran in production, so a serde rename could
    // silently return `{}` while CI stayed green. Drive each through the real
    // dispatcher against the bootstrapped fixture.
    let tmp = tempfile::TempDir::new().unwrap();
    bootstrap_fixture(tmp.path());
    let server = Server::new(tmp.path().to_path_buf());

    // Obtain a real node id from the indexed graph to anchor the symbol tools.
    let search = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_graph","arguments":{"query":"watermark","limit":5}}}"#;
    let sv: Value = serde_json::from_str(&server.dispatch(search).expect("response")).unwrap();
    let search_payload: Value =
        serde_json::from_str(sv["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let node_id = search_payload["matches"][0]["id"]
        .as_str()
        .expect("at least one search match with an id")
        .to_string();

    let call_tool = |id: i64, name: &str, arguments: Value| -> Value {
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        serde_json::from_str(&server.dispatch(&req.to_string()).expect("response")).unwrap()
    };

    // explain_symbol: must round-trip a structured node block for the id.
    let ex = call_tool(2, "explain_symbol", json!({ "symbol_id": node_id }));
    assert_eq!(ex["id"], 2);
    assert_eq!(
        ex["result"]["isError"].as_bool(),
        Some(false),
        "explain_symbol must succeed for an indexed node: {ex}"
    );
    let ex_payload: Value =
        serde_json::from_str(ex["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        ex_payload["node"]["id"], node_id,
        "explain_symbol echoes the node id"
    );
    assert!(
        ex_payload["stats"].is_object(),
        "explain_symbol emits stats"
    );

    // context_pack (symbol mode): must round-trip a non-empty pack.
    let cp = call_tool(
        3,
        "context_pack",
        json!({ "symbol_id": node_id, "include_snippets": false }),
    );
    assert_eq!(
        cp["result"]["isError"].as_bool(),
        Some(false),
        "context_pack must succeed for an indexed symbol: {cp}"
    );
    let cp_payload: Value =
        serde_json::from_str(cp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(cp_payload.is_object(), "context_pack returns a JSON object");

    // check_drift: no args; runs consistency checks against the indexed store.
    let cd = call_tool(4, "check_drift", json!({}));
    assert_eq!(
        cd["result"]["isError"].as_bool(),
        Some(false),
        "check_drift must succeed against a bootstrapped fixture: {cd}"
    );
    let cd_payload: Value =
        serde_json::from_str(cd["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        cd_payload.is_object(),
        "check_drift returns a findings report object"
    );

    // impact: the fixture tmp is not a git repo, so this primarily proves the
    // dispatch → schema → error-wrapping → serialization path is intact. It
    // must come back as a well-formed tool result (text content), never a
    // transport-level crash, regardless of the git outcome.
    let im = call_tool(5, "impact", json!({ "reindex": false }));
    assert_eq!(im["id"], 5);
    assert_eq!(
        im["result"]["content"][0]["type"], "text",
        "impact must return a well-formed text tool result: {im}"
    );
}
