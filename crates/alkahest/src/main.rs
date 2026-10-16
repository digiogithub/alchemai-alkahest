//! `alkahest`: command-line entry point of the Alkahest model server and manager.
//!
//! Only `--version` and `--help` exist for now; the full command tree (`pull`, `ls`, `ps`,
//! `serve`, `desktop`, ...) arrives with ALK-US-0004.

use clap::Parser;

/// Top-level command-line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "alkahest",
    version,
    about = "Local AI model server and manager"
)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
