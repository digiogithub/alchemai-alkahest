//! `alkahest`: command-line entry point of the Alkahest model server and manager.
//!
//! Commands whose story is not built yet print `not implemented yet (ALK-US-xxxx)` and exit
//! with code 2. Exit codes: 0 success, 1 runtime error, 2 not implemented or usage error.

mod catalog_cmd;
mod certs_cmd;
mod cli;
mod config_set_cmd;
mod engines_cmd;
mod hf_cmd;
mod import_cmd;
mod keys_cmd;
mod model_config_cmd;
mod models_cmd;
mod ps_cmd;
mod pull_cmd;
mod run_cmd;
mod search_cmd;
mod serve_cmd;
mod service_cmd;
mod service_launchd;
mod service_windows;
mod store_import_cmd;
mod sysinfo_cmd;
mod update;
// The renderer is consumed by the listing commands (`ls`, `ps`, ...) of ALK-US-0012/0031.
#[allow(dead_code)]
mod output;

use std::process::ExitCode;

use alkahest_core::config::{self, LoadOptions, Loaded};
use alkahest_core::logging::{self, LogOptions};
use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use serde_json::json;

use crate::cli::{Cli, Command, ConfigAction, GlobalArgs};

/// Exit code for commands that exist in the tree but are not built yet.
const EXIT_NOT_IMPLEMENTED: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse_from(effective_args(
        std::env::args_os().collect(),
        std::env::current_exe().ok().as_deref(),
    ));
    // Windows cannot delete a running image, so `alkahest update` leaves `.alkahest.exe.old`
    // behind; the next start removes it.
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        update::apply::cleanup_old(&exe);
    }
    // Completions need no configuration or logging.
    if let Command::Completions { shell } = &cli.command {
        write_completions(*shell, &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }
    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// The command line to parse. Double-clicking `Alkahest.app` starts the bundle executable with
/// no arguments (older macOS releases add a `-psn_<serial>` process-serial-number argument), and
/// a bare `alkahest` would only print the usage text, so inside an app bundle that case means
/// `alkahest desktop` (ALK-US-0075). Every other invocation is passed through unchanged.
fn effective_args(
    mut args: Vec<std::ffi::OsString>,
    exe: Option<&std::path::Path>,
) -> Vec<std::ffi::OsString> {
    let only_psn = args
        .iter()
        .skip(1)
        .all(|a| a.to_string_lossy().starts_with("-psn_"));
    if only_psn && exe.is_some_and(in_app_bundle) {
        args.truncate(1);
        args.push("desktop".into());
    }
    args
}

/// True when `exe` is `<name>.app/Contents/MacOS/<binary>`.
fn in_app_bundle(exe: &std::path::Path) -> bool {
    let mut dirs = exe.ancestors().skip(1);
    dirs.next().is_some_and(|d| d.ends_with("Contents/MacOS"))
        && dirs
            .next()
            .and_then(|d| d.parent())
            .is_some_and(|d| d.extension().is_some_and(|e| e == "app"))
}

/// Writes the completion script for `shell` to `out`.
fn write_completions<W: std::io::Write>(shell: Shell, out: &mut W) {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, out);
}

