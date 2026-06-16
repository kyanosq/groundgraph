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
//!
//! `--help`/`-h` and `--version`/`-V` short-circuit before any stdin read so
//! the binary behaves like an ordinary CLI when probed, instead of silently
//! dropping into the (blocking) JSON-RPC loop.

use std::env;
use std::io::{stdin, stdout, BufReader, BufWriter};
use std::path::PathBuf;
use std::process::ExitCode;

use specslice_mcp::server::Server;

const HELP: &str = "\
specslice-mcp — SpecSlice MCP (Model Context Protocol) stdio server

Usage: specslice-mcp [OPTIONS]

Serves newline-delimited JSON-RPC 2.0 over stdin/stdout (the standard local
MCP transport). Logs go to stderr; stdout carries only JSON-RPC envelopes.

Options:
      --repo-root <PATH>  Default workspace root to analyse. Falls back to
                          $SPECSLICE_REPO_ROOT, then the current directory.
                          Individual tool calls may override it per request.
  -h, --help              Print this help and exit.
  -V, --version           Print version and exit.

Environment:
  SPECSLICE_REPO_ROOT     Default workspace root when --repo-root is absent.
";

/// What the parsed command line asks the binary to do.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    /// Run the server. `repo_root_arg` is the explicit `--repo-root` value if
    /// one was given; env/cwd fallback is applied later (kept out of parsing so
    /// the parser stays pure and testable).
    Serve {
        repo_root_arg: Option<PathBuf>,
    },
}

/// Pure command-line interpreter. `--help`/`--version` take precedence the
/// moment they are seen, mirroring conventional CLI behaviour.
fn parse_invocation<I>(args: I) -> Invocation
where
    I: IntoIterator<Item = String>,
{
    let mut repo_root_arg: Option<PathBuf> = None;
    let mut iter = args.into_iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--help" | "-h" => return Invocation::Help,
            "--version" | "-V" => return Invocation::Version,
            "--repo-root" => {
                if let Some(v) = iter.next() {
                    repo_root_arg = Some(PathBuf::from(v));
                }
            }
            other if other.starts_with("--repo-root=") => {
                repo_root_arg = Some(PathBuf::from(other.trim_start_matches("--repo-root=")));
            }
            _ => {}
        }
    }
    Invocation::Serve { repo_root_arg }
}

fn main() -> ExitCode {
    match parse_invocation(env::args().skip(1)) {
        Invocation::Help => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Invocation::Version => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Invocation::Serve { repo_root_arg } => serve(repo_root_arg),
    }
}

fn serve(repo_root_arg: Option<PathBuf>) -> ExitCode {
    let repo_root = repo_root_arg
        .or_else(env_repo_root)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
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

/// `$SPECSLICE_REPO_ROOT` when set to a non-empty value.
fn env_repo_root() -> Option<PathBuf> {
    match env::var("SPECSLICE_REPO_ROOT") {
        Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Invocation {
        parse_invocation(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_serves_with_fallback_repo_root() {
        assert_eq!(
            parse(&[]),
            Invocation::Serve {
                repo_root_arg: None
            }
        );
    }

    #[test]
    fn help_and_version_flags_are_recognised() {
        assert_eq!(parse(&["--help"]), Invocation::Help);
        assert_eq!(parse(&["-h"]), Invocation::Help);
        assert_eq!(parse(&["--version"]), Invocation::Version);
        assert_eq!(parse(&["-V"]), Invocation::Version);
    }

    #[test]
    fn repo_root_is_captured_in_both_forms() {
        assert_eq!(
            parse(&["--repo-root", "/x"]),
            Invocation::Serve {
                repo_root_arg: Some(PathBuf::from("/x")),
            }
        );
        assert_eq!(
            parse(&["--repo-root=/y"]),
            Invocation::Serve {
                repo_root_arg: Some(PathBuf::from("/y")),
            }
        );
    }

    #[test]
    fn help_takes_precedence_over_a_preceding_repo_root() {
        // A probe like `specslice-mcp --repo-root /x --help` should print help,
        // not start serving.
        assert_eq!(parse(&["--repo-root", "/x", "--help"]), Invocation::Help);
    }
}
