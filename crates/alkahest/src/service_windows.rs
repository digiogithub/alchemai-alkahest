//! `alkahest service ...` for the Windows service control manager (ALK-US-0048,
//! ALK-SP-0009.R1/R2) and the service entry `alkahest serve --service`.
//!
//! Install registers the service `alkahest` with `sc.exe` (start `delayed-auto`, account
//! `NT AUTHORITY\LocalService`, restart on failure), sets `ALKAHEST_HOME` to
//! `%ProgramData%\Alkahest` through the service's `Environment` registry value, grants
//! LocalService modify rights on that directory (by SID, so it works on every display
//! language), and registers the `Alkahest` event log source with `EventCreate.exe` as message
//! file, which renders the text of each event as is. As for systemd and launchd, the plan is
//! computed first, `--dry-run` prints it and `--root` writes below a directory without running
//! anything; the generator is platform independent and tested on Linux.
//!
//! The service entry hands the server to the service dispatcher (`windows-service`): stop and
//! shutdown requests become the server's stop future, the status goes `StartPending` →
//! `Running` once the listener is bound → `StopPending` → `Stopped`, and start, listen, stop
//! and failures are written to the Application event log.

use std::path::PathBuf;
use std::process::ExitCode;

use alkahest_core::config::Loaded;
use anyhow::{Result, bail};
use serde_json::json;

use crate::cli::{ServiceAction, ServiceArgs};
use crate::service_cmd::{Step, apply_steps, executes, shell_line};

/// Service name in the service control manager.
pub const SERVICE_NAME: &str = "alkahest";
/// Display name.
pub const DISPLAY_NAME: &str = "Alkahest";
/// Description shown by `services.msc`.
pub const DESCRIPTION: &str = "Alkahest local AI model server";
/// Event log source.
pub const EVENT_SOURCE: &str = "Alkahest";
/// Built-in low-privilege account the service runs as (ALK-SP-0009.R2).
pub const ACCOUNT: &str = r"NT AUTHORITY\LocalService";
/// SID of LocalService, for `icacls` independent of the display language.
const LOCAL_SERVICE_SID: &str = "*S-1-5-19";
/// Registry key of the service.
const SERVICE_KEY: &str = r"HKLM\SYSTEM\CurrentControlSet\Services\alkahest";
/// Registry key of the event log source.
const EVENT_SOURCE_KEY: &str =
    r"HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\Alkahest";

/// Machine facts the plan needs.
#[derive(Debug, Clone)]
pub struct WinHost {
    /// `%ProgramData%` (normally `C:\ProgramData`).
    pub program_data: String,
}

impl Default for WinHost {
    fn default() -> Self {
        Self {
            program_data: r"C:\ProgramData".to_string(),
        }
    }
}

/// What install does.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WinPlan {
    /// Directories to create (with `--root`: below the root).
    pub dirs: Vec<PathBuf>,
    /// Commands in order.
    pub steps: Vec<Step>,
    /// Notes (not commands).
    pub notes: Vec<String>,
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The data directory of the service, `%ProgramData%\Alkahest`.
pub fn data_dir(host: &WinHost) -> String {
    format!(r"{}\Alkahest", host.program_data.trim_end_matches('\\'))
}

/// `binPath=` value: the quoted executable plus `serve --service`.
pub fn bin_path(bin: &str) -> String {
    format!("\"{bin}\" serve --service")
}

