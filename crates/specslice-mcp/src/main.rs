//! `specslice-mcp` — MCP stdio server entry point.
//!
//! Launch this binary with no arguments to serve newline-delimited
//! JSON-RPC 2.0 on stdin/stdout. The server resolves its default repo
//! root in the following order:
//!
//! 1. `--repo-root <path>` CLI flag.
//! 2. `SPECSLICE_REPO_ROOT` environment variable.
//! 3. The current working directory.
//!
//! Tool calls may override `repo_root` per-call. Logs go to stderr —
//! stdout is reserved for JSON-RPC envelopes.

use std::env;
use std::io::{stdin, stdout, BufReader, BufWriter};
use std::path::PathBuf;
use std::process::ExitCode;

use specslice_mcp::server::Server;

fn main() -> ExitCode {
    let repo_root = resolve_default_repo_root();
    eprintln!(
        "specslice-mcp: ready · protocol=2024-11-05 · repo_root={}",
        repo_root.display()
    );
    let server = Server::new(repo_root);
    let mut reader = BufReader::new(stdin().lock());
    let mut writer = BufWriter::new(stdout().lock());
    if let Err(err) = server.pump(&mut reader, &mut writer) {
        eprintln!("specslice-mcp: transport error: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn resolve_default_repo_root() -> PathBuf {
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--repo-root" => {
                if let Some(v) = args.next() {
                    return PathBuf::from(v);
                }
            }
            other if other.starts_with("--repo-root=") => {
                return PathBuf::from(other.trim_start_matches("--repo-root="));
            }
            _ => {}
        }
    }
    if let Ok(env_val) = env::var("SPECSLICE_REPO_ROOT") {
        if !env_val.is_empty() {
            return PathBuf::from(env_val);
        }
    }
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
