//! Model discovery proxies (ALK-SP-0008.R4), so the desktop app needs neither its own
//! Hugging Face client nor the token: the server holds it.
//!
//! * `GET /alkahest/v1/hf/search?q=&format=gguf|safetensors|all&task=&sort=downloads|likes|recent&author=&limit=`
//! * `GET /alkahest/v1/hf/model?repo=org/name&revision=&readme=true`: Hub metadata, card front
//!   matter (and README body on request) and the recommended parameters.
//! * `GET /alkahest/v1/hf/files?repo=&revision=`: GGUF quantisations (shards grouped, with the
//!   pull reference of each), projectors and safetensors weights.
//! * `GET|PUT|DELETE /alkahest/v1/hf/token`: token presence and account; store or remove the
//!   token in the server's keyring. The token is accepted, never returned.
//! * `GET /alkahest/v1/ollama/search?q=&limit=`: the Ollama library.

use alkahest_hf::{
    HfClient, HfError, RepoFile, SearchQuery, SortKey, Token, gguf_variants, mmproj_files,
    resolve_token,
};
use alkahest_ollama::{LibraryError, OllamaLibrary};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::context::{AdminContext, admin};
use super::{parse_body, reply};
use crate::dialect::ErrorKind;
use crate::service::{HttpError, blocking};
use crate::state::AppState;

/// Largest result page of a search.
const MAX_LIMIT: u32 = 100;

/// Upstream failures: a missing repository is 404, rate limits 503, the rest 502. An HF
/// authentication failure is not a 401 of this server (the desktop would think its own key
/// was refused), so it is a 502 whose message carries the token hint.
fn hf_error(e: &HfError) -> HttpError {
    let kind = match e {
        HfError::NotFound(_) => ErrorKind::NotFound,
        HfError::RateLimited { .. } => ErrorKind::Unavailable,
        HfError::InvalidUrl(_) => ErrorKind::BadRequest,
        _ => ErrorKind::BadGateway,
    };
    HttpError::new(kind, format!("Hugging Face: {e}"))
}

fn library_error(e: &LibraryError) -> HttpError {
    let kind = match e {
        LibraryError::InvalidUrl(_) => ErrorKind::BadRequest,
        LibraryError::Status { status: 404, .. } => ErrorKind::NotFound,
        _ => ErrorKind::BadGateway,
    };
    HttpError::new(kind, format!("Ollama library: {e}"))
}

fn hf_client(ctx: &AdminContext) -> Result<HfClient, HttpError> {
    HfClient::from_config(&ctx.config.hf, &*ctx.env, &*ctx.hf_token).map_err(|e| hf_error(&e))
}

/// A Hugging Face repository id: `org/name`, nothing that could change the request path.
fn check_repo(repo: &str) -> Result<&str, HttpError> {
    let repo = repo
        .trim()
        .trim_start_matches("hf.co/")
        .trim_start_matches("huggingface.co/");
    let mut parts = repo.split('/');
    let ok = matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None)
        if [a, b].iter().all(|p| !p.is_empty() && *p != "." && *p != ".."
            && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))));
    if ok {
        Ok(repo)
    } else {
        Err(HttpError::bad_request(format!(
            "`{repo}` is not a Hugging Face repository id (`org/name`)"
        )))
    }
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct SearchParams {
    #[serde(default)]
    q: String,
    format: Option<String>,
    task: Option<String>,
    sort: Option<String>,
    author: Option<String>,
    limit: Option<u32>,
}

