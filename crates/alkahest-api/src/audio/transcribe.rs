//! `POST /v1/audio/transcriptions` and `POST /v1/audio/translations` (ALK-US-0057,
//! ALK-SP-0005.R4), proxied to whisper.cpp's `whisper-server` `/inference`.
//!
//! Request (multipart): `file`, `model`, and optionally `language` (transcriptions only),
//! `prompt`, `response_format` (`json` default, `text`, `srt`, `vtt`, `verbose_json`),
//! `temperature` and `timestamp_granularities[]` (`word` asks for word timings in
//! `verbose_json`). Translations always produce English text, as OpenAI's.
//!
//! Response shapes follow OpenAI: `json` is `{"text": ...}`; `text`, `srt` and `vtt` are plain
//! text; `verbose_json` is `{task, language, duration, text, segments[, words]}`. whisper-server
//! adds fields OpenAI does not have (language probabilities, token ids); they are dropped so
//! SDKs see the documented shape.
//!
//! whisper-server decodes WAV itself and other containers through `ffmpeg`; a file it cannot
//! decode answers 400 with a hint instead of the engine's bare "failed to read audio data".

use std::time::Instant;

use alkahest_engine::Capability;
use alkahest_sched::Usage;
use alkahest_store::ModelSettings;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use reqwest::multipart::{Form, Part};
use serde_json::{Map, Value, json};

use crate::auth::RequestContext;
use crate::dialect::{ErrorKind, Family};
use crate::generation::acquire;
use crate::service::{self, HttpError, UsageSink, respond};
use crate::state::AppState;
use crate::upstream::engine_error_message;

/// The transcription and translation routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/audio/transcriptions", post(transcriptions))
        .route("/v1/audio/translations", post(translations))
}

type Ctx = Option<axum::Extension<RequestContext>>;

/// The `response_format` values OpenAI defines for transcription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    /// `{"text": ...}`.
    Json,
    /// Plain text.
    Text,
    /// SubRip subtitles.
    Srt,
    /// WebVTT subtitles.
    Vtt,
    /// Text with language, duration, segments and optionally words.
    VerboseJson,
}

impl ResponseFormat {
    /// Parses the field value.
    pub fn parse(s: &str) -> Result<Self, HttpError> {
        Ok(match s {
            "json" => Self::Json,
            "text" => Self::Text,
            "srt" => Self::Srt,
            "vtt" => Self::Vtt,
            "verbose_json" => Self::VerboseJson,
            other => {
                return Err(HttpError::bad_request(format!(
                    "response_format: unsupported value `{other}`; use json, text, srt, vtt or verbose_json"
                )));
            }
        })
    }

    /// The value sent to whisper-server (same names).
    fn engine_value(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Text => "text",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
            Self::VerboseJson => "verbose_json",
        }
    }
}

/// A parsed transcription or translation request.
#[derive(Debug, Clone)]
pub struct TranscribeRequest {
    /// Model name.
    pub model: String,
    /// The audio file.
    pub file: Bytes,
    /// Client file name (used for the upstream part and the media type).
    pub filename: String,
    /// Spoken language (ISO 639-1) or `None` for detection.
    pub language: Option<String>,
    /// Text that primes the decoder (names, spelling, style).
    pub prompt: Option<String>,
    /// Output format.
    pub format: ResponseFormat,
    /// Sampling temperature (0 to 1).
    pub temperature: Option<f64>,
    /// Word timings were requested (`timestamp_granularities[]=word`).
    pub words: bool,
    /// Translate to English instead of transcribing.
    pub translate: bool,
}

