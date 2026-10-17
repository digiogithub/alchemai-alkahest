//! `alkahest certs generate` (ALK-US-0044): a self-signed certificate for HTTPS.
//!
//! Writes a throwaway CA (`ca.pem`) and a server certificate signed by it (`cert.pem`, with
//! the CA appended) plus its private key (`key.pem`, mode 0600) to the paths `[server.tls]`
//! names, or `<data_dir>/tls/` by default. The CA key is never written, so the CA can sign
//! nothing else. Clients trust `ca.pem`: the CLI picks it up by itself when it runs with the
//! same data directory, other machines set `ALKAHEST_CA_CERT`.

use std::process::ExitCode;

use alkahest_api::tls::{self, CertPaths};
use alkahest_core::config::Loaded;
use anyhow::{Context, Result};
use serde_json::json;
use time::format_description::well_known::Rfc3339;

use crate::cli::CertsAction;

/// Runs `alkahest certs ...`.
pub fn run(action: &CertsAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    match action {
        CertsAction::Generate { hosts, days, force } => {
            generate(loaded, hosts, *days, *force, json)
        }
    }
}

/// The names the certificate covers: loopback, this host, the configured bind host when it
/// is a real address, and every `--host`.
fn names(loaded: &Loaded, extra: &[String]) -> Vec<String> {
    let mut names = tls::default_names();
    let bind = loaded.config.server.host.trim();
    if !matches!(bind, "" | "0.0.0.0" | "::" | "[::]") {
        names.push(bind.trim_matches(['[', ']']).to_string());
    }
    names.extend(extra.iter().map(|h| h.trim().to_string()));
    let mut seen = std::collections::BTreeSet::new();
    names.retain(|n| !n.is_empty() && seen.insert(n.to_ascii_lowercase()));
    names
}

fn generate(
    loaded: &Loaded,
    hosts: &[String],
    days: u32,
    force: bool,
    json: bool,
) -> Result<ExitCode> {
    let tls_config = &loaded.config.server.tls;
    let data_dir = &loaded.paths.data_dir;
    let paths = CertPaths {
        cert: tls_config.cert_path(data_dir),
        key: tls_config.key_path(data_dir),
        ca: tls_config.ca_path(data_dir),
    };
    let names = names(loaded, hosts);
    let generated = tls::generate_self_signed(&names, days)?;
    tls::write_generated(&generated, &paths, force)?;
    let expires = generated
        .not_after
        .format(&Rfc3339)
        .context("formatting the expiry")?;
    if json {
        println!(
            "{}",
            json!({
                "cert": paths.cert,
                "key": paths.key,
                "ca": paths.ca,
                "names": generated.names,
                "expires": expires,
                "enabled": tls_config.enabled,
            })
        );
    } else {
        println!("certificate: {}", paths.cert.display());
        println!("private key: {} (owner only)", paths.key.display());
        println!("client CA:   {}", paths.ca.display());
        println!("valid for:   {}", generated.names.join(", "));
        println!("expires:     {expires}");
        if tls_config.enabled {
            println!(
                "a running server picks the new certificate up within \
                 server.tls.reload_interval_secs, or at once on SIGHUP"
            );
        } else {
            println!(
                "enable HTTPS with `[server.tls] enabled = true` in the configuration and \
                 restart the server"
            );
        }
        println!(
            "clients on other machines: copy {} and set ALKAHEST_CA_CERT to it",
            paths.ca.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}