/// `GET /alkahest/v1/hf/search`.
pub(super) async fn hf_search(
    State(state): State<AppState>,
    Query(p): Query<SearchParams>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let filter = match p.format.as_deref().unwrap_or("gguf") {
            "gguf" => Some("gguf".to_string()),
            "safetensors" => Some("safetensors".to_string()),
            "all" => None,
            other => {
                return Err(HttpError::bad_request(format!(
                    "format `{other}`: expected gguf, safetensors or all"
                )));
            }
        };
        let sort = match p.sort.as_deref().unwrap_or("downloads") {
            "downloads" => SortKey::Downloads,
            "likes" => SortKey::Likes,
            "recent" | "updated" => SortKey::LastModified,
            "created" => SortKey::CreatedAt,
            other => {
                return Err(HttpError::bad_request(format!(
                    "sort `{other}`: expected downloads, likes, recent or created"
                )));
            }
        };
        let query = SearchQuery {
            search: p.q.trim().to_string(),
            filter,
            sort,
            limit: p.limit.unwrap_or(20).clamp(1, MAX_LIMIT),
            author: p.author.filter(|a| !a.is_empty()),
            pipeline_tag: p.task.filter(|t| !t.is_empty()),
        };
        let client = hf_client(&ctx)?;
        let rows = client
            .search_models(&query)
            .await
            .map_err(|e| hf_error(&e))?;
        let models: Vec<Value> = rows
            .iter()
            .map(|m| {
                let formats: Vec<&str> = ["gguf", "safetensors"]
                    .into_iter()
                    .filter(|f| m.tags.iter().any(|t| t == f))
                    .collect();
                let mut v = serde_json::to_value(m).unwrap_or(Value::Null);
                v["formats"] = json!(formats);
                v["gated"] = json!(m.gated.is_gated());
                v
            })
            .collect();
        Ok(Json(json!({ "models": models })).into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RepoParams {
    #[serde(default)]
    repo: String,
    revision: Option<String>,
    #[serde(default)]
    readme: bool,
}

/// `GET /alkahest/v1/hf/model`.
pub(super) async fn hf_model(
    State(state): State<AppState>,
    Query(p): Query<RepoParams>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let repo = check_repo(&p.repo)?.to_string();
        let rev = p.revision.as_deref().filter(|r| !r.is_empty());
        let client = hf_client(&ctx)?;
        let info = client
            .model_info(&repo, rev)
            .await
            .map_err(|e| hf_error(&e))?;
        let card = match client.model_card(&repo, rev).await {
            Ok(c) => Some(c),
            Err(HfError::NotFound(_)) => None,
            Err(e) => {
                tracing::debug!(error = %e, %repo, "model card unavailable");
                None
            }
        };
        let recommended = client.recommended_params(&repo, rev).await.ok();
        let mut info_json = serde_json::to_value(&info).unwrap_or(Value::Null);
        if let Some(o) = info_json.as_object_mut() {
            // The file list has its own route; it can hold thousands of entries.
            o.remove("siblings");
        }
        Ok(Json(json!({
            "repo": repo,
            "info": info_json,
            "gated": info.gated.is_gated(),
            "card": card.as_ref().map(|c| c.front_matter.clone()),
            "readme": if p.readme { card.map(|c| c.body) } else { None },
            "recommended": recommended,
        }))
        .into_response())
    }
    .await;
    reply(&state, r)
}

fn file_json(f: &RepoFile) -> Value {
    json!({"path": f.path, "size": f.size, "sha256": f.sha256})
}

/// `GET /alkahest/v1/hf/files`.
pub(super) async fn hf_files(
    State(state): State<AppState>,
    Query(p): Query<RepoParams>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let repo = check_repo(&p.repo)?.to_string();
        let rev = p.revision.as_deref().filter(|r| !r.is_empty());
        let client = hf_client(&ctx)?;
        let files = client
            .repo_files(&repo, rev)
            .await
            .map_err(|e| hf_error(&e))?;
        let gguf: Vec<Value> = gguf_variants(&files)
            .iter()
            .map(|v| {
                json!({
                    "quant": v.quant,
                    "size": v.size(),
                    "files": v.files.iter().map(file_json).collect::<Vec<_>>(),
                    "split": v.files.len() > 1,
                    "pull": match &v.quant {
                        Some(q) => format!("hf.co/{repo}:{q}"),
                        None => format!("hf.co/{repo}"),
                    },
                })
            })
            .collect();
        let projectors: Vec<Value> = mmproj_files(&files).iter().map(file_json).collect();
        let safetensors: Vec<&RepoFile> = files
            .iter()
            .filter(|f| f.path.ends_with(".safetensors"))
            .collect();
        let st_size: Option<u64> = safetensors.iter().map(|f| f.size).sum();
        Ok(Json(json!({
            "repo": repo,
            "revision": rev,
            "gguf": gguf,
            "mmproj": projectors,
            "safetensors": {
                "files": safetensors.len(),
                "size": st_size,
                "pull": (!safetensors.is_empty()).then(|| format!("hf.co/{repo}")),
            },
            "file_count": files.len(),
        }))
        .into_response())
    }
    .await;
    reply(&state, r)
}

/// `GET /alkahest/v1/hf/token`: whether a token is configured and, with `verify=true`, the
/// account it belongs to.
#[derive(Debug, Default, Deserialize)]
pub(super) struct TokenParams {
    #[serde(default)]
    verify: bool,
}

fn token_source(ctx: &AdminContext) -> &'static str {
    if (ctx.env)("HF_TOKEN").is_some_and(|t| !t.trim().is_empty()) {
        "env"
    } else if ctx.hf_token.get().ok().flatten().is_some() {
        "keyring"
    } else {
        "none"
    }
}

