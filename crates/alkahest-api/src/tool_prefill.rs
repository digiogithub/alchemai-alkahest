//! Forced tool calls for Hermes-style JSON templates (ALK-US-0096).
//!
//! llama-server b11669 builds the grammar of a forced tool call (`tool_choice: "required"`,
//! Anthropic `any` / `tool`) from the template's tool format, but its grammar for the
//! `<tool_call>{json}</tool_call>` family (Qwen 2.5, Hermes 2/3, MiMo) still allows free
//! content before the first `<tool_call>` marker (`content_before_tools`, the same parser as
//! `auto`). Small models such as `qwen2.5-coder:0.5b` then write the JSON without the marker,
//! never satisfy the grammar and run until `max_tokens` with no `tool_calls` parsed.
//!
//! The fix keeps llama-server's own template and parser and removes the free prefix: the
//! assistant turn is prefilled with the marker (`<tool_call>\n`), which llama-server feeds to
//! the grammar as accepted tokens, so generation starts inside the JSON tool call and the
//! parser returns real `tool_calls`. Parallel calls are disabled unless the client asked for
//! them (a forced call otherwise repeats until the token limit on small models). A named tool
//! choice narrows `tools` to that tool. Templates of other families are left untouched.

use std::sync::Arc;

use alkahest_engine::TemplateSource;
use alkahest_sched::Lease;
use alkahest_store::Catalog;
use serde_json::{Map, Value, json};

use crate::service::blocking;
use crate::state::AppState;

/// The per-call marker of Hermes-style JSON tool calls.
pub const HERMES_MARKER: &str = "<tool_call>";

/// Whether a chat template emits Hermes-style JSON tool calls (`<tool_call>` + JSON), as
/// opposed to the XML parameter form of Qwen3-Coder (`<function=...><parameter=...>`), which
/// llama-server parses with a dedicated parser.
pub fn is_hermes_json_template(template: &str) -> bool {
    template.contains(HERMES_MARKER)
        && !template.contains("<function=")
        && !template.contains("<parameter=")
}

/// What a request forces, read from its OpenAI-shaped body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forced {
    /// No forced call (`auto`, `none`, no tools).
    No,
    /// Any tool (`required`).
    Any,
    /// The named function.
    Named(String),
}

/// Reads `tools` / `tool_choice` of an OpenAI chat body.
pub fn forced_choice(body: &Map<String, Value>) -> Forced {
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|t| !t.is_empty());
    if !has_tools {
        return Forced::No;
    }
    match body.get("tool_choice") {
        Some(Value::String(s)) if s == "required" => Forced::Any,
        Some(Value::Object(o)) => o
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .map_or(Forced::Any, |n| Forced::Named(n.to_string())),
        _ => Forced::No,
    }
}

/// Rewrites `body` for a forced call when `template` is Hermes-style JSON; returns whether
/// it did. Leaves the body alone when the conversation already ends with an assistant turn
/// (the client's own prefill).
pub fn apply_with_template(body: &mut Map<String, Value>, template: &str) -> bool {
    let forced = forced_choice(body);
    if forced == Forced::No || !is_hermes_json_template(template) {
        return false;
    }
    let Some(Value::Array(messages)) = body.get_mut("messages") else {
        return false;
    };
    if messages
        .last()
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        == Some("assistant")
    {
        return false;
    }
    messages.push(json!({"role": "assistant", "content": format!("{HERMES_MARKER}\n")}));
    if let Forced::Named(name) = forced {
        if let Some(Value::Array(tools)) = body.get_mut("tools") {
            tools.retain(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    == Some(name.as_str())
            });
        }
        body.insert("tool_choice".into(), json!("required"));
    }
    body.entry("parallel_tool_calls")
        .or_insert_with(|| json!(false));
    true
}

/// [`apply_with_template`] with the template the engine actually uses for `model`: an inline
/// Jinja override from the model settings, else the GGUF's own `tokenizer.chat_template`
/// from the catalog. File and built-in overrides are left alone (no Hermes built-in exists).
pub async fn apply(
    state: &AppState,
    model: &str,
    lease: &Lease,
    body: &mut Map<String, Value>,
) -> bool {
    if forced_choice(body) == Forced::No {
        return false;
    }
    let template = match lease.settings().template.as_deref() {
        Some(t) if !t.trim().is_empty() => match TemplateSource::from_setting(t) {
            TemplateSource::OverrideInline(t) => Some(t),
            _ => None,
        },
        _ => gguf_template(state, model).await,
    };
    match template {
        Some(t) => apply_with_template(body, &t),
        None => false,
    }
}

/// The GGUF chat template of a catalog model, if any.
async fn gguf_template(state: &AppState, model: &str) -> Option<String> {
    let catalog = state.service::<Arc<Catalog>>()?;
    let model = model.to_string();
    blocking(move || {
        Ok(catalog
            .resolve(&model)
            .ok()
            .and_then(|e| e.info.gguf_chat_template.clone()))
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const QWEN25: &str = "{%- if tools %}<tools></tools> {{- '\\n<tool_call>\\n{\"name\": \"' }}";

    fn body(choice: Value) -> Map<String, Value> {
        json!({
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [
                {"type": "function", "function": {"name": "get_weather"}},
                {"type": "function", "function": {"name": "get_time"}}
            ],
            "tool_choice": choice,
        })
        .as_object()
        .cloned()
        .unwrap_or_default()
    }

    #[test]
    fn required_choice_prefills_the_marker() {
        let mut b = body(json!("required"));
        assert!(apply_with_template(&mut b, QWEN25));
        let last = b["messages"].as_array().and_then(|m| m.last()).cloned();
        assert_eq!(
            last,
            Some(json!({"role": "assistant", "content": "<tool_call>\n"}))
        );
        assert_eq!(b["parallel_tool_calls"], json!(false));
        assert_eq!(b["tools"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn named_choice_narrows_the_tools() {
        let mut b = body(json!({"type": "function", "function": {"name": "get_time"}}));
        b.insert("parallel_tool_calls".into(), json!(true));
        assert!(apply_with_template(&mut b, QWEN25));
        assert_eq!(b["tool_choice"], "required");
        assert_eq!(b["tools"].as_array().map(Vec::len), Some(1));
        assert_eq!(b["tools"][0]["function"]["name"], "get_time");
        // The client's explicit choice wins.
        assert_eq!(b["parallel_tool_calls"], json!(true));
    }

    #[test]
    fn other_cases_are_left_alone() {
        let mut b = body(json!("auto"));
        assert!(!apply_with_template(&mut b, QWEN25));
        let mut b = body(json!("required"));
        assert!(!apply_with_template(&mut b, "{{ messages }} [TOOL_CALLS]"));
        // Qwen3-Coder XML tool calls have their own llama.cpp parser.
        let mut b = body(json!("required"));
        assert!(!apply_with_template(
            &mut b,
            "<tool_call>\n<function=x>\n<parameter=y>"
        ));
        let mut b = body(json!("required"));
        if let Some(Value::Array(m)) = b.get_mut("messages") {
            m.push(json!({"role": "assistant", "content": "Sure"}));
        }
        assert!(!apply_with_template(&mut b, QWEN25));
    }
}