/// What `service install` does.
pub fn plan_install(args: &ServiceArgs, bin: &str, host: &WinHost) -> WinPlan {
    let data = data_dir(host);
    let mut plan = WinPlan::default();
    plan.dirs.push(match &args.root {
        // `C:\ProgramData\Alkahest` below the root as `ProgramData/Alkahest`.
        Some(root) => data.split_once('\\').map_or_else(
            || root.join(&data),
            |(_, rest)| rest.split('\\').fold(root.clone(), |p, part| p.join(part)),
        ),
        None => PathBuf::from(&data),
    });
    let sc = |rest: &[&str]| {
        let mut v = argv(&["sc.exe"]);
        v.extend(rest.iter().map(|s| (*s).to_string()));
        Step::new(v)
    };
    plan.steps.push(sc(&[
        "create",
        SERVICE_NAME,
        "binPath=",
        &bin_path(bin),
        "start=",
        "delayed-auto",
        "obj=",
        ACCOUNT,
        "DisplayName=",
        DISPLAY_NAME,
    ]));
    plan.steps
        .push(sc(&["description", SERVICE_NAME, DESCRIPTION]));
    // Restart after 5 s, 5 s, then 30 s; the failure count resets after a day.
    plan.steps.push(sc(&[
        "failure",
        SERVICE_NAME,
        "reset=",
        "86400",
        "actions=",
        "restart/5000/restart/5000/restart/30000",
    ]));
    // Also count a non-zero exit code as a failure, not only a crash.
    plan.steps.push(sc(&["failureflag", SERVICE_NAME, "1"]));
    plan.steps.push(Step::new(argv(&[
        "reg.exe",
        "add",
        SERVICE_KEY,
        "/v",
        "Environment",
        "/t",
        "REG_MULTI_SZ",
        "/d",
        &format!("ALKAHEST_HOME={data}"),
        "/f",
    ])));
    plan.steps.push(Step::new(argv(&[
        "reg.exe",
        "add",
        EVENT_SOURCE_KEY,
        "/v",
        "EventMessageFile",
        "/t",
        "REG_EXPAND_SZ",
        "/d",
        r"%SystemRoot%\System32\EventCreate.exe",
        "/f",
    ])));
    plan.steps.push(Step::new(argv(&[
        "reg.exe",
        "add",
        EVENT_SOURCE_KEY,
        "/v",
        "TypesSupported",
        "/t",
        "REG_DWORD",
        "/d",
        "7",
        "/f",
    ])));
    plan.steps.push(Step::new(argv(&[
        "icacls.exe",
        &data,
        "/grant",
        &format!("{LOCAL_SERVICE_SID}:(OI)(CI)M"),
    ])));
    plan.steps.push(sc(&["start", SERVICE_NAME]));
    plan.notes.push(format!(
        "the service runs as {ACCOUNT} with its data in {data}; it cannot read a per-user \
         Ollama store under a user profile; start, stop and failures go to the Application          event log (source {EVENT_SOURCE})"
    ));
    plan
}

/// What `service uninstall` does: stop, delete the service and the event source. The data
/// directory stays.
pub fn plan_uninstall() -> Vec<Step> {
    vec![
        Step::tolerant(argv(&["sc.exe", "stop", SERVICE_NAME])),
        Step::new(argv(&["sc.exe", "delete", SERVICE_NAME])),
        Step::tolerant(argv(&["reg.exe", "delete", EVENT_SOURCE_KEY, "/f"])),
    ]
}

/// Reads the state name from `sc.exe query` output (`STATE : 4  RUNNING`).
pub fn parse_query(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim() != "STATE" {
            return None;
        }
        value.split_whitespace().nth(1).map(str::to_string)
    })
}

/// Runs `alkahest service <action>` against the Windows service manager.
pub fn run(action: &ServiceAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    match action {
        ServiceAction::Install(args) => {
            no_user(args)?;
            let bin = crate::service_cmd::service_binary(args)?;
            let plan = plan_install(args, &bin.display().to_string(), &real_host());
            for dir in &plan.dirs {
                if args.dry_run {
                    println!("# would create {}", dir.display());
                } else {
                    std::fs::create_dir_all(dir)?;
                    println!("created {}", dir.display());
                }
            }
            apply_steps(args, &plan.steps)?;
            for note in &plan.notes {
                println!("note: {note}");
            }
            Ok(ExitCode::SUCCESS)
        }
        ServiceAction::Uninstall(args) => {
            no_user(args)?;
            apply_steps(args, &plan_uninstall())?;
            println!(
                "note: {} was kept; remove it by hand",
                data_dir(&real_host())
            );
            Ok(ExitCode::SUCCESS)
        }
        ServiceAction::Start(args) => {
            no_user(args)?;
            apply_steps(args, &[Step::new(argv(&["sc.exe", "start", SERVICE_NAME]))])?;
            Ok(ExitCode::SUCCESS)
        }
        ServiceAction::Stop(args) => {
            no_user(args)?;
            apply_steps(args, &[Step::new(argv(&["sc.exe", "stop", SERVICE_NAME]))])?;
            Ok(ExitCode::SUCCESS)
        }
        ServiceAction::Status(args) => status(args, loaded, json),
    }
}

fn no_user(args: &ServiceArgs) -> Result<()> {
    if args.user {
        bail!(
            "Windows services are machine-wide; drop --user (or start `alkahest serve` from a \
             logon task for a per-user server)"
        );
    }
    Ok(())
}

fn real_host() -> WinHost {
    match std::env::var("ProgramData") {
        Ok(p) if cfg!(windows) && !p.is_empty() => WinHost { program_data: p },
        _ => WinHost::default(),
    }
}

