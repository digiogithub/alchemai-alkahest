//! `/v1/decisions`: the OpenAI Decisions wire shape LocalAI serves, translated to and from a
//! System One request (ALK-SP-0007.R5).
//!
//! The request has `input` (text or user messages with `input_text` / `input_image` parts)
//! and an ordered list of `predicate`, `choice` and `score` questions; the response lists
//! answers in question order with `probability`, `choice` (the original string or boolean
//! value), `score`, `confidence` and `probabilities`. Question names and choice values
//! become the System One ids the model reads, so the prompt says `billing`, not an opaque
//! id; questions without a name get `q1`, `q2`, ...

use std::collections::HashSet;

use alkahest_decision::{
    Answer, Json, Question, QuestionType, SystemOneRequest, SystemOneResponse,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::dialect::ErrorKind;
use crate::service::HttpError;

/// Body of `POST /v1/decisions`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DecisionsRequest {
    /// Model name.
    #[serde(default)]
    pub model: String,
    /// Text, or a list of user messages.
    #[serde(default)]
    pub input: Option<Value>,
    /// Questions, answered in this order.
    #[serde(default)]
    pub questions: Vec<DecisionQuestion>,
    /// OpenAI end-user identifier; accepted and ignored.
    #[serde(default)]
    pub safety_identifier: Option<String>,
    /// Alkahest / Ollama extension: how long the model stays loaded.
    #[serde(default)]
    pub keep_alive: Option<Value>,
}

/// One question of [`DecisionsRequest`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DecisionQuestion {
    /// `predicate`, `choice` or `score`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Optional name, echoed in the answer.
    #[serde(default)]
    pub name: Option<String>,
    /// The question text (required).
    #[serde(default)]
    pub instructions: Option<String>,
    /// Options of a `choice` question.
    #[serde(default)]
    pub choices: Option<Vec<DecisionChoice>>,
    /// Levels of a `score` question, lowest first.
    #[serde(default)]
    pub levels: Option<Vec<DecisionLevel>>,
}

/// One option of a `choice` question.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DecisionChoice {
    /// A string or a boolean.
    #[serde(default)]
    pub value: Value,
    /// Optional description shown to the model.
    #[serde(default)]
    pub description: String,
}

/// One level of a `score` question.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DecisionLevel {
    /// Level label (required).
    #[serde(default)]
    pub label: Option<String>,
    /// Optional description shown to the model.
    #[serde(default)]
    pub description: String,
}

/// The System One ids a converted request uses: per question, its id and (for choices)
/// the criteria key of each option in request order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionIds(pub Vec<(String, Vec<String>)>);

fn bad(msg: impl Into<String>) -> HttpError {
    HttpError::bad_request(msg)
}

/// Texts and images of `input`.
fn read_input(input: Option<&Value>) -> Result<(Vec<String>, Vec<String>), HttpError> {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    match input {
        Some(Value::String(s)) => texts.push(s.clone()),
        Some(Value::Array(messages)) if !messages.is_empty() => {
            for m in messages {
                let role = m.get("role").and_then(Value::as_str);
                let kind = m.get("type").and_then(Value::as_str).unwrap_or("message");
                if role != Some("user") || kind != "message" {
                    return Err(bad("input supports only user messages"));
                }
                match m.get("content") {
                    Some(Value::String(s)) => texts.push(s.clone()),
                    Some(Value::Array(parts)) if !parts.is_empty() => {
                        for p in parts {
                            read_part(p, &mut texts, &mut images)?;
                        }
                    }
                    _ => return Err(bad("message content must be text or input parts")),
                }
            }
        }
        _ => return Err(bad("input must be text or user messages")),
    }
    Ok((texts, images))
}

fn read_part(
    p: &Value,
    texts: &mut Vec<String>,
    images: &mut Vec<String>,
) -> Result<(), HttpError> {
    match p.get("type").and_then(Value::as_str) {
        Some("input_text") => {
            let text = p
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("input_text requires text"))?;
            texts.push(text.to_owned());
        }
        Some("input_image") => {
            if let Some(d) = p.get("detail").and_then(Value::as_str)
                && d != "auto"
            {
                return Err(bad("only auto image detail is supported"));
            }
            let url = p
                .get("image_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if url.starts_with("http://") || url.starts_with("https://") {
                return Err(bad(
                    "image URLs are not fetched; send the image as a data: URL",
                ));
            }
            if url.is_empty() {
                return Err(bad("input_image requires image_url"));
            }
            images.push(url.to_owned());
        }
        other => {
            return Err(bad(format!(
                "unsupported input part {:?}",
                other.unwrap_or_default()
            )));
        }
    }
    Ok(())
}

