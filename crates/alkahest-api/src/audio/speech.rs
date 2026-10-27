//! `POST /v1/audio/speech` and `GET /v1/audio/voices` (ALK-US-0142, ALK-SP-0005.R4, ADR 0008):
//! text to speech with Phonon models on `ptts-openai-server`.
//!
//! The contract follows Ollama 0.40.11 (`server/speech.go`, `docs/capabilities/speech.mdx`):
//!
//! - `model` and `input` are required; `input` holds at most 4096 characters.
//! - `response_format` defaults to `wav` (16-bit mono 24 kHz, streamed, so the size fields of the
//!   header hold the maximum value); `pcm` is the same samples without a header. The engine also
//!   encodes `mp3` and `opus`, which Alkahest passes through. Anything else is a 400.
//! - `speed` other than 1, `instructions` and a `stream_format` other than `audio` are a 400
//!   instead of being ignored.
//! - `voice` is a voice of the model (any case, with or without accents); omitted means the
//!   checkpoint's `default`. The engine answers an unknown voice with a 400 that lists the voices,
//!   which is passed through.
//!
//! The audio is streamed through as the engine produces it: the response starts with the first
//! chunk, and the model's lease is held until the last one. A failure after the first chunk can
//! only cut the response short, as with Ollama.

use std::time::Instant;

use alkahest_engine::Capability;
use alkahest_sched::Usage;
use alkahest_store::{CatalogEntry, ModelSettings, media_type};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::auth::RequestContext;
use crate::dialect::Family;
use crate::generation::acquire;
use crate::service::{self, HttpError, UsageSink, blocking, respond};
use crate::state::AppState;
use crate::upstream::post_json;

/// The speech routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/audio/speech", post(speech))
        .route("/v1/audio/voices", get(voices))
}

/// Longest `input`, in characters (Ollama's and the engine's limit).
pub const MAX_INPUT_CHARS: usize = 4096;

/// Output formats and their content types; the first is the default.
const FORMATS: [(&str, &str); 4] = [
    ("wav", "audio/wav"),
    ("pcm", "audio/pcm"),
    ("mp3", "audio/mpeg"),
    ("opus", "audio/ogg"),
];

type Ctx = Option<axum::Extension<RequestContext>>;

/// A validated speech request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechRequest {
    /// Model name.
    pub model: String,
    /// Text to speak.
    pub input: String,
    /// Voice name; `None` is the checkpoint's default.
    pub voice: Option<String>,
    /// `wav`, `pcm`, `mp3` or `opus`.
    pub format: &'static str,
}

impl SpeechRequest {
    /// Validates an OpenAI speech request body.
    pub fn parse(body: &[u8]) -> Result<Self, HttpError> {
        let doc: Value = serde_json::from_slice(body).map_err(|e| {
            HttpError::bad_request(format!(
                "we could not parse the JSON body of your request: {e}"
            ))
        })?;
        let text = |name: &str| {
            doc.get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let model = text("model")
            .ok_or_else(|| HttpError::bad_request("you must provide a model parameter"))?
            .to_string();
        let input = doc
            .get("input")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| HttpError::bad_request("input: field required (the text to speak)"))?
            .to_string();
        if input.chars().count() > MAX_INPUT_CHARS {
            return Err(HttpError::bad_request(format!(
                "input must be at most {MAX_INPUT_CHARS} characters"
            )));
        }
        let format = match text("response_format") {
            None => FORMATS[0].0,
            Some(f) => FORMATS
                .iter()
                .map(|(name, _)| *name)
                .find(|name| name.eq_ignore_ascii_case(f))
                .ok_or_else(|| {
                    HttpError::bad_request(format!(
                        "response_format {f:?} is not supported, use wav, pcm, mp3 or opus"
                    ))
                })?,
        };
        match doc.get("speed") {
            None | Some(Value::Null) => {}
            Some(v) if v.as_f64() == Some(1.0) => {}
            Some(_) => return Err(HttpError::bad_request("only speed 1 is supported")),
        }
        if text("instructions").is_some() {
            return Err(HttpError::bad_request("instructions are not supported"));
        }
        if let Some(sf) = text("stream_format").filter(|s| *s != "audio") {
            return Err(HttpError::bad_request(format!(
                "stream_format {sf:?} is not supported, use audio"
            )));
        }
        Ok(Self {
            model,
            input,
            voice: text("voice").map(str::to_string),
            format,
        })
    }

