//! `alkahest service ...` for launchd on macOS (ALK-US-0047, ALK-SP-0009.R1/R2).
//!
//! * `--user`: a LaunchAgent in `~/Library/LaunchAgents`, started in the login session
//!   (`gui/<uid>` domain) with the user's own data directory.
//! * system: a LaunchDaemon in `/Library/LaunchDaemons` running as the hidden `_alkahest`
//!   account (created with `dscl` when missing, ALK-SP-0009.R2) with
//!   `ALKAHEST_HOME=/Library/Application Support/Alkahest`.
//!
//! Both restart the server when it exits (`KeepAlive`), give it 45 s to drain on stop
//! (`ExitTimeOut`, above the 30 s default drain) and raise the open-file limit from macOS's 256.
//! As for systemd, the plan is computed first and applied afterwards, so `--dry-run` prints
//! exactly what a real run does and `--root` writes below a directory without running anything.
//! The generator is platform independent; tests run on Linux.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alkahest_core::config::Loaded;
use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::ServiceArgs;
use crate::service_cmd::{Step, apply_steps, executes, shell_line, under, write_files};

/// launchd job label (reverse DNS, same id as the desktop app).
pub const LABEL: &str = "es.digio.alkahest";
/// Hidden service account of the system daemon (leading underscore: macOS convention for
/// daemon accounts).
pub const DAEMON_USER: &str = "_alkahest";
/// Data directory of the system daemon; becomes `ALKAHEST_HOME`.
pub const DAEMON_HOME: &str = "/Library/Application Support/Alkahest";
/// launchd output of the system daemon (the server also keeps its own rotating log).
pub const DAEMON_LOG_DIR: &str = "/Library/Logs/Alkahest";
/// Seconds launchd waits after SIGTERM before SIGKILL; above the default 30 s drain.
const EXIT_TIMEOUT_SECS: u32 = 45;
/// Soft open-file limit; macOS defaults to 256, too few for a server with engines and clients.
const OPEN_FILES: u32 = 4096;
/// Range searched for a free uid/gid for the daemon account (below 500: hidden system ids).
const ID_RANGE: std::ops::Range<u32> = 250..500;

/// Machine facts the plan needs, injectable for tests.
#[derive(Debug, Clone, Default)]
pub struct MacHost {
    /// Numeric id of the invoking user (`gui/<uid>` domain).
    pub uid: u32,
    /// `$HOME` of the invoking user (LaunchAgents and logs of `--user`).
    pub home: PathBuf,
    /// Whether the daemon account already exists.
    pub daemon_user_exists: bool,
    /// User and group ids already taken (to pick one for the daemon account).
    pub taken_ids: BTreeSet<u32>,
}

/// Files to write, steps to run and notes for the operator.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchdPlan {
    /// Files: path and content.
    pub files: Vec<(PathBuf, String)>,
    /// Commands in order.
    pub steps: Vec<Step>,
    /// Notes (not commands).
    pub notes: Vec<String>,
}

/// Escapes text for a plist `<string>`.
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// What the plist describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// Program and arguments.
    pub program: Vec<String>,
    /// Account to run as (system daemon only).
    pub user_name: Option<String>,
    /// Environment variables.
    pub environment: Vec<(String, String)>,
    /// Working directory.
    pub working_directory: Option<String>,
    /// stdout and stderr file.
    pub log_file: String,
}

