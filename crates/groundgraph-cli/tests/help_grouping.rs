//! #128 — `--help` groups the 36+ subcommands by category and the
//! high-frequency commands carry an `Examples:` block.

use assert_cmd::Command;

#[test]
fn top_help_groups_subcommands_by_category() {
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let help = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        help.contains("by category"),
        "should show a category index: {help}"
    );
    assert!(help.contains("Setup"), "Setup group missing: {help}");
    assert!(help.contains("Query"), "Query group missing: {help}");
    assert!(help.contains("Graph"), "Graph group missing: {help}");
    assert!(help.contains("Business"), "Business group missing: {help}");
    assert!(
        help.contains("Migration"),
        "Migration group missing: {help}"
    );
}

#[test]
fn index_help_has_examples() {
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .args(["index", "--help"])
        .assert()
        .success();
    let help = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        help.contains("Examples"),
        "index --help should show an Examples block: {help}"
    );
    assert!(
        help.contains("groundgraph index"),
        "the examples should reference the command: {help}"
    );
}

#[test]
fn search_help_has_examples() {
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .args(["search", "--help"])
        .assert()
        .success();
    let help = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        help.contains("Examples"),
        "search --help should show an Examples block: {help}"
    );
}

#[test]
fn impact_help_has_examples() {
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .args(["impact", "--help"])
        .assert()
        .success();
    let help = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        help.contains("Examples"),
        "impact --help should show an Examples block: {help}"
    );
}

#[test]
fn top_help_lists_environment_variables() {
    // #234 — the Environment section is generated from env::REGISTRY, so a
    // representative user-facing variable must appear on the top-level --help.
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let help = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        help.contains("Environment"),
        "--help should carry an Environment section: {help}"
    );
    assert!(
        help.contains("GROUNDGRAPH_TIMING"),
        "--help should list GROUNDGRAPH_TIMING: {help}"
    );
    assert!(
        help.contains("GROUNDGRAPH_PARSE_BUDGET_MS"),
        "--help should list GROUNDGRAPH_PARSE_BUDGET_MS: {help}"
    );
    // Test-only variables stay hidden from user-facing --help.
    assert!(
        !help.contains("GROUNDGRAPH_GOLDEN_REQUIRED"),
        "test-only GROUNDGRAPH_GOLDEN_REQUIRED must not appear on --help: {help}"
    );
}

#[test]
fn top_help_admits_verbose_and_quiet_global_flags() {
    // #127 — `-v` / `-q` are global flags so they appear on the top-level
    // --help Options block regardless of the chosen subcommand.
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let help = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        help.contains("--verbose"),
        "--help should document --verbose: {help}"
    );
    assert!(
        help.contains("--quiet"),
        "--help should document --quiet: {help}"
    );
}