fn status(args: &ServiceArgs, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let url = loaded.config.server.client_url();
    let query = argv(&["sc.exe", "query", SERVICE_NAME]);
    let (state, installed) = if executes(args) {
        let out = std::process::Command::new("sc.exe")
            .args(["query", SERVICE_NAME])
            .output()?;
        match parse_query(&String::from_utf8_lossy(&out.stdout)) {
            Some(state) => (state, true),
            // 1060: the service does not exist.
            None => ("not installed".to_string(), false),
        }
    } else {
        println!("# would run: {}", shell_line(&query));
        ("unknown".to_string(), false)
    };
    let running = state == "RUNNING";
    if json {
        println!(
            "{}",
            json!({"service": SERVICE_NAME, "state": state.to_ascii_lowercase(),
                   "running": running, "installed": installed, "listen": url})
        );
    } else if running {
        println!("alkahest service: running, listening on {url}");
    } else {
        println!("alkahest service: {}", state.to_ascii_lowercase());
    }
    Ok(if running || !executes(args) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    })
}

/// `alkahest serve --service`: runs under the service dispatcher (Windows only).
#[cfg(windows)]
pub fn run_service(loaded: &Loaded, allow_unauthenticated: bool) -> Result<ExitCode> {
    entry::run(loaded, allow_unauthenticated)
}

/// `alkahest serve --service` outside Windows: refused.
#[cfg(not(windows))]
pub fn run_service(_loaded: &Loaded, _allow_unauthenticated: bool) -> Result<ExitCode> {
    bail!("`alkahest serve --service` is the Windows service entry; run `alkahest serve`")
}

#[cfg(windows)]
mod entry {
    //! The service dispatcher glue.
    // `define_windows_service!` expands to an `extern "system"` function with an `unsafe`
    // block that parses the arguments the service control manager passes; that is the only
    // unsafe code here.
    #![allow(unsafe_code)]

    use std::ffi::OsString;
    use std::process::ExitCode;
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use alkahest_core::config::Loaded;
    use anyhow::{Context, Result};
    use tokio_util::sync::CancellationToken;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::{SERVICE_NAME, eventlog};
    use crate::serve_cmd::{ServeControl, run_with};

    /// What the service main needs; set once before the dispatcher starts.
    static CONTEXT: OnceLock<(Loaded, bool)> = OnceLock::new();
    /// Exit code of the server run, read back after the dispatcher returns.
    static FAILED: OnceLock<String> = OnceLock::new();

    /// Budget announced to the service manager for start (catalog, hardware probe) and stop
    /// (drain plus hooks).
    const START_HINT: Duration = Duration::from_secs(120);
    const STOP_HINT: Duration = Duration::from_secs(60);

    define_windows_service!(ffi_service_main, service_main);

