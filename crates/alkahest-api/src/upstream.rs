//! Talking to an engine's HTTP server (llama-server's OpenAI-compatible and native routes).
//!
//! Requests are proxied, never buffered when streamed: [`sse_events`] turns the engine's
//! server-sent events into JSON values as bytes arrive. Dropping the stream drops the
//! `reqwest` response, which closes the engine connection, so a client that disconnects stops
//! the generation upstream too.

use std::time::Duration;

use bytes::{Buf, BytesMut};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::dialect::ErrorKind;
use crate::service::HttpError;

/// One server-sent event of an engine stream.
#[derive(Debug, Clone, PartialEq)]
pub enum SseItem {
    /// A `data:` JSON payload.
    Data(Value),
    /// The OpenAI end marker `data: [DONE]`.
    Done,
}

/// Posts `body` to `<base_url><path>` and returns the response when its status is a success.
///
/// Connection failures become 502 (the engine is gone or not listening); engine error
/// statuses keep their class and carry the engine's message.
pub async fn post_json(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: &Value,
) -> Result<reqwest::Response, HttpError> {
    let url = format!("{}{path}", base_url.trim_end_matches('/'));
    let resp = client.post(&url).json(body).send().await.map_err(|e| {
        HttpError::new(
            ErrorKind::BadGateway,
            format!("the model engine did not answer: {e}"),
        )
    })?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    Err(HttpError::new(
        ErrorKind::from_status(status.as_u16()),
        engine_error_message(&text, status.as_u16()),
    ))
}

/// Extracts the message of an engine error body (`{"error":{"message":..}}`,
/// `{"error":".."}` or plain text).
pub fn engine_error_message(text: &str, status: u16) -> String {
    let parsed: Option<Value> = serde_json::from_str(text).ok();
    let msg = parsed.as_ref().and_then(|v| {
        let e = v.get("error")?;
        e.get("message")
            .and_then(Value::as_str)
            .or_else(|| e.as_str())
            .map(str::to_string)
    });
    match msg {
        Some(m) if !m.is_empty() => m,
        _ if !text.trim().is_empty() => text.trim().chars().take(500).collect(),
        _ => format!("the model engine answered with status {status}"),
    }
}

/// Reads the whole JSON body of a successful engine response.
pub async fn read_json(resp: reqwest::Response) -> Result<Value, HttpError> {
    resp.json::<Value>().await.map_err(|e| {
        HttpError::new(
            ErrorKind::BadGateway,
            format!("invalid response from the model engine: {e}"),
        )
    })
}

/// Parses an engine's server-sent event stream into JSON payloads.
///
/// Comment lines (`:`) are skipped; `error:` lines and payloads with an `error` member end
/// the stream with an error.
pub fn sse_events(resp: reqwest::Response) -> impl Stream<Item = Result<SseItem, HttpError>> {
    let mut body = resp.bytes_stream();
    async_stream::stream! {
        let mut buf = BytesMut::new();
        loop {
            // Drain complete lines first.
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line = buf.split_to(pos + 1);
                let mut line = &line[..pos];
                if line.last() == Some(&b'\r') {
                    line = &line[..line.len() - 1];
                }
                match parse_line(line) {
                    Ok(Some(item)) => yield Ok(item),
                    Ok(None) => {}
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
            match body.next().await {
                Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                Some(Err(e)) => {
                    yield Err(HttpError::new(
                        ErrorKind::BadGateway,
                        format!("the model engine stream broke: {e}"),
                    ));
                    return;
                }
                None => {
                    if buf.has_remaining() {
                        let rest = buf.split();
                        if let Ok(Some(item)) = parse_line(&rest) {
                            yield Ok(item);
                        }
                    }
                    return;
                }
            }
        }
    }
}

/// One SSE line: `data: {...}` -> payload, `data: [DONE]` -> end marker, `error: ...` ->
/// error, anything else (blank, comments, `event:`) -> nothing.
fn parse_line(line: &[u8]) -> Result<Option<SseItem>, HttpError> {
    let Ok(text) = std::str::from_utf8(line) else {
        return Ok(None);
    };
    let text = text.trim_end();
    if let Some(rest) = text.strip_prefix("data:") {
        let payload = rest.trim_start();
        if payload == "[DONE]" {
            return Ok(Some(SseItem::Done));
        }
        if payload.is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(payload).map_err(|e| {
            HttpError::new(
                ErrorKind::BadGateway,
                format!("invalid event from the model engine: {e}"),
            )
        })?;
        if let Some(err) = value.get("error") {
            let status = err
                .get("code")
                .and_then(Value::as_u64)
                .and_then(|c| u16::try_from(c).ok())
                .unwrap_or(500);
            return Err(HttpError::new(
                ErrorKind::from_status(status),
                engine_error_message(&value.to_string(), status),
            ));
        }
        return Ok(Some(SseItem::Data(value)));
    }
    if let Some(rest) = text.strip_prefix("error:") {
        return Err(HttpError::new(
            ErrorKind::BadGateway,
            engine_error_message(rest.trim(), 500),
        ));
    }
    Ok(None)
}

/// llama-server's per-request `timings` object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct Timings {
    /// Prompt tokens served from the KV cache.
    #[serde(default)]
    pub cache_n: Option<u64>,
    /// Prompt tokens evaluated.
    #[serde(default)]
    pub prompt_n: u64,
    /// Milliseconds spent on the prompt.
    #[serde(default)]
    pub prompt_ms: f64,
    /// Tokens generated.
    #[serde(default)]
    pub predicted_n: u64,
    /// Milliseconds spent generating.
    #[serde(default)]
    pub predicted_ms: f64,
}

