//! `alkahest completions <shell>` prints a script and rejects unknown shells.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn bash_completions_are_printed() {
    Command::cargo_bin("alkahest")
        .unwrap()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(contains("_alkahest"));
}

#[test]
fn unknown_shell_is_a_usage_error() {
    Command::cargo_bin("alkahest")
        .unwrap()
        .args(["completions", "nosuchshell"])
        .assert()
        .code(2);
}
