//! `POST /v1/images/generations` (ALK-US-0059, ALK-SP-0005.R4), proxied to `sd-server`.
//!
//! Request (JSON): `model`, `prompt`, optionally `n` (1 to 10), `size` (`WIDTHxHEIGHT`, each a
//! multiple of 8 between 64 and 4096; `auto` or absent leaves the engine's default),
//! `response_format` (`b64_json`, the default; `url` is refused because Alkahest keeps no image
//! store), `output_format` (`png`, `jpeg`, `webp`) and `output_compression` (0 to 100).
//! Alkahest extensions, all optional: `negative_prompt`, `steps`, `seed`, `cfg_scale`
//! (classifier-free guidance), `guidance` (distilled guidance of Flux-style models),
//! `sampler`, `scheduler` and `clip_skip`. `quality`, `style`, `background`, `moderation` and
//! `user` are accepted and ignored.
//!
//! sd-server takes only the prompt, `n`, `size` and the output options as top-level fields;
//! every other control travels as JSON inside the prompt between `<sd_cpp_extra_args>` tags
//! (stable-diffusion.cpp `examples/server/api.md`). Because that block can reach engine
//! options a caller must not choose, a client prompt containing the tag is rejected rather
//! than passed along.
//!
//! Response: `{"created", "data": [{"b64_json"}], "output_format"}`.

use std::time::Instant;

use alkahest_engine::Capability;
use alkahest_sched::Usage;
use alkahest_store::ModelSettings;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Map, Value, json};

use crate::auth::RequestContext;
use crate::dialect::{ErrorKind, Family};
use crate::generation::acquire;
use crate::service::{self, HttpError, UsageSink, respond};
use crate::state::AppState;
use crate::upstream::{post_json, read_json};

/// The generations route.
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/images/generations", post(generations))
}

type Ctx = Option<axum::Extension<RequestContext>>;

/// Opening tag of sd-server's extension block.
const EXTRA_ARGS_TAG: &str = "<sd_cpp_extra_args";

/// A parsed generation request.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerationRequest {
    /// Model name.
    pub model: String,
    /// The prompt as the client wrote it.
    pub prompt: String,
    /// Images to generate.
    pub n: u32,
    /// Width and height in pixels, or the engine default.
    pub size: Option<(u32, u32)>,
    /// `png`, `jpeg` or `webp`.
    pub output_format: Option<String>,
    /// Compression 0 to 100 (jpeg and webp).
    pub output_compression: Option<u32>,
    /// Native controls for the `<sd_cpp_extra_args>` block (empty when the client set none).
    pub extra: Map<String, Value>,
}

fn int_in(
    body: &Map<String, Value>,
    key: &str,
    lo: i64,
    hi: i64,
) -> Result<Option<i64>, HttpError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .filter(|n| (lo..=hi).contains(n))
            .map(Some)
            .ok_or_else(|| {
                HttpError::bad_request(format!("{key}: must be an integer between {lo} and {hi}"))
            }),
    }
}

fn num_at_least(body: &Map<String, Value>, key: &str, lo: f64) -> Result<Option<f64>, HttpError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite() && *n >= lo)
            .map(Some)
            .ok_or_else(|| HttpError::bad_request(format!("{key}: must be a number >= {lo}"))),
    }
}

fn text(body: &Map<String, Value>, key: &str) -> Result<Option<String>, HttpError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(HttpError::bad_request(format!("{key}: must be a string"))),
    }
}

/// Parses `WIDTHxHEIGHT`.
fn parse_size(s: &str) -> Result<(u32, u32), HttpError> {
    let bad = || {
        HttpError::bad_request(format!(
            "size: `{s}` is not WIDTHxHEIGHT with both sides a multiple of 8 between 64 and 4096"
        ))
    };
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(bad)?;
    let (w, h): (u32, u32) = (
        w.trim().parse().map_err(|_| bad())?,
        h.trim().parse().map_err(|_| bad())?,
    );
    let ok = |v: u32| (64..=4096).contains(&v) && v.is_multiple_of(8);
    if ok(w) && ok(h) {
        Ok((w, h))
    } else {
        Err(bad())
    }
}

