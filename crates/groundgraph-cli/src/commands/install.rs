use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::exit_code::bail_user;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTarget {
    Cursor,
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallLocation {
    Local,
    Global,
}

#[derive(Debug, Clone)]
pub struct InstallRunArgs {
    pub agents: Vec<AgentTarget>,
    pub location: InstallLocation,
    pub dry_run: bool,
    pub auto_allow: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileAction {
    Created,
    Updated,
    Unchanged,
    Skipped,
}

#[derive(Debug, Clone)]
struct FileReport {
    path: PathBuf,
    action: FileAction,
    note: Option<String>,
}

pub fn run(repo_root: &Path, args: InstallRunArgs) -> Result<()> {
    let repo_root = absolute_repo_root(repo_root)?;
    let mut reports = Vec::new();

    for agent in &args.agents {
        match (*agent, args.location) {
            (AgentTarget::Cursor, InstallLocation::Local) => {
                reports.push(write_mcp_json(
                    &repo_root.join(".cursor/mcp.json"),
                    &mcp_server_config(vec![
                        "--repo-root".to_string(),
                        repo_root.to_string_lossy().into_owned(),
                    ]),
                    args.dry_run,
                )?);
            }
            (AgentTarget::Cursor, InstallLocation::Global) => {
                let home = home_dir()?;
                reports.push(write_mcp_json(
                    &home.join(".cursor/mcp.json"),
                    &mcp_server_config(vec![
                        "--repo-root".to_string(),
                        "${workspaceFolder}".to_string(),
                    ]),
                    args.dry_run,
                )?);
            }
            (AgentTarget::Claude, InstallLocation::Local) => {
                reports.push(write_mcp_json(
                    &repo_root.join(".mcp.json"),
                    &mcp_server_config(vec![
                        "--repo-root".to_string(),
                        repo_root.to_string_lossy().into_owned(),
                    ]),
                    args.dry_run,
                )?);
                if args.auto_allow {
                    reports.push(write_claude_permissions(
                        &repo_root.join(".claude/settings.json"),
                        args.dry_run,
                    )?);
                }
            }
            (AgentTarget::Claude, InstallLocation::Global) => {
                let home = home_dir()?;
                reports.push(write_mcp_json(
                    &home.join(".claude.json"),
                    &mcp_server_config(Vec::new()),
                    args.dry_run,
                )?);
                if args.auto_allow {
                    reports.push(write_claude_permissions(
                        &home.join(".claude/settings.json"),
                        args.dry_run,
                    )?);
                }
            }
            (AgentTarget::Codex, InstallLocation::Local) => {
                reports.push(FileReport {
                    path: repo_root.join(".codex/config.toml"),
                    action: FileAction::Skipped,
                    note: Some(
                        "Codex CLI does not support project-local MCP config; use --location global"
                            .to_string(),
                    ),
                });
            }
            (AgentTarget::Codex, InstallLocation::Global) => {
                let home = home_dir()?;
                reports.push(write_codex_toml(&home.join(".codex/config.toml"), args.dry_run)?);
            }
        }
    }

    print_report(args.location, args.dry_run, &reports);
    Ok(())
}

fn absolute_repo_root(repo_root: &Path) -> Result<PathBuf> {
    let path = if repo_root.is_absolute() {
        repo_root.to_path_buf()
    } else {
        env::current_dir()
            .context("resolving current directory for --repo-root")?
            .join(repo_root)
    };
    path.canonicalize()
        .with_context(|| format!("canonicalizing repo root {}", path.display()))
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("HOME/USERPROFILE is not set; cannot write global agent config")
}

fn mcp_server_config(args: Vec<String>) -> Value {
    json!({
        "type": "stdio",
        "command": "groundgraph-mcp",
        "args": args,
    })
}

fn write_mcp_json(path: &Path, server_config: &Value, dry_run: bool) -> Result<FileReport> {
    let existed = path.exists();
    let mut value = read_json_object(path)?;
    let root = value
        .as_object_mut()
        .context("MCP JSON root must be an object")?;
    let mcp_servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("`mcpServers` in {} must be an object", path.display()))?;

    let before = mcp_servers.get("groundgraph");
    let action = if before == Some(server_config) {
        FileAction::Unchanged
    } else if existed {
        FileAction::Updated
    } else {
        FileAction::Created
    };
    if action != FileAction::Unchanged && !dry_run {
        mcp_servers.insert("groundgraph".to_string(), server_config.clone());
        write_json(path, &value)?;
    }
    Ok(FileReport {
        path: path.to_path_buf(),
        action,
        note: None,
    })
}

fn write_claude_permissions(path: &Path, dry_run: bool) -> Result<FileReport> {
    let existed = path.exists();
    let mut value = read_json_object(path)?;
    let root = value
        .as_object_mut()
        .context("Claude settings JSON root must be an object")?;
    let permissions = root
        .entry("permissions")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("`permissions` in {} must be an object", path.display()))?;
    let allow = permissions
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .with_context(|| format!("`permissions.allow` in {} must be an array", path.display()))?;

    let before = allow.clone();
    for permission in groundgraph_permissions() {
        let value = Value::String(permission.to_string());
        if !allow.contains(&value) {
            allow.push(value);
        }
    }

    let action = if *allow == before {
        FileAction::Unchanged
    } else if existed {
        FileAction::Updated
    } else {
        FileAction::Created
    };
    if action != FileAction::Unchanged && !dry_run {
        write_json(path, &value)?;
    }
    Ok(FileReport {
        path: path.to_path_buf(),
        action,
        note: Some("Claude auto-allow permissions".to_string()),
    })
}

