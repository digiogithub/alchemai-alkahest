//! OpenAI Responses request -> llama-server OpenAI chat request.
//!
//! - `instructions` -> leading system message (not carried over to follow-up responses, as
//!   OpenAI specifies);
//! - `input` string -> one user message; `input` items: messages (`role` user / system /
//!   developer / assistant, content string or `input_text` / `input_image` / `output_text`
//!   parts), `function_call` (-> assistant `tool_calls`, consecutive calls merged into one
//!   assistant turn), `function_call_output` (-> `tool` message); `reasoning` items are
//!   dropped (a local model cannot use another model's reasoning);
//! - `tools` of type `function` -> chat function tools (hosted tools -> 400); `tool_choice`
//!   string or `{type: function, name}`; `parallel_tool_calls`;
//! - `max_output_tokens` -> `max_tokens`; `temperature`, `top_p`;
//! - `text.format` `json_schema` / `json_object` -> `response_format`;
//! - `reasoning.effort` -> `chat_template_kwargs` (`none` disables thinking).

use serde_json::{Map, Value, json};

use crate::service::HttpError;

/// A translated request.
#[derive(Debug, Clone, PartialEq)]
pub struct Translated {
    /// Conversation messages (history and input, without `instructions`): what a follow-up
    /// response continues from.
    pub conversation: Vec<Value>,
    /// The system message built from `instructions`, if any.
    pub instructions: Option<String>,
    /// Chat body fields other than `messages`, `model` and `stream`.
    pub params: Map<String, Value>,
    /// The request carries image parts.
    pub images: bool,
    /// The request declares tools.
    pub tools: bool,
}

fn bad(msg: impl Into<String>) -> HttpError {
    HttpError::bad_request(msg)
}

/// Translates `req`, continuing `history` (the stored conversation of
/// `previous_response_id`).
pub fn translate(req: &Map<String, Value>, history: Vec<Value>) -> Result<Translated, HttpError> {
    let mut conversation = history;
    let mut images = false;
    match req.get("input") {
        Some(Value::String(s)) => conversation.push(json!({"role": "user", "content": s})),
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                input_item(i, item, &mut conversation, &mut images)?;
            }
        }
        None | Some(Value::Null) => return Err(bad("input: field required")),
        Some(_) => return Err(bad("input: must be a string or an array of items")),
    }
    let instructions = match req.get("instructions") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(bad("instructions: must be a string")),
    };

    let mut params = Map::new();
    if let Some(v) = req.get("max_output_tokens").filter(|v| !v.is_null()) {
        match v.as_u64() {
            Some(n) if n >= 1 => {
                params.insert("max_tokens".into(), json!(n));
            }
            _ => return Err(bad("max_output_tokens: must be a positive integer")),
        }
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = req.get(key).filter(|v| !v.is_null()) {
            if !v.is_number() {
                return Err(bad(format!("{key}: must be a number")));
            }
            params.insert(key.into(), v.clone());
        }
    }
    if let Some(v) = req.get("parallel_tool_calls").and_then(Value::as_bool) {
        params.insert("parallel_tool_calls".into(), json!(v));
    }
    let tools = translate_tools(req, &mut params)?;
    if let Some(format) = req.get("text").and_then(|t| t.get("format")) {
        match format.get("type").and_then(Value::as_str) {
            None | Some("text") => {}
            Some("json_object") => {
                params.insert("response_format".into(), json!({"type": "json_object"}));
            }
            Some("json_schema") => {
                let mut schema = json!({
                    "name": format.get("name").cloned().unwrap_or(json!("response")),
                    "schema": format.get("schema").cloned().unwrap_or(json!({})),
                });
                if let Some(strict) = format.get("strict").filter(|s| !s.is_null()) {
                    schema["strict"] = strict.clone();
                }
                params.insert(
                    "response_format".into(),
                    json!({"type": "json_schema", "json_schema": schema}),
                );
            }
            Some(other) => {
                return Err(bad(format!(
                    "text.format.type: unsupported value {other:?}"
                )));
            }
        }
    }
    if let Some(effort) = req
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
    {
        let kwargs = match effort {
            "none" => json!({"enable_thinking": false}),
            "minimal" | "low" | "medium" | "high" | "xhigh" => {
                json!({"enable_thinking": true, "reasoning_effort": effort})
            }
            other => {
                return Err(bad(format!(
                    "reasoning.effort: unsupported value {other:?}"
                )));
            }
        };
        params.insert("chat_template_kwargs".into(), kwargs);
    }
    Ok(Translated {
        conversation,
        instructions,
        params,
        images,
        tools,
    })
}

