//! The admin API under `/alkahest/v1/` (ALK-SP-0005.R6): runtime, catalog, model settings,
//! pulls, discovery proxies, engines, API keys, network settings, server info and logs.
//!
//! Every route needs the `admin` scope for non-loopback callers (and on loopback once keys
//! are required there): the authentication middleware enforces it for the whole
//! `/alkahest/` prefix (`dialect::requires_admin`), so no handler can forget it.
//!
//! [`ROUTES`] is the single list of routes: the router is built from it and
//! `GET /alkahest/v1/openapi.json` is generated from it, so the description cannot drift
//! from what is served.
//!
//! Handlers find their dependencies in the [`AppState`]: the scheduler, the catalog and the
//! [`AdminContext`] (configuration, paths, [`JobManager`], pull and engine back ends).

pub mod context;
pub mod discover;
pub mod engines;
pub mod jobs;
pub mod keys;
pub mod models;
pub mod pulls;
pub mod runtime;
pub mod settings;
pub mod system;

use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, delete, get, post, put};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

pub use context::{AdminContext, AdminContextBuilder};
pub use engines::{
    EngineInstallRequest, EngineListing, EngineManager, EngineVariant, RecipeEngineManager,
};
pub use jobs::{Job, JobHandle, JobKind, JobManager, JobState};
pub use pulls::PullRunner;
pub use settings::NetworkSettings;

use crate::dialect::Family;
use crate::service::{HttpError, respond};
use crate::state::AppState;

/// One documented route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDoc {
    /// HTTP method, upper case.
    pub method: &'static str,
    /// Path in axum / OpenAPI template syntax (`/alkahest/v1/models/{name}`).
    pub path: &'static str,
    /// Group (OpenAPI tag).
    pub tag: &'static str,
    /// One-line description.
    pub summary: &'static str,
}

macro_rules! routes {
    ($( $method:ident $path:literal $tag:literal $summary:literal => $handler:expr ;)*) => {
        /// Every admin route, in documentation order.
        pub const ROUTES: &[RouteDoc] = &[
            $( RouteDoc { method: routes!(@m $method), path: $path, tag: $tag, summary: $summary }, )*
        ];

        fn table() -> Vec<(&'static str, MethodRouter<AppState>)> {
            vec![ $( ($path, $method($handler)), )* ]
        }
    };
    (@m get) => { "GET" };
    (@m post) => { "POST" };
    (@m put) => { "PUT" };
    (@m delete) => { "DELETE" };
}

routes! {
    get "/alkahest/v1/openapi.json" "meta" "This description (OpenAPI 3.1)" => openapi;
    get "/alkahest/v1/info" "system" "Server version, uptime, bind, paths, store counts, hardware and accelerators" => system::info;
    get "/alkahest/v1/ps" "runtime" "Scheduler snapshot: loaded models, memory, requests" => runtime::snapshot;
    get "/alkahest/v1/events" "runtime" "Server-sent events: `status` (scheduler) and `job` (pulls, engine installs)" => runtime::events;
    post "/alkahest/v1/unload" "runtime" "Unload one model (`model`) or all (`all: true`)" => runtime::unload;
    get "/metrics" "runtime" "Prometheus metrics" => runtime::metrics;
    get "/alkahest/v1/models" "models" "Merged catalog with kind, format, source, engine, context, paths and runtime state; store paths" => models::list;
    get "/alkahest/v1/models/{name}" "models" "One model: summary, template, system prompt, licence, default parameters" => models::detail;
    delete "/alkahest/v1/models/{name}" "models" "Unload and delete a model (Ollama-store models are read-only)" => models::delete;
    post "/alkahest/v1/copy" "models" "Copy a model under a new name" => models::copy;
    get "/alkahest/v1/models/{name}/settings" "models" "Per-model overlay, recommended layer, effective values and their layers" => models::get_settings;
    put "/alkahest/v1/models/{name}/settings" "models" "Replace the per-model overlay" => models::put_settings;
    delete "/alkahest/v1/models/{name}/settings" "models" "Remove the per-model overlay" => models::delete_settings;
    get "/alkahest/v1/models/{name}/recommended" "models" "Recommended defaults stored at pull time" => models::recommended;
    get "/alkahest/v1/models/{name}/estimate" "models" "Memory estimate for the effective settings with overrides" => models::estimate_memory;
    post "/alkahest/v1/pulls" "pulls" "Start a pull job (Ollama or Hugging Face reference, optional quant)" => pulls::start_pull;
    get "/alkahest/v1/pulls" "pulls" "Pull jobs with progress" => pulls::list_pulls;
    delete "/alkahest/v1/pulls/{id}" "pulls" "Cancel a pull job (the partial download stays resumable)" => pulls::cancel_job;
    get "/alkahest/v1/jobs" "jobs" "Every background job" => pulls::list_jobs;
    get "/alkahest/v1/jobs/{id}" "jobs" "One job" => pulls::get_job;
    post "/alkahest/v1/jobs/{id}/cancel" "jobs" "Cancel a running job" => pulls::cancel_job;
    delete "/alkahest/v1/jobs/{id}" "jobs" "Forget a finished job" => pulls::remove_job;
    get "/alkahest/v1/hf/search" "discover" "Search Hugging Face (q, format, task, sort, author, limit)" => discover::hf_search;
    get "/alkahest/v1/hf/model" "discover" "Hugging Face model info, card and recommended parameters (repo, revision, readme)" => discover::hf_model;
    get "/alkahest/v1/hf/files" "discover" "Hugging Face GGUF quantisations, projectors and safetensors weights (repo, revision)" => discover::hf_files;
    get "/alkahest/v1/hf/token" "discover" "Hugging Face token presence and account (verify)" => discover::hf_token_status;
    put "/alkahest/v1/hf/token" "discover" "Store the Hugging Face token in the server keyring" => discover::hf_token_set;
    delete "/alkahest/v1/hf/token" "discover" "Remove the stored Hugging Face token" => discover::hf_token_delete;
    get "/alkahest/v1/ollama/search" "discover" "Search the Ollama library (q, limit)" => discover::ollama_search;
    get "/alkahest/v1/engines" "engines" "Installed engine builds and installable recipe variants" => engines::list;
    post "/alkahest/v1/engines/install" "engines" "Start an engine install job" => engines::start_install;
    delete "/alkahest/v1/engines/{id}" "engines" "Remove managed installs of an engine (version, variant)" => engines::remove;
    get "/alkahest/v1/keys" "keys" "API keys with usage counters (never secrets or hashes)" => keys::list;
    post "/alkahest/v1/keys" "keys" "Create an API key; the secret is returned once" => keys::create;
    delete "/alkahest/v1/keys/{id}" "keys" "Revoke an API key" => keys::revoke;
    get "/alkahest/v1/settings/network" "settings" "Running and saved network settings, restart flag" => settings::get_network;
    put "/alkahest/v1/settings/network" "settings" "Save network settings to the config file (applied on restart)" => settings::put_network;
    get "/alkahest/v1/logs/engines" "logs" "Engine log files" => system::engine_logs;
    get "/alkahest/v1/logs/engines/{name}" "logs" "Tail of one engine log (lines)" => system::engine_log;
    get "/alkahest/v1/logs/server" "logs" "Tail of the newest server log (lines)" => system::server_log;
}

