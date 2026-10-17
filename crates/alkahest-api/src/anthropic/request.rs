//! Anthropic Messages request -> llama-server OpenAI chat request.
//!
//! Mapping (ALK-SP-0005.R3):
//!
//! - `system` (string or text blocks) becomes the leading system message;
//! - user `text` / `image` blocks become OpenAI content parts (`image_url` with a data URL
//!   for base64 sources, the URL itself for `url` sources); `tool_result` blocks become
//!   `tool` messages placed before the rest of the user turn, as OpenAI requires;
//! - assistant `text`, `thinking` and `tool_use` blocks become `content`,
//!   `reasoning_content` and `tool_calls` (arguments serialised as a JSON string);
//! - `tools` (`name`, `description`, `input_schema`) become function tools, `tool_choice`
//!   `auto` / `any` / `tool` / `none` become `auto` / `required` / the named function /
//!   `none`, `disable_parallel_tool_use` becomes `parallel_tool_calls: false`;
//! - `thinking` `enabled` / `adaptive` / `disabled` becomes
//!   `chat_template_kwargs.enable_thinking`;
//! - `max_tokens` (required), `temperature`, `top_p`, `top_k` pass through;
//!   `stop_sequences` are matched by the stream translator so the response can name the
//!   sequence that stopped the generation (llama-server does not report it).

use serde_json::{Map, Value, json};

use crate::service::HttpError;

/// A translated request.
#[derive(Debug, Clone, PartialEq)]
pub struct Translated {
    /// OpenAI chat body for llama-server (without `model` and `stream`).
    pub body: Map<String, Value>,
    /// Client stop sequences, matched by [`super::stream::Translator`].
    pub stop_sequences: Vec<String>,
    /// The request carries image blocks (needs the Vision capability).
    pub images: bool,
    /// The request declares tools.
    pub tools: bool,
    /// The request has a top-level `system`.
    pub has_system: bool,
    /// `stream: true`.
    pub stream: bool,
}

fn bad(msg: impl Into<String>) -> HttpError {
    HttpError::bad_request(msg)
}

/// Translates an Anthropic Messages body. `count_only` skips the `max_tokens` requirement
/// (`/v1/messages/count_tokens` has none).
pub fn translate(req: &Map<String, Value>, count_only: bool) -> Result<Translated, HttpError> {
    let mut body = Map::new();
    let mut messages = Vec::new();
    let mut images = false;

    let mut has_system = false;
    match req.get("system") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => {
            if !s.is_empty() {
                messages.push(json!({"role": "system", "content": s}));
                has_system = true;
            }
        }
        Some(Value::Array(blocks)) => {
            let mut text = Vec::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => text.push(str_field(b, "text")?.to_string()),
                    other => {
                        return Err(bad(format!(
                            "system: unsupported block type {}",
                            other.unwrap_or("(missing)")
                        )));
                    }
                }
            }
            if !text.is_empty() {
                messages.push(json!({"role": "system", "content": text.join("\n")}));
                has_system = true;
            }
        }
        Some(_) => return Err(bad("system: must be a string or an array of text blocks")),
    }

    let Some(Value::Array(msgs)) = req.get("messages") else {
        return Err(bad("messages: field required"));
    };
    for (i, m) in msgs.iter().enumerate() {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        let content = m
            .get("content")
            .ok_or_else(|| bad(format!("messages.{i}.content: field required")))?;
        match role {
            "user" => user_turn(i, content, &mut messages, &mut images)?,
            "assistant" => messages.push(assistant_turn(i, content)?),
            other => {
                return Err(bad(format!(
                    "messages.{i}.role: unexpected value {other:?}; expected \"user\" or \"assistant\""
                )));
            }
        }
    }
    body.insert("messages".into(), Value::Array(messages));

    if !count_only {
        match req.get("max_tokens") {
            Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n >= 1) => {
                body.insert("max_tokens".into(), Value::Number(n.clone()));
            }
            Some(_) => return Err(bad("max_tokens: must be a positive integer")),
            None => return Err(bad("max_tokens: field required")),
        }
    }
    for key in ["temperature", "top_p", "top_k"] {
        if let Some(v) = req.get(key).filter(|v| !v.is_null()) {
            if !v.is_number() {
                return Err(bad(format!("{key}: must be a number")));
            }
            body.insert(key.into(), v.clone());
        }
    }

    let stop_sequences = match req.get("stop_sequences") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|s| {
                s.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| bad("stop_sequences: must be an array of strings"))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect(),
        Some(_) => return Err(bad("stop_sequences: must be an array of strings")),
    };

    let tools = translate_tools(req, &mut body)?;

    match req.get("thinking") {
        None | Some(Value::Null) => {}
        Some(t) => match t.get("type").and_then(Value::as_str) {
            Some("enabled" | "adaptive") => {
                body.insert(
                    "chat_template_kwargs".into(),
                    json!({"enable_thinking": true}),
                );
            }
            Some("disabled") => {
                body.insert(
                    "chat_template_kwargs".into(),
                    json!({"enable_thinking": false}),
                );
            }
            _ => {
                return Err(bad(
                    "thinking.type: expected \"enabled\", \"adaptive\" or \"disabled\"",
                ));
            }
        },
    }

    Ok(Translated {
        body,
        stop_sequences,
        images,
        tools,
        has_system,
        stream: req.get("stream").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn str_field<'a>(b: &'a Value, key: &str) -> Result<&'a str, HttpError> {
    b.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("{key}: field required")))
}

