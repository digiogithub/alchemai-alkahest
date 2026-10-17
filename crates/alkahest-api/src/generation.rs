//! Engine generation streams turned into dialect-neutral pieces.
//!
//! The Ollama handlers always ask the engine for a stream and either forward it as NDJSON or
//! aggregate it into one object, the way Ollama itself drives llama-server. [`chat_pieces`]
//! reads llama-server's `/v1/chat/completions` stream (ADR 0001: Jinja templates rendered by
//! the engine), [`completion_pieces`] its native `/completion` stream (raw prompts). Both own
//! the [`Lease`], race it against [`Lease::lost`] so a dying engine turns into a 502 at once,
//! and report token usage when the generation ends.

use std::collections::BTreeMap;
use std::time::Instant;

use alkahest_core::KeepAlive;
use alkahest_sched::{Lease, Scheduler, Usage};
use alkahest_store::ModelSettings;
use futures::{Stream, StreamExt};
use serde_json::{Value, json};

use crate::auth::RequestContext;
use crate::service::{self, HttpError, UsageSink};
use crate::state::AppState;
use crate::upstream::{SseItem, Timings, sse_events, usage_of};

/// A part of a generation.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    /// Generated text and/or reasoning text.
    Text {
        /// Answer text.
        content: String,
        /// Reasoning ("thinking") text.
        thinking: String,
    },
    /// Complete tool calls in OpenAI shape (`{id, type, function: {name, arguments}}`, the
    /// arguments a JSON string), emitted once when the engine finished them.
    ToolCalls(Vec<Value>),
    /// The generation ended.
    Finished(Finish),
}

/// How a generation ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Finish {
    /// `stop` or `length`.
    pub reason: String,
    /// Engine timings, when it reported them.
    pub timings: Option<Timings>,
    /// OpenAI usage counts `(prompt, completion)`, when reported.
    pub usage: Option<(u64, u64)>,
}

impl Finish {
    /// Prompt tokens (timings first: they include cached tokens).
    pub fn prompt_tokens(&self) -> u64 {
        self.timings
            .map(|t| t.prompt_tokens())
            .or(self.usage.map(|u| u.0))
            .unwrap_or(0)
    }

    /// Generated tokens.
    pub fn completion_tokens(&self) -> u64 {
        self.timings
            .map(|t| t.predicted_n)
            .or(self.usage.map(|u| u.1))
            .unwrap_or(0)
    }

    /// The usage report for the metrics and key counters.
    pub fn usage_report(&self) -> Usage {
        Usage {
            prompt_tokens: self.prompt_tokens(),
            completion_tokens: self.completion_tokens(),
            generation_time: self.timings.map(|t| t.eval_duration()),
        }
    }
}

/// A leased model ready for a request, with the time the load took.
#[derive(Debug)]
pub struct Acquired {
    /// The lease; keeps the model loaded until dropped.
    pub lease: Lease,
    /// Time spent waiting for the model (load or queue), Ollama's `load_duration`.
    pub load: std::time::Duration,
}

/// Checks the caller may use `model`, then leases it from the scheduler.
pub async fn acquire(
    state: &AppState,
    ctx: Option<&RequestContext>,
    model: &str,
    settings: &ModelSettings,
    keep_alive: Option<KeepAlive>,
) -> Result<Acquired, HttpError> {
    if model.trim().is_empty() {
        return Err(HttpError::bad_request("model is required"));
    }
    service::check_model_access(state, ctx, model).await?;
    let scheduler: Scheduler = service::scheduler(state)?;
    let started = Instant::now();
    let lease = scheduler
        .acquire(model, settings, keep_alive)
        .await
        .map_err(|e| HttpError::from_sched(&e))?;
    Ok(Acquired {
        lease,
        load: started.elapsed(),
    })
}

/// Accumulates streamed OpenAI tool call deltas by index.
#[derive(Debug, Default)]
struct ToolCallAcc {
    calls: BTreeMap<u64, (String, String, String)>,
}

impl ToolCallAcc {
    fn push(&mut self, deltas: &[Value]) {
        for (pos, d) in deltas.iter().enumerate() {
            let index = d.get("index").and_then(Value::as_u64).unwrap_or(pos as u64);
            let entry = self.calls.entry(index).or_default();
            if let Some(id) = d.get("id").and_then(Value::as_str)
                && entry.0.is_empty()
            {
                entry.0 = id.to_string();
            }
            if let Some(f) = d.get("function") {
                if let Some(name) = f.get("name").and_then(Value::as_str)
                    && entry.1.is_empty()
                {
                    entry.1 = name.to_string();
                }
                match f.get("arguments") {
                    Some(Value::String(a)) => entry.2.push_str(a),
                    Some(other) if !other.is_null() => entry.2.push_str(&other.to_string()),
                    _ => {}
                }
            }
        }
    }

    fn take(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.calls)
            .into_iter()
            .map(|(i, (id, name, args))| {
                let id = if id.is_empty() {
                    format!("call_{i}")
                } else {
                    id
                };
                json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}})
            })
            .collect()
    }
}