/// Reads the multipart body into a [`TranscribeRequest`].
pub async fn parse_multipart(
    mut mp: Multipart,
    translate: bool,
) -> Result<TranscribeRequest, HttpError> {
    let bad = |e: axum::extract::multipart::MultipartError| {
        HttpError::bad_request(format!(
            "invalid multipart/form-data body: {}",
            e.body_text()
        ))
    };
    let mut req = TranscribeRequest {
        model: String::new(),
        file: Bytes::new(),
        filename: "audio".into(),
        language: None,
        prompt: None,
        format: ResponseFormat::Json,
        temperature: None,
        words: false,
        translate,
    };
    let mut have_file = false;
    while let Some(field) = mp.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "file" => {
                if let Some(f) = field.file_name().filter(|f| !f.is_empty()) {
                    req.filename = f.to_string();
                }
                req.file = field.bytes().await.map_err(bad)?;
                have_file = true;
            }
            _ => {
                let text = field.text().await.map_err(bad)?;
                match name.as_str() {
                    "model" => req.model = text,
                    "language" if !translate && !text.trim().is_empty() => {
                        req.language = Some(text.trim().to_string());
                    }
                    "prompt" if !text.is_empty() => req.prompt = Some(text),
                    "response_format" => req.format = ResponseFormat::parse(text.trim())?,
                    "temperature" if !text.trim().is_empty() => {
                        req.temperature = Some(
                            text.trim()
                                .parse::<f64>()
                                .ok()
                                .filter(|t| (0.0..=1.0).contains(t))
                                .ok_or_else(|| {
                                    HttpError::bad_request("temperature: must be between 0 and 1")
                                })?,
                        );
                    }
                    "timestamp_granularities[]" | "timestamp_granularities" => match text.trim() {
                        "word" => req.words = true,
                        "segment" => {}
                        other => {
                            return Err(HttpError::bad_request(format!(
                                "timestamp_granularities: unsupported value `{other}`"
                            )));
                        }
                    },
                    "stream" if matches!(text.trim(), "true" | "1") => {
                        return Err(HttpError::bad_request(
                            "stream: streaming transcription is not supported",
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    if req.model.trim().is_empty() {
        return Err(HttpError::bad_request("you must provide a model parameter"));
    }
    if !have_file || req.file.is_empty() {
        return Err(HttpError::bad_request("file: field required (the audio)"));
    }
    if req.words && req.format != ResponseFormat::VerboseJson {
        return Err(HttpError::bad_request(
            "timestamp_granularities: word timings need response_format=verbose_json",
        ));
    }
    Ok(req)
}

/// The multipart form sent to `whisper-server /inference`.
fn upstream_form(req: &TranscribeRequest) -> Form {
    let mime = mime_of(&req.filename);
    let part = Part::bytes(req.file.to_vec())
        .file_name(req.filename.clone())
        .mime_str(mime)
        .unwrap_or_else(|_| Part::bytes(req.file.to_vec()).file_name(req.filename.clone()));
    // whisper-server defaults to English; OpenAI detects the language when none is given.
    let mut form = Form::new()
        .part("file", part)
        .text("response_format", req.format.engine_value())
        .text(
            "language",
            req.language.clone().unwrap_or_else(|| "auto".into()),
        );
    if let Some(p) = &req.prompt {
        form = form.text("prompt", p.clone());
    }
    if let Some(t) = req.temperature {
        form = form.text("temperature", t.to_string());
    }
    if req.translate {
        form = form.text("translate", "true");
    }
    if req.words {
        form = form.text("token_timestamps", "true");
    }
    form
}

/// Media type for an audio file name; the engine ignores it but proxies and logs read it.
fn mime_of(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "wav" => "audio/wav",
        "mp3" | "mpga" | "mpeg" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    }
}

/// Shapes a `verbose_json` document from whisper-server into OpenAI's.
pub fn shape_verbose(doc: &Value, words: bool, translate: bool) -> Value {
    let text = doc
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut all_words: Vec<Value> = Vec::new();
    let segments: Vec<Value> = doc
        .get("segments")
        .and_then(Value::as_array)
        .map(|segs| {
            segs.iter()
                .map(|s| {
                    if words {
                        collect_words(s, &mut all_words);
                    }
                    let mut o = Map::new();
                    for k in [
                        "id",
                        "start",
                        "end",
                        "text",
                        "tokens",
                        "temperature",
                        "avg_logprob",
                        "no_speech_prob",
                    ] {
                        if let Some(v) = s.get(k) {
                            o.insert(k.to_string(), v.clone());
                        }
                    }
                    Value::Object(o)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut out = json!({
        "task": if translate { "translate" } else { "transcribe" },
        "language": doc.get("language").cloned().unwrap_or(Value::Null),
        "duration": doc.get("duration").cloned().unwrap_or(json!(0)),
        "text": text,
        "segments": segments,
    });
    if words {
        out["words"] = Value::Array(all_words);
    }
    out
}

/// Appends the words of one whisper segment (`words[]` entries with text and timings),
/// skipping special tokens such as `[_BEG_]` and entries without timings.
fn collect_words(segment: &Value, out: &mut Vec<Value>) {
    let Some(words) = segment.get("words").and_then(Value::as_array) else {
        return;
    };
    for w in words {
        let (Some(word), Some(start), Some(end)) = (
            w.get("word").and_then(Value::as_str),
            w.get("start").and_then(Value::as_f64),
            w.get("end").and_then(Value::as_f64),
        ) else {
            continue;
        };
        let word = word.trim();
        if word.is_empty() || (word.starts_with("[_") && word.ends_with("_]")) {
            continue;
        }
        out.push(json!({"word": word, "start": start, "end": end}));
    }
}

/// `POST /v1/audio/transcriptions`.
async fn transcriptions(State(state): State<AppState>, ctx: Ctx, request: Request) -> Response {
    handle(state, ctx, request, false).await
}

/// `POST /v1/audio/translations`.
async fn translations(State(state): State<AppState>, ctx: Ctx, request: Request) -> Response {
    handle(state, ctx, request, true).await
}

async fn handle(state: AppState, ctx: Ctx, request: Request, translate: bool) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = handle_inner(&state, ctx.as_ref(), request, translate).await;
    respond(&state, Family::OpenAi, r)
}

async fn handle_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    request: Request,
    translate: bool,
) -> Result<Response, HttpError> {
    let multipart = Multipart::from_request(request, &()).await.map_err(|e| {
        HttpError::bad_request(format!(
            "the request must be multipart/form-data: {}",
            e.body_text()
        ))
    })?;
    let req = parse_multipart(multipart, translate).await?;
    let acquired = acquire(state, ctx, &req.model, &ModelSettings::default(), None).await?;
    let lease = acquired.lease;
    service::require(&lease, &req.model, Capability::SpeechToText)?;

    let client = service::upstream(state);
    let url = format!("{}/inference", lease.base_url().trim_end_matches('/'));
    let started = Instant::now();
    let call = async {
        let resp = client
            .post(&url)
            .multipart(upstream_form(&req))
            .send()
            .await
            .map_err(|e| {
                HttpError::new(
                    ErrorKind::BadGateway,
                    format!("the model engine did not answer: {e}"),
                )
            })?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            HttpError::new(
                ErrorKind::BadGateway,
                format!("invalid response from the model engine: {e}"),
            )
        })?;
        if status.is_success() {
            return Ok(body);
        }
        let msg = engine_error_message(&body, status.as_u16());
        let msg = if msg.contains("failed to read audio") || msg.contains("ffmpeg") {
            format!(
                "could not decode the audio file ({msg}); whisper.cpp reads 16-bit WAV directly, \
                 other formats need ffmpeg on the server's PATH"
            )
        } else {
            msg
        };
        Err(HttpError::new(ErrorKind::from_status(status.as_u16()), msg))
    };
    let body = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        b = call => b?,
    };
    UsageSink::new(state, ctx).record(
        &lease,
        Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            generation_time: Some(started.elapsed()),
        },
    );
    render(&req, &body)
}

/// Turns whisper-server's body into the response for `req.format`.
fn render(req: &TranscribeRequest, body: &str) -> Result<Response, HttpError> {
    let plain = |text: String| {
        let mut r = text.into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        r
    };
    let json_of = || {
        serde_json::from_str::<Value>(body).map_err(|e| {
            HttpError::new(
                ErrorKind::BadGateway,
                format!("invalid response from the model engine: {e}"),
            )
        })
    };
    Ok(match req.format {
        ResponseFormat::Text => plain(format!("{}\n", body.trim())),
        ResponseFormat::Srt | ResponseFormat::Vtt => plain(body.to_string()),
        ResponseFormat::Json => {
            let doc = json_of()?;
            let text = doc.get("text").and_then(Value::as_str).unwrap_or_default();
            axum::Json(json!({"text": text.trim()})).into_response()
        }
        ResponseFormat::VerboseJson => {
            axum::Json(shape_verbose(&json_of()?, req.words, req.translate)).into_response()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_parse_and_reject() {
        assert_eq!(ResponseFormat::parse("srt").unwrap(), ResponseFormat::Srt);
        assert!(ResponseFormat::parse("xml").is_err());
    }

    #[test]
    fn verbose_json_drops_engine_extras_and_collects_words() {
        let doc = json!({
            "task": "transcribe", "language": "english", "duration": 2.5, "text": " Hello world.",
            "detected_language": "english", "language_probabilities": {"en": 0.9},
            "segments": [{
                "id": 0, "start": 0.0, "end": 2.5, "text": " Hello world.", "tokens": [1, 2],
                "words": [
                    {"word": "[_BEG_]", "start": 0.0, "end": 0.0, "probability": 1.0},
                    {"word": " Hello", "start": 0.1, "end": 0.6, "probability": 0.9},
                    {"word": " world.", "start": 0.7, "end": 1.2, "probability": 0.9}
                ],
                "temperature": 0.0, "avg_logprob": -0.2, "no_speech_prob": 0.01
            }]
        });
        let out = shape_verbose(&doc, true, false);
        assert_eq!(out["task"], "transcribe");
        assert_eq!(out["text"], "Hello world.");
        assert!(out.get("language_probabilities").is_none());
        assert!(out["segments"][0].get("words").is_none());
        assert_eq!(out["words"].as_array().unwrap().len(), 2);
        assert_eq!(
            out["words"][0],
            json!({"word": "Hello", "start": 0.1, "end": 0.6})
        );
        assert!(shape_verbose(&doc, false, true).get("words").is_none());
        assert_eq!(shape_verbose(&doc, false, true)["task"], "translate");
    }

    #[test]
    fn mime_types() {
        assert_eq!(mime_of("a.WAV"), "audio/wav");
        assert_eq!(mime_of("a.m4a"), "audio/mp4");
        assert_eq!(mime_of("noext"), "application/octet-stream");
    }
}
