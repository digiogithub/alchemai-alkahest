//! OpenAI audio endpoints (ALK-SP-0005.R4): `POST /v1/audio/transcriptions` and
//! `POST /v1/audio/translations` (speech to text, [`transcribe`]) and `POST /v1/audio/speech` ([`speech`], 501 until an engine exists).

use axum::Router;

use crate::state::AppState;

pub mod speech;
pub mod transcribe;

/// The audio routes, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    transcribe::routes().merge(speech::routes())
}