/// Pieces of a llama-server `/v1/chat/completions` stream.
pub fn chat_pieces(
    resp: reqwest::Response,
    lease: Lease,
    sink: UsageSink,
) -> impl Stream<Item = Result<Piece, HttpError>> + Send {
    async_stream::stream! {
        let events = sse_events(resp);
        futures::pin_mut!(events);
        let lost = lease.lost();
        tokio::pin!(lost);
        let mut tools = ToolCallAcc::default();
        let mut finish = Finish::default();
        loop {
            let next = tokio::select! {
                biased;
                e = &mut lost => {
                    yield Err(HttpError::from_sched(&e));
                    return;
                }
                n = events.next() => n,
            };
            let ev = match next {
                None | Some(Ok(SseItem::Done)) => break,
                Some(Err(e)) => {
                    yield Err(e);
                    return;
                }
                Some(Ok(SseItem::Data(v))) => v,
            };
            if let Some(t) = Timings::from_payload(&ev) {
                finish.timings = Some(t);
            }
            if let Some(u) = usage_of(&ev) {
                finish.usage = Some(u);
            }
            let Some(choice) = ev.get("choices").and_then(|c| c.get(0)) else {
                continue;
            };
            let delta = choice.get("delta").or_else(|| choice.get("message"));
            if let Some(delta) = delta {
                let content = delta.get("content").and_then(Value::as_str).unwrap_or("");
                let thinking = delta
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !content.is_empty() || !thinking.is_empty() {
                    yield Ok(Piece::Text {
                        content: content.to_string(),
                        thinking: thinking.to_string(),
                    });
                }
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    tools.push(calls);
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                finish.reason = match reason {
                    "length" => "length".to_string(),
                    _ => "stop".to_string(),
                };
            }
        }
        let calls = tools.take();
        if !calls.is_empty() {
            yield Ok(Piece::ToolCalls(calls));
        }
        if finish.reason.is_empty() {
            finish.reason = "stop".to_string();
        }
        sink.record(&lease, finish.usage_report());
        yield Ok(Piece::Finished(finish));
    }
}

/// Pieces of a llama-server native `/completion` stream (raw prompts).
pub fn completion_pieces(
    resp: reqwest::Response,
    lease: Lease,
    sink: UsageSink,
) -> impl Stream<Item = Result<Piece, HttpError>> + Send {
    async_stream::stream! {
        let events = sse_events(resp);
        futures::pin_mut!(events);
        let lost = lease.lost();
        tokio::pin!(lost);
        let mut finish = Finish::default();
        loop {
            let next = tokio::select! {
                biased;
                e = &mut lost => {
                    yield Err(HttpError::from_sched(&e));
                    return;
                }
                n = events.next() => n,
            };
            let ev = match next {
                None | Some(Ok(SseItem::Done)) => break,
                Some(Err(e)) => {
                    yield Err(e);
                    return;
                }
                Some(Ok(SseItem::Data(v))) => v,
            };
            let content = ev.get("content").and_then(Value::as_str).unwrap_or("");
            if !content.is_empty() {
                yield Ok(Piece::Text { content: content.to_string(), thinking: String::new() });
            }
            if let Some(t) = Timings::from_payload(&ev) {
                finish.timings = Some(t);
            }
            if ev.get("stop").and_then(Value::as_bool) == Some(true) {
                finish.reason = match ev.get("stop_type").and_then(Value::as_str) {
                    Some("limit") => "length".to_string(),
                    _ => "stop".to_string(),
                };
                if finish.timings.is_none() {
                    let p = ev.get("tokens_evaluated").and_then(Value::as_u64).unwrap_or(0);
                    let c = ev.get("tokens_predicted").and_then(Value::as_u64).unwrap_or(0);
                    finish.usage = Some((p, c));
                }
                break;
            }
        }
        if finish.reason.is_empty() {
            finish.reason = "stop".to_string();
        }
        sink.record(&lease, finish.usage_report());
        yield Ok(Piece::Finished(finish));
    }
}

/// An OpenAI tool call (arguments as a JSON string) in Ollama's shape (arguments as an
/// object, `function.index`).
pub fn ollama_tool_call(index: usize, call: &Value) -> Value {
    let f = call.get("function").cloned().unwrap_or(Value::Null);
    let name = f.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = match f.get("arguments") {
        Some(Value::String(s)) if s.trim().is_empty() => json!({}),
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| {
            tracing::warn!(
                tool = name,
                "engine produced tool arguments that are not JSON"
            );
            json!({})
        }),
        Some(Value::Object(o)) => Value::Object(o.clone()),
        _ => json!({}),
    };
    let mut out = json!({"function": {"index": index, "name": name, "arguments": arguments}});
    if let Some(id) = call.get("id").and_then(Value::as_str) {
        out["id"] = json!(id);
    }
    out
}

/// Ollama's final statistics (`total_duration` ... `eval_duration`, nanoseconds).
pub fn ollama_stats(started: Instant, load: std::time::Duration, f: &Finish) -> Value {
    let ns = |d: std::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
    let mut m = serde_json::Map::new();
    m.insert("total_duration".into(), json!(ns(started.elapsed())));
    m.insert("load_duration".into(), json!(ns(load)));
    m.insert("prompt_eval_count".into(), json!(f.prompt_tokens()));
    if let Some(t) = f.timings {
        m.insert(
            "prompt_eval_duration".into(),
            json!(ns(t.prompt_duration())),
        );
    }
    m.insert("eval_count".into(), json!(f.completion_tokens()));
    if let Some(t) = f.timings {
        m.insert("eval_duration".into(), json!(ns(t.eval_duration())));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_deltas_assemble() {
        let mut acc = ToolCallAcc::default();
        acc.push(&[
            json!({"index":0,"id":"c1","function":{"name":"get_weather","arguments":"{\"ci"}}),
        ]);
        acc.push(&[json!({"index":0,"function":{"arguments":"ty\":\"Paris\"}"}})]);
        let calls = acc.take();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["arguments"], "{\"city\":\"Paris\"}");
        let o = ollama_tool_call(0, &calls[0]);
        assert_eq!(o["function"]["arguments"]["city"], "Paris");
        assert_eq!(o["id"], "c1");
    }
}