    /// The body sent to the engine: the format always explicit (the engine's own default is
    /// mp3, Ollama's is wav).
    pub fn upstream_body(&self) -> Value {
        let mut m = Map::new();
        m.insert("model".into(), json!(self.model));
        m.insert("input".into(), json!(self.input));
        m.insert("response_format".into(), json!(self.format));
        if let Some(v) = &self.voice {
            m.insert("voice".into(), json!(v));
        }
        Value::Object(m)
    }

    /// The content type of the answer.
    pub fn content_type(&self) -> &'static str {
        FORMATS
            .iter()
            .find(|(name, _)| *name == self.format)
            .map_or("application/octet-stream", |(_, ct)| ct)
    }
}

/// `POST /v1/audio/speech`.
async fn speech(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = speech_inner(&state, ctx.as_ref(), &body).await;
    respond(&state, Family::OpenAi, r)
}

async fn speech_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: &[u8],
) -> Result<Response, HttpError> {
    let req = SpeechRequest::parse(body)?;
    let acquired = acquire(state, ctx, &req.model, &ModelSettings::default(), None).await?;
    let lease = acquired.lease;
    service::require(&lease, &req.model, Capability::TextToSpeech)?;

    let client = service::upstream(state);
    let started = Instant::now();
    let base_url = lease.base_url();
    let upstream_body = req.upstream_body();
    let call = post_json(&client, &base_url, "/v1/audio/speech", &upstream_body);
    let resp = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        r = call => r?,
    };
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static(req.content_type()));
    let sink = UsageSink::new(state, ctx);
    let mut upstream = resp.bytes_stream();
    let stream = async_stream::stream! {
        // The lease moves in here: the model stays loaded until the last chunk is sent.
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => yield Ok(bytes),
                Err(e) => {
                    tracing::warn!(error = %e, "speech stream from the engine failed");
                    yield Err(std::io::Error::other(e));
                    return;
                }
            }
        }
        sink.record(&lease, Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            generation_time: Some(started.elapsed()),
        });
    };
    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, content_type);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(resp)
}

/// Query of `GET /v1/audio/voices`.
#[derive(Debug, Deserialize)]
struct VoicesQuery {
    #[serde(default)]
    model: String,
}

/// `GET /v1/audio/voices?model=<name>`: the voices of a speech model, from its checkpoint
/// files (the model need not be loaded).
async fn voices(State(state): State<AppState>, ctx: Ctx, Query(q): Query<VoicesQuery>) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = async {
        if q.model.trim().is_empty() {
            return Err(HttpError::bad_request("you must provide a model parameter"));
        }
        service::check_model_access(&state, ctx.as_ref(), &q.model).await?;
        let catalog = service::catalog(&state)?;
        let model = q.model.clone();
        let entry = blocking(move || {
            catalog.resolve(&model).map_err(|e| match e {
                alkahest_store::StoreError::ModelNotFound { .. } => {
                    HttpError::model_not_found(&model)
                }
                other => HttpError::from_store(&other),
            })
        })
        .await?;
        if !is_speech_model(&entry) {
            return Err(HttpError::bad_request(format!(
                "{:?} does not support speech",
                q.model
            )));
        }
        Ok(axum::Json(json!({
            "model": q.model,
            "voices": speech_voices(&entry),
        }))
        .into_response())
    }
    .await;
    respond(&state, Family::OpenAi, r)
}