/// The criteria key of a choice value: the string itself, or `true` / `false`.
fn choice_key(v: &Value) -> Result<String, HttpError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(bad("choice value must be string or boolean")),
    }
}

/// Converts a `/v1/decisions` request into a System One request plus the ids it used.
pub fn from_decisions_request(
    req: &DecisionsRequest,
) -> Result<(SystemOneRequest, QuestionIds), HttpError> {
    if req.model.trim().is_empty() {
        return Err(bad("you must provide a model parameter"));
    }
    let (texts, images) = read_input(req.input.as_ref())?;
    let text = texts.join("\n");
    // Image-only evidence must not become a blank text state, which the compiler refuses.
    let state = if !images.is_empty() && text.trim().is_empty() {
        Json::Object(Vec::new())
    } else {
        Json::String(text)
    };

    let mut names = HashSet::new();
    for q in &req.questions {
        if let Some(n) = q.name.as_deref().filter(|n| !n.is_empty())
            && !names.insert(n.to_owned())
        {
            return Err(bad(format!("duplicate question name {n:?}")));
        }
    }
    let mut out = SystemOneRequest {
        model: req.model.clone(),
        state: Some(state),
        images,
        keep_alive: req.keep_alive.clone(),
        ..SystemOneRequest::default()
    };
    let mut ids = QuestionIds::default();
    for (i, q) in req.questions.iter().enumerate() {
        let id = match q.name.as_deref().filter(|n| !n.is_empty()) {
            Some(n) => n.to_owned(),
            None => {
                let mut n = i + 1;
                while names.contains(&format!("q{n}")) {
                    n += 1000;
                }
                let id = format!("q{n}");
                names.insert(id.clone());
                id
            }
        };
        let instructions = q
            .instructions
            .clone()
            .ok_or_else(|| bad("question instructions are required"))?;
        let (question, keys) = match q.kind.as_str() {
            "predicate" => {
                if q.choices.is_some() || q.levels.is_some() {
                    return Err(bad("predicate does not support choices or levels"));
                }
                (Question::new(QuestionType::Noul, instructions), Vec::new())
            }
            "choice" => {
                if q.levels.is_some() {
                    return Err(bad("choice does not support levels"));
                }
                let mut keys: Vec<String> = Vec::new();
                let mut criteria = Vec::new();
                for c in q.choices.as_deref().unwrap_or_default() {
                    let key = choice_key(&c.value)?;
                    if keys.contains(&key) {
                        return Err(bad("duplicate choice value"));
                    }
                    let desc = if c.description.is_empty() {
                        Json::Null
                    } else {
                        Json::String(c.description.clone())
                    };
                    criteria.push((key.clone(), desc));
                    keys.push(key);
                }
                let question = Question::new(QuestionType::Choice, instructions)
                    .with_criteria(Json::Object(criteria));
                (question, keys)
            }
            "score" => {
                if q.choices.is_some() {
                    return Err(bad("score does not support choices"));
                }
                let mut levels = Vec::new();
                for l in q.levels.as_deref().unwrap_or_default() {
                    let label = l
                        .label
                        .clone()
                        .ok_or_else(|| bad("score level requires label"))?;
                    levels.push(Json::String(if l.description.is_empty() {
                        label
                    } else {
                        format!("{label}: {}", l.description)
                    }));
                }
                let question = Question::new(QuestionType::Score, instructions)
                    .with_criteria(Json::Array(levels));
                (question, Vec::new())
            }
            other => return Err(bad(format!("unsupported question type {other:?}"))),
        };
        out.questions.insert(id.clone(), question);
        ids.0.push((id, keys));
    }
    Ok((out, ids))
}

fn invalid_answer() -> HttpError {
    HttpError::new(ErrorKind::BadGateway, "invalid decision backend response")
}

