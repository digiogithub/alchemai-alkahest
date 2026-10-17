//! `cargo xtask`: repository automation.
//!
//! Commands:
//! - `check-deps`: only `alkahest-desktop` may depend on GPUI, and only its `src/ui/` module
//!   may name the gpui-kit facade (AGENTS.md section 4).
//! - `tokens [--check] [--contrast]`: generate the desktop theme from `design/tokens.json`,
//!   check it is current, check WCAG contrast.
//! - `icons`: render the platform icon sets from `design/brand/` into `packaging/icons/`.
//! - `notice [--check]`: generate and verify THIRD_PARTY_LICENSES and NOTICE (ALK-US-0077).
//! - `engines-check [--fail-on-update]`: report newer upstream engine releases (ALK-US-0053).
//! - `docs-cli [--check]`: generate the CLI and configuration reference pages of the user guide (ALK-US-0080).
//! - `bundle`: build the Linux release artefacts: headless tar.gz, desktop .deb and AppImage (ALK-US-0074).

mod bundle;
mod deps;
mod docs;
mod engines;
mod icons;
mod notice;
mod tokens;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        usage();
        return ExitCode::FAILURE;
    };
    let result = match cmd.as_str() {
        "check-deps" => deps::run(),
        "tokens" => workspace_root().and_then(|root| tokens::run(&root, rest)),
        "icons" => workspace_root().and_then(|root| icons::run(&root, rest)),
        "engines-check" => workspace_root().and_then(|root| engines::run(&root, rest)),
        "bundle" => workspace_root().and_then(|root| bundle::run(&root, rest)),
        "docs-cli" => workspace_root().and_then(|root| docs::run(&root, rest)),
        "notice" => workspace_root().and_then(|root| notice::run(&root, rest)),
        other => {
            eprintln!("xtask: unknown command `{other}`");
            usage();
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root: the parent of the xtask manifest directory.
fn workspace_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("xtask manifest has no parent directory"))
}

fn usage() {
    eprintln!(
        "usage: cargo xtask <check-deps|tokens [--check] [--contrast]|icons|notice [--check]|engines-check [--fail-on-update]|docs-cli [--check]|bundle [--flavour ..] [--formats ..] [--no-build]>"
    );
}