fn str_field<'a>(v: &'a Value, key: &str, at: &str) -> Result<&'a str, HttpError> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("{at}.{key}: field required")))
}

fn input_item(
    i: usize,
    item: &Value,
    out: &mut Vec<Value>,
    images: &mut bool,
) -> Result<(), HttpError> {
    let at = format!("input.{i}");
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    match kind {
        "message" => {
            let role = str_field(item, "role", &at)?;
            let role = match role {
                "user" | "system" | "assistant" => role,
                "developer" => "system",
                other => return Err(bad(format!("{at}.role: unsupported value {other:?}"))),
            };
            let content = message_content(&at, item.get("content"), images)?;
            out.push(json!({"role": role, "content": content}));
        }
        "function_call" => {
            let call = json!({
                "id": str_field(item, "call_id", &at)?,
                "type": "function",
                "function": {
                    "name": str_field(item, "name", &at)?,
                    "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
                },
            });
            // Parallel calls of one turn arrive as consecutive items: one assistant message.
            if let Some(last) = out.last_mut()
                && last.get("role").and_then(Value::as_str) == Some("assistant")
                && let Some(Value::Array(calls)) = last.get_mut("tool_calls")
            {
                calls.push(call);
            } else {
                out.push(json!({"role": "assistant", "content": "", "tool_calls": [call]}));
            }
        }
        "function_call_output" => {
            let output = match item.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => return Err(bad(format!("{at}.output: field required"))),
            };
            out.push(json!({
                "role": "tool",
                "tool_call_id": str_field(item, "call_id", &at)?,
                "content": output,
            }));
        }
        "reasoning" => {}
        other => return Err(bad(format!("{at}.type: unsupported item type {other:?}"))),
    }
    Ok(())
}

fn message_content(
    at: &str,
    content: Option<&Value>,
    images: &mut bool,
) -> Result<Value, HttpError> {
    let parts = match content {
        Some(Value::String(s)) => return Ok(json!(s)),
        Some(Value::Array(p)) => p,
        _ => return Err(bad(format!("{at}.content: field required"))),
    };
    let mut out = Vec::new();
    for p in parts {
        match p.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text" | "text") => {
                out.push(json!({"type": "text", "text": str_field(p, "text", at)?}));
            }
            Some("input_image") => {
                let url = match p.get("image_url") {
                    Some(Value::String(u)) => u.clone(),
                    Some(Value::Object(o)) => o
                        .get("url")
                        .and_then(Value::as_str)
                        .ok_or_else(|| bad(format!("{at}: input_image.image_url.url required")))?
                        .to_string(),
                    _ => {
                        return Err(bad(format!(
                            "{at}: input_image needs image_url (file_id is not supported)"
                        )));
                    }
                };
                *images = true;
                out.push(json!({"type": "image_url", "image_url": {"url": url}}));
            }
            other => {
                return Err(bad(format!(
                    "{at}.content: unsupported part type {}",
                    other.unwrap_or("(missing)")
                )));
            }
        }
    }
    if out
        .iter()
        .all(|p| p.get("type").and_then(Value::as_str) == Some("text"))
    {
        let text: Vec<&str> = out
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        return Ok(json!(text.join("\n")));
    }
    Ok(Value::Array(out))
}

