//! llama-server OpenAI chat stream -> Anthropic Messages stream events.
//!
//! [`Translator`] consumes the engine's `chat.completion.chunk` objects and produces the
//! Anthropic event sequence: `message_start`, `ping`, then per content block
//! `content_block_start` / `content_block_delta` (`text_delta`, `thinking_delta`,
//! `input_json_delta`) / `content_block_stop`, then `message_delta` (`stop_reason`,
//! `stop_sequence`, cumulative `usage`) and `message_stop`. Non-streaming responses are the
//! same events folded by [`aggregate`], so both shapes always agree.
//!
//! `message_start` is held back until the engine reports prompt timings (llama-server sends
//! them with every token when asked for `timings_per_token`), so `usage.input_tokens` is
//! real. Client stop sequences are matched here, on answer text only: text that could be the
//! start of a stop sequence is held back until it is decided, and a match ends the
//! generation with `stop_reason: "stop_sequence"` and the matched sequence.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::upstream::{Timings, usage_of};

/// One Anthropic SSE event: name and data object.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// SSE `event:` name (equals `data.type`).
    pub name: &'static str,
    /// The data object.
    pub data: Value,
}

impl Event {
    fn new(name: &'static str, mut data: Value) -> Self {
        data["type"] = json!(name);
        Self { name, data }
    }

    /// The SSE wire form (`event: ...\ndata: ...\n\n`).
    pub fn to_sse(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.name, self.data)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Text,
    Thinking,
    Tool(u64),
}

/// The stream state machine.
#[derive(Debug)]
pub struct Translator {
    id: String,
    model: String,
    stop_sequences: Vec<String>,
    started: bool,
    open: Option<(usize, Block)>,
    next_index: usize,
    /// Upstream tool call index -> (block index, id, name).
    tools: BTreeMap<u64, usize>,
    pending_text: String,
    finish_reason: Option<String>,
    stop_sequence: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    saw_tool: bool,
}

impl Translator {
    /// A translator for message `id` answering as `model`.
    pub fn new(id: String, model: String, stop_sequences: Vec<String>) -> Self {
        Self {
            id,
            model,
            stop_sequences,
            started: false,
            open: None,
            next_index: 0,
            tools: BTreeMap::new(),
            pending_text: String::new(),
            finish_reason: None,
            stop_sequence: None,
            input_tokens: 0,
            output_tokens: 0,
            saw_tool: false,
        }
    }

    /// Prompt and generated token counts so far.
    pub fn usage(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }

    /// True once a stop sequence matched: the caller stops reading the engine (dropping the
    /// connection cancels the generation) and calls [`Self::finish`].
    pub fn stopped(&self) -> bool {
        self.stop_sequence.is_some()
    }

