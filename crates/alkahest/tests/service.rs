//! `alkahest service install|uninstall|status` (ALK-US-0045, ALK-SP-0009). Nothing here touches
//! the real system: every run is `--dry-run` or confined to a temporary `--root`.
#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn alkahest(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("alkahest").unwrap();
    for (key, _) in std::env::vars() {
        if key.starts_with("ALKAHEST_") || key == "OLLAMA_MODELS" {
            cmd.env_remove(key);
        }
    }
    cmd.env("ALKAHEST_HOME", home);
    cmd
}

#[cfg(target_os = "linux")]
#[test]
fn dry_run_prints_the_unit_and_commands_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    alkahest(home.path())
        .args([
            "service",
            "install",
            "--dry-run",
            "--bin",
            "/usr/bin/alkahest",
        ])
        .assert()
        .success()
        .stdout(
            contains("# would write /etc/systemd/system/alkahest.service")
                .and(contains("Restart=always"))
                .and(contains("ExecStart=/usr/bin/alkahest serve"))
                .and(contains(
                    "# would run: systemctl enable --now alkahest.service",
                )),
        );
}

#[cfg(target_os = "linux")]
#[test]
fn system_install_into_a_root_writes_the_unit_and_runs_nothing() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("etc")).unwrap();
    std::fs::write(
        root.path().join("etc/group"),
        "render:x:105:\nvideo:x:44:\nollama:x:990:\n",
    )
    .unwrap();
    alkahest(home.path())
        .args([
            "service",
            "install",
            "--bin",
            "/opt/alkahest/bin/alkahest",
            "--root",
        ])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("# would run: useradd --system"));
    let unit =
        std::fs::read_to_string(root.path().join("etc/systemd/system/alkahest.service")).unwrap();
    assert!(unit.contains("User=alkahest"), "{unit}");
    assert!(
        unit.contains("SupplementaryGroups=render video ollama"),
        "{unit}"
    );
    assert!(
        unit.contains("ExecStart=/opt/alkahest/bin/alkahest serve"),
        "{unit}"
    );
    assert!(unit.contains("Restart=always"), "{unit}");

    // Status reads the unit file under the root; uninstall removes it.
    alkahest(home.path())
        .args(["service", "status", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("installed"));
    alkahest(home.path())
        .args(["service", "uninstall", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("removed"));
    assert!(
        !root
            .path()
            .join("etc/systemd/system/alkahest.service")
            .exists()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn user_install_goes_to_the_xdg_systemd_user_dir() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    alkahest(home.path())
        .args([
            "service",
            "install",
            "--user",
            "--bin",
            "/usr/bin/alkahest",
            "--root",
        ])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("systemctl --user enable --now alkahest.service"));
    let unit =
        std::fs::read_to_string(root.path().join(".config/systemd/user/alkahest.service")).unwrap();
    assert!(unit.contains("WantedBy=default.target"), "{unit}");
    assert!(!unit.contains("User="), "{unit}");
}

#[cfg(target_os = "linux")]
#[test]
fn status_json_reports_the_listen_address() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    alkahest(home.path())
        .args(["--json", "service", "status", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(
            contains("\"listen\":\"http://127.0.0.1:11435\"").and(contains("\"installed\":false")),
        );
}
