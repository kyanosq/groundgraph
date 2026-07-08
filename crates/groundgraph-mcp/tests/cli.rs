//! End-to-end CLI behaviour for the `groundgraph-mcp` binary.
//!
//! Before this, the server ignored every flag except `--repo-root` and
//! immediately entered the stdin-blocking JSON-RPC loop — so `groundgraph-mcp
//! --help` / `--version` either hung on an interactive terminal or printed
//! nothing. A long-lived stdio server still needs the two universal CLI
//! affordances; these tests pin them.

use assert_cmd::Command;
use predicates::str::contains;

/// `--help` must print usage to stdout and exit 0 *without* entering server
/// mode. We feed empty stdin so a regression (falling through to the server)
/// can't hang the test — it would instead fail the `Usage` assertion.
#[test]
fn help_flag_prints_usage_and_exits_zero() {
    Command::cargo_bin("groundgraph-mcp")
        .unwrap()
        .arg("--help")
        .write_stdin("")
        .assert()
        .success()
        .stdout(contains("Usage"))
        .stdout(contains("--repo-root"));
}

#[test]
fn short_help_flag_is_supported() {
    Command::cargo_bin("groundgraph-mcp")
        .unwrap()
        .arg("-h")
        .write_stdin("")
        .assert()
        .success()
        .stdout(contains("Usage"));
}

#[test]
fn version_flag_prints_version_and_exits_zero() {
    Command::cargo_bin("groundgraph-mcp")
        .unwrap()
        .arg("--version")
        .write_stdin("")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}