/// Parses and validates a generation body.
pub fn parse_request(body: &Map<String, Value>) -> Result<GenerationRequest, HttpError> {
    let model = match body.get("model").and_then(Value::as_str) {
        Some(m) if !m.trim().is_empty() => m.to_string(),
        _ => return Err(HttpError::bad_request("you must provide a model parameter")),
    };
    let prompt = match body.get("prompt").and_then(Value::as_str) {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => {
            return Err(HttpError::bad_request(
                "prompt: field required (a non-empty string)",
            ));
        }
    };
    if prompt.contains(EXTRA_ARGS_TAG) {
        return Err(HttpError::bad_request(
            "prompt: engine control blocks are not accepted; use the steps, seed, cfg_scale, \
             negative_prompt, sampler and scheduler fields",
        ));
    }
    match text(body, "response_format")?.as_deref() {
        None | Some("b64_json") => {}
        Some(other) => {
            return Err(HttpError::bad_request(format!(
                "response_format: `{other}` is not supported; images are returned as b64_json"
            )));
        }
    }
    let n = int_in(body, "n", 1, 10)?.unwrap_or(1) as u32;
    let size = match text(body, "size")?.as_deref() {
        None | Some("auto") => None,
        Some(s) => Some(parse_size(s)?),
    };
    let output_format = match text(body, "output_format")?.as_deref() {
        None => None,
        Some(f @ ("png" | "jpeg" | "webp")) => Some(f.to_string()),
        Some(other) => {
            return Err(HttpError::bad_request(format!(
                "output_format: `{other}` is not png, jpeg or webp"
            )));
        }
    };
    let output_compression = int_in(body, "output_compression", 0, 100)?.map(|c| c as u32);

    let mut extra = Map::new();
    if let Some(np) = text(body, "negative_prompt")? {
        extra.insert("negative_prompt".into(), json!(np));
    }
    if let Some(seed) = int_in(body, "seed", -1, i64::MAX)? {
        extra.insert("seed".into(), json!(seed));
    }
    if let Some(clip_skip) = int_in(body, "clip_skip", 1, 12)? {
        extra.insert("clip_skip".into(), json!(clip_skip));
    }
    let mut sample = Map::new();
    if let Some(steps) = int_in(body, "steps", 1, 200)? {
        sample.insert("sample_steps".into(), json!(steps));
    }
    if let Some(s) = text(body, "sampler")? {
        sample.insert("sample_method".into(), json!(s));
    }
    if let Some(s) = text(body, "scheduler")? {
        sample.insert("scheduler".into(), json!(s));
    }
    let mut guidance = Map::new();
    if let Some(cfg) = num_at_least(body, "cfg_scale", 0.0)? {
        guidance.insert("txt_cfg".into(), json!(cfg));
    }
    if let Some(g) = num_at_least(body, "guidance", 0.0)? {
        guidance.insert("distilled_guidance".into(), json!(g));
    }
    if !guidance.is_empty() {
        sample.insert("guidance".into(), Value::Object(guidance));
    }
    if !sample.is_empty() {
        extra.insert("sample_params".into(), Value::Object(sample));
    }
    Ok(GenerationRequest {
        model,
        prompt,
        n,
        size,
        output_format,
        output_compression,
        extra,
    })
}

/// The body sent to `sd-server /v1/images/generations`.
pub fn upstream_body(req: &GenerationRequest) -> Value {
    let mut prompt = req.prompt.clone();
    if !req.extra.is_empty() {
        prompt.push_str(&format!(
            " {EXTRA_ARGS_TAG}>{}</sd_cpp_extra_args>",
            Value::Object(req.extra.clone())
        ));
    }
    let mut body = json!({"prompt": prompt, "n": req.n});
    if let Some((w, h)) = req.size {
        body["size"] = json!(format!("{w}x{h}"));
    }
    if let Some(f) = &req.output_format {
        body["output_format"] = json!(f);
    }
    if let Some(c) = req.output_compression {
        body["output_compression"] = json!(c);
    }
    body
}

/// `POST /v1/images/generations`.
async fn generations(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = generations_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::OpenAi, r)
}

