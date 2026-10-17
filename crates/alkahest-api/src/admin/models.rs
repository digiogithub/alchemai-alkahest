//! Catalog administration (ALK-SP-0005.R6, ALK-SP-0008.R3/R5, ALK-US-0097).
//!
//! Model names travel as one percent-encoded path segment (`hf.co%2Forg%2Frepo%3AQ4_K_M`),
//! because they contain `/` and `:`.
//!
//! * `GET /alkahest/v1/models`: the merged catalog (every `ModelSummary` field plus the
//!   runtime state of loaded models) and the store paths and counts.
//! * `GET /alkahest/v1/models/{name}`: summary, template, system prompt, licence,
//!   `default_parameters` (the params layer; `parameters` stays the parameter-count label).
//! * `DELETE /alkahest/v1/models/{name}`: unloads, then deletes (Ollama-store models: 403).
//! * `POST /alkahest/v1/copy` `{"source": .., "destination": ..}`.
//! * `GET|PUT|DELETE /alkahest/v1/models/{name}/settings`: the per-model overlay; `GET` adds
//!   the recommended layer, the effective values and the layer that decided each one.
//! * `GET /alkahest/v1/models/{name}/recommended`: the stored recommended defaults.
//! * `GET /alkahest/v1/models/{name}/estimate?context_length=&num_parallel=&kv_cache_type_k=&kv_cache_type_v=&flash_attention=&gpu_layers=`:
//!   memory estimate for the effective settings with these overrides (GGUF models).

use std::str::FromStr;

use alkahest_engine::LaunchRequest;
use alkahest_gguf::{Gguf, ModelFormat};
use alkahest_sched::{
    DEFAULT_CONTEXT_LENGTH, EstimateConfig, GpuLayers, KvCacheType, ModelProfile, ModelStatus,
    SchedError, Scheduler, estimate,
};
use alkahest_store::{
    Catalog, CatalogEntry, MAX_NUM_PARALLEL, ModelName, ModelRef, ModelSettings, Origin, ScopedRef,
    StoreError,
};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::context::admin;
use super::{parse_body, reply};
use crate::service::{self, HttpError, blocking};
use crate::state::AppState;

fn not_found(e: StoreError, model: &str) -> HttpError {
    match e {
        StoreError::ModelNotFound { .. } => HttpError::model_not_found(model),
        other => HttpError::from_store(&other),
    }
}

/// A short name without the default `:latest` tag (`llama3.2` loads as `llama3.2:latest`).
fn bare(name: &str) -> &str {
    name.strip_suffix(":latest").unwrap_or(name)
}

/// The runtime fields of a loaded model, matched by display name or digest.
fn runtime_of(entry: &CatalogEntry, loaded: &[ModelStatus]) -> Value {
    let name = entry.entry.name.display_short();
    let digest = entry.entry.manifest_digest.to_string();
    loaded
        .iter()
        .find(|m| m.digest == digest || bare(&m.name) == bare(&name))
        .map_or(Value::Null, |m| {
            json!({
                "state": m.state,
                "processor": m.processor_label,
                "engine": m.engine.to_string(),
                "context_length": m.context_length,
                "ram_bytes": m.ram_bytes,
                "vram_bytes": m.vram_bytes,
                "active_requests": m.active_requests,
                "queued_requests": m.queued_requests,
                "expires_in_ms": m.expires_in_ms,
            })
        })
}

fn row(entry: &CatalogEntry, loaded: &[ModelStatus]) -> Value {
    let mut v = serde_json::to_value(entry.summary()).unwrap_or(Value::Null);
    v["runtime"] = runtime_of(entry, loaded);
    v
}

async fn loaded(state: &AppState) -> Vec<ModelStatus> {
    match state.service::<Scheduler>() {
        Some(s) => s.snapshot().models,
        None => Vec::new(),
    }
}