pub(super) async fn hf_token_status(
    State(state): State<AppState>,
    Query(p): Query<TokenParams>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let c2 = ctx.clone();
        let (present, source) = blocking(move || {
            Ok((
                resolve_token(&*c2.env, &*c2.hf_token).is_some(),
                token_source(&c2),
            ))
        })
        .await?;
        let mut doc = json!({"present": present, "source": source});
        if p.verify && present {
            let client = hf_client(&ctx)?;
            match client.whoami().await {
                Ok(w) => {
                    doc["account"] =
                        json!({"name": w.name, "fullname": w.fullname, "token_name": w.token_name})
                }
                Err(e) => doc["error"] = json!(e.to_string()),
            }
        }
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetToken {
    token: String,
    #[serde(default)]
    verify: bool,
}

/// `PUT /alkahest/v1/hf/token` `{"token": "hf_...", "verify": true}`.
pub(super) async fn hf_token_set(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let req: SetToken = parse_body(&body)?;
        let token = Token::new(req.token.trim())
            .ok_or_else(|| HttpError::bad_request("token must not be empty"))?;
        let ctx = admin(&state)?;
        let mut account = Value::Null;
        if req.verify {
            let endpoint = alkahest_hf::resolve_endpoint(&ctx.config.hf, &*ctx.env);
            let client = HfClient::new(&endpoint, Some(token.clone())).map_err(|e| hf_error(&e))?;
            let w = client.whoami().await.map_err(|e| {
                HttpError::bad_request(format!("Hugging Face refused the token: {e}"))
            })?;
            account = json!({"name": w.name, "fullname": w.fullname, "token_name": w.token_name});
        }
        let c2 = ctx.clone();
        blocking(move || {
            c2.hf_token
                .set(&token)
                .map_err(|e| HttpError::internal(format!("storing the token: {e}")))
        })
        .await?;
        tracing::info!("Hugging Face token stored through the admin API");
        let shadowed = token_source(&ctx) == "env";
        Ok(Json(json!({
            "present": true,
            "source": "keyring",
            "account": account,
            "shadowed_by_env": shadowed,
        }))
        .into_response())
    }
    .await;
    reply(&state, r)
}

/// `DELETE /alkahest/v1/hf/token`.
pub(super) async fn hf_token_delete(State(state): State<AppState>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let c2 = ctx.clone();
        blocking(move || {
            c2.hf_token
                .delete()
                .map_err(|e| HttpError::internal(format!("removing the token: {e}")))
        })
        .await?;
        tracing::info!("Hugging Face token removed through the admin API");
        let source = token_source(&ctx);
        Ok(Json(json!({"present": source != "none", "source": source})).into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct LibraryParams {
    #[serde(default)]
    q: String,
    limit: Option<usize>,
}

/// `GET /alkahest/v1/ollama/search`.
pub(super) async fn ollama_search(
    State(state): State<AppState>,
    Query(p): Query<LibraryParams>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let lib = OllamaLibrary::new(&ctx.ollama_library).map_err(|e| library_error(&e))?;
        let models = lib
            .search(
                p.q.trim(),
                p.limit.unwrap_or(20).clamp(1, MAX_LIMIT as usize),
            )
            .await
            .map_err(|e| library_error(&e))?;
        Ok(Json(json!({ "models": models })).into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct TagsParams {
    #[serde(default)]
    name: String,
}

/// `GET /alkahest/v1/ollama/tags?name=`: the tags of one library model (quantisations and
/// sizes with digest, download size, context window), scraped from its tags page.
pub(super) async fn ollama_tags(
    State(state): State<AppState>,
    Query(p): Query<TagsParams>,
) -> Response {
    let r = async {
        if p.name.trim().is_empty() {
            return Err(HttpError::bad_request("name is required"));
        }
        let ctx = admin(&state)?;
        let lib = OllamaLibrary::new(&ctx.ollama_library).map_err(|e| library_error(&e))?;
        let tags = lib
            .tags(p.name.trim())
            .await
            .map_err(|e| library_error(&e))?;
        Ok(Json(json!({ "name": p.name.trim(), "tags": tags })).into_response())
    }
    .await;
    reply(&state, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_ids_are_checked() {
        assert_eq!(
            check_repo("Qwen/Qwen3-8B-GGUF").unwrap(),
            "Qwen/Qwen3-8B-GGUF"
        );
        assert_eq!(check_repo("hf.co/a/b").unwrap(), "a/b");
        for bad in ["", "a", "a/b/c", "../x", "a/..", "a/b?x=1", "a /b"] {
            assert!(check_repo(bad).is_err(), "{bad}");
        }
    }
}