fn groundgraph_permissions() -> &'static [&'static str] {
    &[
        "mcp__groundgraph__search_graph",
        "mcp__groundgraph__explain_symbol",
        "mcp__groundgraph__get_subgraph",
        "mcp__groundgraph__context_pack",
        "mcp__groundgraph__impact",
        "mcp__groundgraph__check_drift",
        "mcp__groundgraph__dead_code",
    ]
}

fn read_json_object(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let body =
        fs::read_to_string(path).with_context(|| format!("reading JSON config {}", path.display()))?;
    let value: Value = serde_json::from_str(&body)
        .with_context(|| format!("parsing JSON config {}", path.display()))?;
    if !value.is_object() {
        // exit 2: user input — config file shape is wrong; user can fix it.
        bail_user!("JSON config {} must contain an object", path.display());
    }
    Ok(value)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)? + "\n";
    atomic_write(path, &content)
}

fn write_codex_toml(path: &Path, dry_run: bool) -> Result<FileReport> {
    let existed = path.exists();
    let current = if existed {
        fs::read_to_string(path)
            .with_context(|| format!("reading Codex TOML config {}", path.display()))?
    } else {
        String::new()
    };
    let block = build_codex_toml_block();
    let (next, changed) = upsert_toml_table(&current, "mcp_servers.groundgraph", &block);
    let action = if !changed {
        FileAction::Unchanged
    } else if existed {
        FileAction::Updated
    } else {
        FileAction::Created
    };
    if changed && !dry_run {
        atomic_write(path, &next)?;
    }
    Ok(FileReport {
        path: path.to_path_buf(),
        action,
        note: None,
    })
}

fn build_codex_toml_block() -> String {
    [
        "[mcp_servers.groundgraph]".to_string(),
        format!("command = {}", toml_string("groundgraph-mcp")),
        "args = []".to_string(),
    ]
    .join("\n")
}

fn upsert_toml_table(current: &str, table: &str, block: &str) -> (String, bool) {
    let header = format!("[{table}]");
    let lines: Vec<&str> = current.lines().collect();
    let Some(start) = lines.iter().position(|line| line.trim() == header) else {
        let mut next = current.trim_end().to_string();
        if !next.is_empty() {
            next.push_str("\n\n");
        }
        next.push_str(block);
        next.push('\n');
        return (next, true);
    };

    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(idx, line)| {
            let trimmed = line.trim();
            (trimmed.starts_with('[') && trimmed.ends_with(']')).then_some(idx)
        })
        .unwrap_or(lines.len());
    let existing = lines[start..end].join("\n");
    if existing.trim_end() == block {
        return (current.trim_end().to_string() + "\n", false);
    }

    let mut next_lines = Vec::new();
    next_lines.extend_from_slice(&lines[..start]);
    next_lines.extend(block.lines());
    next_lines.extend_from_slice(&lines[end..]);
    (next_lines.join("\n").trim_end().to_string() + "\n", true)
}

fn toml_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

fn atomic_write(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory {}", parent.display()))?;
    }
    let tmp = path.with_extension(format!(
        "{}tmp{}",
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!("{ext}."))
            .unwrap_or_default(),
        std::process::id()
    ));
    fs::write(&tmp, content).with_context(|| format!("writing temp file {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| {
        format!(
            "renaming temp config {} to {}",
            tmp.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn print_report(location: InstallLocation, dry_run: bool, reports: &[FileReport]) {
    let location = match location {
        InstallLocation::Local => "local",
        InstallLocation::Global => "global",
    };
    println!("GroundGraph agent install ({location})");
    if dry_run {
        println!("dry run: no files written");
    }
    for report in reports {
        let action = match (dry_run, report.action) {
            (_, FileAction::Unchanged) => "unchanged",
            (_, FileAction::Skipped) => "skipped",
            (true, FileAction::Created) => "would create",
            (true, FileAction::Updated) => "would update",
            (false, FileAction::Created) => "created",
            (false, FileAction::Updated) => "updated",
        };
        if let Some(note) = &report.note {
            println!("  {action:<12} {} ({note})", report.path.display());
        } else {
            println!("  {action:<12} {}", report.path.display());
        }
    }
}
