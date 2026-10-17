//! `alkahest service install|uninstall|start|stop|status` for systemd (ALK-US-0045,
//! ALK-SP-0009.R1 to R3).
//!
//! Everything is planned first ([`plan_install`] and friends return the files to write and the
//! commands to run) and only then applied, so `--dry-run` prints exactly what a real run does
//! and tests assert on the plan. With `--root <dir>` the unit is written below that directory
//! and no command runs: nothing touches the real system (packaging and tests).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alkahest_core::config::Loaded;
use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cli::{ServiceAction, ServiceArgs};

/// Unit template of the system service.
const SYSTEM_UNIT: &str = include_str!("../../../packaging/systemd/alkahest.service");
/// Unit template of the per-user service.
const USER_UNIT: &str = include_str!("../../../packaging/systemd/alkahest.user.service");

/// Dedicated service account of the system install (ALK-SP-0009.R2).
const SERVICE_USER: &str = "alkahest";
/// State directory of the system service; becomes `ALKAHEST_HOME`.
const STATE_DIR: &str = "/var/lib/alkahest";
/// Unit file name.
const UNIT_NAME: &str = "alkahest.service";
/// Groups the service user joins for GPU access, when the distribution has them.
const GPU_GROUPS: [&str; 2] = ["render", "video"];
/// Group that owns an Ollama store installed by the Ollama package, for read-only reuse.
const OLLAMA_GROUP: &str = "ollama";

/// One thing the install changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Files to write: path and content.
    pub files: Vec<(PathBuf, String)>,
    /// Commands to run in order, as argument vectors.
    pub commands: Vec<Vec<String>>,
    /// Notes for the operator (not commands).
    pub notes: Vec<String>,
}

/// The inputs of a plan that come from the machine, injectable for tests.
pub struct Host<'a> {
    /// Whether a group exists.
    pub has_group: &'a dyn Fn(&str) -> bool,
    /// Whether a user exists.
    pub has_user: &'a dyn Fn(&str) -> bool,
    /// `$XDG_CONFIG_HOME` or `$HOME/.config`, used for the user unit without `--root`.
    pub user_config_dir: PathBuf,
}

/// Whether `name` is a group in `<root>/etc/group`.
fn group_exists(root: &Path, name: &str) -> bool {
    first_field_exists(&root.join("etc/group"), name)
}

/// Whether `name` is a user in `<root>/etc/passwd`.
fn user_exists(root: &Path, name: &str) -> bool {
    first_field_exists(&root.join("etc/passwd"), name)
}

fn first_field_exists(file: &Path, name: &str) -> bool {
    std::fs::read_to_string(file)
        .map(|t| t.lines().any(|l| l.split(':').next() == Some(name)))
        .unwrap_or(false)
}