fn translate_tools(
    req: &Map<String, Value>,
    params: &mut Map<String, Value>,
) -> Result<bool, HttpError> {
    let tools = match req.get("tools") {
        None | Some(Value::Null) => return Ok(false),
        Some(Value::Array(t)) => t,
        Some(_) => return Err(bad("tools: must be an array")),
    };
    let mut out = Vec::new();
    for (i, t) in tools.iter().enumerate() {
        match t.get("type").and_then(Value::as_str) {
            Some("function") => {}
            other => {
                return Err(bad(format!(
                    "tools.{i}.type: {} tools are not supported by local models",
                    other.unwrap_or("(missing)")
                )));
            }
        }
        let mut f = json!({
            "name": str_field(t, "name", &format!("tools.{i}"))?,
            "parameters": t.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"})),
        });
        if let Some(d) = t.get("description").and_then(Value::as_str) {
            f["description"] = json!(d);
        }
        if let Some(s) = t.get("strict").filter(|s| !s.is_null()) {
            f["strict"] = s.clone();
        }
        out.push(json!({"type": "function", "function": f}));
    }
    let declared = !out.is_empty();
    if declared {
        params.insert("tools".into(), Value::Array(out));
        if let Some(choice) = req.get("tool_choice").filter(|c| !c.is_null()) {
            let mapped = match choice {
                Value::String(s) if matches!(s.as_str(), "auto" | "none" | "required") => {
                    choice.clone()
                }
                Value::Object(o) if o.get("type").and_then(Value::as_str) == Some("function") => {
                    let name = o
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| bad("tool_choice.name: field required"))?;
                    json!({"type": "function", "function": {"name": name}})
                }
                _ => {
                    return Err(bad(
                        "tool_choice: expected \"auto\", \"none\", \"required\" or a function",
                    ));
                }
            };
            params.insert("tool_choice".into(), mapped);
        }
    }
    Ok(declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(v: Value) -> Result<Translated, HttpError> {
        translate(v.as_object().unwrap(), Vec::new())
    }

    #[test]
    fn string_input_and_parameters() {
        let t = tr(json!({"model": "m", "input": "hi", "instructions": "Be brief.",
            "max_output_tokens": 16, "temperature": 0.1,
            "text": {"format": {"type": "json_schema", "name": "x", "schema": {"type": "object"}, "strict": true}},
            "reasoning": {"effort": "low"}}))
        .unwrap();
        assert_eq!(
            t.conversation,
            vec![json!({"role": "user", "content": "hi"})]
        );
        assert_eq!(t.instructions.as_deref(), Some("Be brief."));
        assert_eq!(t.params["max_tokens"], 16);
        assert_eq!(t.params["response_format"]["json_schema"]["strict"], true);
        assert_eq!(t.params["chat_template_kwargs"]["reasoning_effort"], "low");
    }

    #[test]
    fn items_tools_and_outputs() {
        let t = tr(json!({"model": "m",
        "tools": [{"type": "function", "name": "f", "parameters": {"type": "object"}}],
        "tool_choice": {"type": "function", "name": "f"},
        "input": [
            {"role": "developer", "content": "rules"},
            {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "look"},
                {"type": "input_image", "image_url": "data:image/png;base64,iVBOR"}]},
            {"type": "function_call", "call_id": "c1", "name": "f", "arguments": "{}"},
            {"type": "function_call", "call_id": "c2", "name": "f", "arguments": "{\"a\":1}"},
            {"type": "function_call_output", "call_id": "c1", "output": "one"},
            {"type": "reasoning", "id": "rs_1", "summary": []},
        ]}))
        .unwrap();
        assert!(t.images && t.tools);
        let c = &t.conversation;
        assert_eq!(c[0], json!({"role": "system", "content": "rules"}));
        assert_eq!(
            c[1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,iVBOR"
        );
        assert_eq!(c[2]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(
            c[3],
            json!({"role": "tool", "tool_call_id": "c1", "content": "one"})
        );
        assert_eq!(c.len(), 4);
        assert_eq!(t.params["tool_choice"]["function"]["name"], "f");
    }

    #[test]
    fn invalid_requests() {
        assert!(tr(json!({"model": "m"})).is_err());
        assert!(
            tr(json!({"model": "m", "input": "x", "tools": [{"type": "web_search"}]})).is_err()
        );
        assert!(tr(json!({"model": "m", "input": [{"type": "file_search_call"}]})).is_err());
        assert!(tr(json!({"model": "m", "input": "x", "max_output_tokens": 0})).is_err());
    }
}
