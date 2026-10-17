//! Shared helpers for pooled models (embeddings and rerank, ALK-SP-0005.R4).
//!
//! Embedding and rerank engines run with their batch equal to the context (or
//! [`EMBED_BATCH_DEFAULT`]), so an input up to that many tokens fits. Ollama's `/api/embed`
//! truncates longer inputs by default (`truncate: true`), as Ollama does; [`fit_inputs`]
//! implements it with the engine's own tokenizer (`/tokenize`, `/detokenize`) and context
//! size (`/props`). The OpenAI endpoint does not truncate: like OpenAI, a too-long input is
//! the engine's error.

use alkahest_engine::EMBED_BATCH_DEFAULT;
use serde_json::{Value, json};

use crate::service::HttpError;
use crate::upstream::{post_json, read_json};

/// Room for the special tokens (BOS/CLS, EOS/SEP) the engine adds around an input.
const SPECIAL_TOKENS: usize = 2;

/// Inputs no longer than this many bytes cannot exceed any context the server uses (a token
/// covers at least one byte), so they skip the tokenizer round trip.
const SAFE_BYTES: usize = 256;

/// The engine's per-slot context size from `/props`.
async fn engine_context(client: &reqwest::Client, base: &str) -> Result<Option<u64>, HttpError> {
    let resp = client
        .get(format!("{base}/props"))
        .send()
        .await
        .map_err(|e| HttpError::new(crate::ErrorKind::BadGateway, e.to_string()))?;
    let doc: Value = resp.json().await.unwrap_or(Value::Null);
    Ok(doc
        .get("default_generation_settings")
        .and_then(|d| d.get("n_ctx"))
        .and_then(Value::as_u64))
}

/// Truncates each input to what one engine batch holds: `min(n_ctx, batch)` tokens minus the
/// special tokens, where `batch` is the configured context length or
/// [`EMBED_BATCH_DEFAULT`].
pub async fn fit_inputs(
    client: &reqwest::Client,
    base: &str,
    texts: &[String],
    context_length: Option<u64>,
) -> Result<Vec<String>, HttpError> {
    let batch = context_length
        .filter(|c| *c > 0)
        .unwrap_or(EMBED_BATCH_DEFAULT);
    let floor = usize::try_from(batch).unwrap_or(usize::MAX).min(SAFE_BYTES);
    if texts.iter().all(|t| t.len() <= floor) {
        return Ok(texts.to_vec());
    }
    let n_ctx = engine_context(client, base).await?.unwrap_or(batch);
    let limit = usize::try_from(n_ctx.min(batch))
        .unwrap_or(usize::MAX)
        .saturating_sub(SPECIAL_TOKENS)
        .max(1);
    let mut out = Vec::with_capacity(texts.len());
    for t in texts {
        if t.len() <= limit {
            out.push(t.clone());
            continue;
        }
        let doc =
            read_json(post_json(client, base, "/tokenize", &json!({"content": t})).await?).await?;
        let tokens: Vec<Value> = doc
            .get("tokens")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if tokens.len() <= limit {
            out.push(t.clone());
            continue;
        }
        let doc = read_json(
            post_json(
                client,
                base,
                "/detokenize",
                &json!({"tokens": &tokens[..limit]}),
            )
            .await?,
        )
        .await?;
        let text = doc
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        tracing::debug!(
            from = tokens.len(),
            to = limit,
            "truncated an embedding input"
        );
        out.push(text);
    }
    Ok(out)
}

/// llama-server answers an input longer than its batch with a 500 ("input (N tokens) is too
/// large to process"); for the client that is a bad request, as Ollama and OpenAI report it.
pub fn input_too_long(e: HttpError) -> HttpError {
    if e.message.contains("too large to process") {
        HttpError::bad_request(format!(
            "the input length exceeds the context length ({})",
            e.message
        ))
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    #[test]
    fn too_long_inputs_are_bad_requests() {
        let e = input_too_long(HttpError::internal(
            "input (5002 tokens) is too large to process. increase the physical batch size",
        ));
        assert_eq!(e.kind, ErrorKind::BadRequest);
        assert!(
            e.message
                .starts_with("the input length exceeds the context length")
        );
        let other = input_too_long(HttpError::internal("boom"));
        assert_eq!(other.kind, ErrorKind::Internal);
    }
}