/// `root` joined with the absolute `path` (`/` stays the real root).
fn under(root: Option<&Path>, path: &str) -> PathBuf {
    match root {
        Some(r) => r.join(path.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

/// Quotes `s` for an `ExecStart=` word when it has spaces.
fn exec_word(s: &str) -> String {
    if s.contains(' ') {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// The unit file text for the system service with the given supplementary groups.
pub fn render_system_unit(bin: &Path, groups: &[String]) -> String {
    let supp = if groups.is_empty() {
        "# No render/video/ollama group on this machine.".to_string()
    } else {
        format!("SupplementaryGroups={}", groups.join(" "))
    };
    SYSTEM_UNIT
        .replace("@SUPPLEMENTARY_GROUPS@", &supp)
        .replace("@EXEC_START@", &exec_word(&bin.display().to_string()))
}

/// The unit file text for the per-user service.
pub fn render_user_unit(bin: &Path) -> String {
    USER_UNIT.replace("@EXEC_START@", &exec_word(&bin.display().to_string()))
}

/// Where the unit file goes.
fn unit_path(user: bool, root: Option<&Path>, host: &Host<'_>) -> PathBuf {
    if user {
        let base = match root {
            Some(r) => r.join(".config"),
            None => host.user_config_dir.clone(),
        };
        base.join("systemd/user").join(UNIT_NAME)
    } else {
        under(root, "/etc/systemd/system").join(UNIT_NAME)
    }
}

fn systemctl(user: bool, rest: &[&str]) -> Vec<String> {
    let mut c = vec!["systemctl".to_string()];
    if user {
        c.push("--user".into());
    }
    c.extend(rest.iter().map(|s| (*s).to_string()));
    c
}

/// What `service install` does.
pub fn plan_install(args: &ServiceArgs, bin: &Path, host: &Host<'_>) -> Plan {
    let root = args.root.as_deref();
    let unit = unit_path(args.user, root, host);
    let mut plan = Plan {
        files: Vec::new(),
        commands: Vec::new(),
        notes: Vec::new(),
    };
    if args.user {
        plan.files.push((unit, render_user_unit(bin)));
        plan.notes.push(
            "run `loginctl enable-linger $USER` to start the service at boot without a login"
                .into(),
        );
    } else {
        let mut groups: Vec<String> = GPU_GROUPS
            .iter()
            .filter(|g| (host.has_group)(g))
            .map(|g| (*g).to_string())
            .collect();
        // Read access to a system Ollama store (ALK-T-0133); never write access.
        if (host.has_group)(OLLAMA_GROUP) {
            groups.push(OLLAMA_GROUP.into());
        }
        if !(host.has_user)(SERVICE_USER) {
            let mut useradd: Vec<String> = [
                "useradd",
                "--system",
                "--user-group",
                "--home-dir",
                STATE_DIR,
                "--shell",
                "/usr/sbin/nologin",
            ]
            .map(String::from)
            .to_vec();
            if !groups.is_empty() {
                useradd.extend(["--groups".into(), groups.join(",")]);
            }
            useradd.push(SERVICE_USER.into());
            plan.commands.push(useradd);
        } else if !groups.is_empty() {
            plan.commands.push(vec![
                "usermod".into(),
                "--append".into(),
                "--groups".into(),
                groups.join(","),
                SERVICE_USER.into(),
            ]);
        }
        // The models directory tree belongs to the service user; StateDirectory= keeps it so
        // on every start, this makes it right before the first one.
        plan.commands.push(
            [
                "install",
                "-d",
                "-o",
                SERVICE_USER,
                "-g",
                SERVICE_USER,
                "-m",
                "0750",
                STATE_DIR,
            ]
            .map(String::from)
            .to_vec(),
        );
        plan.files.push((unit, render_system_unit(bin, &groups)));
    }
    plan.commands.push(systemctl(args.user, &["daemon-reload"]));
    plan.commands
        .push(systemctl(args.user, &["enable", "--now", UNIT_NAME]));
    plan
}

/// What `service uninstall` does. The service user and its data are left in place.
pub fn plan_uninstall(args: &ServiceArgs, host: &Host<'_>) -> (PathBuf, Vec<Vec<String>>) {
    let unit = unit_path(args.user, args.root.as_deref(), host);
    let cmds = vec![
        systemctl(args.user, &["disable", "--now", UNIT_NAME]),
        systemctl(args.user, &["daemon-reload"]),
    ];
    (unit, cmds)
}

/// Runs `cmd`, failing with its status.
fn run_command(cmd: &[String]) -> Result<()> {
    let (prog, rest) = cmd.split_first().context("empty command")?;
    let status = std::process::Command::new(prog)
        .args(rest)
        .status()
        .with_context(|| format!("running {}", cmd.join(" ")))?;
    if !status.success() {
        bail!("`{}` failed with {status}", cmd.join(" "));
    }
    Ok(())
}

fn shell_line(cmd: &[String]) -> String {
    cmd.join(" ")
}

/// The real machine for [`Host`].
fn with_host<T>(args: &ServiceArgs, f: impl FnOnce(&Host<'_>) -> T) -> T {
    let root = args.root.clone().unwrap_or_else(|| PathBuf::from("/"));
    let has_group = |g: &str| group_exists(&root, g);
    let has_user = |u: &str| user_exists(&root, u);
    let user_config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    f(&Host {
        has_group: &has_group,
        has_user: &has_user,
        user_config_dir,
    })
}

/// Whether commands run: not with `--dry-run` and not with `--root`.
fn executes(args: &ServiceArgs) -> bool {
    !args.dry_run && args.root.is_none()
}

/// Runs `alkahest service <action>`.
pub fn run(action: &ServiceAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    if !cfg!(target_os = "linux") {
        bail!(
            "`alkahest service` supports systemd on Linux only for now (macOS and Windows: ALK-US-0046, ALK-US-0047)"
        );
    }
    match action {
        ServiceAction::Install(args) => install(args),
        ServiceAction::Uninstall(args) => uninstall(args),
        ServiceAction::Start(args) => control(args, "start"),
        ServiceAction::Stop(args) => control(args, "stop"),
        ServiceAction::Status(args) => status(args, loaded, json),
    }
}

fn install(args: &ServiceArgs) -> Result<ExitCode> {
    let bin = match &args.bin {
        Some(b) => b.clone(),
        None => std::env::current_exe().context("locating the alkahest binary")?,
    };
    let plan = with_host(args, |h| plan_install(args, &bin, h));
    for (path, content) in &plan.files {
        if args.dry_run {
            println!("# would write {}\n{content}", path.display());
        } else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
            println!("wrote {}", path.display());
        }
    }
    for cmd in &plan.commands {
        if executes(args) {
            println!("+ {}", shell_line(cmd));
            run_command(cmd)?;
        } else {
            println!("# would run: {}", shell_line(cmd));
        }
    }
    for note in &plan.notes {
        println!("note: {note}");
    }
    Ok(ExitCode::SUCCESS)
}

fn uninstall(args: &ServiceArgs) -> Result<ExitCode> {
    let (unit, cmds) = with_host(args, |h| plan_uninstall(args, h));
    for cmd in &cmds {
        if executes(args) {
            println!("+ {}", shell_line(cmd));
            // Disabling a unit that is not loaded fails; removing it still has to go on.
            if let Err(e) = run_command(cmd) {
                eprintln!("warning: {e:#}");
            }
        } else {
            println!("# would run: {}", shell_line(cmd));
        }
    }
    if args.dry_run {
        println!("# would remove {}", unit.display());
    } else if unit.exists() {
        std::fs::remove_file(&unit).with_context(|| format!("removing {}", unit.display()))?;
        println!("removed {}", unit.display());
    } else {
        println!("{} is not installed", unit.display());
    }
    if !args.user {
        println!("note: the `{SERVICE_USER}` user and {STATE_DIR} were kept; remove them by hand");
    }
    Ok(ExitCode::SUCCESS)
}

fn control(args: &ServiceArgs, verb: &str) -> Result<ExitCode> {
    let cmd = systemctl(args.user, &[verb, UNIT_NAME]);
    if executes(args) {
        run_command(&cmd)?;
    } else {
        println!("# would run: {}", shell_line(&cmd));
    }
    Ok(ExitCode::SUCCESS)
}

/// `systemctl is-active` output to a human state; exit code 3 when not running.
fn status(args: &ServiceArgs, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let installed = with_host(args, |h| unit_path(args.user, args.root.as_deref(), h)).exists();
    let url = loaded.config.server.client_url();
    let state = if !executes(args) {
        println!(
            "# would run: {}",
            shell_line(&systemctl(args.user, &["is-active", UNIT_NAME]))
        );
        if installed {
            "installed"
        } else {
            "not installed"
        }
        .to_string()
    } else {
        let mut c = std::process::Command::new("systemctl");
        if args.user {
            c.arg("--user");
        }
        let out = c
            .args(["is-active", UNIT_NAME])
            .output()
            .context("running systemctl")?;
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let running = state == "active";
    if json {
        println!(
            "{}",
            json!({"service": UNIT_NAME, "state": state, "running": running,
                   "installed": installed, "listen": url})
        );
    } else if running {
        println!("alkahest service: running, listening on {url}");
    } else {
        println!("alkahest service: {state}");
    }
    Ok(if running || !executes(args) {
        ExitCode::SUCCESS
    } else {
        // systemctl is-active exits 3 for an inactive unit.
        ExitCode::from(3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(user: bool, root: Option<&Path>) -> ServiceArgs {
        ServiceArgs {
            user,
            dry_run: false,
            root: root.map(Path::to_path_buf),
            bin: None,
        }
    }

    fn host<'a>(groups: &'a dyn Fn(&str) -> bool, users: &'a dyn Fn(&str) -> bool) -> Host<'a> {
        Host {
            has_group: groups,
            has_user: users,
            user_config_dir: PathBuf::from("/home/u/.config"),
        }
    }

    #[test]
    fn system_unit_has_restart_always_and_groups() {
        let g = |n: &str| matches!(n, "render" | "video" | "ollama");
        let u = |_: &str| false;
        let p = plan_install(
            &args(false, None),
            Path::new("/usr/bin/alkahest"),
            &host(&g, &u),
        );
        let (path, text) = &p.files[0];
        assert_eq!(path, Path::new("/etc/systemd/system/alkahest.service"));
        for want in [
            "User=alkahest",
            "SupplementaryGroups=render video ollama",
            "Restart=always",
            "ExecStart=/usr/bin/alkahest serve",
            "ALKAHEST_HOME=/var/lib/alkahest",
            "WantedBy=multi-user.target",
        ] {
            assert!(text.contains(want), "missing {want}:\n{text}");
        }
        assert!(!text.contains('@'), "unfilled placeholder:\n{text}");
        let cmds: Vec<String> = p.commands.iter().map(|c| c.join(" ")).collect();
        assert!(cmds[0].starts_with("useradd --system --user-group"));
        assert!(cmds[0].contains("--groups render,video,ollama alkahest"));
        assert_eq!(
            cmds.last().unwrap(),
            "systemctl enable --now alkahest.service"
        );
    }

    #[test]
    fn missing_groups_are_left_out_and_existing_user_is_reused() {
        let g = |n: &str| n == "video";
        let u = |n: &str| n == "alkahest";
        let p = plan_install(
            &args(false, None),
            Path::new("/opt/a b/alkahest"),
            &host(&g, &u),
        );
        let text = &p.files[0].1;
        assert!(text.contains("SupplementaryGroups=video\n"));
        assert!(text.contains("ExecStart=\"/opt/a b/alkahest\" serve"));
        assert!(
            p.commands[0]
                .join(" ")
                .starts_with("usermod --append --groups video")
        );
        let none = plan_install(&args(false, None), Path::new("/x"), &host(&|_| false, &u));
        assert!(!none.files[0].1.contains("SupplementaryGroups="));
    }

    #[test]
    fn user_unit_goes_to_the_xdg_dir_without_system_changes() {
        let g = |_: &str| true;
        let u = |_: &str| false;
        let p = plan_install(
            &args(true, None),
            Path::new("/usr/bin/alkahest"),
            &host(&g, &u),
        );
        assert_eq!(
            p.files[0].0,
            Path::new("/home/u/.config/systemd/user/alkahest.service")
        );
        assert!(!p.files[0].1.contains("User="));
        assert!(p.files[0].1.contains("WantedBy=default.target"));
        assert!(
            p.commands
                .iter()
                .all(|c| c[0] == "systemctl" && c[1] == "--user")
        );
    }

    #[test]
    fn root_prefixes_paths_and_reads_groups_from_it() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("etc")).unwrap();
        std::fs::write(tmp.path().join("etc/group"), "root:x:0:\nrender:x:105:\n").unwrap();
        assert!(group_exists(tmp.path(), "render"));
        assert!(!group_exists(tmp.path(), "video"));
        assert!(!user_exists(tmp.path(), "alkahest"));
        assert_eq!(
            under(Some(tmp.path()), "/etc/systemd/system"),
            tmp.path().join("etc/systemd/system")
        );
    }

    #[test]
    fn uninstall_plan_disables_first() {
        let g = |_: &str| false;
        let (unit, cmds) = plan_uninstall(&args(false, None), &host(&g, &g));
        assert_eq!(unit, Path::new("/etc/systemd/system/alkahest.service"));
        assert_eq!(
            cmds[0].join(" "),
            "systemctl disable --now alkahest.service"
        );
    }
}