/// Renders the launchd property list of `spec`.
pub fn render_plist(spec: &JobSpec) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Alkahest local AI model server (ALK-SP-0009.R1); written by `alkahest service install`. -->\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    let key = |out: &mut String, k: &str| out.push_str(&format!("\t<key>{k}</key>\n"));
    let string = |out: &mut String, indent: &str, v: &str| {
        out.push_str(&format!("{indent}<string>{}</string>\n", xml(v)));
    };
    key(&mut out, "Label");
    string(&mut out, "\t", LABEL);
    key(&mut out, "ProgramArguments");
    out.push_str("\t<array>\n");
    for arg in &spec.program {
        string(&mut out, "\t\t", arg);
    }
    out.push_str("\t</array>\n");
    if let Some(user) = &spec.user_name {
        key(&mut out, "UserName");
        string(&mut out, "\t", user);
        key(&mut out, "GroupName");
        string(&mut out, "\t", user);
    }
    if !spec.environment.is_empty() {
        key(&mut out, "EnvironmentVariables");
        out.push_str("\t<dict>\n");
        for (k, v) in &spec.environment {
            out.push_str(&format!("\t\t<key>{}</key>\n", xml(k)));
            string(&mut out, "\t\t", v);
        }
        out.push_str("\t</dict>\n");
    }
    if let Some(dir) = &spec.working_directory {
        key(&mut out, "WorkingDirectory");
        string(&mut out, "\t", dir);
    }
    key(&mut out, "RunAtLoad");
    out.push_str("\t<true/>\n");
    // Restart whenever it exits, like systemd's Restart=always; launchd throttles respawns.
    key(&mut out, "KeepAlive");
    out.push_str("\t<true/>\n");
    key(&mut out, "ExitTimeOut");
    out.push_str(&format!("\t<integer>{EXIT_TIMEOUT_SECS}</integer>\n"));
    key(&mut out, "SoftResourceLimits");
    out.push_str(&format!(
        "\t<dict>\n\t\t<key>NumberOfFiles</key>\n\t\t<integer>{OPEN_FILES}</integer>\n\t</dict>\n"
    ));
    key(&mut out, "StandardOutPath");
    string(&mut out, "\t", &spec.log_file);
    key(&mut out, "StandardErrorPath");
    string(&mut out, "\t", &spec.log_file);
    out.push_str("</dict>\n</plist>\n");
    out
}

/// Where the plist goes.
pub fn plist_path(user: bool, root: Option<&Path>, host: &MacHost) -> PathBuf {
    let name = format!("{LABEL}.plist");
    if user {
        match root {
            Some(r) => r.join("Library/LaunchAgents").join(name),
            None => host.home.join("Library/LaunchAgents").join(name),
        }
    } else {
        under(root, "/Library/LaunchDaemons").join(name)
    }
}

/// launchd domain target: `gui/<uid>` or `system`.
fn domain(user: bool, host: &MacHost) -> String {
    if user {
        format!("gui/{}", host.uid)
    } else {
        "system".to_string()
    }
}

/// Service target: `<domain>/<label>`.
fn service_target(user: bool, host: &MacHost) -> String {
    format!("{}/{LABEL}", domain(user, host))
}

