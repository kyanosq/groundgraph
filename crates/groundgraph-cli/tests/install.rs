//! CLI e2e tests for `groundgraph install`.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::json;

#[test]
fn install_local_writes_cursor_and_claude_mcp_configs_preserving_siblings() {
    let repo = tempfile::TempDir::new().unwrap();
    let cursor_config = repo.path().join(".cursor/mcp.json");
    fs::create_dir_all(cursor_config.parent().unwrap()).unwrap();
    fs::write(
        &cursor_config,
        serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "other": { "command": "other-mcp" }
            }
        }))
        .unwrap()
            + "\n",
    )
    .unwrap();

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(repo.path())
        .args(["install", "--agent", "cursor,claude"])
        .assert()
        .success()
        .stdout(contains(".cursor/mcp.json"))
        .stdout(contains(".mcp.json"));

    let repo_root = repo.path().canonicalize().unwrap();
    let repo_arg = repo_root.to_string_lossy();

    let cursor: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cursor_config).unwrap()).unwrap();
    assert_eq!(
        cursor["mcpServers"]["groundgraph"]["command"],
        "groundgraph-mcp"
    );
    assert_eq!(
        cursor["mcpServers"]["groundgraph"]["args"],
        json!(["--repo-root", repo_arg.as_ref()])
    );
    assert_eq!(cursor["mcpServers"]["other"]["command"], "other-mcp");

    let claude_config = repo.path().join(".mcp.json");
    let claude: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claude_config).unwrap()).unwrap();
    assert_eq!(
        claude["mcpServers"]["groundgraph"]["command"],
        "groundgraph-mcp"
    );
    assert_eq!(
        claude["mcpServers"]["groundgraph"]["args"],
        json!(["--repo-root", repo_arg.as_ref()])
    );
}

#[test]
fn install_dry_run_reports_targets_without_writing_files() {
    let repo = tempfile::TempDir::new().unwrap();

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(repo.path())
        .args(["install", "--agent", "cursor,claude", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("dry run"))
        .stdout(contains(".cursor/mcp.json"))
        .stdout(contains(".mcp.json"));

    assert!(!repo.path().join(".cursor/mcp.json").exists());
    assert!(!repo.path().join(".mcp.json").exists());
}

#[test]
fn install_global_codex_writes_codex_toml_under_home() {
    let repo = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();

    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(repo.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .args(["install", "--location", "global", "--agent", "codex"])
        .assert()
        .success()
        .stdout(contains(".codex/config.toml"));

    let toml = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
    assert!(toml.contains("[mcp_servers.groundgraph]"));
    assert!(toml.contains("command = \"groundgraph-mcp\""));
}
