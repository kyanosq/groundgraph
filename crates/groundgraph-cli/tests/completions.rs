//! #113 — `groundgraph completions <shell>` emits a shell-completion script
//! for bash / zsh / fish / powershell / elvish. Each script must be non-empty
//! and reference the subcommands so a user actually gets completion candidates.

use assert_cmd::Command;
use tempfile::TempDir;

fn completion_stdout(shell: &str) -> String {
    let tmp = TempDir::new().unwrap();
    let output = Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["completions", shell])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap()
}

#[test]
fn bash_completion_is_nonempty_and_lists_subcommands() {
    let s = completion_stdout("bash");
    assert!(!s.is_empty());
    assert!(
        s.contains("index"),
        "bash completions should list `index`: {s}"
    );
}

#[test]
fn zsh_completion_is_nonempty_and_lists_subcommands() {
    let s = completion_stdout("zsh");
    assert!(!s.is_empty());
    assert!(
        s.contains("index"),
        "zsh completions should list `index`: {s}"
    );
}

#[test]
fn fish_completion_is_nonempty_and_lists_subcommands() {
    let s = completion_stdout("fish");
    assert!(!s.is_empty());
    assert!(
        s.contains("index"),
        "fish completions should list `index`: {s}"
    );
}

#[test]
fn powershell_completion_is_nonempty_and_lists_subcommands() {
    let s = completion_stdout("powershell");
    assert!(!s.is_empty());
    assert!(
        s.contains("index"),
        "powershell completions should list `index`: {s}"
    );
}

#[test]
fn elvish_completion_is_nonempty_and_lists_subcommands() {
    let s = completion_stdout("elvish");
    assert!(!s.is_empty());
    assert!(
        s.contains("index"),
        "elvish completions should list `index`: {s}"
    );
}

#[test]
fn unknown_shell_rejected_with_exit_2() {
    let tmp = TempDir::new().unwrap();
    Command::cargo_bin("groundgraph")
        .unwrap()
        .current_dir(tmp.path())
        .args(["completions", "tcsh"])
        .assert()
        .failure()
        .code(2);
}