impl Timings {
    /// Reads `timings` from an engine payload.
    pub fn from_payload(v: &Value) -> Option<Self> {
        v.get("timings")
            .and_then(|t| serde_json::from_value(t.clone()).ok())
    }

    /// Prompt tokens including cached ones (Ollama's `prompt_eval_count`).
    pub fn prompt_tokens(&self) -> u64 {
        self.prompt_n + self.cache_n.unwrap_or(0)
    }

    /// Prompt evaluation time.
    pub fn prompt_duration(&self) -> Duration {
        ms(self.prompt_ms)
    }

    /// Generation time.
    pub fn eval_duration(&self) -> Duration {
        ms(self.predicted_ms)
    }
}

fn ms(v: f64) -> Duration {
    if v.is_finite() && v > 0.0 {
        Duration::from_secs_f64(v / 1000.0)
    } else {
        Duration::ZERO
    }
}

/// OpenAI `usage` token counts of a payload: `(prompt, completion)`.
pub fn usage_of(v: &Value) -> Option<(u64, u64)> {
    let u = v.get("usage")?;
    if u.is_null() {
        return None;
    }
    Some((
        u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
        u.get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    ))
}

/// Inserts `key: value` unless the map already has the key.
pub fn insert_default(map: &mut Map<String, Value>, key: &str, value: Value) {
    if !map.contains_key(key) {
        map.insert(key.to_string(), value);
    }
}

/// Truncates each vector to `dims` and renormalises it to unit length (what Ollama and
/// OpenAI do for `dimensions`).
pub fn truncate_embeddings(vectors: &mut [Vec<f64>], dims: usize) {
    for v in vectors {
        if dims > 0 && dims < v.len() {
            v.truncate(dims);
            let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            if norm > 0.0 {
                for x in v.iter_mut() {
                    *x /= norm;
                }
            }
        }
    }
}

/// Little-endian `f32` bytes of `v`, base64 encoded (OpenAI `encoding_format: "base64"`).
pub fn embedding_base64(v: &[f64]) -> String {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&(*x as f32).to_le_bytes());
    }
    base64_encode(&bytes)
}

/// Standard base64 with padding. Small enough to keep a dependency out.
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Embedding vectors of an OpenAI-shaped `/v1/embeddings` response, in input order.
pub fn embeddings_of(v: &Value) -> Result<Vec<Vec<f64>>, HttpError> {
    let bad = || HttpError::new(ErrorKind::BadGateway, "invalid embeddings from the engine");
    let data = v.get("data").and_then(Value::as_array).ok_or_else(bad)?;
    let mut rows: Vec<(u64, Vec<f64>)> = Vec::with_capacity(data.len());
    for (i, item) in data.iter().enumerate() {
        let idx = item
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(i as u64);
        let emb = item.get("embedding").ok_or_else(bad)?;
        // llama-server may nest a single vector one level deeper for pooled outputs.
        let arr = match emb.as_array() {
            Some(a) if a.first().is_some_and(Value::is_array) => {
                a.first().and_then(Value::as_array).ok_or_else(bad)?
            }
            Some(a) => a,
            None => return Err(bad()),
        };
        rows.push((idx, arr.iter().filter_map(Value::as_f64).collect()));
    }
    rows.sort_by_key(|(i, _)| *i);
    Ok(rows.into_iter().map(|(_, v)| v).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn sse_lines() {
        assert_eq!(parse_line(b"data: [DONE]").unwrap(), Some(SseItem::Done));
        assert_eq!(parse_line(b": ping").unwrap(), None);
        assert_eq!(
            parse_line(br#"data: {"a":1}"#).unwrap(),
            Some(SseItem::Data(serde_json::json!({"a":1})))
        );
        let e = parse_line(br#"data: {"error":{"code":400,"message":"bad"}}"#).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadRequest);
        assert_eq!(e.message, "bad");
    }

    #[test]
    fn truncation_renormalises() {
        let mut v = vec![vec![3.0, 4.0, 12.0]];
        truncate_embeddings(&mut v, 2);
        assert!((v[0][0] - 0.6).abs() < 1e-9 && (v[0][1] - 0.8).abs() < 1e-9);
    }
}