fn launchctl(rest: &[&str]) -> Vec<String> {
    std::iter::once("launchctl")
        .chain(rest.iter().copied())
        .map(String::from)
        .collect()
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The first id free as both uid and gid.
fn free_id(taken: &BTreeSet<u32>) -> Option<u32> {
    ID_RANGE.clone().find(|id| !taken.contains(id))
}

/// `dscl` commands creating the hidden daemon group and account.
fn create_daemon_account(id: u32) -> Vec<Step> {
    let id = id.to_string();
    let group = format!("/Groups/{DAEMON_USER}");
    let user = format!("/Users/{DAEMON_USER}");
    let mut steps = Vec::new();
    let mut dscl = |path: &str, rest: &[&str]| {
        let mut v = argv(&["dscl", ".", "-create", path]);
        v.extend(rest.iter().map(|s| (*s).to_string()));
        steps.push(Step::new(v));
    };
    dscl(&group, &[]);
    dscl(&group, &["PrimaryGroupID", &id]);
    dscl(&group, &["RealName", "Alkahest server"]);
    dscl(&group, &["Password", "*"]);
    dscl(&user, &[]);
    dscl(&user, &["UniqueID", &id]);
    dscl(&user, &["PrimaryGroupID", &id]);
    dscl(&user, &["UserShell", "/usr/bin/false"]);
    dscl(&user, &["NFSHomeDirectory", DAEMON_HOME]);
    dscl(&user, &["RealName", "Alkahest server"]);
    dscl(&user, &["Password", "*"]);
    dscl(&user, &["IsHidden", "1"]);
    steps
}

/// What `service install` does.
pub fn plan_install(args: &ServiceArgs, bin: &Path, host: &MacHost) -> LaunchdPlan {
    let root = args.root.as_deref();
    let plist = plist_path(args.user, root, host);
    let plist_str = plist.display().to_string();
    let program = vec![bin.display().to_string(), "serve".to_string()];
    let mut plan = LaunchdPlan::default();
    if args.user {
        let log_dir = host.home.join("Library/Logs/Alkahest");
        let spec = JobSpec {
            program,
            user_name: None,
            environment: Vec::new(),
            working_directory: None,
            log_file: log_dir.join("alkahest.log").display().to_string(),
        };
        plan.files.push((plist, render_plist(&spec)));
        plan.steps.push(Step::new(argv(&[
            "mkdir",
            "-p",
            &log_dir.display().to_string(),
        ])));
    } else {
        if !host.daemon_user_exists {
            match free_id(&host.taken_ids) {
                Some(id) => plan.steps.extend(create_daemon_account(id)),
                None => plan.notes.push(format!(
                    "no free id in {}..{} for the {DAEMON_USER} account; create it by hand",
                    ID_RANGE.start, ID_RANGE.end
                )),
            }
        }
        let owner = format!("{DAEMON_USER}:{DAEMON_USER}");
        plan.steps.push(Step::new(argv(&[
            "mkdir",
            "-p",
            DAEMON_HOME,
            DAEMON_LOG_DIR,
        ])));
        plan.steps.push(Step::new(argv(&[
            "chown",
            "-R",
            &owner,
            DAEMON_HOME,
            DAEMON_LOG_DIR,
        ])));
        plan.steps
            .push(Step::new(argv(&["chmod", "0750", DAEMON_HOME])));
        let spec = JobSpec {
            program,
            user_name: Some(DAEMON_USER.to_string()),
            environment: vec![
                ("ALKAHEST_HOME".to_string(), DAEMON_HOME.to_string()),
                ("HOME".to_string(), DAEMON_HOME.to_string()),
            ],
            working_directory: Some(DAEMON_HOME.to_string()),
            log_file: format!("{DAEMON_LOG_DIR}/alkahest.log"),
        };
        plan.files.push((plist, render_plist(&spec)));
        // launchd refuses daemon plists not owned by root or writable by others.
        plan.steps
            .push(Step::new(argv(&["chown", "root:wheel", &plist_str])));
        plan.steps
            .push(Step::new(argv(&["chmod", "0644", &plist_str])));
    }
    // A job left loaded by an earlier install must go before it can be bootstrapped again.
    plan.steps.push(Step::tolerant(launchctl(&[
        "bootout",
        &service_target(args.user, host),
    ])));
    plan.steps.push(Step::new(launchctl(&[
        "enable",
        &service_target(args.user, host),
    ])));
    plan.steps.push(Step::new(launchctl(&[
        "bootstrap",
        &domain(args.user, host),
        &plist_str,
    ])));
    if !args.user {
        plan.notes.push(format!(
            "the daemon runs as {DAEMON_USER} with its data in {DAEMON_HOME}; an Ollama store \
             is reused only where {DAEMON_USER} can read it"
        ));
    }
    plan
}

/// What `service uninstall` does: unload, then remove the plist. Account and data stay.
pub fn plan_uninstall(args: &ServiceArgs, host: &MacHost) -> (PathBuf, Vec<Step>) {
    let plist = plist_path(args.user, args.root.as_deref(), host);
    let steps = vec![Step::tolerant(launchctl(&[
        "bootout",
        &service_target(args.user, host),
    ]))];
    (plist, steps)
}

/// `service start`: load the job (starts it through `RunAtLoad`), or kick an already loaded
/// one.
pub fn plan_start(args: &ServiceArgs, host: &MacHost) -> Vec<Step> {
    let plist = plist_path(args.user, args.root.as_deref(), host);
    vec![
        Step::tolerant(launchctl(&[
            "bootstrap",
            &domain(args.user, host),
            &plist.display().to_string(),
        ])),
        Step::new(launchctl(&["kickstart", &service_target(args.user, host)])),
    ]
}

/// `service stop`: unload the job so `KeepAlive` does not restart it; it loads again at the
/// next boot (daemon) or login (agent), or with `service start`.
pub fn plan_stop(args: &ServiceArgs, host: &MacHost) -> Vec<Step> {
    vec![Step::new(launchctl(&[
        "bootout",
        &service_target(args.user, host),
    ]))]
}

/// Reads `state = running` and `pid = N` from `launchctl print` output.
pub fn parse_print(text: &str) -> (bool, Option<u32>) {
    let mut running = false;
    let mut pid = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("state = ") {
            running = v.trim() == "running";
        } else if let Some(v) = line.strip_prefix("pid = ") {
            pid = v.trim().parse().ok();
        }
    }
    (running, pid)
}