/// An Anthropic image block -> an OpenAI `image_url` part.
fn image_part(b: &Value) -> Result<Value, HttpError> {
    let source = b
        .get("source")
        .ok_or_else(|| bad("image.source: field required"))?;
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media = str_field(source, "media_type")?;
            let data = str_field(source, "data")?;
            format!("data:{media};base64,{data}")
        }
        Some("url") => str_field(source, "url")?.to_string(),
        other => {
            return Err(bad(format!(
                "image.source.type: unsupported value {}",
                other.unwrap_or("(missing)")
            )));
        }
    };
    Ok(json!({"type": "image_url", "image_url": {"url": url}}))
}

/// Text of a `tool_result` content (string or text blocks; images are not supported in tool
/// results by OpenAI tool messages and are refused).
fn tool_result_text(content: Option<&Value>) -> Result<String, HttpError> {
    match content {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Array(blocks)) => {
            let mut out = Vec::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => out.push(str_field(b, "text")?.to_string()),
                    other => {
                        return Err(bad(format!(
                            "tool_result.content: unsupported block type {}",
                            other.unwrap_or("(missing)")
                        )));
                    }
                }
            }
            Ok(out.join("\n"))
        }
        Some(_) => Err(bad("tool_result.content: must be a string or an array")),
    }
}

/// Parts that are all text collapse into one string, which every chat template renders.
fn content_value(parts: Vec<Value>) -> Value {
    if parts
        .iter()
        .all(|p| p.get("type").and_then(Value::as_str) == Some("text"))
    {
        let text: Vec<&str> = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        Value::String(text.join("\n"))
    } else {
        Value::Array(parts)
    }
}

fn user_turn(
    i: usize,
    content: &Value,
    out: &mut Vec<Value>,
    images: &mut bool,
) -> Result<(), HttpError> {
    let blocks = match content {
        Value::String(s) => {
            out.push(json!({"role": "user", "content": s}));
            return Ok(());
        }
        Value::Array(b) => b,
        _ => {
            return Err(bad(format!(
                "messages.{i}.content: must be a string or an array"
            )));
        }
    };
    let mut parts = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(json!({"type": "text", "text": str_field(b, "text")?})),
            Some("image") => {
                *images = true;
                parts.push(image_part(b)?);
            }
            Some("tool_result") => {
                let id = str_field(b, "tool_use_id")?;
                let mut text = tool_result_text(b.get("content"))?;
                if b.get("is_error").and_then(Value::as_bool) == Some(true) {
                    text = format!("Error: {text}");
                }
                out.push(json!({"role": "tool", "tool_call_id": id, "content": text}));
            }
            Some("document") => {
                let source = b.get("source");
                match source.and_then(|s| s.get("type")).and_then(Value::as_str) {
                    Some("text") => parts.push(json!({
                        "type": "text",
                        "text": source.and_then(|s| s.get("data")).and_then(Value::as_str).unwrap_or(""),
                    })),
                    _ => {
                        return Err(bad(format!(
                            "messages.{i}.content: only plain-text documents are supported"
                        )));
                    }
                }
            }
            other => {
                return Err(bad(format!(
                    "messages.{i}.content: unsupported block type {}",
                    other.unwrap_or("(missing)")
                )));
            }
        }
    }
    if !parts.is_empty() {
        out.push(json!({"role": "user", "content": content_value(parts)}));
    }
    Ok(())
}

fn assistant_turn(i: usize, content: &Value) -> Result<Value, HttpError> {
    let blocks = match content {
        Value::String(s) => return Ok(json!({"role": "assistant", "content": s})),
        Value::Array(b) => b,
        _ => {
            return Err(bad(format!(
                "messages.{i}.content: must be a string or an array"
            )));
        }
    };
    let mut text = Vec::new();
    let mut thinking = Vec::new();
    let mut calls = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => text.push(str_field(b, "text")?.to_string()),
            Some("thinking") => thinking.push(str_field(b, "thinking")?.to_string()),
            // Encrypted reasoning of another provider: nothing a local model can use.
            Some("redacted_thinking") => {}
            Some("tool_use") => {
                let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
                calls.push(json!({
                    "id": str_field(b, "id")?,
                    "type": "function",
                    "function": {"name": str_field(b, "name")?, "arguments": input.to_string()},
                }));
            }
            other => {
                return Err(bad(format!(
                    "messages.{i}.content: unsupported block type {} in an assistant turn",
                    other.unwrap_or("(missing)")
                )));
            }
        }
    }
    let mut m = json!({"role": "assistant", "content": text.join("")});
    if !thinking.is_empty() {
        m["reasoning_content"] = json!(thinking.join(""));
    }
    if !calls.is_empty() {
        m["tool_calls"] = Value::Array(calls);
    }
    Ok(m)
}

