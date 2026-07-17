//! #233 — typed exit-code contract (0 / 2 / 70).
//!
//! GroundGraph's process exit codes follow a small, documented contract so
//! shell scripts can distinguish "you wrote the invocation wrong" from
//! "something broke internally":
//!
//! - 0 success.
//! - 2 user error: invalid argument, missing file/config/database, check or
//!   doctor findings, partial index. Matches clap's parse-error exit code so
//!   "user input wrong" is uniform whether clap or a runner caught it.
//! - 70 internal failure (EX_SOFTWARE from sysexits.h).

use assert_cmd::Command;

#[test]
fn clap_parse_error_exits_2() {
    // An unknown flag is rejected by clap at parse time. clap's own parse
    // errors already exit 2; this pins the contract so a future "helpful"
    // change cannot silently regress it to 1.
    Command::cargo_bin("groundgraph")
        .unwrap()
        .arg("--definitely-not-a-real-flag")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn unknown_subcommand_exits_2() {
    Command::cargo_bin("groundgraph")
        .unwrap()
        .arg("not-a-real-subcommand")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn uninitialized_workspace_exits_2_not_1() {
    // Running a query in a directory with no `.groundgraph.yaml` is the
    // canonical "forgot to `groundgraph init`" user error → the engine
    // raises `NoWorkspace` (UserInput) → exit 2, never the legacy exit 1.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "anything"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn operational_store_failure_exits_70() {
    // graph-diff pointed at a path on a read-only filesystem fails inside
    // the SQLite layer (Operational). The user cannot fix this by changing
    // arguments, so it is the internal exit code 70 — proving the contract
    // distinguishes user errors (2) from operational/internal ones (70).
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args([
            "graph-diff",
            "--base-db",
            "/path/that/does/not/exist/base.db",
            "--head-db",
            "/path/that/does/not/exist/head.db",
        ])
        .assert()
        .failure()
        .code(70);
}

#[test]
fn candidate_show_unknown_id_exits_2() {
    // candidate-show's pre-existing not-found exit code (2) is preserved
    // verbatim under the new contract.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["candidate", "show", "no-such-candidate"])
        .assert()
        .failure()
        .code(2);
}

// Argument-validation failures must exit 2 (user error), not the internal 70
// they produced when they went through `anyhow::bail!`. Each test below maps
// to a `bail_user!` site; before the fix the assertions failed with code 70.

#[test]
fn search_without_a_query_exits_2() {
    // pick_query: no positional query / --code / --file supplied.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("search")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn search_with_mutually_exclusive_inputs_exits_2() {
    // pick_query: --code and --file (and a positional query) are mutually exclusive.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "--code", "x", "--file", "y"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn search_with_unknown_kind_exits_2() {
    // match_kind: --kind value is not in the alias / canonical table.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "foo", "--kind", "not-a-real-kind"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn search_with_file_but_no_line_exits_2() {
    // pick_query: --file requires its --line counterpart.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "--file", "lib/foo.rs"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn constants_with_unknown_kind_exits_2() {
    // parse_kind: --kind value is not int/float/str/bool/char.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["constants", "--kind", "blob"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn facts_with_unknown_purity_exits_2() {
    // parse_purity (via `facts`): --purity value is not pure/impure/unknown.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["facts", "--purity", "nope"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn purity_with_unknown_only_exits_2() {
    // parse_purity (via the `purity` subcommand): --only value is not pure/impure/unknown.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["purity", "--only", "nope"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn feature_pack_with_mutually_exclusive_selectors_exits_2() {
    // --path and --requirement are mutually exclusive.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["feature-pack", "--path", "a", "--requirement", "b"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn feature_pack_without_a_selector_exits_2() {
    // One of --path / --requirement is required.
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .arg("feature-pack")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn install_with_non_object_mcp_json_exits_2() {
    // read_json_object: a user-supplied mcp.json whose root is not an object
    // (here a JSON array) is a user-correctable config error.
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join(".cursor")).unwrap();
    std::fs::write(tmp.path().join(".cursor/mcp.json"), "[1, 2, 3]").unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["install", "--agent", "cursor", "--location", "local"])
        .assert()
        .failure()
        .code(2);
}
