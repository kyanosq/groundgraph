//! Brand-level tests for the GroundGraph MCP binary.

use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

#[test]
fn groundgraph_mcp_binary_is_the_primary_server_name() {
    Command::cargo_bin("groundgraph-mcp")
        .expect("locate groundgraph-mcp binary")
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("groundgraph-mcp"))
        .stdout(contains("GroundGraph MCP"));
}

#[test]
fn cargo_manifest_does_not_ship_old_specslice_mcp_alias() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .expect("read groundgraph-mcp manifest");

    assert!(
        !manifest.contains("name = \"specslice-mcp\""),
        "groundgraph-mcp must not ship a specslice-mcp binary alias"
    );
    assert!(
        !manifest_dir.join("src/bin/specslice-mcp.rs").exists(),
        "old specslice-mcp binary entrypoint should be removed"
    );
}