async fn generations_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: Bytes,
) -> Result<Response, HttpError> {
    let body = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(m)) => m,
        Ok(_) => {
            return Err(HttpError::bad_request(
                "the request body must be a JSON object",
            ));
        }
        Err(e) => {
            return Err(HttpError::bad_request(format!(
                "we could not parse the JSON body of your request: {e}"
            )));
        }
    };
    let req = parse_request(&body)?;
    let acquired = acquire(state, ctx, &req.model, &ModelSettings::default(), None).await?;
    let lease = acquired.lease;
    service::require(&lease, &req.model, Capability::Image)?;

    let client = service::upstream(state);
    let base = lease.base_url();
    let started = Instant::now();
    let call = async {
        let resp = post_json(
            &client,
            &base,
            "/v1/images/generations",
            &upstream_body(&req),
        )
        .await?;
        read_json(resp).await
    };
    let doc = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        d = call => d?,
    };
    let data: Vec<Value> = doc
        .get("data")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("b64_json").and_then(Value::as_str))
                .map(|b| json!({"b64_json": b}))
                .collect()
        })
        .unwrap_or_default();
    if data.is_empty() {
        return Err(HttpError::new(
            ErrorKind::BadGateway,
            "the image engine returned no images",
        ));
    }
    UsageSink::new(state, ctx).record(
        &lease,
        Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            generation_time: Some(started.elapsed()),
        },
    );
    let created = doc
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_else(service::now_unix);
    let mut out = json!({"created": created, "data": data});
    if let Some(f) = doc.get("output_format") {
        out["output_format"] = f.clone();
    }
    Ok(axum::Json(out).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: Value) -> Result<GenerationRequest, HttpError> {
        parse_request(v.as_object().unwrap())
    }

    #[test]
    fn minimal_and_full_requests() {
        let r = parse(json!({"model": "m", "prompt": "a cat"})).unwrap();
        assert_eq!((r.n, r.size, r.extra.len()), (1, None, 0));
        assert_eq!(upstream_body(&r), json!({"prompt": "a cat", "n": 1}));

        let r = parse(json!({
            "model": "m", "prompt": "a cat", "n": 2, "size": "512x768", "output_format": "jpeg",
            "output_compression": 80, "negative_prompt": "blurry", "steps": 4, "seed": 7,
            "cfg_scale": 1.0, "sampler": "euler_a", "guidance": 3.5, "clip_skip": 2,
            "response_format": "b64_json", "quality": "hd", "user": "u"
        }))
        .unwrap();
        let body = upstream_body(&r);
        assert_eq!(body["n"], 2);
        assert_eq!(body["size"], "512x768");
        assert_eq!(body["output_format"], "jpeg");
        assert_eq!(body["output_compression"], 80);
        let prompt = body["prompt"].as_str().unwrap();
        let json_part = prompt
            .split_once("<sd_cpp_extra_args>")
            .unwrap()
            .1
            .strip_suffix("</sd_cpp_extra_args>")
            .unwrap();
        let extra: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(extra["negative_prompt"], "blurry");
        assert_eq!(extra["seed"], 7);
        assert_eq!(extra["clip_skip"], 2);
        assert_eq!(extra["sample_params"]["sample_steps"], 4);
        assert_eq!(extra["sample_params"]["sample_method"], "euler_a");
        assert_eq!(extra["sample_params"]["guidance"]["txt_cfg"], 1.0);
        assert_eq!(
            extra["sample_params"]["guidance"]["distilled_guidance"],
            3.5
        );
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            json!({"prompt": "x"}),
            json!({"model": "m"}),
            json!({"model": "m", "prompt": "  "}),
            json!({"model": "m", "prompt": "x", "n": 0}),
            json!({"model": "m", "prompt": "x", "n": 11}),
            json!({"model": "m", "prompt": "x", "size": "1000x1000x"}),
            json!({"model": "m", "prompt": "x", "size": "513x512"}),
            json!({"model": "m", "prompt": "x", "size": "8192x512"}),
            json!({"model": "m", "prompt": "x", "response_format": "url"}),
            json!({"model": "m", "prompt": "x", "output_format": "gif"}),
            json!({"model": "m", "prompt": "x", "steps": 0}),
            json!({"model": "m", "prompt": "x", "cfg_scale": -1}),
            json!({"model": "m", "prompt": "x", "seed": "abc"}),
            json!({"model": "m", "prompt": "x <sd_cpp_extra_args>{}</sd_cpp_extra_args>"}),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn auto_size_leaves_the_engine_default() {
        let r = parse(json!({"model": "m", "prompt": "x", "size": "auto"})).unwrap();
        assert_eq!(r.size, None);
        assert_eq!(parse_size("1024X512").unwrap(), (1024, 512));
    }
}