    fn start(&mut self, out: &mut Vec<Event>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(Event::new(
            "message_start",
            json!({"message": {
                "id": self.id, "type": "message", "role": "assistant", "content": [],
                "model": self.model, "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": self.input_tokens, "output_tokens": 1},
            }}),
        ));
        out.push(Event::new("ping", json!({})));
    }

    fn close(&mut self, out: &mut Vec<Event>) {
        if let Some((index, _)) = self.open.take() {
            out.push(Event::new("content_block_stop", json!({"index": index})));
        }
    }

    /// Makes `kind` the open block, closing the previous one; returns its index.
    fn ensure(&mut self, kind: Block, start_block: Value, out: &mut Vec<Event>) -> usize {
        if let Some((index, open)) = self.open
            && open == kind
        {
            return index;
        }
        self.close(out);
        let index = self.next_index;
        self.next_index += 1;
        self.open = Some((index, kind));
        out.push(Event::new(
            "content_block_start",
            json!({"index": index, "content_block": start_block}),
        ));
        index
    }

    fn emit_text(&mut self, text: &str, out: &mut Vec<Event>) {
        if text.is_empty() {
            return;
        }
        let index = self.ensure(Block::Text, json!({"type": "text", "text": ""}), out);
        out.push(Event::new(
            "content_block_delta",
            json!({"index": index, "delta": {"type": "text_delta", "text": text}}),
        ));
    }

    /// Adds answer text, holding back a possible stop-sequence prefix.
    fn push_text(&mut self, text: &str, out: &mut Vec<Event>) {
        if self.stop_sequences.is_empty() {
            self.emit_text(text, out);
            return;
        }
        self.pending_text.push_str(text);
        let earliest = self
            .stop_sequences
            .iter()
            .filter_map(|s| self.pending_text.find(s.as_str()).map(|p| (p, s.clone())))
            .min_by_key(|(p, _)| *p);
        if let Some((pos, seq)) = earliest {
            let before = self.pending_text[..pos].to_string();
            self.pending_text.clear();
            self.emit_text(&before, out);
            self.stop_sequence = Some(seq);
            return;
        }
        // Longest suffix of the pending text that is a proper prefix of a stop sequence.
        let mut keep = 0;
        for s in &self.stop_sequences {
            for (cut, _) in s.char_indices().skip(1) {
                if cut > keep && self.pending_text.ends_with(&s[..cut]) {
                    keep = cut;
                }
            }
        }
        let safe = self.pending_text.len() - keep;
        let ready = self.pending_text[..safe].to_string();
        self.pending_text.drain(..safe);
        self.emit_text(&ready, out);
    }

    fn flush_text(&mut self, out: &mut Vec<Event>) {
        let rest = std::mem::take(&mut self.pending_text);
        self.emit_text(&rest, out);
    }

    /// Translates one engine chunk.
    pub fn on_chunk(&mut self, chunk: &Value) -> Vec<Event> {
        let mut out = Vec::new();
        if self.stopped() {
            return out;
        }
        if let Some(t) = Timings::from_payload(chunk) {
            self.input_tokens = t.prompt_tokens();
            self.output_tokens = t.predicted_n;
        }
        if let Some((p, c)) = usage_of(chunk) {
            if self.input_tokens == 0 {
                self.input_tokens = p;
            }
            self.output_tokens = self.output_tokens.max(c);
        }
        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
            return out;
        };
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        let content = delta
            .and_then(|d| d.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let thinking = delta
            .and_then(|d| d.get("reasoning_content"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let calls = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array);
        let finish = choice.get("finish_reason").and_then(Value::as_str);
        let has_payload = !content.is_empty() || !thinking.is_empty() || calls.is_some();
        // Hold message_start back until the prompt size is known (or output begins).
        if has_payload || finish.is_some() || self.input_tokens > 0 {
            self.start(&mut out);
        }
        if !thinking.is_empty() {
            let index = self.ensure(
                Block::Thinking,
                json!({"type": "thinking", "thinking": "", "signature": ""}),
                &mut out,
            );
            out.push(Event::new(
                "content_block_delta",
                json!({"index": index, "delta": {"type": "thinking_delta", "thinking": thinking}}),
            ));
        }
        if !content.is_empty() {
            self.push_text(content, &mut out);
            if self.stopped() {
                return out;
            }
        }
        if let Some(calls) = calls {
            self.flush_text(&mut out);
            for (pos, d) in calls.iter().enumerate() {
                self.tool_delta(pos as u64, d, &mut out);
            }
        }
        if let Some(f) = finish {
            self.finish_reason = Some(f.to_string());
        }
        out
    }

    fn tool_delta(&mut self, pos: u64, d: &Value, out: &mut Vec<Event>) {
        let upstream = d.get("index").and_then(Value::as_u64).unwrap_or(pos);
        let f = d.get("function");
        let index = match self.tools.get(&upstream) {
            Some(i) if self.open == Some((*i, Block::Tool(upstream))) => *i,
            Some(_) => return, // a late delta for a block already closed
            None => {
                let id = d
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| format!("toolu_{}_{upstream}", self.id), str::to_string);
                let name = f
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                self.saw_tool = true;
                let i = self.ensure(
                    Block::Tool(upstream),
                    json!({"type": "tool_use", "id": id, "name": name, "input": {}}),
                    out,
                );
                self.tools.insert(upstream, i);
                i
            }
        };
        let args = match f.and_then(|f| f.get("arguments")) {
            Some(Value::String(s)) => s.clone(),
            Some(v) if !v.is_null() => v.to_string(),
            _ => String::new(),
        };
        if !args.is_empty() {
            out.push(Event::new(
                "content_block_delta",
                json!({"index": index, "delta": {"type": "input_json_delta", "partial_json": args}}),
            ));
        }
    }

    /// Closes the message: the open block, `message_delta` and `message_stop`.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        self.start(&mut out);
        self.flush_text(&mut out);
        self.close(&mut out);
        let stop_reason = if self.stop_sequence.is_some() {
            "stop_sequence"
        } else if self.saw_tool || self.finish_reason.as_deref() == Some("tool_calls") {
            "tool_use"
        } else if self.finish_reason.as_deref() == Some("length") {
            "max_tokens"
        } else {
            "end_turn"
        };
        out.push(Event::new(
            "message_delta",
            json!({
                "delta": {"stop_reason": stop_reason, "stop_sequence": self.stop_sequence},
                "usage": {"input_tokens": self.input_tokens, "output_tokens": self.output_tokens},
            }),
        ));
        out.push(Event::new("message_stop", json!({})));
        out
    }
}