/// Loads configuration, starts logging and dispatches the command.
fn run(cli: &Cli) -> Result<ExitCode> {
    let mut loaded = config::load(&LoadOptions {
        config_file: cli.global.config.clone(),
        paths: None,
    })
    .context("loading configuration")?;
    // The flag beats the file and the environment.
    if let Some(host) = &cli.global.host {
        loaded
            .config
            .server
            .apply_host_spec(host)
            .context("--host")?;
    }
    if let Some(port) = cli.global.port {
        loaded.config.server.port = port;
    }
    if let Command::Serve {
        ollama_compat,
        no_ollama_compat,
        ollama_port,
        ..
    } = &cli.command
    {
        if *ollama_compat {
            loaded.config.server.ollama_compat = true;
        }
        if *no_ollama_compat {
            loaded.config.server.ollama_compat = false;
        }
        if let Some(p) = ollama_port {
            loaded.config.server.ollama_port = *p;
        }
    }

    // Only the long-running server writes log files; short commands stay on stderr.
    let is_serve = matches!(cli.command, Command::Serve { .. });
    // `alkahest run --verbose` means timing statistics (as in Ollama), not debug logs that
    // would interleave with the streamed answer.
    let is_run = matches!(cli.command, Command::Run(_));
    let _log_guard = logging::init(
        &loaded.config.logging,
        &loaded.config.resolve_paths(&loaded.paths).log_dir,
        &LogOptions {
            verbosity: if is_run { 0 } else { cli.global.verbose },
            level: cli.global.log_level.clone(),
            stderr: true,
            file: Some(is_serve),
        },
    )
    .context("starting logging")?;
    tracing::debug!(command = cli.command.name(), "dispatching");

    match &cli.command {
        Command::Config { action } => config_command(action, &cli.global, &loaded),
        Command::Sysinfo => sysinfo_cmd::run(cli.global.json),
        Command::Update(args) => update::run(args, &loaded, cli.global.json),
        Command::Hf { action } => hf_cmd::run(action, &loaded.config, cli.global.json),
        Command::Ls => models_cmd::ls(&loaded, cli.global.json),
        Command::Ps => ps_cmd::ps(&loaded, cli.global.json),
        Command::Stop { model } => ps_cmd::stop(model, &loaded, cli.global.json),
        Command::Run(args) => run_cmd::run(args, &loaded, cli.global.verbose > 0),
        Command::Rm { models } => models_cmd::rm(models, &loaded, cli.global.json),
        Command::Cp {
            source,
            destination,
        } => models_cmd::cp(source, destination, &loaded, cli.global.json),
        Command::Create(args) => import_cmd::create(args, &loaded, cli.global.json),
        Command::Import(args) if args.store => {
            store_import_cmd::run(&args.path, &loaded, cli.global.json)
        }
        Command::Import(args) => import_cmd::import(args, &loaded, cli.global.json),
        Command::Gc { dry_run } => models_cmd::gc(*dry_run, &loaded, cli.global.json),
        Command::Show(args) => models_cmd::show(args, &loaded, cli.global.json),
        Command::Search(args) => search_cmd::run(args, &loaded, cli.global.json),
        Command::Pull {
            model,
            insecure,
            force,
            safetensors,
            catalog,
        } => pull_cmd::run(
            model,
            pull_cmd::Flags {
                insecure: *insecure,
                force: *force,
                safetensors: *safetensors,
                catalog: catalog.clone(),
            },
            &loaded,
            cli.global.json,
        ),
        Command::Engines { action } => engines_cmd::run(action, &loaded, cli.global.json),
        Command::Catalog { action } => catalog_cmd::run(action, &loaded, cli.global.json),
        Command::Serve {
            allow_unauthenticated,
            service,
            ..
        } => serve_cmd::run(&loaded, *allow_unauthenticated, *service),
        Command::Keys { action } => keys_cmd::run(action, &loaded, cli.global.json),
        Command::Service { action } => service_cmd::run(action, &loaded, cli.global.json),
        Command::Certs { action } => certs_cmd::run(action, &loaded, cli.global.json),
        Command::Desktop(args) => desktop(args, &cli.global, &loaded),
        other => Ok(not_implemented(other)),
    }
}

/// Reports an unbuilt command and names the story that owns it.
fn not_implemented(command: &Command) -> ExitCode {
    let story = command.owner().unwrap_or("unassigned");
    eprintln!("alkahest {}: not implemented yet ({story})", command.name());
    ExitCode::from(EXIT_NOT_IMPLEMENTED)
}

/// `alkahest desktop`: launches the GPUI app against the configured server. The app may start
/// a local server with this very executable (`alkahest [--config <file>] [--host <h>] serve`).
#[cfg(feature = "desktop")]
fn desktop(args: &cli::DesktopArgs, global: &GlobalArgs, loaded: &Loaded) -> Result<ExitCode> {
    #[cfg(windows)]
    detach_own_console();
    let server = &loaded.config.server;
    let client =
        alkahest_client::Client::for_server(&server.host, server.port, server.tls.enabled)?;
    let mut serve_args = Vec::new();
    if let Some(config) = &global.config {
        serve_args.push("--config".into());
        serve_args.push(config.clone().into_os_string());
    }
    if let Some(host) = &global.host {
        serve_args.push("--host".into());
        serve_args.push(host.into());
    }
    let options = alkahest_desktop::LaunchOptions {
        server_url: client.base_url().to_string(),
        generated_ca: server
            .tls
            .enabled
            .then(|| server.tls.ca_path(&loaded.paths.data_dir)),
        api_key: std::env::var(alkahest_client::API_KEY_ENV).ok(),
        settings_file: Some(loaded.paths.config_dir.join("desktop.toml")),
        serve_program: std::env::current_exe().ok(),
        serve_args,
        serve_stderr_log: Some(
            loaded
                .config
                .resolve_paths(&loaded.paths)
                .log_dir
                .join("desktop-serve.stderr.log"),
        ),
        smoke_test: args.smoke_test,
    };
    alkahest_desktop::run(options)?;
    Ok(ExitCode::SUCCESS)
}

