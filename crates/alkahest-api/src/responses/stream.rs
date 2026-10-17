//! llama-server OpenAI chat stream -> OpenAI Responses stream events.
//!
//! Event sequence: `response.created`, `response.in_progress`, then per output item
//! `response.output_item.added`, its parts and deltas, `response.output_item.done`, and
//! finally `response.completed` (or `response.incomplete` when `max_output_tokens` cut the
//! answer). Item kinds:
//!
//! - `reasoning` (engine `reasoning_content`): `content_part.added` (`reasoning_text`),
//!   `reasoning_text.delta`, `reasoning_text.done`, `content_part.done`;
//! - `message`: `content_part.added` (`output_text`), `output_text.delta`,
//!   `output_text.done`, `content_part.done`;
//! - `function_call`: `function_call_arguments.delta`, `function_call_arguments.done`.
//!
//! Every event carries a `sequence_number`. The non-streaming response is the final
//! `response` object of the same translator.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::upstream::{Timings, usage_of};

/// One Responses SSE event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// SSE `event:` name (equals `data.type`).
    pub name: &'static str,
    /// The data object.
    pub data: Value,
}

impl Event {
    /// The SSE wire form.
    pub fn to_sse(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.name, self.data)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Reasoning(usize),
    Message(usize),
    Tool(usize, u64),
}

/// The stream state machine for one response.
#[derive(Debug)]
pub struct Translator {
    id: String,
    created_at: i64,
    model: String,
    echo: Map<String, Value>,
    seq: u64,
    started: bool,
    items: Vec<Value>,
    open: Option<Open>,
    tools: BTreeMap<u64, usize>,
    finish_reason: Option<String>,
    input_tokens: u64,
    cached_tokens: u64,
    output_tokens: u64,
}

