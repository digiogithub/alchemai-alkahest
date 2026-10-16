//! The Hub API client.

use std::fmt;
use std::time::Duration;

use alkahest_core::config::HfConfig;
use alkahest_core::paths::EnvFn;
use reqwest::header::{ACCEPT, RETRY_AFTER};
use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;
use url::Url;

use crate::card::ModelCard;
use crate::error::{HfError, Result};
use crate::recommended::RecommendedParams;
use crate::token::{Token, TokenStore, resolve_endpoint, resolve_token};
use crate::types::{ModelInfo, ModelSummary, RepoFile, SearchQuery, TreeEntry, WhoAmI};

/// Revision used when the caller names none.
pub const DEFAULT_REVISION: &str = "main";

/// Upper bound of tree pages followed, a guard against a misbehaving `Link` header.
const MAX_TREE_PAGES: usize = 50;

/// Client for one Hub endpoint. The token, when present, is sent only to this endpoint.
#[derive(Clone)]
pub struct HfClient {
    http: Client,
    endpoint: Url,
    token: Option<Token>,
}

impl fmt::Debug for HfClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HfClient")
            .field("endpoint", &self.endpoint.as_str())
            .field("token", &self.token)
            .finish()
    }
}

impl HfClient {
    /// A client for `endpoint` (for example `https://huggingface.co`) with an optional token.
    pub fn new(endpoint: &str, token: Option<Token>) -> Result<Self> {
        let endpoint = Url::parse(endpoint.trim_end_matches('/'))
            .map_err(|e| HfError::InvalidUrl(format!("{endpoint}: {e}")))?;
        if endpoint.cannot_be_a_base() {
            return Err(HfError::InvalidUrl(endpoint.to_string()));
        }
        let http = Client::builder()
            .user_agent(concat!("alkahest/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            endpoint,
            token,
        })
    }

    /// Builds a client from the configuration: endpoint from [`resolve_endpoint`] (config or
    /// `ALKAHEST_HF_ENDPOINT`, then `HF_ENDPOINT`), token from [`resolve_token`] (environment,
    /// then `store`).
    pub fn from_config(config: &HfConfig, env: EnvFn<'_>, store: &dyn TokenStore) -> Result<Self> {
        Self::new(&resolve_endpoint(config, env), resolve_token(env, store))
    }