    pub(super) fn run(loaded: &Loaded, allow_unauthenticated: bool) -> Result<ExitCode> {
        let _ = CONTEXT.set((loaded.clone(), allow_unauthenticated));
        service_dispatcher::start(SERVICE_NAME, ffi_service_main).context(
            "starting the service dispatcher (`alkahest serve --service` only works when \
             started by the Windows service manager)",
        )?;
        Ok(if FAILED.get().is_some() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }

    fn status(
        state: ServiceState,
        accept: ServiceControlAccept,
        hint: Duration,
        code: u32,
    ) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: if code == 0 {
                ServiceExitCode::Win32(0)
            } else {
                ServiceExitCode::ServiceSpecific(code)
            },
            checkpoint: 0,
            wait_hint: hint,
            process_id: None,
        }
    }

    fn service_main(_arguments: Vec<OsString>) {
        let Some((loaded, allow)) = CONTEXT.get() else {
            return;
        };
        let stop = CancellationToken::new();
        let handle_slot: Arc<OnceLock<service_control_handler::ServiceStatusHandle>> =
            Arc::new(OnceLock::new());
        let handler = {
            let stop = stop.clone();
            let slot = handle_slot.clone();
            move |event| match event {
                ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                    if let Some(h) = slot.get() {
                        let _ = h.set_service_status(status(
                            ServiceState::StopPending,
                            ServiceControlAccept::empty(),
                            STOP_HINT,
                            0,
                        ));
                    }
                    stop.cancel();
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let Ok(handle) = service_control_handler::register(SERVICE_NAME, handler) else {
            eventlog::error("cannot register the service control handler");
            return;
        };
        let _ = handle_slot.set(handle);
        let _ = handle.set_service_status(status(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            START_HINT,
            0,
        ));
        eventlog::info("Alkahest service starting");
        let result = run_with(loaded, *allow, || {
            let token = stop.clone();
            Ok(ServeControl {
                stop: Box::pin(async move {
                    token.cancelled().await;
                    "service stop"
                }),
                force: Box::pin(std::future::pending()),
                on_listening: Box::new(move |url| {
                    let _ = handle.set_service_status(status(
                        ServiceState::Running,
                        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
                        Duration::ZERO,
                        0,
                    ));
                    eventlog::info(&format!("Alkahest service listening on {url}"));
                }),
            })
        });
        let code = match result {
            Ok(()) => {
                eventlog::info("Alkahest service stopped");
                0
            }
            Err(e) => {
                let message = format!("Alkahest service failed: {e:#}");
                eventlog::error(&message);
                let _ = FAILED.set(message);
                1
            }
        };
        let _ = handle.set_service_status(status(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            Duration::ZERO,
            code,
        ));
    }
}

/// Application event log output of the service (source `Alkahest`).
#[cfg(windows)]
pub(crate) mod eventlog {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::System::EventLog::{
        DeregisterEventSource, EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE, REPORT_EVENT_TYPE,
        RegisterEventSourceW, ReportEventW,
    };

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Writes one event; failures are ignored (logging must never stop the service).
    #[allow(unsafe_code)]
    fn report(kind: REPORT_EVENT_TYPE, event_id: u32, message: &str) {
        let source = wide(super::EVENT_SOURCE);
        let text = wide(message);
        let strings = [text.as_ptr()];
        // SAFETY: `source` and `text` are NUL-terminated UTF-16 buffers that outlive the
        // calls; `strings` holds exactly the one pointer announced; the handle returned by
        // RegisterEventSourceW is checked for null and released with DeregisterEventSource.
        unsafe {
            let handle = RegisterEventSourceW(std::ptr::null(), source.as_ptr());
            if handle.is_null() {
                return;
            }
            ReportEventW(
                handle,
                kind,
                0,
                event_id,
                std::ptr::null_mut(),
                1,
                0,
                strings.as_ptr(),
                std::ptr::null(),
            );
            DeregisterEventSource(handle);
        }
    }

    /// Information event (id 1).
    pub(crate) fn info(message: &str) {
        report(EVENTLOG_INFORMATION_TYPE, 1, message);
    }

    /// Error event (id 2).
    pub(crate) fn error(message: &str) {
        report(EVENTLOG_ERROR_TYPE, 2, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| shell_line(&s.argv)).collect()
    }

    #[test]
    fn install_registers_a_delayed_auto_service_as_local_service() {
        let p = plan_install(
            &ServiceArgs::default(),
            r"C:\Program Files\Alkahest\alkahest.exe",
            &WinHost::default(),
        );
        let cmds = lines(&p.steps);
        assert_eq!(
            p.steps[0].argv,
            vec![
                "sc.exe",
                "create",
                "alkahest",
                "binPath=",
                r#""C:\Program Files\Alkahest\alkahest.exe" serve --service"#,
                "start=",
                "delayed-auto",
                "obj=",
                r"NT AUTHORITY\LocalService",
                "DisplayName=",
                "Alkahest",
            ]
        );
        assert!(
            cmds.iter()
                .any(|c| c.starts_with("sc.exe failure alkahest reset= 86400"))
        );
        assert!(
            cmds.iter()
                .any(|c| c.contains(r"ALKAHEST_HOME=C:\ProgramData\Alkahest"))
        );
        assert!(cmds.iter().any(|c| c.contains("EventCreate.exe")));
        assert!(cmds.iter().any(|c| {
            c.starts_with(r"icacls.exe C:\ProgramData\Alkahest /grant *S-1-5-19:(OI)(CI)M")
        }));
        assert_eq!(cmds.last().unwrap(), "sc.exe start alkahest");
        assert!(p.steps.iter().all(|s| !s.may_fail));
        assert_eq!(p.dirs, vec![PathBuf::from(r"C:\ProgramData\Alkahest")]);
    }

    #[test]
    fn root_maps_the_data_dir_below_it() {
        let args = ServiceArgs {
            root: Some(PathBuf::from("/tmp/r")),
            ..ServiceArgs::default()
        };
        let p = plan_install(&args, "alkahest.exe", &WinHost::default());
        assert_eq!(p.dirs, vec![PathBuf::from("/tmp/r/ProgramData/Alkahest")]);
    }

    #[test]
    fn uninstall_stops_then_deletes() {
        let steps = plan_uninstall();
        assert_eq!(
            lines(&steps),
            vec![
                "sc.exe stop alkahest",
                "sc.exe delete alkahest",
                r"reg.exe delete HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\Alkahest /f",
            ]
        );
        assert!(steps[0].may_fail && !steps[1].may_fail);
    }

    #[test]
    fn status_reads_the_state_line() {
        let out = "SERVICE_NAME: alkahest\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        STATE              : 4  RUNNING\r\n";
        assert_eq!(parse_query(out).as_deref(), Some("RUNNING"));
        assert_eq!(
            parse_query("[SC] EnumQueryServicesStatus:OpenService FAILED 1060:"),
            None
        );
    }

    #[test]
    fn data_dir_follows_program_data() {
        let h = WinHost {
            program_data: r"D:\Data\".into(),
        };
        assert_eq!(data_dir(&h), r"D:\Data\Alkahest");
    }
}