/// Every admin route, built from [`ROUTES`].
pub fn routes() -> Router<AppState> {
    table()
        .into_iter()
        .fold(Router::new(), |r, (path, handler)| r.route(path, handler))
}

/// The OpenAPI 3.1 description of [`ROUTES`].
pub fn openapi_document() -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for r in ROUTES {
        let params: Vec<Value> = r
            .path
            .split('/')
            .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .map(|name| json!({"name": name, "in": "path", "required": true, "schema": {"type": "string"},
                               "description": if name == "name" { "model name, percent-encoded as one segment" } else { "" }}))
            .collect();
        let op_id = format!(
            "{}_{}",
            r.method.to_ascii_lowercase(),
            r.path
                .trim_start_matches("/alkahest/v1/")
                .trim_start_matches('/')
                .replace(['/', '.'], "_")
                .replace(['{', '}'], "")
        );
        let mut op = json!({
            "tags": [r.tag],
            "summary": r.summary,
            "operationId": op_id,
            "responses": {
                "200": {"description": "Success"},
                "400": {"description": "Invalid request"},
                "401": {"description": "Missing or invalid API key (non-loopback callers)"},
                "403": {"description": "The key lacks the admin scope"},
            },
        });
        if !params.is_empty() {
            op["parameters"] = Value::Array(params);
        }
        let item = paths
            .entry(r.path.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        item[r.method.to_ascii_lowercase()] = op;
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Alkahest admin API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Administration of an Alkahest server. Loopback callers need no key \
                            unless auth.require_on_loopback is set; remote callers need a key with \
                            the admin scope.",
        },
        "components": {"securitySchemes": {
            "bearer": {"type": "http", "scheme": "bearer"},
            "apiKey": {"type": "apiKey", "in": "header", "name": "x-api-key"},
        }},
        "security": [{"bearer": []}, {"apiKey": []}],
        "paths": paths,
    })
}

async fn openapi(State(_state): State<AppState>) -> Response {
    Json(openapi_document()).into_response()
}

/// Parses a JSON body; an empty body is the type's default (so `{}`-like requests work
/// without a body). Unknown fields are refused where the type says so.
pub(crate) fn parse_body<T: DeserializeOwned + Default>(body: &Bytes) -> Result<T, HttpError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| HttpError::bad_request(format!("invalid JSON: {e}")))
}

/// Renders an admin handler result.
pub(crate) fn reply(state: &AppState, r: Result<Response, HttpError>) -> Response {
    respond(state, Family::Admin, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_describes_every_route() {
        let doc = openapi_document();
        for r in ROUTES {
            assert!(
                doc["paths"][r.path][r.method.to_ascii_lowercase()].is_object(),
                "{} {} missing",
                r.method,
                r.path
            );
        }
        assert_eq!(table().len(), ROUTES.len());
        let ids: std::collections::BTreeSet<_> = doc["paths"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|item| {
                item.as_object()
                    .unwrap()
                    .values()
                    .map(|op| op["operationId"].clone())
            })
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), ROUTES.len(), "operation ids are unique");
    }

    #[test]
    fn the_router_builds_with_shared_paths() {
        let _ = routes();
    }
}