    /// The Hub endpoint this client talks to.
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str().trim_end_matches('/')
    }

    /// Whether a token is configured.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// The bearer token to use for `url`, only when `url` is on the configured Hub endpoint
    /// (same scheme, host and port). Downloads from CDN hosts get none.
    pub fn bearer_for(&self, url: &str) -> Option<&str> {
        let parsed = Url::parse(url).ok()?;
        let same = parsed.scheme() == self.endpoint.scheme()
            && parsed.host_str() == self.endpoint.host_str()
            && parsed.port_or_known_default() == self.endpoint.port_or_known_default();
        if same {
            self.token.as_ref().map(Token::expose)
        } else {
            None
        }
    }

    /// `{endpoint}/{repo}/resolve/{revision}/{file}`: the download URL of a file. Each path
    /// segment is percent-encoded; a revision containing `/` becomes one encoded segment.
    pub fn resolve_url(&self, repo: &str, revision: Option<&str>, file: &str) -> Result<String> {
        let rev = revision.unwrap_or(DEFAULT_REVISION);
        let mut url = self.endpoint.clone();
        {
            let mut seg = url
                .path_segments_mut()
                .map_err(|()| HfError::InvalidUrl(self.endpoint.to_string()))?;
            seg.pop_if_empty();
            seg.extend(repo.split('/'));
            seg.push("resolve");
            seg.push(rev);
            seg.extend(file.split('/'));
        }
        Ok(url.to_string())
    }

    /// Searches models (`GET /api/models`).
    pub async fn search_models(&self, query: &SearchQuery) -> Result<Vec<ModelSummary>> {
        let mut url = self.api_url(&["models"])?;
        {
            let mut q = url.query_pairs_mut();
            if !query.search.is_empty() {
                q.append_pair("search", &query.search);
            }
            if let Some(f) = &query.filter {
                q.append_pair("filter", f);
            }
            if let Some(a) = &query.author {
                q.append_pair("author", a);
            }
            if let Some(t) = &query.pipeline_tag {
                q.append_pair("pipeline_tag", t);
            }
            q.append_pair("sort", query.sort.as_param());
            q.append_pair("direction", "-1");
            q.append_pair("limit", &query.limit.to_string());
        }
        self.get_json(url, "model search").await
    }

    /// Repository metadata with file sizes and LFS digests (`GET /api/models/{repo}?blobs=true`,
    /// or `.../revision/{rev}` when a revision is given).
    pub async fn model_info(&self, repo: &str, revision: Option<&str>) -> Result<ModelInfo> {
        let mut url = self.repo_api_url(repo, revision.map(|r| ["revision", r]))?;
        url.query_pairs_mut().append_pair("blobs", "true");
        self.get_json(url, repo).await
    }

    /// Flat file list of a revision from the `siblings` of [`HfClient::model_info`].
    pub async fn repo_files(&self, repo: &str, revision: Option<&str>) -> Result<Vec<RepoFile>> {
        let info = self.model_info(repo, revision).await?;
        Ok(info.siblings.iter().map(RepoFile::from).collect())
    }

    /// Recursive file tree (`GET /api/models/{repo}/tree/{rev}?recursive=true`), following
    /// `Link: rel="next"` pagination.
    pub async fn tree(&self, repo: &str, revision: Option<&str>) -> Result<Vec<TreeEntry>> {
        let rev = revision.unwrap_or(DEFAULT_REVISION);
        let mut url = self.repo_api_url(repo, Some(["tree", rev]))?;
        url.query_pairs_mut().append_pair("recursive", "true");
        let mut out = Vec::new();
        let mut next = Some(url);
        for _ in 0..MAX_TREE_PAGES {
            let Some(url) = next.take() else { break };
            let resp = self.send(url, repo).await?;
            next = next_link(&resp);
            let page: Vec<TreeEntry> = serde_json::from_slice(&resp.bytes().await?)?;
            out.extend(page);
        }
        Ok(out)
    }

    /// The raw `README.md` of a revision.
    pub async fn readme(&self, repo: &str, revision: Option<&str>) -> Result<String> {
        let url = Url::parse(&self.resolve_url(repo, revision, "README.md")?)
            .map_err(|e| HfError::InvalidUrl(e.to_string()))?;
        Ok(self.send(url, repo).await?.text().await?)
    }

    /// The parsed model card of a revision.
    pub async fn model_card(&self, repo: &str, revision: Option<&str>) -> Result<ModelCard> {
        ModelCard::parse(&self.readme(repo, revision).await?)
    }

    /// Who the configured token belongs to (`GET /api/whoami-v2`). Fails with
    /// [`HfError::Unauthorized`] when there is no token or it is invalid.
    pub async fn whoami(&self) -> Result<WhoAmI> {
        let url = self.api_url(&["whoami-v2"])?;
        let value: serde_json::Value = self.get_json(url, "whoami").await?;
        WhoAmI::from_json(&value).ok_or_else(|| {
            HfError::Parse(<serde_json::Error as serde::de::Error>::custom(
                "whoami-v2 response has no `name`",
            ))
        })
    }

    /// `generation_config.json` of a revision; `None` when the repository has none (GGUF-only
    /// repositories usually do not).
    pub async fn generation_config(
        &self,
        repo: &str,
        revision: Option<&str>,
    ) -> Result<Option<serde_json::Value>> {
        let url = Url::parse(&self.resolve_url(repo, revision, "generation_config.json")?)
            .map_err(|e| HfError::InvalidUrl(e.to_string()))?;
        match self.send(url, repo).await {
            Ok(resp) => Ok(Some(serde_json::from_slice(&resp.bytes().await?)?)),
            Err(HfError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Recommended defaults of a repository (ALK-SP-0010.R3): its `generation_config.json`
    /// and model card, the trained context length from the Hub metadata, and, when the
    /// repository itself has neither (a GGUF conversion), the same files of its `base_model`.
    /// A missing README is not an error.
    pub async fn recommended_params(
        &self,
        repo: &str,
        revision: Option<&str>,
    ) -> Result<RecommendedParams> {
        let trained = self
            .model_info(repo, revision)
            .await
            .ok()
            .and_then(|i| i.gguf.and_then(|g| g.context_length));
        let mut params = self.params_of(repo, revision).await?;
        if params.is_empty() {
            let base = match self.model_card(repo, revision).await {
                Ok(card) => card.base_model(),
                Err(HfError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            if let Some(base) = base.filter(|b| b != repo) {
                params = self.params_of(&base, None).await?;
            }
        }
        Ok(params.with_trained_context(trained))
    }

    /// Params from one repository's own files.
    async fn params_of(&self, repo: &str, revision: Option<&str>) -> Result<RecommendedParams> {
        let gen_cfg = self.generation_config(repo, revision).await?;
        let card = match self.model_card(repo, revision).await {
            Ok(card) => Some(card),
            Err(HfError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        Ok(RecommendedParams::combine(gen_cfg.as_ref(), card.as_ref()))
    }

    fn api_url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.endpoint.clone();
        {
            let mut seg = url
                .path_segments_mut()
                .map_err(|()| HfError::InvalidUrl(self.endpoint.to_string()))?;
            seg.pop_if_empty();
            seg.push("api");
            seg.extend(segments);
        }
        Ok(url)
    }

    fn repo_api_url<const N: usize>(&self, repo: &str, tail: Option<[&str; N]>) -> Result<Url> {
        let mut url = self.api_url(&["models"])?;
        {
            let mut seg = url
                .path_segments_mut()
                .map_err(|()| HfError::InvalidUrl(self.endpoint.to_string()))?;
            seg.extend(repo.split('/'));
            if let Some(tail) = tail {
                seg.extend(tail);
            }
        }
        Ok(url)
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url, target: &str) -> Result<T> {
        let resp = self.send(url, target).await?;
        Ok(serde_json::from_slice(&resp.bytes().await?)?)
    }

    /// GET with the token (same-endpoint URLs only) and status mapping.
    async fn send(&self, url: Url, target: &str) -> Result<Response> {
        let mut req = self
            .http
            .get(url.clone())
            .header(ACCEPT, "application/json");
        if let Some(t) = self.bearer_for(url.as_str()) {
            req = req.bearer_auth(t);
        }
        map_status(req.send().await?, target)
    }
}

/// Maps Hub statuses to typed errors.
fn map_status(resp: Response, target: &str) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let code = resp
        .headers()
        .get("x-error-code")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Err(match status {
        StatusCode::UNAUTHORIZED => HfError::Unauthorized {
            target: target.to_string(),
        },
        StatusCode::FORBIDDEN => HfError::Forbidden {
            target: target.to_string(),
        },
        StatusCode::NOT_FOUND => HfError::NotFound(match code {
            Some(c) => format!("{target} ({c})"),
            None => target.to_string(),
        }),
        StatusCode::TOO_MANY_REQUESTS => HfError::RateLimited {
            retry_after_secs: resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok()),
        },
        s => HfError::Status {
            status: s.as_u16(),
            url: resp
                .url()
                .as_str()
                .split('?')
                .next()
                .unwrap_or("")
                .to_string(),
        },
    })
}

/// The `rel="next"` target of a `Link` header, if any.
fn next_link(resp: &Response) -> Option<Url> {
    let header = resp.headers().get("link")?.to_str().ok()?;
    header.split(',').find_map(|part| {
        let (target, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"")
            .then(|| Url::parse(target.trim().trim_start_matches('<').trim_end_matches('>')).ok())?
    })
}
