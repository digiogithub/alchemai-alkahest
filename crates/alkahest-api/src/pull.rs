//! Pull orchestration shared by `alkahest pull` and `POST /api/pull`: parse the reference,
//! pick the Ollama registry, Hugging Face or git catalog path (`alkahest/<namespace>/<model>`,
//! ALK-SP-0011.R13), transfer into the Alkahest store and persist recommended defaults.
//!
//! It lives here rather than in `alkahest-pull` because it needs both registry clients, and
//! `alkahest-ollama` / `alkahest-hf` themselves depend on `alkahest-pull`.

use std::time::Duration;

use alkahest_catalog::{CatalogPullError, KeyringCredentials, load_states, pull_catalog};
use alkahest_core::Config;
use alkahest_core::config::ResolvedPaths;
use alkahest_core::paths::process_env;
use alkahest_fetch::FetchConfig;
use alkahest_hf::pull::{HfPullError, PullFormat, pull_hf_with};
use alkahest_hf::{HfClient, KeyringStore};
use alkahest_ollama::{
    PullError as OllamaPullError, PullOptions as OllamaPullOptions, PullOutcome, RegistryClient,
    RegistryError, pull_ollama,
};
use alkahest_pull::{PullProgress, Transfer};
use alkahest_store::{AlkahestStore, ModelOverlays, ModelRef, OllamaStore, ScopedRef};
use tokio_util::sync::CancellationToken;

/// Options of one pull.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullOptions {
    /// Plain HTTP to the registry (private registries without TLS).
    pub insecure: bool,
    /// Download into the Alkahest store even when the Ollama store already has the model.
    pub force: bool,
    /// Pull a safetensors snapshot for vLLM instead of GGUF (`pull --safetensors`).
    pub safetensors: bool,
    /// Catalog models: take the entry from this configured catalog instead of the highest
    /// layer that has it (`pull --catalog <name>`).
    pub catalog: Option<String>,
}

/// How a pull ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullSummary {
    /// The model is installed in the Alkahest store.
    Installed {
        /// Fully qualified stored name.
        name: String,
        /// Short display name.
        short_name: String,
        /// Manifest digest (`sha256:...`).
        manifest_digest: String,
        /// Bytes downloaded by this run.
        downloaded: u64,
        /// Hugging Face commit, for HF pulls.
        revision: Option<String>,
        /// Files fetched, for HF and catalog pulls.
        files: Vec<String>,
        /// Catalog models: the catalog layer the entry came from (`official@2d45ae95a500`).
        origin: Option<String>,
    },
    /// The Ollama store already has the model; it is used from there.
    ReusedOllama {
        /// Fully qualified name.
        name: String,
        /// Short display name.
        short_name: String,
    },
}

/// Why a pull failed.
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    /// The reference is not a pullable model name.
    #[error("{0}")]
    InvalidReference(String),
    /// The model (or the requested quantisation) does not exist upstream.
    #[error("{0}")]
    NotFound(String),
    /// Anything else (network, store, registry).
    #[error("{0}")]
    Failed(String),
}

/// Pulls models into the Alkahest store with the server's configuration.
#[derive(Debug, Clone)]
pub struct PullService {
    config: Config,
    paths: ResolvedPaths,
}

impl PullService {
    /// A service for `config` with its resolved paths.
    pub fn new(config: Config, paths: ResolvedPaths) -> Self {
        Self { config, paths }
    }

