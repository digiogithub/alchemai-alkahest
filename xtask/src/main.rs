//! `cargo xtask`: repository automation.
//!
//! Commands:
//! - `check-deps`: only `alkahest-desktop` may depend on GPUI, and only its `src/ui/` module
//!   may name the gpui-kit facade (AGENTS.md section 4).
//! - `tokens`, `icons`, `notice`, `bundle`: placeholders filled by later stories.

mod deps;

use std::process::ExitCode;

/// Subcommands that exist but are implemented by later stories.
const PLANNED: &[&str] = &["tokens", "icons", "notice", "bundle"];

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("check-deps") => match deps::run() {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(err) => {
                eprintln!("xtask: {err:#}");
                ExitCode::FAILURE
            }
        },
        Some(cmd) if PLANNED.contains(&cmd) => {
            println!("xtask {cmd}: not implemented yet");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("xtask: unknown command `{other}`");
            usage();
            ExitCode::FAILURE
        }
        None => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!("usage: cargo xtask <check-deps|tokens|icons|notice|bundle>");
}