/// Closes the console window Windows opens for a console-subsystem program started from a
/// shortcut or Explorer (ALK-US-0076). When another process shares the console (a terminal, a
/// script), it is left alone so `alkahest desktop` run from a shell keeps its output.
#[cfg(all(windows, feature = "desktop"))]
#[allow(unsafe_code)]
fn detach_own_console() {
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut pids = [0u32; 2];
    // SAFETY: `pids` is a valid writable buffer of the length passed; the call only reads
    // process state. `FreeConsole` takes no arguments and only detaches this process.
    unsafe {
        if GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) == 1 {
            FreeConsole();
        }
    }
}

/// `alkahest desktop` in a build without the `desktop` feature: explains how to get the app
/// and exits 2 (ALK-SP-0008.R1).
#[cfg(not(feature = "desktop"))]
fn desktop(_args: &cli::DesktopArgs, _global: &GlobalArgs, _loaded: &Loaded) -> Result<ExitCode> {
    eprintln!(
        "alkahest desktop: this build has no desktop app; \
         rebuild with `cargo build --features desktop` or install the desktop package"
    );
    Ok(ExitCode::from(EXIT_NOT_IMPLEMENTED))
}

/// `alkahest config show|path`.
fn config_command(action: &ConfigAction, global: &GlobalArgs, loaded: &Loaded) -> Result<ExitCode> {
    match action {
        ConfigAction::Show => {
            let resolved = loaded.config.resolve_paths(&loaded.paths);
            if global.json {
                let doc = json!({
                    "config": loaded.config,
                    "sources": loaded.sources,
                    "paths": resolved,
                });
                println!("{}", serde_json::to_string_pretty(&doc)?);
            } else {
                for source in &loaded.sources {
                    println!("# source: {source}");
                }
                print!("{}", loaded.config.to_toml()?);
                println!("\n# resolved paths");
                println!("# models_dir   = {}", resolved.models_dir.display());
                match &resolved.ollama_store {
                    Some(p) => println!("# ollama_store = {} (read-only)", p.display()),
                    None => println!("# ollama_store = (none)"),
                }
                println!("# engines_dir  = {}", resolved.engines_dir.display());
                println!("# log_dir      = {}", resolved.log_dir.display());
            }
        }
        ConfigAction::Set { assignments } => {
            return config_set_cmd::set(assignments, loaded, global.json);
        }
        ConfigAction::Unset { keys } => {
            return config_set_cmd::unset(keys, loaded, global.json);
        }
        ConfigAction::Model { model, action } => {
            return model_config_cmd::run(model, action.as_ref(), loaded, global.json);
        }
        ConfigAction::Path => {
            if global.json {
                let files: Vec<_> = loaded
                    .searched_files
                    .iter()
                    .map(|p| json!({ "path": p, "exists": p.is_file() }))
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "files": files }))?
                );
            } else {
                for p in &loaded.searched_files {
                    let mark = if p.is_file() { "found  " } else { "missing" };
                    println!("{mark} {}", p.display());
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<std::ffi::OsString> {
        list.iter().map(Into::into).collect()
    }

    #[test]
    fn a_bare_launch_inside_an_app_bundle_opens_the_desktop() {
        let exe = std::path::Path::new("/Applications/alkahest.app/Contents/MacOS/alkahest");
        assert_eq!(
            effective_args(args(&["alkahest"]), Some(exe)),
            args(&["alkahest", "desktop"])
        );
        assert_eq!(
            effective_args(args(&["alkahest", "-psn_0_12345"]), Some(exe)),
            args(&["alkahest", "desktop"])
        );
    }

    #[test]
    fn explicit_commands_and_plain_installs_are_left_alone() {
        let app = std::path::Path::new("/Applications/alkahest.app/Contents/MacOS/alkahest");
        let plain = std::path::Path::new("/usr/local/bin/alkahest");
        assert_eq!(
            effective_args(args(&["alkahest", "serve"]), Some(app)),
            args(&["alkahest", "serve"])
        );
        assert_eq!(
            effective_args(args(&["alkahest"]), Some(plain)),
            args(&["alkahest"])
        );
        assert_eq!(
            effective_args(args(&["alkahest"]), None),
            args(&["alkahest"])
        );
        assert!(!in_app_bundle(std::path::Path::new(
            "/opt/Contents/MacOS/alkahest"
        )));
    }

    #[test]
    fn completions_for_every_shell_mention_the_binary() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
            let mut buf = Vec::new();
            write_completions(shell, &mut buf);
            let text = String::from_utf8(buf).unwrap();
            assert!(text.contains("alkahest"), "{shell}");
            assert!(text.contains("completions"), "{shell} lacks subcommand");
        }
    }
}