/// The real machine.
fn real_host(args: &ServiceArgs) -> MacHost {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    let mut host = MacHost {
        uid: current_uid(),
        home,
        ..MacHost::default()
    };
    // Directory services only exist on a real macOS run; plans for --root or another
    // platform assume a fresh machine.
    if cfg!(target_os = "macos") && args.root.is_none() {
        host.daemon_user_exists = std::process::Command::new("dscl")
            .args([".", "-read", &format!("/Users/{DAEMON_USER}")])
            .output()
            .is_ok_and(|o| o.status.success());
        for (path, attr) in [("/Users", "UniqueID"), ("/Groups", "PrimaryGroupID")] {
            if let Ok(out) = std::process::Command::new("dscl")
                .args([".", "-list", path, attr])
                .output()
            {
                host.taken_ids.extend(
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .filter_map(|l| l.split_whitespace().last()?.parse::<u32>().ok()),
                );
            }
        }
    }
    host
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    // Only reached for plans of another platform; the first macOS user id.
    501
}

/// Runs `alkahest service <action>` against launchd.
pub fn run(action: &crate::cli::ServiceAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    use crate::cli::ServiceAction;
    match action {
        ServiceAction::Install(args) => {
            let bin = crate::service_cmd::service_binary(args)?;
            let host = real_host(args);
            let plan = plan_install(args, &bin, &host);
            write_files(args, &plan.files)?;
            apply_steps(args, &plan.steps)?;
            for note in &plan.notes {
                println!("note: {note}");
            }
            Ok(ExitCode::SUCCESS)
        }
        ServiceAction::Uninstall(args) => {
            let host = real_host(args);
            let (plist, steps) = plan_uninstall(args, &host);
            apply_steps(args, &steps)?;
            crate::service_cmd::remove_file(args, &plist)?;
            if !args.user {
                println!(
                    "note: the {DAEMON_USER} account and {DAEMON_HOME} were kept; remove them by hand"
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        ServiceAction::Start(args) => {
            apply_steps(args, &plan_start(args, &real_host(args)))?;
            Ok(ExitCode::SUCCESS)
        }
        ServiceAction::Stop(args) => {
            apply_steps(args, &plan_stop(args, &real_host(args)))?;
            Ok(ExitCode::SUCCESS)
        }
        ServiceAction::Status(args) => status(args, loaded, json),
    }
}

fn status(args: &ServiceArgs, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let host = real_host(args);
    let installed = plist_path(args.user, args.root.as_deref(), &host).exists();
    let url = loaded.config.server.client_url();
    let target = service_target(args.user, &host);
    let (state, running, pid) = if executes(args) {
        let out = std::process::Command::new("launchctl")
            .args(["print", &target])
            .output()
            .context("running launchctl")?;
        if out.status.success() {
            let (running, pid) = parse_print(&String::from_utf8_lossy(&out.stdout));
            let state = if running { "running" } else { "loaded" };
            (state.to_string(), running, pid)
        } else {
            let state = if installed {
                "not loaded"
            } else {
                "not installed"
            };
            (state.to_string(), false, None)
        }
    } else {
        println!(
            "# would run: {}",
            shell_line(&launchctl(&["print", &target]))
        );
        let state = if installed {
            "installed"
        } else {
            "not installed"
        };
        (state.to_string(), false, None)
    };
    if json {
        println!(
            "{}",
            json!({"service": LABEL, "state": state, "running": running, "pid": pid,
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
        ExitCode::from(3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(user: bool) -> ServiceArgs {
        ServiceArgs {
            user,
            ..ServiceArgs::default()
        }
    }

    fn host() -> MacHost {
        MacHost {
            uid: 501,
            home: PathBuf::from("/Users/ana"),
            daemon_user_exists: false,
            taken_ids: [250, 251].into_iter().collect(),
        }
    }

    fn lines(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| s.argv.join(" ")).collect()
    }

    #[test]
    fn daemon_plist_runs_as_the_service_account_and_keeps_alive() {
        let p = plan_install(&args(false), Path::new("/usr/local/bin/alkahest"), &host());
        let (path, text) = &p.files[0];
        assert_eq!(
            path,
            Path::new("/Library/LaunchDaemons/es.digio.alkahest.plist")
        );
        for want in [
            "<key>Label</key>\n\t<string>es.digio.alkahest</string>",
            "<string>/usr/local/bin/alkahest</string>\n\t\t<string>serve</string>",
            "<key>UserName</key>\n\t<string>_alkahest</string>",
            "<key>ALKAHEST_HOME</key>\n\t\t<string>/Library/Application Support/Alkahest</string>",
            "<key>KeepAlive</key>\n\t<true/>",
            "<key>RunAtLoad</key>\n\t<true/>",
            "<key>ExitTimeOut</key>\n\t<integer>45</integer>",
            "<string>/Library/Logs/Alkahest/alkahest.log</string>",
        ] {
            assert!(text.contains(want), "missing {want}:\n{text}");
        }
        let cmds = lines(&p.steps);
        // First free id after the taken ones, for both the group and the user.
        assert!(cmds.contains(&"dscl . -create /Users/_alkahest UniqueID 252".to_string()));
        assert!(cmds.contains(&"dscl . -create /Groups/_alkahest PrimaryGroupID 252".to_string()));
        assert!(cmds.contains(
            &"chown root:wheel /Library/LaunchDaemons/es.digio.alkahest.plist".to_string()
        ));
        assert_eq!(
            cmds.last().unwrap(),
            "launchctl bootstrap system /Library/LaunchDaemons/es.digio.alkahest.plist"
        );
        // Re-installing over a loaded job: the bootout may fail, nothing else may.
        let tolerant: Vec<_> = p.steps.iter().filter(|s| s.may_fail).collect();
        assert_eq!(tolerant.len(), 1);
        assert_eq!(
            tolerant[0].argv.join(" "),
            "launchctl bootout system/es.digio.alkahest"
        );
    }

    #[test]
    fn existing_account_is_reused() {
        let mut h = host();
        h.daemon_user_exists = true;
        let p = plan_install(&args(false), Path::new("/a"), &h);
        assert!(p.steps.iter().all(|s| s.argv[0] != "dscl"));
    }

    #[test]
    fn agent_goes_to_the_user_library_without_system_changes() {
        let p = plan_install(&args(true), Path::new("/opt/a & b/alkahest"), &host());
        let (path, text) = &p.files[0];
        assert_eq!(
            path,
            Path::new("/Users/ana/Library/LaunchAgents/es.digio.alkahest.plist")
        );
        assert!(!text.contains("UserName"));
        assert!(!text.contains("ALKAHEST_HOME"));
        assert!(text.contains("<string>/opt/a &amp; b/alkahest</string>"));
        assert!(text.contains("/Users/ana/Library/Logs/Alkahest/alkahest.log"));
        let cmds = lines(&p.steps);
        assert!(
            cmds.iter()
                .all(|c| !c.starts_with("dscl") && !c.starts_with("chown"))
        );
        assert_eq!(
            cmds.last().unwrap(),
            "launchctl bootstrap gui/501 /Users/ana/Library/LaunchAgents/es.digio.alkahest.plist"
        );
    }

    #[test]
    fn root_prefixes_the_plist_path() {
        let mut a = args(false);
        a.root = Some(PathBuf::from("/tmp/r"));
        assert_eq!(
            plist_path(false, a.root.as_deref(), &host()),
            Path::new("/tmp/r/Library/LaunchDaemons/es.digio.alkahest.plist")
        );
        assert_eq!(
            plist_path(true, a.root.as_deref(), &host()),
            Path::new("/tmp/r/Library/LaunchAgents/es.digio.alkahest.plist")
        );
    }

    #[test]
    fn start_stop_and_uninstall_target_the_job() {
        let h = host();
        assert_eq!(
            lines(&plan_start(&args(true), &h)),
            vec![
                "launchctl bootstrap gui/501 /Users/ana/Library/LaunchAgents/es.digio.alkahest.plist",
                "launchctl kickstart gui/501/es.digio.alkahest",
            ]
        );
        assert_eq!(
            lines(&plan_stop(&args(false), &h)),
            vec!["launchctl bootout system/es.digio.alkahest"]
        );
        let (plist, steps) = plan_uninstall(&args(false), &h);
        assert!(plist.ends_with("es.digio.alkahest.plist"));
        assert!(steps[0].may_fail);
    }

    #[test]
    fn status_reads_state_and_pid() {
        let text =
            "system/es.digio.alkahest = {\n\tactive count = 1\n\tstate = running\n\tpid = 4242\n}";
        assert_eq!(parse_print(text), (true, Some(4242)));
        assert_eq!(parse_print("\tstate = not running\n"), (false, None));
    }

    #[test]
    fn plist_is_well_formed() {
        let p = plan_install(&args(false), Path::new("/a<b>"), &host());
        let text = &p.files[0].1;
        assert!(text.contains("<string>/a&lt;b&gt;</string>"));
        assert_eq!(
            text.matches("<dict>").count(),
            text.matches("</dict>").count()
        );
        assert_eq!(
            text.matches("<array>").count(),
            text.matches("</array>").count()
        );
        assert!(text.trim_end().ends_with("</plist>"));
    }
}