/// Folds a complete event sequence into the non-streaming `message` object.
pub fn aggregate(events: &[Event]) -> Value {
    let mut message = json!({});
    let mut blocks: Vec<Value> = Vec::new();
    let mut partial: BTreeMap<usize, String> = BTreeMap::new();
    for e in events {
        let d = &e.data;
        match e.name {
            "message_start" => message = d["message"].clone(),
            "content_block_start" => blocks.push(d["content_block"].clone()),
            "content_block_delta" => {
                let index = d["index"].as_u64().unwrap_or(0) as usize;
                let Some(block) = blocks.get_mut(index) else {
                    continue;
                };
                let delta = &d["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => append(block, "text", &delta["text"]),
                    Some("thinking_delta") => append(block, "thinking", &delta["thinking"]),
                    Some("input_json_delta") => partial
                        .entry(index)
                        .or_default()
                        .push_str(delta["partial_json"].as_str().unwrap_or("")),
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = d["index"].as_u64().unwrap_or(0) as usize;
                if let (Some(json), Some(block)) = (partial.remove(&index), blocks.get_mut(index)) {
                    block["input"] = parse_tool_input(&json);
                }
            }
            "message_delta" => {
                message["stop_reason"] = d["delta"]["stop_reason"].clone();
                message["stop_sequence"] = d["delta"]["stop_sequence"].clone();
                message["usage"] = d["usage"].clone();
            }
            _ => {}
        }
    }
    message["content"] = Value::Array(blocks);
    message
}

fn append(block: &mut Value, key: &str, piece: &Value) {
    let mut s = block[key].as_str().unwrap_or("").to_string();
    s.push_str(piece.as_str().unwrap_or(""));
    block[key] = json!(s);
}

/// Tool arguments the model produced; anything that is not a JSON object becomes `{}` (the
/// Anthropic shape requires an object) with a warning.
fn parse_tool_input(s: &str) -> Value {
    if s.trim().is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(s) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            tracing::warn!("engine produced tool arguments that are not a JSON object");
            json!({})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(delta: Value, finish: Value) -> Value {
        json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    }

    fn run(t: &mut Translator, chunks: &[Value]) -> Vec<Event> {
        let mut all = Vec::new();
        for c in chunks {
            all.extend(t.on_chunk(c));
            if t.stopped() {
                break;
            }
        }
        all.extend(t.finish());
        all
    }

    fn names(ev: &[Event]) -> Vec<&str> {
        ev.iter().map(|e| e.name).collect()
    }

    #[test]
    fn text_thinking_and_usage() {
        let mut t = Translator::new("msg_1".into(), "m".into(), vec![]);
        let mut first = chunk(json!({"reasoning_content": "hm"}), Value::Null);
        first["timings"] = json!({"prompt_n": 7, "cache_n": 3, "predicted_n": 1});
        let mut last = chunk(json!({}), json!("stop"));
        last["timings"] = json!({"prompt_n": 7, "cache_n": 3, "predicted_n": 4});
        let ev = run(
            &mut t,
            &[
                chunk(json!({"role": "assistant", "content": null}), Value::Null),
                first,
                chunk(json!({"content": "Hell"}), Value::Null),
                chunk(json!({"content": "o"}), Value::Null),
                last,
            ],
        );
        assert_eq!(
            names(&ev),
            [
                "message_start",
                "ping",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        assert_eq!(ev[0].data["message"]["usage"]["input_tokens"], 10);
        let m = aggregate(&ev);
        assert_eq!(
            m["content"][0],
            json!({"type": "thinking", "thinking": "hm", "signature": ""})
        );
        assert_eq!(m["content"][1], json!({"type": "text", "text": "Hello"}));
        assert_eq!(m["stop_reason"], "end_turn");
        assert_eq!(m["usage"]["output_tokens"], 4);
        assert_eq!(m["id"], "msg_1");
    }

    #[test]
    fn tool_use_streams_input_json() {
        let mut t = Translator::new("msg_2".into(), "m".into(), vec![]);
        let ev = run(
            &mut t,
            &[
                chunk(json!({"content": "Let me check."}), Value::Null),
                chunk(
                    json!({"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "get_weather", "arguments": "{\"ci"}}]}),
                    Value::Null,
                ),
                chunk(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": "ty\":\"Paris\"}"}}]}),
                    Value::Null,
                ),
                chunk(json!({}), json!("tool_calls")),
            ],
        );
        let deltas: Vec<&Value> = ev
            .iter()
            .filter(|e| e.data["delta"]["type"] == "input_json_delta")
            .map(|e| &e.data["delta"]["partial_json"])
            .collect();
        assert_eq!(deltas, [&json!("{\"ci"), &json!("ty\":\"Paris\"}")]);
        let m = aggregate(&ev);
        assert_eq!(m["content"][1]["type"], "tool_use");
        assert_eq!(m["content"][1]["id"], "call_1");
        assert_eq!(m["content"][1]["input"], json!({"city": "Paris"}));
        assert_eq!(m["stop_reason"], "tool_use");
    }

    #[test]
    fn stop_sequences_are_matched_across_chunks() {
        let mut t = Translator::new("m3".into(), "m".into(), vec!["END".into()]);
        let ev = run(
            &mut t,
            &[
                chunk(json!({"content": "one E"}), Value::Null),
                chunk(json!({"content": "N"}), Value::Null),
                chunk(json!({"content": "D two"}), Value::Null),
                chunk(json!({"content": "never"}), Value::Null),
            ],
        );
        let m = aggregate(&ev);
        assert_eq!(m["content"][0]["text"], "one ");
        assert_eq!(m["stop_reason"], "stop_sequence");
        assert_eq!(m["stop_sequence"], "END");

        // A held-back prefix that does not complete is flushed at the end.
        let mut t = Translator::new("m4".into(), "m".into(), vec!["END".into()]);
        let ev = run(
            &mut t,
            &[chunk(json!({"content": "fin E"}), json!("length"))],
        );
        let m = aggregate(&ev);
        assert_eq!(m["content"][0]["text"], "fin E");
        assert_eq!(m["stop_reason"], "max_tokens");
        assert_eq!(m["stop_sequence"], Value::Null);
    }

    #[test]
    fn empty_generation_still_forms_a_message() {
        let mut t = Translator::new("m5".into(), "m".into(), vec![]);
        let ev = run(&mut t, &[]);
        assert_eq!(
            names(&ev),
            ["message_start", "ping", "message_delta", "message_stop"]
        );
        assert_eq!(aggregate(&ev)["content"], json!([]));
        assert!(ev[0].to_sse().starts_with("event: message_start\ndata: {"));
    }
}
