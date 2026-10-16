//! The binary reports the workspace version.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn version_flag_prints_the_crate_version() {
    Command::cargo_bin("alkahest")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(concat!("alkahest ", env!("CARGO_PKG_VERSION"))));
}