impl Translator {
    /// A translator for response `id`; `echo` holds the request fields the response object
    /// repeats (`instructions`, `tools`, `tool_choice`, `temperature`, ...).
    pub fn new(id: String, created_at: i64, model: String, echo: Map<String, Value>) -> Self {
        Self {
            id,
            created_at,
            model,
            echo,
            seq: 0,
            started: false,
            items: Vec::new(),
            open: None,
            tools: BTreeMap::new(),
            finish_reason: None,
            input_tokens: 0,
            cached_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Prompt and generated token counts so far.
    pub fn usage(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }

    fn event(&mut self, name: &'static str, mut data: Value) -> Event {
        data["type"] = json!(name);
        data["sequence_number"] = json!(self.seq);
        self.seq += 1;
        Event { name, data }
    }

    /// The response object in its current state.
    pub fn response(&self, status: &str) -> Value {
        let mut r = Map::new();
        r.insert("id".into(), json!(self.id));
        r.insert("object".into(), json!("response"));
        r.insert("created_at".into(), json!(self.created_at));
        r.insert("status".into(), json!(status));
        r.insert("model".into(), json!(self.model));
        r.insert("output".into(), Value::Array(self.items.clone()));
        r.insert("error".into(), Value::Null);
        r.insert(
            "incomplete_details".into(),
            if status == "incomplete" {
                json!({"reason": "max_output_tokens"})
            } else {
                Value::Null
            },
        );
        r.insert(
            "usage".into(),
            if status == "in_progress" {
                Value::Null
            } else {
                json!({
                    "input_tokens": self.input_tokens,
                    "input_tokens_details": {"cached_tokens": self.cached_tokens, "cache_write_tokens": 0},
                    "output_tokens": self.output_tokens,
                    "output_tokens_details": {"reasoning_tokens": 0},
                    "total_tokens": self.input_tokens + self.output_tokens,
                })
            },
        );
        for (k, v) in &self.echo {
            r.insert(k.clone(), v.clone());
        }
        Value::Object(r)
    }

    fn start(&mut self, out: &mut Vec<Event>) {
        if self.started {
            return;
        }
        self.started = true;
        let r = self.response("in_progress");
        let e = self.event("response.created", json!({"response": r}));
        out.push(e);
        let r = self.response("in_progress");
        let e = self.event("response.in_progress", json!({"response": r}));
        out.push(e);
    }

    fn item_id(&self, prefix: &str) -> String {
        let bare = self.id.strip_prefix("resp_").unwrap_or(&self.id);
        format!("{prefix}_{bare}_{}", self.items.len())
    }

    fn close(&mut self, out: &mut Vec<Event>) {
        let Some(open) = self.open.take() else {
            return;
        };
        match open {
            Open::Message(i) | Open::Reasoning(i) => {
                let reasoning = matches!(open, Open::Reasoning(_));
                let item_id = self.items[i]["id"].clone();
                let text = self.items[i]["content"][0]["text"].clone();
                let (done, part) = if reasoning {
                    (
                        self.event(
                            "response.reasoning_text.done",
                            json!({"item_id": item_id, "output_index": i, "content_index": 0, "text": text}),
                        ),
                        json!({"type": "reasoning_text", "text": text}),
                    )
                } else {
                    (
                        self.event(
                            "response.output_text.done",
                            json!({"item_id": item_id, "output_index": i, "content_index": 0,
                                   "text": text, "logprobs": []}),
                        ),
                        json!({"type": "output_text", "text": text, "annotations": []}),
                    )
                };
                out.push(done);
                let e = self.event(
                    "response.content_part.done",
                    json!({"item_id": item_id, "output_index": i, "content_index": 0, "part": part}),
                );
                out.push(e);
            }
            Open::Tool(i, _) => {
                let item = self.items[i].clone();
                let e = self.event(
                    "response.function_call_arguments.done",
                    json!({"item_id": item["id"], "output_index": i,
                           "arguments": item["arguments"], "name": item["name"]}),
                );
                out.push(e);
            }
        }
        let i = match open {
            Open::Message(i) | Open::Reasoning(i) | Open::Tool(i, _) => i,
        };
        self.items[i]["status"] = json!("completed");
        let item = self.items[i].clone();
        let e = self.event(
            "response.output_item.done",
            json!({"output_index": i, "item": item}),
        );
        out.push(e);
    }

    fn open_text_item(&mut self, reasoning: bool, out: &mut Vec<Event>) -> usize {
        match (self.open, reasoning) {
            (Some(Open::Reasoning(i)), true) | (Some(Open::Message(i)), false) => return i,
            _ => {}
        }
        self.close(out);
        let i = self.items.len();
        let (item, part) = if reasoning {
            (
                json!({"id": self.item_id("rs"), "type": "reasoning", "summary": [],
                       "content": [], "status": "in_progress"}),
                json!({"type": "reasoning_text", "text": ""}),
            )
        } else {
            (
                json!({"id": self.item_id("msg"), "type": "message", "status": "in_progress",
                       "role": "assistant", "content": []}),
                json!({"type": "output_text", "text": "", "annotations": []}),
            )
        };
        let item_id = item["id"].clone();
        self.items.push(item.clone());
        let e = self.event(
            "response.output_item.added",
            json!({"output_index": i, "item": item}),
        );
        out.push(e);
        self.items[i]["content"] = json!([part.clone()]);
        let e = self.event(
            "response.content_part.added",
            json!({"item_id": item_id, "output_index": i, "content_index": 0, "part": part}),
        );
        out.push(e);
        self.open = Some(if reasoning {
            Open::Reasoning(i)
        } else {
            Open::Message(i)
        });
        i
    }

    fn push_text(&mut self, reasoning: bool, text: &str, out: &mut Vec<Event>) {
        let i = self.open_text_item(reasoning, out);
        let mut s = self.items[i]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        s.push_str(text);
        self.items[i]["content"][0]["text"] = json!(s);
        let item_id = self.items[i]["id"].clone();
        let e = if reasoning {
            self.event(
                "response.reasoning_text.delta",
                json!({"item_id": item_id, "output_index": i, "content_index": 0, "delta": text}),
            )
        } else {
            self.event(
                "response.output_text.delta",
                json!({"item_id": item_id, "output_index": i, "content_index": 0,
                       "delta": text, "logprobs": []}),
            )
        };
        out.push(e);
    }

    fn tool_delta(&mut self, pos: u64, d: &Value, out: &mut Vec<Event>) {
        let upstream = d.get("index").and_then(Value::as_u64).unwrap_or(pos);
        let f = d.get("function");
        let i = match self.tools.get(&upstream) {
            Some(i) if self.open == Some(Open::Tool(*i, upstream)) => *i,
            Some(_) => return,
            None => {
                self.close(out);
                let i = self.items.len();
                let call_id = d
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| format!("call_{i}"), str::to_string);
                let name = f
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let item = json!({"id": self.item_id("fc"), "type": "function_call",
                    "status": "in_progress", "call_id": call_id, "name": name, "arguments": ""});
                self.items.push(item.clone());
                let e = self.event(
                    "response.output_item.added",
                    json!({"output_index": i, "item": item}),
                );
                out.push(e);
                self.tools.insert(upstream, i);
                self.open = Some(Open::Tool(i, upstream));
                i
            }
        };
        let args = match f.and_then(|f| f.get("arguments")) {
            Some(Value::String(s)) => s.clone(),
            Some(v) if !v.is_null() => v.to_string(),
            _ => String::new(),
        };
        if args.is_empty() {
            return;
        }
        let mut all = self.items[i]["arguments"]
            .as_str()
            .unwrap_or("")
            .to_string();
        all.push_str(&args);
        self.items[i]["arguments"] = json!(all);
        let item_id = self.items[i]["id"].clone();
        let e = self.event(
            "response.function_call_arguments.delta",
            json!({"item_id": item_id, "output_index": i, "delta": args}),
        );
        out.push(e);
    }

    /// Translates one engine chunk.
    pub fn on_chunk(&mut self, chunk: &Value) -> Vec<Event> {
        let mut out = Vec::new();
        self.start(&mut out);
        if let Some(t) = Timings::from_payload(chunk) {
            self.input_tokens = t.prompt_tokens();
            self.cached_tokens = t.cache_n.unwrap_or(0);
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
        let field = |k: &str| {
            delta
                .and_then(|d| d.get(k))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let thinking = field("reasoning_content");
        if !thinking.is_empty() {
            self.push_text(true, &thinking, &mut out);
        }
        let content = field("content");
        if !content.is_empty() {
            self.push_text(false, &content, &mut out);
        }
        if let Some(calls) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for (pos, d) in calls.iter().enumerate() {
                self.tool_delta(pos as u64, d, &mut out);
            }
        }
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(f.to_string());
        }
        out
    }

    /// Closes the response; returns the final events and the final response object.
    pub fn finish(&mut self) -> (Vec<Event>, Value) {
        let mut out = Vec::new();
        self.start(&mut out);
        self.close(&mut out);
        let incomplete = self.finish_reason.as_deref() == Some("length");
        let status = if incomplete {
            "incomplete"
        } else {
            "completed"
        };
        let response = self.response(status);
        let name = if incomplete {
            "response.incomplete"
        } else {
            "response.completed"
        };
        let e = self.event(name, json!({"response": response.clone()}));
        out.push(e);
        (out, response)
    }

    /// The output as chat messages, for the stored conversation a follow-up continues.
    pub fn assistant_messages(&self) -> Vec<Value> {
        let mut text = String::new();
        let mut calls = Vec::new();
        for item in &self.items {
            match item["type"].as_str() {
                Some("message") => text.push_str(item["content"][0]["text"].as_str().unwrap_or("")),
                Some("function_call") => calls.push(json!({
                    "id": item["call_id"], "type": "function",
                    "function": {"name": item["name"], "arguments": item["arguments"]},
                })),
                _ => {}
            }
        }
        let mut m = json!({"role": "assistant", "content": text});
        if !calls.is_empty() {
            m["tool_calls"] = Value::Array(calls);
        }
        vec![m]
    }
}

/// Error event inside a stream.
pub fn error_event(seq: u64, code: &str, message: &str) -> Event {
    Event {
        name: "error",
        data: json!({"type": "error", "sequence_number": seq, "code": code, "message": message, "param": null}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(delta: Value, finish: Value) -> Value {
        json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    }

    fn names(ev: &[Event]) -> Vec<&str> {
        ev.iter().map(|e| e.name).collect()
    }

    #[test]
    fn reasoning_then_text() {
        let mut t = Translator::new("resp_a".into(), 1, "m".into(), Map::new());
        let mut ev = Vec::new();
        ev.extend(t.on_chunk(&chunk(json!({"reasoning_content": "hm"}), Value::Null)));
        ev.extend(t.on_chunk(&chunk(json!({"content": "Hell"}), Value::Null)));
        let mut last = chunk(json!({"content": "o"}), json!("stop"));
        last["timings"] = json!({"prompt_n": 5, "cache_n": 1, "predicted_n": 3});
        ev.extend(t.on_chunk(&last));
        let (fin, resp) = t.finish();
        ev.extend(fin);
        assert_eq!(
            names(&ev),
            [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.reasoning_text.delta",
                "response.reasoning_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        for (n, e) in ev.iter().enumerate() {
            assert_eq!(e.data["sequence_number"], n as u64);
            assert_eq!(e.data["type"], e.name);
        }
        assert_eq!(resp["status"], "completed");
        assert_eq!(resp["output"][1]["content"][0]["text"], "Hello");
        assert_eq!(resp["output"][0]["content"][0]["text"], "hm");
        assert_eq!(resp["usage"]["input_tokens"], 6);
        assert_eq!(resp["usage"]["input_tokens_details"]["cached_tokens"], 1);
        assert_eq!(resp["usage"]["total_tokens"], 9);
        assert_eq!(t.assistant_messages()[0]["content"], "Hello");
    }

    #[test]
    fn function_calls_and_incomplete() {
        let mut t = Translator::new("resp_b".into(), 1, "m".into(), Map::new());
        let mut ev = Vec::new();
        ev.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_9", "function": {"name": "f", "arguments": "{\"a"}}]}),
            Value::Null,
        )));
        ev.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "\":1}"}}]}),
            json!("tool_calls"),
        )));
        let (fin, resp) = t.finish();
        ev.extend(fin);
        assert!(names(&ev).contains(&"response.function_call_arguments.done"));
        assert_eq!(resp["output"][0]["type"], "function_call");
        assert_eq!(resp["output"][0]["call_id"], "call_9");
        assert_eq!(resp["output"][0]["arguments"], "{\"a\":1}");
        assert_eq!(resp["output"][0]["status"], "completed");
        assert_eq!(t.assistant_messages()[0]["tool_calls"][0]["id"], "call_9");

        let mut t = Translator::new("resp_c".into(), 1, "m".into(), Map::new());
        t.on_chunk(&chunk(json!({"content": "x"}), json!("length")));
        let (fin, resp) = t.finish();
        assert_eq!(fin.last().unwrap().name, "response.incomplete");
        assert_eq!(resp["incomplete_details"]["reason"], "max_output_tokens");
    }
}
