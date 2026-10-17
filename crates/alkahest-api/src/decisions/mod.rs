//! Decision API (ALK-SP-0007): `POST /v1/systemone` (R1, Ollama 0.35 / llama-server shape)
//! and `POST /v1/decisions` (R5, the LocalAI / OpenAI Decisions shape mapped onto the same
//! engine).
//!
//! Both routes run [`decide`]: the model is resolved in the catalog, its GGUF and Ollama
//! metadata become an [`alkahest_decision::DecisionMetadata`] and a serving plan (native
//! llama-server `/v1/systemone`, or prompt compilation plus logit scoring), the request is
//! validated before anything is loaded, then a scheduler lease runs it through
//! [`alkahest_decision::DecisionEngine`]. `/v1/systemone` answers errors in Ollama's
//! `{"error": "..."}` shape (its clients are Ollama's), `/v1/decisions` in the OpenAI error
//! object.

mod alias;

use alkahest_core::KeepAlive;
use alkahest_decision::{
    DecisionBackend, DecisionEngine, DecisionError, DecisionMetadata, SystemOneRequest,
    SystemOneResponse, compile, validate_limits,
};
use alkahest_sched::Usage;
use alkahest_store::{CatalogEntry, ModelSettings};
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};

use crate::auth::RequestContext;
use crate::dialect::{ErrorKind, Family};
use crate::generation::acquire;
use crate::service::{self, HttpError, UsageSink, blocking, respond};
use crate::state::AppState;

pub use alias::{
    DecisionChoice, DecisionLevel, DecisionQuestion, DecisionsRequest, QuestionIds,
    from_decisions_request, to_decisions_response,
};

/// The decision routes, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/systemone", post(systemone))
        .route("/v1/decisions", post(decisions))
}

type Ctx = Option<axum::Extension<RequestContext>>;

/// `POST /v1/systemone`.
async fn systemone(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = async {
        let req: SystemOneRequest = serde_json::from_slice(&body)
            .map_err(|e| HttpError::bad_request(format!("invalid request body: {e}")))?;
        let resp = decide(&state, ctx.as_ref(), req).await?;
        Ok(Json(resp).into_response())
    }
    .await;
    respond(&state, Family::SystemOne, r)
}

/// `POST /v1/decisions`.
async fn decisions(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = async {
        let req: DecisionsRequest = serde_json::from_slice(&body).map_err(|e| {
            HttpError::bad_request(format!(
                "we could not parse the JSON body of your request: {e}"
            ))
        })?;
        let (internal, ids) = from_decisions_request(&req)?;
        let resp = decide(&state, ctx.as_ref(), internal).await?;
        let out = to_decisions_response(&req, &ids, &resp)?;
        Ok(Json(out).into_response())
    }
    .await;
    respond(&state, Family::Decisions, r)
}

/// Maps a compiler or scorer failure to its HTTP class (`DecisionError::http_status`).
pub fn decision_error(e: &DecisionError) -> HttpError {
    HttpError::new(ErrorKind::from_status(e.http_status()), e.to_string())
}

/// Builds the decision metadata of a catalog entry; `system` is the model's Ollama
/// `system` layer.
///
/// A model whose Ollama config declares the `decision` capability but no `renderer` is
/// served with the generic encoding, because that is what Ollama (0.35 and later) does with
/// it; the Tev1-by-basename rule of [`DecisionMetadata::plan`] then only applies to GGUFs
/// without an Ollama config (measured on `tev1:0.8b`: the Tev1 encoding flips the noul
/// answer Ollama gives, the generic one keeps every chosen value).
pub fn metadata(model: &str, entry: &CatalogEntry, system: Option<String>) -> DecisionMetadata {
    let info = &entry.info;
    let ollama_marked = entry
        .entry
        .config
        .as_ref()
        .is_some_and(|c| c.capabilities.iter().any(|x| x == "decision"));
    let basename = if ollama_marked && info.decision.renderer.is_none() {
        None
    } else {
        info.decision.basename.clone()
    };
    DecisionMetadata {
        model: model.to_owned(),
        decision_type: info.decision.decision_type.clone(),
        has_systemone_template: info.decision.has_systemone_template,
        renderer: info.decision.renderer.clone(),
        capabilities: info.capabilities.clone(),
        tags: info.decision.tags.clone(),
        basename,
        architecture: info.architecture.clone(),
        system: system.filter(|s| !s.trim().is_empty()),
    }
}

/// Answers one System One request: plan from the catalog, validation, lease, scoring.
///
/// Everything that can be refused without the model (unknown model, not a decision model,
/// invalid questions, an encoding this engine cannot score) is refused before a lease is
/// taken, so a bad request never loads weights.
pub async fn decide(
    state: &AppState,
    ctx: Option<&RequestContext>,
    req: SystemOneRequest,
) -> Result<SystemOneResponse, HttpError> {
    if req.model.trim().is_empty() {
        return Err(HttpError::bad_request("model is required"));
    }
    let keep_alive = req
        .keep_alive
        .clone()
        .filter(|v| !v.is_null())
        .map(serde_json::from_value::<KeepAlive>)
        .transpose()
        .map_err(|e| HttpError::bad_request(format!("invalid keep_alive: {e}")))?;
    service::check_model_access(state, ctx, &req.model).await?;

    let catalog = service::catalog(state)?;
    let model = req.model.clone();
    let meta = blocking(move || {
        let entry = catalog
            .resolve(&model)
            .map_err(|e| HttpError::from_store(&e))?;
        let detail = catalog
            .detail(&entry)
            .map_err(|e| HttpError::from_store(&e))?;
        Ok(metadata(&model, &entry, detail.system))
    })
    .await?;
    let plan = meta.plan().map_err(|e| decision_error(&e))?;
    match plan.backend {
        DecisionBackend::Native => validate_limits(&req).map_err(|e| decision_error(&e))?,
        DecisionBackend::Prompt(encoding) if !encoding.scores_letters() => {
            return Err(decision_error(&DecisionError::Unsupported(format!(
                "the {encoding} encoding needs a model-native decision head; llama-server \
                 serves it only for GGUFs with decision metadata"
            ))));
        }
        DecisionBackend::Prompt(encoding) => {
            compile(&req, encoding).map_err(|e| decision_error(&e))?;
        }
    }

    let acquired = acquire(
        state,
        ctx,
        &req.model,
        &ModelSettings::default(),
        keep_alive,
    )
    .await?;
    let lease = acquired.lease;
    let engine = DecisionEngine::with_client(service::upstream(state), lease.base_url());
    let resp = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        r = engine.decide(&req, &plan) => r.map_err(|e| decision_error(&e))?,
    };
    UsageSink::new(state, ctx).record(
        &lease,
        Usage {
            prompt_tokens: resp.usage.input_tokens,
            completion_tokens: resp.usage.output_tokens,
            generation_time: None,
        },
    );
    Ok(resp)
}
