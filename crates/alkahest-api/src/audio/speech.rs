//! `POST /v1/audio/speech` (ALK-US-0058, ALK-SP-0005.R4): text to speech.
//!
//! No text-to-speech engine ships in v1 (ADR 0003): the licence-clean candidates are one-shot
//! command line tools without an OpenAI voice model, and the server-shaped ones are GPL. The
//! route exists so that clients get an OpenAI error object that says exactly that, instead of
//! a bare 404. A malformed request is still a 400, so SDK users see their own mistakes first.

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use serde_json::Value;

use crate::dialect::{ErrorKind, Family};
use crate::service::{HttpError, respond};
use crate::state::AppState;

/// The speech route.
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/audio/speech", post(speech))
}

/// The message of the 501.
pub const NOT_AVAILABLE: &str = "text to speech is not available: this version of Alkahest ships no \
text-to-speech engine (see ADR 0003); use /v1/audio/transcriptions for speech to text";

/// `POST /v1/audio/speech`.
async fn speech(State(state): State<AppState>, body: Bytes) -> Response {
    respond(&state, Family::OpenAi, speech_inner(&body))
}

fn speech_inner(body: &[u8]) -> Result<Response, HttpError> {
    let doc: Value = serde_json::from_slice(body).map_err(|e| {
        HttpError::bad_request(format!(
            "we could not parse the JSON body of your request: {e}"
        ))
    })?;
    let field = |name: &str| {
        doc.get(name)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    if !field("model") {
        return Err(HttpError::bad_request("you must provide a model parameter"));
    }
    if !field("input") {
        return Err(HttpError::bad_request(
            "input: field required (the text to speak)",
        ));
    }
    // When an engine advertises `Capability::TextToSpeech`, this handler acquires the model and
    // requires the capability like the other modality routes.
    Err(HttpError::new(ErrorKind::NotImplemented, NOT_AVAILABLE))
}
