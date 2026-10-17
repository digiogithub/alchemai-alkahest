//! Image generation endpoints (ALK-SP-0005.R4): `POST /v1/images/generations`
//! ([`generations`]), served by stable-diffusion.cpp's `sd-server`.
//!
//! Not to be confused with [`crate::images`], which handles images sent *to* vision models.

use axum::Router;

use crate::state::AppState;

pub mod generations;

/// The image generation routes, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    generations::routes()
}