/// Whether the catalog lists `entry` as a speech model.
pub fn is_speech_model(entry: &CatalogEntry) -> bool {
    entry.info.capabilities.iter().any(|c| c == "speech")
}

/// The voices of a Phonon checkpoint, from its file layers: `default-voice.safetensors` is
/// `default`, `voices/<Name>.safetensors` (or the older `embeddings/` folder) is `<Name>`.
/// Sorted case-insensitively, without duplicates, as Ollama lists them.
pub fn speech_voices(entry: &CatalogEntry) -> Vec<String> {
    let mut voices: Vec<String> = entry
        .entry
        .layers
        .iter()
        .filter(|l| l.descriptor.media_type == media_type::SAFETENSORS_FILE)
        .filter_map(|l| voice_of_file(l.descriptor.name.as_deref()?))
        .collect();
    voices.sort_by_key(|v| v.to_lowercase());
    voices.dedup();
    voices
}

/// The voice a checkpoint file holds, by its path in the checkpoint.
pub fn voice_of_file(path: &str) -> Option<String> {
    if path == "default-voice.safetensors" {
        return Some("default".into());
    }
    let (dir, file) = path.split_once('/')?;
    let name = file.strip_suffix(".safetensors")?;
    (matches!(dir, "voices" | "embeddings") && !name.is_empty() && !name.contains('/'))
        .then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn parse(v: Value) -> Result<SpeechRequest, HttpError> {
        SpeechRequest::parse(v.to_string().as_bytes())
    }

    #[test]
    fn defaults_to_wav_and_the_default_voice() {
        let r = parse(json!({"model": "phonon", "input": "Hello."})).unwrap();
        assert_eq!(r.format, "wav");
        assert_eq!(r.voice, None);
        assert_eq!(r.content_type(), "audio/wav");
        assert_eq!(
            r.upstream_body(),
            json!({"model": "phonon", "input": "Hello.", "response_format": "wav"})
        );
    }

    #[test]
    fn accepts_what_ollama_and_the_engine_accept() {
        let r = parse(json!({"model": "phonon", "input": "Hi", "voice": "Zoey",
            "response_format": "PCM", "speed": 1.0, "stream_format": "audio"}))
        .unwrap();
        assert_eq!((r.format, r.voice.as_deref()), ("pcm", Some("Zoey")));
        assert_eq!(
            parse(json!({"model": "m", "input": "x", "response_format": "mp3"}))
                .unwrap()
                .format,
            "mp3"
        );
    }

    #[test]
    fn rejects_what_it_cannot_honour() {
        for body in [
            json!({"input": "x"}),
            json!({"model": "m"}),
            json!({"model": "m", "input": "   "}),
            json!({"model": "m", "input": "x", "response_format": "flac"}),
            json!({"model": "m", "input": "x", "speed": 1.5}),
            json!({"model": "m", "input": "x", "instructions": "cheerful"}),
            json!({"model": "m", "input": "x", "stream_format": "sse"}),
            json!({"model": "m", "input": "a".repeat(MAX_INPUT_CHARS + 1)}),
        ] {
            assert_eq!(
                parse(body.clone()).unwrap_err().status().as_u16(),
                400,
                "{body}"
            );
        }
        assert!(parse(json!({"model": "m", "input": "é".repeat(MAX_INPUT_CHARS)})).is_ok());
    }

    #[test]
    fn voices_come_from_checkpoint_files() {
        assert_eq!(
            voice_of_file("default-voice.safetensors").as_deref(),
            Some("default")
        );
        assert_eq!(
            voice_of_file("voices/Captain Silas Blackwater.safetensors").as_deref(),
            Some("Captain Silas Blackwater")
        );
        assert_eq!(
            voice_of_file("embeddings/Zoey.safetensors").as_deref(),
            Some("Zoey")
        );
        assert_eq!(voice_of_file("model.safetensors"), None);
        assert_eq!(voice_of_file("voices/a/b.safetensors"), None);
        assert_eq!(voice_of_file("other/Zoey.safetensors"), None);
    }
}
