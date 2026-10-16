//! `alkahest hf login|logout|whoami`: the Hugging Face token in the OS keyring (ALK-T-0060).
//!
//! The token is read without echo from a terminal, or as one line from a pipe, and is never
//! printed, logged or put on a command line. `HF_TOKEN` keeps precedence over the keyring
//! everywhere else, so `whoami` reports which of the two is in effect.

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

use alkahest_core::config::{Config, HfConfig};
use alkahest_core::paths::{EnvFn, process_env};
use alkahest_hf::{HfClient, KeyringStore, Token, TokenStore, WhoAmI};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use crate::cli::HfAction;

/// Runs the command against the real keyring, environment and terminal.
pub fn run(action: &HfAction, config: &Config, json: bool) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let endpoint = alkahest_hf::resolve_endpoint(&config.hf, &process_env);
    runtime.block_on(execute(
        action,
        &endpoint,
        &process_env,
        &KeyringStore,
        &mut read_token_interactive,
        json,
        &mut std::io::stdout().lock(),
    ))
}

/// The testable core: every side effect is injected.
pub(crate) async fn execute(
    action: &HfAction,
    endpoint: &str,
    env: EnvFn<'_>,
    store: &dyn TokenStore,
    read_token: &mut dyn FnMut() -> Result<String>,
    json: bool,
    out: &mut dyn Write,
) -> Result<ExitCode> {
    match action {
        HfAction::Login { no_verify } => {
            let token = Token::new(read_token()?).ok_or_else(|| anyhow!("no token given"))?;
            let who = if *no_verify {
                None
            } else {
                let client = HfClient::new(endpoint, Some(token.clone()))?;
                Some(
                    client
                        .whoami()
                        .await
                        .context("the Hub rejected the token")?,
                )
            };
            store
                .set(&token)
                .context("storing the token in the OS keyring (set HF_TOKEN instead if no keyring is available)")?;
            match who {
                Some(w) => writeln!(
                    out,
                    "Logged in as {}; token stored in the OS keyring.",
                    w.name
                )?,
                None => writeln!(out, "Token stored in the OS keyring (not verified).")?,
            }
            Ok(ExitCode::SUCCESS)
        }
        HfAction::Logout => {
            store
                .delete()
                .context("removing the token from the OS keyring")?;
            writeln!(out, "Token removed from the OS keyring.")?;
            if HfConfig::token(env).is_some() {
                writeln!(
                    out,
                    "Note: HF_TOKEN is still set in the environment and keeps being used."
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        HfAction::Whoami => {
            let (token, source) = match HfConfig::token(env).and_then(Token::new) {
                Some(t) => (t, "HF_TOKEN"),
                None => match store.get().ok().flatten() {
                    Some(t) => (t, "OS keyring"),
                    None => {
                        eprintln!("not logged in (run `alkahest hf login` or set HF_TOKEN)");
                        return Ok(ExitCode::FAILURE);
                    }
                },
            };
            let who = HfClient::new(endpoint, Some(token))?.whoami().await?;
            print_whoami(&who, source, json, out)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Prints the identity, never the token.
fn print_whoami(who: &WhoAmI, source: &str, as_json: bool, out: &mut dyn Write) -> Result<()> {
    if as_json {
        let doc = json!({ "account": who, "token_source": source });
        writeln!(out, "{}", serde_json::to_string_pretty(&doc)?)?;
        return Ok(());
    }
    writeln!(out, "{}", who.name)?;
    if let Some(full) = &who.fullname {
        writeln!(out, "  name:   {full}")?;
    }
    if !who.orgs.is_empty() {
        writeln!(out, "  orgs:   {}", who.orgs.join(", "))?;
    }
    if let Some(role) = &who.token_role {
        writeln!(out, "  token:  {role} (from {source})")?;
    } else {
        writeln!(out, "  token:  from {source}")?;
    }
    Ok(())
}

/// A hidden prompt on a terminal, a plain stdin line otherwise.
fn read_token_interactive() -> Result<String> {
    if std::io::stdin().is_terminal() {
        return rpassword::prompt_password("Hugging Face token: ").context("reading the token");
    }
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading the token from stdin")?;
    if line.trim().is_empty() {
        bail!("no token on stdin");
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[derive(Default)]
    struct Memory(RefCell<Option<Token>>);

    impl TokenStore for Memory {
        fn get(&self) -> alkahest_hf::Result<Option<Token>> {
            Ok(self.0.borrow().clone())
        }
        fn set(&self, token: &Token) -> alkahest_hf::Result<()> {
            *self.0.borrow_mut() = Some(token.clone());
            Ok(())
        }
        fn delete(&self) -> alkahest_hf::Result<()> {
            *self.0.borrow_mut() = None;
            Ok(())
        }
    }

    const WHOAMI: &str = r#"{"type":"user","name":"jose","fullname":"Jose R","orgs":[{"name":"digio"}],
        "auth":{"type":"access_token","accessToken":{"displayName":"laptop","role":"read"}}}"#;

    async fn hub(token: &str, status: u16) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/whoami-v2"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(status).set_body_raw(WHOAMI, "application/json"))
            .mount(&server)
            .await;
        server
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    async fn run_action(
        action: HfAction,
        server: &MockServer,
        env: EnvFn<'_>,
        store: &Memory,
        token: &str,
    ) -> (Result<ExitCode>, String) {
        let mut out = Vec::new();
        let token = token.to_string();
        let res = execute(
            &action,
            &server.uri(),
            env,
            store,
            &mut || Ok(format!("{token}\n")),
            false,
            &mut out,
        )
        .await;
        (res, String::from_utf8(out).unwrap_or_default())
    }

    #[tokio::test]
    async fn login_verifies_then_stores_and_never_prints_the_token() {
        let server = hub("hf_secret", 200).await;
        let store = Memory::default();
        let (res, out) = run_action(
            HfAction::Login { no_verify: false },
            &server,
            &no_env,
            &store,
            "hf_secret",
        )
        .await;
        assert!(res.is_ok());
        assert!(out.contains("Logged in as jose"));
        assert!(!out.contains("hf_secret"));
        assert_eq!(store.get().unwrap().unwrap().expose(), "hf_secret");
    }

    #[tokio::test]
    async fn login_with_a_rejected_token_stores_nothing() {
        let server = hub("hf_bad", 401).await;
        let store = Memory::default();
        let (res, _) = run_action(
            HfAction::Login { no_verify: false },
            &server,
            &no_env,
            &store,
            "hf_bad",
        )
        .await;
        let err = format!("{:#}", res.unwrap_err());
        assert!(err.contains("rejected"), "{err}");
        assert!(!err.contains("hf_bad"));
        assert!(store.get().unwrap().is_none());
    }

    #[tokio::test]
    async fn whoami_uses_the_keyring_token_and_reports_the_account() {
        let server = hub("hf_secret", 200).await;
        let store = Memory::default();
        store.set(&Token::new("hf_secret").unwrap()).unwrap();
        let (res, out) = run_action(HfAction::Whoami, &server, &no_env, &store, "").await;
        assert!(res.is_ok());
        assert!(
            out.contains("jose") && out.contains("digio") && out.contains("read (from OS keyring)")
        );
        assert!(!out.contains("hf_secret"));
    }

    #[tokio::test]
    async fn environment_token_wins_over_the_keyring() {
        let server = hub("hf_env", 200).await;
        let store = Memory::default();
        store.set(&Token::new("hf_old").unwrap()).unwrap();
        let env = |k: &str| (k == "HF_TOKEN").then(|| "hf_env".to_string());
        let (res, out) = run_action(HfAction::Whoami, &server, &env, &store, "").await;
        assert!(res.is_ok());
        assert!(out.contains("from HF_TOKEN"));
    }

    #[tokio::test]
    async fn logout_clears_the_store_and_notes_a_lingering_env_token() {
        let server = MockServer::start().await;
        let store = Memory::default();
        store.set(&Token::new("hf_secret").unwrap()).unwrap();
        let env = |k: &str| (k == "HF_TOKEN").then(|| "hf_env".to_string());
        let (res, out) = run_action(HfAction::Logout, &server, &env, &store, "").await;
        assert!(res.is_ok());
        assert!(store.get().unwrap().is_none());
        assert!(out.contains("HF_TOKEN is still set"));
    }
}