/// `GET /alkahest/v1/models`.
pub(super) async fn list(State(state): State<AppState>) -> Response {
    let r = async {
        let catalog = service::catalog(&state)?;
        let loaded = loaded(&state).await;
        let doc = blocking(move || {
            let entries = catalog.list().map_err(|e| HttpError::from_store(&e))?;
            let models: Vec<Value> = entries.iter().map(|e| row(e, &loaded)).collect();
            let count = |label: &str| {
                entries
                    .iter()
                    .filter(|e| e.source_label() == label)
                    .fold((0usize, 0u64), |(n, b), e| (n + 1, b + e.entry.size))
            };
            let (alk_n, alk_b) = count("alkahest");
            let ollama_count = catalog
                .ollama()
                .and_then(|_| catalog.list_origin(Origin::Ollama).ok())
                .map(|v| v.len());
            Ok(json!({
                "models": models,
                "stores": {
                    "alkahest": {
                        "path": catalog.alkahest().root(),
                        "models": alk_n,
                        "size": alk_b,
                    },
                    "ollama": catalog.ollama().map(|o| json!({
                        "path": o.root(),
                        "models": ollama_count,
                        "read_only": true,
                    })),
                },
                "warnings": catalog.warnings(),
            }))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `GET /alkahest/v1/models/{name}`.
pub(super) async fn detail(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let r = async {
        let catalog = service::catalog(&state)?;
        let loaded = loaded(&state).await;
        let doc = blocking(move || {
            let entry = catalog.resolve(&name).map_err(|e| not_found(e, &name))?;
            let d = catalog
                .detail(&entry)
                .map_err(|e| HttpError::from_store(&e))?;
            let mut v = row(&entry, &loaded);
            v["template"] = json!(d.template);
            v["template_from_gguf"] = json!(d.template_from_gguf);
            v["system"] = json!(d.system);
            v["license"] = json!(d.license);
            // `parameters` stays the parameter-count label of the row; the params layer's
            // default map gets its own key so both keep one JSON type (ALK-US-0103).
            v["default_parameters"] = json!(d.parameters);
            v["source_info"] = json!(d.source);
            v["shadows"] = json!(entry.shadows.iter().map(|o| o.as_str()).collect::<Vec<_>>());
            v["not_loadable_reason"] = json!(entry.not_loadable_reason());
            Ok(v)
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `DELETE /alkahest/v1/models/{name}`.
pub(super) async fn delete(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let r = async {
        let catalog = service::catalog(&state)?;
        if let Some(s) = state.service::<Scheduler>() {
            match s.unload(&name).await {
                Ok(()) | Err(SchedError::NotLoaded { .. }) => {}
                Err(e) => tracing::warn!(model = %name, error = %e, "unload before delete"),
            }
        }
        let n = name.clone();
        let report = blocking(move || catalog.remove(&n).map_err(|e| not_found(e, &n))).await?;
        Ok(Json(json!({
            "deleted": name,
            "blobs_removed": report.blobs_removed,
            "blobs_kept": report.blobs_kept,
            "reclaimed": report.reclaimed,
        }))
        .into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyRequest {
    source: String,
    destination: String,
}

/// `POST /alkahest/v1/copy`.
pub(super) async fn copy(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let req: CopyRequest = parse_body(&body)?;
        if req.source.trim().is_empty() || req.destination.trim().is_empty() {
            return Err(HttpError::bad_request(
                "source and destination are required",
            ));
        }
        let catalog = service::catalog(&state)?;
        let (name, report) = blocking(move || {
            catalog
                .copy(&req.source, &req.destination)
                .map_err(|e| not_found(e, &req.source))
        })
        .await?;
        Ok(Json(json!({
            "name": name.display_short(),
            "blobs_copied": report.blobs_copied,
            "bytes_copied": report.bytes_copied,
        }))
        .into_response())
    }
    .await;
    reply(&state, r)
}

/// The store name to configure: the installed model's, else the parsed reference (a model may
/// be configured before it is pulled, as `alkahest config model` allows).
fn target_name(
    catalog: &Catalog,
    input: &str,
) -> Result<(ModelName, Option<CatalogEntry>), HttpError> {
    match catalog.resolve(input) {
        Ok(e) => Ok((e.entry.name.clone(), Some(e))),
        Err(StoreError::ModelNotFound { .. }) => {
            let scoped = ScopedRef::from_str(input).map_err(|e| {
                HttpError::bad_request(format!("invalid model name `{input}`: {e}"))
            })?;
            match scoped.reference {
                ModelRef::Digest(_) => Err(HttpError::model_not_found(input)),
                r => r
                    .store_name()
                    .map_err(|e| HttpError::bad_request(e.to_string()))?
                    .map(|n| (n, None))
                    .ok_or_else(|| HttpError::model_not_found(input)),
            }
        }
        Err(e) => Err(HttpError::from_store(&e)),
    }
}

fn settings_doc(
    catalog: &Catalog,
    global: &ModelSettings,
    name: &ModelName,
    entry: Option<&CatalogEntry>,
) -> Result<Value, HttpError> {
    let overlays = catalog
        .overlays()
        .ok_or_else(|| HttpError::internal("this catalog has no overlay store"))?;
    let st = |e: StoreError| HttpError::from_store(&e);
    let overlay = overlays.get(name).map_err(st)?;
    let recommended = overlays.recommended(name).map_err(st)?;
    let eff = match entry {
        Some(e) => catalog
            .effective_settings(e, &ModelSettings::default(), global, None)
            .map_err(st)?,
        None => overlays
            .resolve(
                name,
                &ModelSettings::default(),
                global,
                None,
                &ModelSettings::default(),
            )
            .map_err(st)?,
    };
    let origins: Map<String, Value> = eff
        .origins
        .iter()
        .map(|(k, v)| (k.clone(), json!(v.as_str())))
        .collect();
    Ok(json!({
        "model": name.display_short(),
        "installed": entry.is_some(),
        "path": overlays.model_path(name),
        "overlay": overlay,
        "recommended": recommended,
        "effective": eff.settings,
        "origins": origins,
    }))
}

fn global_defaults(state: &AppState) -> ModelSettings {
    admin(state)
        .map(|c| ModelSettings::global_defaults(&c.config))
        .unwrap_or_default()
}

/// `GET /alkahest/v1/models/{name}/settings`.
pub(super) async fn get_settings(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let r = async {
        let catalog = service::catalog(&state)?;
        let global = global_defaults(&state);
        let doc = blocking(move || {
            let (n, entry) = target_name(&catalog, &name)?;
            settings_doc(&catalog, &global, &n, entry.as_ref())
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `PUT /alkahest/v1/models/{name}/settings`: replaces the overlay with the body (a
/// `ModelSettings` object; unknown keys are refused). Takes effect on the next load.
pub(super) async fn put_settings(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    let r = async {
        let settings: ModelSettings = parse_body(&body)?;
        validate(&settings)?;
        let catalog = service::catalog(&state)?;
        let global = global_defaults(&state);
        let doc = blocking(move || {
            let (n, entry) = target_name(&catalog, &name)?;
            let overlays = catalog
                .overlays()
                .ok_or_else(|| HttpError::internal("this catalog has no overlay store"))?;
            overlays
                .put(&n, &settings)
                .map_err(|e| HttpError::from_store(&e))?;
            settings_doc(&catalog, &global, &n, entry.as_ref())
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `DELETE /alkahest/v1/models/{name}/settings`: removes the overlay.
pub(super) async fn delete_settings(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let r = async {
        let catalog = service::catalog(&state)?;
        let global = global_defaults(&state);
        let doc = blocking(move || {
            let (n, entry) = target_name(&catalog, &name)?;
            let overlays = catalog
                .overlays()
                .ok_or_else(|| HttpError::internal("this catalog has no overlay store"))?;
            overlays
                .put(&n, &ModelSettings::default())
                .map_err(|e| HttpError::from_store(&e))?;
            settings_doc(&catalog, &global, &n, entry.as_ref())
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// Value checks the type system does not make: values an engine would refuse at load time
/// are better refused when the user saves them.
fn validate(s: &ModelSettings) -> Result<(), HttpError> {
    let range = |name: &str, v: Option<f64>, lo: f64, hi: f64| match v {
        Some(x) if !(lo..=hi).contains(&x) => Err(HttpError::bad_request(format!(
            "{name} must be between {lo} and {hi}"
        ))),
        _ => Ok(()),
    };
    range("temperature", s.temperature, 0.0, 5.0)?;
    range("top_p", s.top_p, 0.0, 1.0)?;
    range("min_p", s.min_p, 0.0, 1.0)?;
    if s.context_length == Some(0) {
        return Err(HttpError::bad_request("context_length must be at least 1"));
    }
    if let Some(n) = s.num_parallel
        && !(1..=MAX_NUM_PARALLEL).contains(&n)
    {
        return Err(HttpError::bad_request(format!(
            "num_parallel must be between 1 and {MAX_NUM_PARALLEL}"
        )));
    }
    for (name, v) in [
        ("kv_cache_type_k", &s.kv_cache_type_k),
        ("kv_cache_type_v", &s.kv_cache_type_v),
    ] {
        if let Some(t) = v
            && KvCacheType::parse(t).is_none()
            && !matches!(
                t.to_ascii_lowercase().as_str(),
                "f32" | "bf16" | "q4_1" | "q5_0" | "q5_1" | "iq4_nl"
            )
        {
            return Err(HttpError::bad_request(format!(
                "{name}: unknown cache type `{t}`"
            )));
        }
    }
    Ok(())
}

/// `GET /alkahest/v1/models/{name}/recommended`.
pub(super) async fn recommended(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let r = async {
        let catalog = service::catalog(&state)?;
        let doc = blocking(move || {
            let (n, entry) = target_name(&catalog, &name)?;
            let overlays = catalog
                .overlays()
                .ok_or_else(|| HttpError::internal("this catalog has no overlay store"))?;
            let layer = overlays
                .recommended(&n)
                .map_err(|e| HttpError::from_store(&e))?;
            Ok(json!({
                "model": n.display_short(),
                "recommended": layer,
                "as_settings": layer.as_ref().map(|l| l.to_settings(None)),
                "trained_context_length": entry.as_ref().and_then(|e| e.info.context_length),
            }))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct EstimateParams {
    context_length: Option<u64>,
    num_parallel: Option<u32>,
    kv_cache_type_k: Option<String>,
    kv_cache_type_v: Option<String>,
    flash_attention: Option<bool>,
    /// `-1` all, `0` none, `n` layers; absent = automatic.
    gpu_layers: Option<i32>,
}

/// `GET /alkahest/v1/models/{name}/estimate`.
pub(super) async fn estimate_memory(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(p): Query<EstimateParams>,
) -> Response {
    let r = async {
        let catalog = service::catalog(&state)?;
        let ctx = admin(&state).ok();
        let global = global_defaults(&state);
        let doc = blocking(move || {
            let entry = catalog.resolve(&name).map_err(|e| not_found(e, &name))?;
            let eff = catalog
                .effective_settings(&entry, &ModelSettings::default(), &global, None)
                .map_err(|e| HttpError::from_store(&e))?;
            if entry.info.format != ModelFormat::Gguf {
                return Err(HttpError::bad_request(
                    "memory estimates are available for GGUF models",
                ));
            }
            let system = ctx.as_ref().map(|c| c.system());
            let accel = system
                .as_ref()
                .map_or(alkahest_sysinfo::Accelerator::Cpu, |s| s.best_accelerator());
            let req = LaunchRequest::from_catalog(&entry, &eff, accel, 0)
                .map_err(|e| HttpError::bad_request(e.to_string()))?;
            let gguf = Gguf::read_file(&req.model_path)
                .map_err(|e| HttpError::internal(format!("reading the GGUF header: {e}")))?;
            let profile = ModelProfile::from_gguf(&gguf)
                .map_err(|e| HttpError::bad_request(format!("cannot estimate this model: {e}")))?;
            let kv = |q: &Option<String>, s: &Option<String>| -> Result<KvCacheType, HttpError> {
                match q.as_ref().or(s.as_ref()) {
                    None => Ok(KvCacheType::default()),
                    Some(t) => KvCacheType::parse(t).ok_or_else(|| {
                        HttpError::bad_request(format!(
                            "unsupported cache type `{t}` for estimates"
                        ))
                    }),
                }
            };
            let gpu_layers_setting = p.gpu_layers.or(eff.settings.gpu_layers);
            let free_vram: u64 = system
                .as_ref()
                .map(|s| {
                    s.gpus
                        .iter()
                        .map(|g| g.vram_free_bytes.unwrap_or(g.vram_total_bytes))
                        .sum()
                })
                .unwrap_or(0);
            let policy = if !accel.is_gpu() {
                GpuLayers::None
            } else {
                match gpu_layers_setting {
                    None => GpuLayers::Auto,
                    Some(n) if n < 0 => GpuLayers::All,
                    Some(0) => GpuLayers::None,
                    Some(n) => GpuLayers::Count(n.unsigned_abs()),
                }
            };
            let projector = req
                .projector_path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.len());
            let context_length = p
                .context_length
                .or(eff.settings.context_length)
                .unwrap_or(DEFAULT_CONTEXT_LENGTH);
            let cfg = EstimateConfig {
                context_length,
                num_parallel: p
                    .num_parallel
                    .or(eff.settings.num_parallel)
                    .unwrap_or(1)
                    .max(1),
                cache_type_k: kv(&p.kv_cache_type_k, &eff.settings.kv_cache_type_k)?,
                cache_type_v: kv(&p.kv_cache_type_v, &eff.settings.kv_cache_type_v)?,
                flash_attention: p
                    .flash_attention
                    .or(eff.settings.flash_attention)
                    .unwrap_or(false),
                mmproj_bytes: projector,
                gpu_layers: policy,
                free_vram_bytes: if policy == GpuLayers::None {
                    0
                } else {
                    free_vram
                },
                ..EstimateConfig::default()
            };
            let est = estimate(&profile, &cfg);
            let ram = system.as_ref().map(|s| s.memory.available_bytes);
            Ok(json!({
                "model": entry.entry.name.display_short(),
                "accelerator": accel.label(),
                "context_length": context_length,
                "num_parallel": cfg.num_parallel,
                "estimate": est,
                "free_vram_bytes": free_vram,
                "available_ram_bytes": ram,
                "fits": est.fits(ram.unwrap_or(u64::MAX), free_vram, cfg.vram_fraction),
            }))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}
