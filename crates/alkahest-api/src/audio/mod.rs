//! OpenAI audio endpoints (ALK-SP-0005.R4): `POST /v1/audio/transcriptions` and
//! `POST /v1/audio/translations` (speech to text, [`transcribe`]).

use axum::Router;

use crate::state::AppState;

pub mod transcribe;

/// The audio routes, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    transcribe::routes()
}