/// `tools` and `tool_choice`; returns whether any tool is declared.
fn translate_tools(
    req: &Map<String, Value>,
    body: &mut Map<String, Value>,
) -> Result<bool, HttpError> {
    let tools = match req.get("tools") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(t)) => t.clone(),
        Some(_) => return Err(bad("tools: must be an array")),
    };
    let mut out = Vec::new();
    for (i, t) in tools.iter().enumerate() {
        match t.get("type").and_then(Value::as_str) {
            None | Some("custom") => {}
            Some(other) => {
                return Err(bad(format!(
                    "tools.{i}: server tool {other:?} is not supported by local models"
                )));
            }
        }
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| bad(format!("tools.{i}.name: field required")))?;
        let mut f = json!({
            "name": name,
            "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"})),
        });
        if let Some(d) = t.get("description").and_then(Value::as_str) {
            f["description"] = json!(d);
        }
        out.push(json!({"type": "function", "function": f}));
    }
    let declared = !out.is_empty();
    if declared {
        body.insert("tools".into(), Value::Array(out));
    }
    if let Some(choice) = req.get("tool_choice").filter(|c| !c.is_null()) {
        let mapped = match choice.get("type").and_then(Value::as_str) {
            Some("auto") => json!("auto"),
            Some("any") => json!("required"),
            Some("none") => json!("none"),
            Some("tool") => {
                let name = str_field(choice, "name")?;
                json!({"type": "function", "function": {"name": name}})
            }
            _ => {
                return Err(bad(
                    "tool_choice.type: expected \"auto\", \"any\", \"tool\" or \"none\"",
                ));
            }
        };
        if declared {
            body.insert("tool_choice".into(), mapped);
        }
        if choice
            .get("disable_parallel_tool_use")
            .and_then(Value::as_bool)
            == Some(true)
        {
            body.insert("parallel_tool_calls".into(), json!(false));
        }
    }
    Ok(declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(v: Value) -> Result<Translated, HttpError> {
        translate(v.as_object().unwrap(), false)
    }

    #[test]
    fn system_blocks_and_text_messages() {
        let t = tr(json!({
            "model": "m", "max_tokens": 10,
            "system": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
            "messages": [{"role": "user", "content": "hi"},
                         {"role": "assistant", "content": [{"type": "text", "text": "yo"}]},
                         {"role": "user", "content": [{"type": "text", "text": "x"}, {"type": "text", "text": "y"}]}],
            "temperature": 0.5, "top_k": 3, "stop_sequences": ["END"],
        }))
        .unwrap();
        assert!(t.has_system);
        assert_eq!(
            t.body["messages"][0],
            json!({"role": "system", "content": "a\nb"})
        );
        assert_eq!(
            t.body["messages"][2],
            json!({"role": "assistant", "content": "yo"})
        );
        assert_eq!(t.body["messages"][3]["content"], "x\ny");
        assert_eq!(t.body["max_tokens"], 10);
        assert_eq!(t.body["top_k"], 3);
        assert_eq!(t.stop_sequences, vec!["END"]);
    }

    #[test]
    fn images_tools_and_results() {
        let t = tr(json!({
            "model": "m", "max_tokens": 10,
            "tools": [{"name": "get_weather", "description": "w", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "any", "disable_parallel_tool_use": true},
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "messages": [
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBOR"}},
                    {"type": "text", "text": "what?"}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": ""},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "sunny"}]},
                    {"type": "text", "text": "thanks"}]},
            ],
        }))
        .unwrap();
        assert!(t.images && t.tools);
        let m = t.body["messages"].as_array().unwrap();
        assert_eq!(
            m[0]["content"][0]["image_url"]["url"],
            "data:image/png;base64,iVBOR"
        );
        assert_eq!(m[1]["reasoning_content"], "hmm");
        assert_eq!(
            m[1]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        assert_eq!(
            m[2],
            json!({"role": "tool", "tool_call_id": "toolu_1", "content": "sunny"})
        );
        assert_eq!(m[3], json!({"role": "user", "content": "thanks"}));
        assert_eq!(
            t.body["tools"][0]["function"]["parameters"],
            json!({"type": "object"})
        );
        assert_eq!(t.body["tool_choice"], "required");
        assert_eq!(t.body["parallel_tool_calls"], false);
        assert_eq!(t.body["chat_template_kwargs"]["enable_thinking"], true);
    }

    #[test]
    fn invalid_requests() {
        assert!(
            tr(json!({"model": "m", "messages": []})).is_err(),
            "max_tokens required"
        );
        assert!(
            translate(
                json!({"model": "m", "messages": []}).as_object().unwrap(),
                true
            )
            .is_ok()
        );
        assert!(tr(json!({"model": "m", "max_tokens": 1, "messages": [{"role": "system", "content": "x"}]})).is_err());
        assert!(
            tr(json!({"model": "m", "max_tokens": 1, "messages": [],
            "tools": [{"type": "web_search_20250305", "name": "web_search"}]}))
            .is_err()
        );
        assert!(tr(json!({"model": "m", "max_tokens": 0, "messages": []})).is_err());
    }
}