    /// Pulls `reference` (`llama3.2:3b`, `hf.co/org/repo:Q4_K_M`, `host/ns/model:tag`).
    /// `progress` receives transfer events; cancelling `cancel` stops the transfer (the
    /// partial download stays resumable).
    pub async fn pull(
        &self,
        reference: &str,
        opts: PullOptions,
        progress: Option<PullProgress>,
        cancel: CancellationToken,
    ) -> Result<PullSummary, PullError> {
        let scoped: ScopedRef = reference.parse().map_err(|e| {
            PullError::InvalidReference(format!("invalid model reference `{reference}`: {e}"))
        })?;
        let store = AlkahestStore::open_resolved(&self.paths)
            .map_err(|e| PullError::Failed(format!("opening the model store: {e}")))?;
        let fetch = FetchConfig {
            max_bytes_per_sec: self.config.pull.bandwidth_cap(),
            ..FetchConfig::default()
        };
        match scoped.reference {
            ModelRef::Name { name, .. } if name.is_catalog() => {
                let http = http_client()?;
                let mut transfer = Transfer::new(store, http, fetch, cancel);
                if let Some(p) = progress {
                    transfer = transfer.with_progress(p);
                }
                let states = load_states(&self.paths.data_dir, &self.config.catalogs);
                let overlays = ModelOverlays::in_data_dir(&self.paths.data_dir);
                let outcome = pull_catalog(
                    &transfer,
                    &states,
                    &name,
                    opts.catalog.as_deref(),
                    &KeyringCredentials,
                    Some(&overlays),
                )
                .await
                .map_err(|e| {
                    let msg = format!("pulling {name}: {e}");
                    match e {
                        e if e.is_not_found() => PullError::NotFound(msg),
                        CatalogPullError::NotACatalogName(_) | CatalogPullError::Name(_) => {
                            PullError::InvalidReference(msg)
                        }
                        _ => PullError::Failed(msg),
                    }
                })?;
                let short = &outcome.commit[..outcome.commit.len().min(12)];
                Ok(PullSummary::Installed {
                    short_name: outcome.name.display_short(),
                    name: outcome.name.to_string(),
                    manifest_digest: outcome.manifest_digest.to_string(),
                    downloaded: outcome.downloaded,
                    revision: Some(outcome.commit.clone()).filter(|c| !c.is_empty()),
                    files: outcome.files,
                    origin: Some(if short.is_empty() {
                        outcome.catalog
                    } else {
                        format!("{}@{short}", outcome.catalog)
                    }),
                })
            }
            ModelRef::Name { name, .. } => {
                let registry = RegistryClient::for_name(&name, opts.insecure)
                    .map_err(|e| PullError::InvalidReference(e.to_string()))?;
                let mut transfer = Transfer::new(store, registry.http().clone(), fetch, cancel);
                if let Some(p) = progress {
                    transfer = transfer.with_progress(p);
                }
                let ollama = OllamaStore::discover(&self.paths)
                    .map_err(|e| PullError::Failed(format!("opening the Ollama store: {e}")))?;
                let outcome = pull_ollama(
                    &transfer,
                    &registry,
                    ollama.as_ref(),
                    &name,
                    OllamaPullOptions { force: opts.force },
                )
                .await
                .map_err(|e| {
                    let msg = format!("pulling {}: {e}", name.display_short());
                    match e {
                        OllamaPullError::Registry(RegistryError::NotFound(_)) => {
                            PullError::NotFound(msg)
                        }
                        OllamaPullError::NotARegistryName(_) => PullError::InvalidReference(msg),
                        _ => PullError::Failed(msg),
                    }
                })?;
                Ok(match outcome {
                    PullOutcome::Installed {
                        name,
                        manifest_digest,
                        downloaded,
                    } => PullSummary::Installed {
                        short_name: name.display_short(),
                        name: name.to_string(),
                        manifest_digest: manifest_digest.to_string(),
                        downloaded,
                        revision: None,
                        files: Vec::new(),
                        origin: None,
                    },
                    PullOutcome::ReusedOllama { name } => PullSummary::ReusedOllama {
                        short_name: name.display_short(),
                        name: name.to_string(),
                    },
                })
            }
            ModelRef::HuggingFace(hf) => {
                let client = HfClient::from_config(&self.config.hf, &process_env, &KeyringStore)
                    .map_err(|e| PullError::Failed(e.to_string()))?;
                let http = http_client()?;
                let mut transfer = Transfer::new(store, http, fetch, cancel);
                if let Some(p) = progress {
                    transfer = transfer.with_progress(p);
                }
                let overlays = ModelOverlays::in_data_dir(&self.paths.data_dir);
                let format = if opts.safetensors {
                    PullFormat::Safetensors
                } else {
                    PullFormat::Auto
                };
                let outcome = pull_hf_with(&transfer, &client, &hf, Some(&overlays), format)
                    .await
                    .map_err(|e| {
                        let msg = format!("pulling {hf}: {e}");
                        match e {
                            HfPullError::NotFound { .. }
                            | HfPullError::NoGguf(_)
                            | HfPullError::NoSafetensors(_) => PullError::NotFound(msg),
                            HfPullError::Name(_) => PullError::InvalidReference(msg),
                            _ => PullError::Failed(msg),
                        }
                    })?;
                Ok(PullSummary::Installed {
                    short_name: outcome.name.display_short(),
                    name: outcome.name.to_string(),
                    manifest_digest: outcome.manifest_digest.to_string(),
                    downloaded: outcome.downloaded,
                    revision: Some(outcome.revision),
                    files: outcome.files,
                    origin: None,
                })
            }
            ModelRef::Digest(d) => Err(PullError::InvalidReference(format!(
                "`{d}` is a digest, not a model name; nothing to pull"
            ))),
        }
    }
}

/// The HTTP client of Hugging Face and catalog downloads (redirects followed, the bearer
/// dropped by reqwest when a redirect leaves the host).
fn http_client() -> Result<reqwest::Client, PullError> {
    reqwest::Client::builder()
        .user_agent(concat!("alkahest/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| PullError::Failed(format!("building the HTTP client: {e}")))
}