/// Converts a System One response back into the `/v1/decisions` shape.
pub fn to_decisions_response(
    req: &DecisionsRequest,
    ids: &QuestionIds,
    resp: &SystemOneResponse,
) -> Result<Value, HttpError> {
    let mut answers = Vec::with_capacity(req.questions.len());
    for (q, (id, keys)) in req.questions.iter().zip(&ids.0) {
        let answer = resp.answers.get(id).ok_or_else(invalid_answer)?;
        let mut out = Map::new();
        out.insert("type".into(), json!(q.kind));
        if let Some(n) = &q.name {
            out.insert("name".into(), json!(n));
        }
        match (q.kind.as_str(), answer) {
            ("predicate", Answer::Noul { noul }) => {
                out.insert("probability".into(), json!(noul));
            }
            (
                "choice",
                Answer::Choice {
                    choice,
                    probabilities,
                    confidence,
                },
            ) => {
                let choices = q.choices.as_deref().unwrap_or_default();
                let mut probs = Vec::with_capacity(keys.len());
                for (key, c) in keys.iter().zip(choices) {
                    let p = probabilities
                        .iter()
                        .find_map(|(k, p)| (k == key).then_some(*p))
                        .ok_or_else(invalid_answer)?;
                    probs.push(json!({"value": c.value, "probability": p}));
                    if key == choice {
                        out.insert("choice".into(), c.value.clone());
                    }
                }
                if !out.contains_key("choice") {
                    return Err(invalid_answer());
                }
                out.insert("confidence".into(), json!(confidence));
                out.insert("probabilities".into(), Value::Array(probs));
            }
            (
                "score",
                Answer::Score {
                    score,
                    probabilities,
                    confidence,
                    ..
                },
            ) => {
                let levels = q.levels.as_deref().unwrap_or_default();
                let mut probs = Vec::with_capacity(levels.len());
                for (j, l) in levels.iter().enumerate() {
                    let key = j.to_string();
                    let p = probabilities
                        .iter()
                        .find_map(|(k, p)| (*k == key).then_some(*p))
                        .ok_or_else(invalid_answer)?;
                    probs.push(json!({"value": j, "label": l.label, "probability": p}));
                }
                out.insert("score".into(), json!(score));
                out.insert("confidence".into(), json!(confidence));
                out.insert("probabilities".into(), Value::Array(probs));
            }
            _ => return Err(invalid_answer()),
        }
        answers.push(Value::Object(out));
    }
    let (input, output) = (resp.usage.input_tokens, resp.usage.output_tokens);
    Ok(json!({
        "model": req.model,
        "answers": answers,
        "usage": {
            "input_tokens": input,
            "output_tokens": output,
            "total_tokens": input + output,
            "input_tokens_details": {},
            "output_tokens_details": {},
        },
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn req(v: Value) -> DecisionsRequest {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn converts_questions_with_names_and_values_as_ids() {
        let r = req(json!({
            "model": "tev1:0.8b",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "charged twice"}]}],
            "questions": [
                {"type": "predicate", "name": "refund", "instructions": "Refund?"},
                {"type": "choice", "instructions": "Team?", "choices": [
                    {"value": "billing", "description": "money"}, {"value": "shipping"}]},
                {"type": "score", "instructions": "Urgency?", "levels": [
                    {"label": "low"}, {"label": "high", "description": "today"}]},
            ]
        }));
        let (s, ids) = from_decisions_request(&r).unwrap();
        assert_eq!(s.state, Some(Json::String("charged twice".into())));
        let keys: Vec<&str> = s.questions.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["refund", "q2", "q3"]);
        assert_eq!(ids.0[1].1, ["billing", "shipping"]);
        let text = serde_json::to_string(&s.questions).unwrap();
        assert!(
            text.contains(r#""criteria":{"billing":"money","shipping":null}"#),
            "{text}"
        );
        assert!(
            text.contains(r#""criteria":["low","high: today"]"#),
            "{text}"
        );
        assert!(text.contains(r#""refund":{"type":"noul""#), "{text}");
    }

    #[test]
    fn rejects_invalid_requests() {
        for (body, msg) in [
            (json!({"model": "", "input": "x", "questions": []}), "model"),
            (
                json!({"model": "m", "input": 3, "questions": []}),
                "input must be",
            ),
            (
                json!({"model": "m", "input": [{"role": "system", "content": "x"}]}),
                "only user messages",
            ),
            (
                json!({"model": "m", "input": "x", "questions": [{"type": "predicate"}]}),
                "instructions are required",
            ),
            (
                json!({"model": "m", "input": "x", "questions": [{"type": "nope", "instructions": "i"}]}),
                "unsupported question type",
            ),
            (
                json!({"model": "m", "input": "x", "questions": [{"type": "choice", "instructions": "i", "choices": [{"value": 1}]}]}),
                "string or boolean",
            ),
            (
                json!({"model": "m", "input": "x", "questions": [{"type": "choice", "instructions": "i", "choices": [{"value": "a"}, {"value": "a"}]}]}),
                "duplicate choice",
            ),
            (
                json!({"model": "m", "input": "x", "questions": [
                    {"type": "predicate", "name": "a", "instructions": "i"},
                    {"type": "predicate", "name": "a", "instructions": "i"}]}),
                "duplicate question name",
            ),
            (
                json!({"model": "m", "input": [{"role": "user", "content": [{"type": "input_image", "image_url": "https://x/y.png"}]}]}),
                "data: URL",
            ),
        ] {
            let e = from_decisions_request(&req(body.clone())).unwrap_err();
            assert_eq!(e.kind, ErrorKind::BadRequest, "{body}");
            assert!(e.message.contains(msg), "{body}: {}", e.message);
        }
    }

    #[test]
    fn image_only_input_gets_an_object_state() {
        let r = req(json!({"model": "m", "input": [{"role": "user", "content": [
            {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}]}],
            "questions": [{"type": "predicate", "instructions": "cat?"}]}));
        let (s, _) = from_decisions_request(&r).unwrap();
        assert_eq!(s.state, Some(Json::Object(vec![])));
        assert_eq!(s.images, ["data:image/png;base64,AAAA"]);
    }

    #[test]
    fn converts_answers_back_in_question_order() {
        let r = req(json!({
            "model": "m", "input": "x",
            "questions": [
                {"type": "predicate", "name": "refund", "instructions": "Refund?"},
                {"type": "choice", "instructions": "Agree?", "choices": [{"value": true}, {"value": false}]},
                {"type": "score", "instructions": "Urgency?", "levels": [{"label": "low"}, {"label": "high"}]},
            ]
        }));
        let (_, ids) = from_decisions_request(&r).unwrap();
        let resp: SystemOneResponse = serde_json::from_value(json!({
            "model": "m",
            "answers": {
                "q3": {"type": "score", "score": 0.25, "legend": {"0": "low", "1": "high"},
                       "probabilities": {"0": 0.75, "1": 0.25}, "confidence": 0.2},
                "refund": {"type": "noul", "noul": 0.9},
                "q2": {"type": "choice", "choice": "false", "probabilities": {"true": 0.4, "false": 0.6}, "confidence": 0.1},
            },
            "usage": {"input_tokens": 10, "output_tokens": 3}
        }))
        .unwrap();
        let out = to_decisions_response(&r, &ids, &resp).unwrap();
        assert_eq!(
            out["answers"][0],
            json!({"type": "predicate", "name": "refund", "probability": 0.9})
        );
        assert_eq!(out["answers"][1]["choice"], json!(false));
        assert_eq!(
            out["answers"][1]["probabilities"][1],
            json!({"value": false, "probability": 0.6})
        );
        assert_eq!(out["answers"][2]["score"], json!(0.25));
        assert_eq!(
            out["answers"][2]["probabilities"][0],
            json!({"value": 0, "label": "low", "probability": 0.75})
        );
        assert_eq!(out["usage"]["total_tokens"], json!(13));
    }

    #[test]
    fn missing_or_mismatched_answers_are_bad_gateway() {
        let r = req(json!({"model": "m", "input": "x",
            "questions": [{"type": "predicate", "instructions": "?"}]}));
        let (_, ids) = from_decisions_request(&r).unwrap();
        let resp: SystemOneResponse = serde_json::from_value(json!({
            "model": "m", "answers": {"q1": {"type": "choice", "choice": "a",
            "probabilities": {"a": 1.0}, "confidence": 1.0}}}))
        .unwrap();
        let e = to_decisions_response(&r, &ids, &resp).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadGateway);
    }
}
