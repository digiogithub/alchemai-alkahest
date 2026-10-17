//! `alkahest`: command-line entry point of the Alkahest model server and manager.
//!
//! Commands whose story is not built yet print `not implemented yet (ALK-US-xxxx)` and exit
//! with code 2. Exit codes: 0 success, 1 runtime error, 2 not implemented or usage error.

mod cli;
mod engines_cmd;
mod hf_cmd;
mod keys_cmd;
mod model_config_cmd;
mod models_cmd;
mod ps_cmd;
mod pull_cmd;
mod search_cmd;
mod serve_cmd;
mod service_cmd;
mod sysinfo_cmd;
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
    let cli = Cli::parse();
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

    // Only the long-running server writes log files; short commands stay on stderr.
    let is_serve = matches!(cli.command, Command::Serve { .. });
    let _log_guard = logging::init(
        &loaded.config.logging,
        &loaded.config.resolve_paths(&loaded.paths).log_dir,
        &LogOptions {
            verbosity: cli.global.verbose,
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
        Command::Hf { action } => hf_cmd::run(action, &loaded.config, cli.global.json),
        Command::Ls => models_cmd::ls(&loaded, cli.global.json),
        Command::Ps => ps_cmd::ps(&loaded, cli.global.json),
        Command::Stop { model } => ps_cmd::stop(model, &loaded, cli.global.json),
        Command::Rm { models } => models_cmd::rm(models, &loaded, cli.global.json),
        Command::Cp {
            source,
            destination,
        } => models_cmd::cp(source, destination, &loaded, cli.global.json),
        Command::Gc { dry_run } => models_cmd::gc(*dry_run, &loaded, cli.global.json),
        Command::Show(args) => models_cmd::show(args, &loaded, cli.global.json),
        Command::Search(args) => search_cmd::run(args, &loaded.config, cli.global.json),
        Command::Pull {
            model,
            insecure,
            force,
            safetensors,
        } => pull_cmd::run(
            model,
            *insecure,
            *force,
            *safetensors,
            &loaded,
            cli.global.json,
        ),
        Command::Engines { action } => engines_cmd::run(action, &loaded, cli.global.json),
        Command::Serve {
            allow_unauthenticated,
        } => serve_cmd::run(&loaded, *allow_unauthenticated),
        Command::Keys { action } => keys_cmd::run(action, &loaded, cli.global.json),
        Command::Service { action } => service_cmd::run(action, &loaded, cli.global.json),
        Command::Desktop(args) => desktop(args, &loaded),
        other => Ok(not_implemented(other)),
    }
}

/// Reports an unbuilt command and names the story that owns it.
fn not_implemented(command: &Command) -> ExitCode {
    let story = command.owner().unwrap_or("unassigned");
    eprintln!("alkahest {}: not implemented yet ({story})", command.name());
    ExitCode::from(EXIT_NOT_IMPLEMENTED)
}

/// `alkahest desktop`: launches the GPUI app against the configured server.
#[cfg(feature = "desktop")]
fn desktop(args: &cli::DesktopArgs, loaded: &Loaded) -> Result<ExitCode> {
    let server = &loaded.config.server;
    let client = alkahest_client::Client::for_bind(&server.host, server.port)?;
    let options = alkahest_desktop::LaunchOptions {
        server_url: client.base_url().to_string(),
        smoke_test: args.smoke_test,
    };
    alkahest_desktop::run(options)?;
    Ok(ExitCode::SUCCESS)
}

/// `alkahest desktop` in a build without the `desktop` feature: explains how to get the app
/// and exits 2 (ALK-SP-0008.R1).
#[cfg(not(feature = "desktop"))]
fn desktop(_args: &cli::DesktopArgs, _loaded: &Loaded) -> Result<ExitCode> {
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
