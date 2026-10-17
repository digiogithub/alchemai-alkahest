//! The decision request compiler (spec ALK-SP-0007.R2, R4): validates a System One request
//! and turns it into scoring input for one encoding, then turns candidate scores back into
//! answers.
//!
//! The encodings reproduce the prompt formats the decision models were trained on, as
//! documented by the publishers and implemented by Ollama's `decision` package (MIT); see
//! `docs/research/decision-encodings.md`.

use serde::Serialize;

use crate::encoding::Encoding;
use crate::error::{DecisionError, Result};
use crate::json::{Json, kev_text, python_json, python_json_compact_sorted};
use crate::types::{
    Answer, Answers, Question, QuestionType, SystemOneRequest, SystemOneResponse, Usage,
};

/// Most questions one request may carry.
pub const MAX_QUESTIONS: usize = 64;
/// Fewest levels of a score question.
pub const MIN_SCORE_LEVELS: usize = 2;
/// Most levels of a score question.
pub const MAX_SCORE_LEVELS: usize = 10;
/// Most candidates of a letter-coded question (A to Z).
pub const MAX_LETTER_CANDIDATES: usize = 26;
/// Most candidates Tev1 was trained with.
pub const MAX_TEV1_CANDIDATES: usize = 24;
/// Most options of a Kev or Strands question.
pub const MAX_HEAD_OPTIONS: usize = 255;

/// The value an option stands for in the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceValue {
    /// `false` / `true` of a noul question.
    Bool(bool),
    /// A choice key or a score level index (`"0"`, `"1"`, ...).
    Key(String),
}

impl ChoiceValue {
    /// The text form (`false`, `true`, the key).
    pub fn text(&self) -> String {
        match self {
            ChoiceValue::Bool(b) => b.to_string(),
            ChoiceValue::Key(k) => k.clone(),
        }
    }

    fn to_json(&self) -> Json {
        match self {
            ChoiceValue::Bool(b) => Json::Bool(*b),
            ChoiceValue::Key(k) => Json::String(k.clone()),
        }
    }
}

/// One allowed answer of a compiled question.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// The letter the model answers with (`A`, `B`, ...); empty for head-based encodings.
    pub code: String,
    /// What the option means in the answer.
    pub value: ChoiceValue,
    /// The description the request gave (string, any JSON value for clef), `None` when it
    /// gave none.
    pub description: Option<Json>,
}

/// A question as the compiler resolved it.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledField {
    /// Question id.
    pub name: String,
    /// Question type.
    pub kind: QuestionType,
    /// Rendered instructions.
    pub description: String,
    /// Allowed answers, request order.
    pub choices: Vec<Choice>,
    /// Option texts for model-native heads.
    pub options: Vec<String>,
    /// Model option index to request option index, when the encoding reorders options.
    order: Vec<usize>,
}

/// A chat message of a scoring prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    /// `system` or `user`.
    pub role: String,
    /// Message text.
    pub content: String,
}

impl ChatMessage {
    fn user(content: String) -> Self {
        Self {
            role: "user".into(),
            content,
        }
    }
}

/// A question's schema as a model-native head reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RowQuestion {
    /// `noul`, `choice` or `score`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Rendered instructions.
    pub instructions: String,
    /// Option texts.
    pub options: Vec<String>,
}

/// One letter-scored prompt: the chat messages (before the model's system prompt and chat
/// template) and the single-letter candidates whose next-token probabilities answer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRow {
    /// The user turn (the system prompt is added when rendering).
    pub messages: Vec<ChatMessage>,
    /// Candidate continuations, one ordinary token each (`A`, `B`, ...).
    pub candidates: Vec<String>,
    /// The schema, for heads that read it.
    pub question: RowQuestion,
}

/// A clef question: segment index ranges of its instruction and of each option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentField {
    /// 0 noul, 1 choice, 2 score.
    #[serde(rename = "type")]
    pub kind: u8,
    /// `[start, end)` segment indices of the instruction.
    pub question: [usize; 2],
    /// `[start, end)` segment indices of each option, model order.
    pub options: Vec<[usize; 2]>,
}

/// A Strands question: an independent prompt with option byte spans for a pointer head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PointerRow {
    /// The `<state>` block shared by every row.
    pub prefix: String,
    /// The `<question>` block.
    pub prompt: String,
    /// 0 noul, 1 choice, 2 score.
    #[serde(rename = "type")]
    pub kind: u8,
    /// `[start, end)` byte spans of each option in `prompt`.
    pub options: Vec<[usize; 2]>,
}

/// What the scorer receives, by encoding family.
#[derive(Debug, Clone, PartialEq)]
pub enum ScoreInput {
    /// Generic and Tev1: one prompt per question, scored by candidate letters.
    Prompts(Vec<PromptRow>),
    /// Kev: the shared state text and each question's schema.
    Kev {
        /// Rendered state.
        state: String,
        /// One schema per question.
        questions: Vec<RowQuestion>,
    },
    /// Clef: one joint prompt in segments.
    Clef {
        /// Prompt segments; their concatenation is the prompt.
        segments: Vec<String>,
        /// One entry per question.
        fields: Vec<SegmentField>,
        /// Segment boundary where images go.
        image_position: usize,
    },
    /// Strands: one pointer row per question.
    Strands(Vec<PointerRow>),
}

/// A compiled request: the scoring input and what is needed to read the scores back.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    /// The encoding used.
    pub encoding: Encoding,
    /// Questions in request order.
    pub fields: Vec<CompiledField>,
    /// Scorer input.
    pub input: ScoreInput,
    /// Images of the request, unchanged.
    pub images: Vec<String>,
}

/// Candidate scores for every question, from a scorer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScoreOutput {
    /// Per question, one logit (or log-probability: a shared offset does not change the
    /// softmax) per candidate, in model option order.
    pub logits: Vec<Vec<f64>>,
    /// Prompt tokens of all questions.
    pub input_tokens: u64,
    /// Tokens generated to read the distributions.
    pub output_tokens: u64,
    /// Prompt tokens restored from cache, when the engine reports it.
    pub cached_tokens: Option<u64>,
}

/// Checks the limits every encoding shares (spec ALK-SP-0007.R2): 1 to 64 questions, no
/// videos, a non-empty state unless images carry it, and 2 to 10 score levels.
pub fn validate_limits(req: &SystemOneRequest) -> Result<()> {
    validate_questions(req)?;
    if !req.videos.is_empty() {
        return Err(DecisionError::invalid("video inputs are not supported"));
    }
    if let Some(Ok(state)) = req.state.as_ref().map(content)
        && state.trim().is_empty()
        && req.images.is_empty()
    {
        return Err(DecisionError::invalid("state must not be empty"));
    }
    Ok(())
}

fn validate_questions(req: &SystemOneRequest) -> Result<()> {
    let n = req.questions.len();
    if !(1..=MAX_QUESTIONS).contains(&n) {
        return Err(DecisionError::invalid(format!(
            "questions must contain 1–{MAX_QUESTIONS} fields, got {n}"
        )));
    }
    for (name, q) in req.questions.iter() {
        if q.kind != QuestionType::Score {
            continue;
        }
        let levels = q.criteria.as_ref().and_then(Json::as_array).map(<[_]>::len);
        if !levels.is_some_and(|l| (MIN_SCORE_LEVELS..=MAX_SCORE_LEVELS).contains(&l)) {
            return Err(DecisionError::invalid(format!(
                "question {name:?}: score criteria must be an array of \
                 {MIN_SCORE_LEVELS}–{MAX_SCORE_LEVELS} levels"
            )));
        }
    }
    Ok(())
}

/// Validates `req` and compiles it with `encoding`.
pub fn compile(req: &SystemOneRequest, encoding: Encoding) -> Result<Compiled> {
    if req.model.trim().is_empty() {
        return Err(DecisionError::invalid("model is required"));
    }
    if encoding == Encoding::Kev {
        return compile_kev(req);
    }
    validate_limits(req)?;
    if !req.images.is_empty() && encoding != Encoding::Clef {
        return Err(DecisionError::Unsupported(
            "image inputs are not supported by this decision model".into(),
        ));
    }
    if let Some(i) = req.images.iter().position(String::is_empty) {
        return Err(DecisionError::invalid(format!(
            "image {i} must not be empty"
        )));
    }
    let state = req
        .state
        .as_ref()
        .ok_or_else(|| DecisionError::invalid("state: must be a string, object, or array"))?;
    let context = if encoding == Encoding::Clef {
        clef_content(state)
    } else {
        content(state)
    }
    .map_err(|e| DecisionError::invalid(format!("state: {e}")))?;
    if context.trim().is_empty() && req.images.is_empty() {
        return Err(DecisionError::invalid("state must not be empty"));
    }
    match encoding {
        Encoding::Clef => compile_clef(req, context),
        Encoding::Strands => compile_strands(req),
        _ => compile_letters(req, state, &context, encoding),
    }
}

fn question_error(name: &str, e: impl std::fmt::Display) -> DecisionError {
    DecisionError::invalid(format!("question {name:?}: {e}"))
}

/// Instructions and states as the decoder encodings show them: strings verbatim, objects and
/// arrays as Python-style JSON.
fn content(v: &Json) -> std::result::Result<String, String> {
    match v {
        Json::String(s) => Ok(s.clone()),
        Json::Object(_) | Json::Array(_) => python_json(v, 0).map_err(|e| e.to_string()),
        _ => Err("must be a string, object, or array".into()),
    }
}

/// Clef shows strings verbatim and every other value as compact JSON with sorted keys.
fn clef_content(v: &Json) -> std::result::Result<String, String> {
    match v {
        Json::String(s) => Ok(s.clone()),
        other => python_json_compact_sorted(other).map_err(|e| e.to_string()),
    }
}

fn letter(i: usize) -> String {
    // Callers cap candidates at 26, so this stays within A..=Z.
    char::from(b'A' + (i % 26) as u8).to_string()
}

/// Resolves a question for the letter-scored encodings (generic and Tev1).
fn letter_field(name: &str, q: &Question) -> std::result::Result<CompiledField, String> {
    if name.trim().is_empty() {
        return Err("field name must not be empty".into());
    }
    let description = q
        .instructions
        .as_ref()
        .and_then(|i| content(i).ok())
        .filter(|d| !d.trim().is_empty())
        .ok_or("instructions must be a nonempty string, object, or array")?;
    let mut f = CompiledField {
        name: name.into(),
        kind: q.kind.clone(),
        description,
        choices: Vec::new(),
        options: Vec::new(),
        order: Vec::new(),
    };
    let add = |f: &mut CompiledField, value: ChoiceValue, description: Option<String>| {
        let code = letter(f.choices.len());
        f.choices.push(Choice {
            code,
            value,
            description: description.map(Json::String),
        });
    };
    match &q.kind {
        QuestionType::Noul => {
            let (mut no, mut yes) = (None, None);
            if let Some(c) = &q.criteria {
                let members = c
                    .as_object()
                    .ok_or("noul criteria must be an object of true/false descriptions")?;
                for (key, value) in members {
                    let text = value.as_str().ok_or("noul descriptions must be strings")?;
                    match key.as_str() {
                        "false" => no = Some(text.to_owned()),
                        "true" => yes = Some(text.to_owned()),
                        other => return Err(format!("unknown noul criterion {other:?}")),
                    }
                }
            }
            f.options = vec![
                no.as_deref().filter(|d| !d.is_empty()).map_or_else(
                    || "false: no, the statement does not hold".into(),
                    |d| format!("false: {d}"),
                ),
                yes.as_deref().filter(|d| !d.is_empty()).map_or_else(
                    || "true: yes, the statement holds".into(),
                    |d| format!("true: {d}"),
                ),
            ];
            add(&mut f, ChoiceValue::Bool(false), no);
            add(&mut f, ChoiceValue::Bool(true), yes);
        }
        QuestionType::Choice => {
            let members = q
                .criteria
                .as_ref()
                .and_then(Json::as_object)
                .ok_or("choice criteria must map option keys to descriptions or null")?;
            for (key, value) in members {
                if key.trim().is_empty() {
                    return Err("choice keys must not be empty".into());
                }
                let description = match value {
                    Json::Null => None,
                    Json::String(s) => Some(s.clone()),
                    _ => {
                        return Err(
                            "choice criteria must map option keys to descriptions or null".into(),
                        );
                    }
                };
                f.options.push(match description.as_deref() {
                    Some(d) if !d.is_empty() => format!("{key}: {d}"),
                    _ => key.clone(),
                });
                add(&mut f, ChoiceValue::Key(key.clone()), description);
            }
        }
        QuestionType::Score => {
            let levels = q
                .criteria
                .as_ref()
                .and_then(Json::as_array)
                .ok_or("score criteria must be an array of descriptions")?;
            for (i, level) in levels.iter().enumerate() {
                let text = level.as_str().ok_or("score descriptions must be strings")?;
                f.options.push(format!("level {i}: {text}"));
                add(
                    &mut f,
                    ChoiceValue::Key(i.to_string()),
                    Some(text.to_owned()),
                );
            }
        }
        QuestionType::Other(_) => return Err("type must be choice, noul, or score".into()),
    }
    if !(2..=MAX_LETTER_CANDIDATES).contains(&f.choices.len()) {
        return Err(format!(
            "criteria must contain 2–{MAX_LETTER_CANDIDATES} candidates"
        ));
    }
    Ok(f)
}

fn row_question(f: &CompiledField) -> RowQuestion {
    RowQuestion {
        kind: f.kind.to_string(),
        instructions: f.description.clone(),
        options: f.options.clone(),
    }
}

fn compile_letters(
    req: &SystemOneRequest,
    state: &Json,
    context: &str,
    encoding: Encoding,
) -> Result<Compiled> {
    let mut fields = Vec::with_capacity(req.questions.len());
    for (name, q) in req.questions.iter() {
        fields.push(letter_field(name, q).map_err(|e| question_error(name, e))?);
    }
    let prompts = if encoding == Encoding::Tev1 {
        tev1_prompts(state, &fields)?
    } else {
        generic_prompts(context, &fields)?
    };
    let rows = fields
        .iter()
        .zip(prompts)
        .map(|(f, prompt)| PromptRow {
            messages: vec![ChatMessage::user(prompt)],
            candidates: f.choices.iter().map(|c| c.code.clone()).collect(),
            question: row_question(f),
        })
        .collect();
    Ok(Compiled {
        encoding,
        fields,
        input: ScoreInput::Prompts(rows),
        images: req.images.clone(),
    })
}

fn render_err(e: impl std::fmt::Display) -> DecisionError {
    DecisionError::invalid(e.to_string())
}

/// Generic: every prompt carries the context and the whole schema, then names the field to
/// answer. Chat delimiters are escaped so state text cannot close the user turn.
fn generic_prompts(context: &str, fields: &[CompiledField]) -> Result<Vec<String>> {
    let schema = fields
        .iter()
        .map(|f| {
            let choices = f
                .choices
                .iter()
                .map(|c| {
                    let mut m = vec![
                        ("code".to_owned(), Json::String(c.code.clone())),
                        ("value".to_owned(), c.value.to_json()),
                    ];
                    if let Some(d) = &c.description {
                        m.push(("description".to_owned(), d.clone()));
                    }
                    Json::Object(m)
                })
                .collect();
            Json::Object(vec![
                ("name".into(), Json::String(f.name.clone())),
                ("description".into(), Json::String(f.description.clone())),
                ("choices".into(), Json::Array(choices)),
            ])
        })
        .collect();
    let data = Json::Object(vec![
        ("context".into(), Json::String(context.to_owned())),
        ("schema".into(), Json::Array(schema)),
    ]);
    let escape = |s: &str| s.replace('<', "\\u003c").replace('>', "\\u003e");
    let data = escape(&python_json(&data, 0).map_err(render_err)?);
    fields
        .iter()
        .map(|f| {
            let name = python_json(&Json::String(f.name.clone()), 0).map_err(render_err)?;
            Ok(format!("{data}\n\nRequested field: {}", escape(&name)))
        })
        .collect()
}

/// Tev1: one `{state, question, options}` payload per question; object states stay nested
/// and every option has a description (noul `No` / `Yes`, bare choices their key).
fn tev1_prompts(state: &Json, fields: &[CompiledField]) -> Result<Vec<String>> {
    fields
        .iter()
        .map(|f| {
            if f.choices.len() > MAX_TEV1_CANDIDATES {
                return Err(question_error(
                    &f.name,
                    format!("Tev1 supports at most {MAX_TEV1_CANDIDATES} candidates"),
                ));
            }
            let mut options = Vec::with_capacity(f.choices.len());
            for c in &f.choices {
                let description = match (&c.description, &c.value) {
                    (Some(Json::String(d)), _) => d.clone(),
                    (None, ChoiceValue::Bool(false)) => "No".into(),
                    (None, ChoiceValue::Bool(true)) => "Yes".into(),
                    (None, ChoiceValue::Key(k)) => k.clone(),
                    (Some(_), _) => String::new(),
                };
                if description.trim().is_empty() {
                    return Err(question_error(
                        &f.name,
                        "Tev1 requires nonempty candidate descriptions",
                    ));
                }
                options.push(Json::Object(vec![
                    ("label".into(), Json::String(c.code.clone())),
                    ("key".into(), Json::String(c.value.text())),
                    ("description".into(), Json::String(description)),
                ]));
            }
            let payload = Json::Object(vec![
                ("state".into(), state.clone()),
                ("question".into(), Json::String(f.description.clone())),
                ("options".into(), Json::Array(options)),
            ]);
            python_json(&payload, 0).map_err(render_err)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Kev

fn compile_kev(req: &SystemOneRequest) -> Result<Compiled> {
    validate_questions(req)?;
    if !req.images.is_empty() || !req.videos.is_empty() {
        return Err(DecisionError::Unsupported(
            "Kev accepts text state only".into(),
        ));
    }
    let state = req
        .state
        .as_ref()
        .ok_or_else(|| DecisionError::invalid("state must be a JSON value"))?;
    let state = kev_text(state).map_err(|e| DecisionError::invalid(format!("state: {e}")))?;
    let mut fields = Vec::with_capacity(req.questions.len());
    for (name, q) in req.questions.iter() {
        fields.push(kev_field(name, q).map_err(|e| question_error(name, e))?);
    }
    let questions = fields.iter().map(row_question).collect();
    Ok(Compiled {
        encoding: Encoding::Kev,
        fields,
        input: ScoreInput::Kev { state, questions },
        images: Vec::new(),
    })
}

fn kev_field(name: &str, q: &Question) -> std::result::Result<CompiledField, String> {
    if name.trim().is_empty() {
        return Err("field name must not be empty".into());
    }
    let text = |v: &Json| kev_text(v).map_err(|e| e.to_string());
    let description = q
        .instructions
        .as_ref()
        .map(text)
        .transpose()?
        .unwrap_or_default();
    let mut f = CompiledField {
        name: name.into(),
        kind: q.kind.clone(),
        description,
        choices: Vec::new(),
        options: Vec::new(),
        order: Vec::new(),
    };
    let add = |f: &mut CompiledField, value: ChoiceValue, description: String, option: String| {
        f.choices.push(Choice {
            code: String::new(),
            value,
            description: Some(Json::String(description)),
        });
        f.options.push(option);
    };
    match &q.kind {
        QuestionType::Noul => {
            let empty = Vec::new();
            let members = match &q.criteria {
                None | Some(Json::Null) => &empty,
                Some(Json::Object(m)) => m,
                Some(_) => return Err("noul criteria must be an object".into()),
            };
            for (key, label, value) in [("false", "no", false), ("true", "yes", true)] {
                let description = members
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| text(v))
                    .transpose()?
                    .unwrap_or_default();
                let option = if description.is_empty() {
                    label.to_owned()
                } else {
                    format!("{label}: {description}")
                };
                add(&mut f, ChoiceValue::Bool(value), description, option);
            }
            if let Some((k, _)) = members.iter().find(|(k, _)| k != "false" && k != "true") {
                return Err(format!("unknown noul criterion {k:?}"));
            }
        }
        QuestionType::Choice => {
            let members = q
                .criteria
                .as_ref()
                .and_then(Json::as_object)
                .ok_or("choice criteria must be an object")?;
            for (key, value) in members {
                if key.trim().is_empty() {
                    return Err("choice keys must not be empty".into());
                }
                let description = text(value)?;
                let option = if description.is_empty() {
                    key.clone()
                } else {
                    format!("{key}: {description}")
                };
                add(&mut f, ChoiceValue::Key(key.clone()), description, option);
            }
        }
        QuestionType::Score => {
            let levels = q
                .criteria
                .as_ref()
                .and_then(Json::as_array)
                .ok_or("score criteria must be an array")?;
            for (i, level) in levels.iter().enumerate() {
                let t = text(level)?;
                add(&mut f, ChoiceValue::Key(i.to_string()), t.clone(), t);
            }
        }
        QuestionType::Other(_) => return Err("type must be choice, noul, or score".into()),
    }
    if !(1..=MAX_HEAD_OPTIONS).contains(&f.options.len()) {
        return Err(format!(
            "criteria must contain 1–{MAX_HEAD_OPTIONS} options"
        ));
    }
    Ok(f)
}

fn round_kev(v: f64) -> f64 {
    // Kev publishes four decimals, rounding half to even.
    (v * 1e4).round_ties_even() / 1e4
}

fn kev_confidence(p: &[f64], kind: &QuestionType) -> f64 {
    if p.len() == 1 {
        return 1.0;
    }
    let mode = argmax(p);
    if *kind == QuestionType::Choice {
        let uniform = 1.0 / p.len() as f64;
        return (p[mode] - uniform) / (1.0 - uniform);
    }
    let center = (p.len() - 1) as f64 / 2.0;
    let (mut spread, mut deviation) = (0.0, 0.0);
    for (i, pi) in p.iter().enumerate() {
        spread += pi * (i as f64 - mode as f64).abs();
        deviation += (i as f64 - center).abs();
    }
    (1.0 - spread / (deviation / p.len() as f64)).max(0.0)
}

// ---------------------------------------------------------------------------------------
// Clef

fn compile_clef(req: &SystemOneRequest, state: String) -> Result<Compiled> {
    let mut segments: Vec<String> = Vec::new();
    let add = |segments: &mut Vec<String>, text: String| -> [usize; 2] {
        let n = segments.len();
        segments.push(text);
        [n, n + 1]
    };
    add(
        &mut segments,
        "<|im_start|>system\nRead the complete state and schema. Decide every field jointly. \
         Each answer must be exactly one of that field's allowed options.<|im_end|>\n\
         <|im_start|>user\nSTATE:\n"
            .into(),
    );
    add(&mut segments, state);
    add(&mut segments, "\n\nSCHEMA FIELDS:\n".into());
    let mut fields = Vec::with_capacity(req.questions.len());
    let mut seg_fields = Vec::with_capacity(req.questions.len());
    for (name, q) in req.questions.iter() {
        let mut f = clef_field(name, q).map_err(|e| question_error(name, e))?;
        add(
            &mut segments,
            format!(
                "\nFIELD {}\nID: {name}\nTYPE: {}\nINSTRUCTION: ",
                fields.len() + 1,
                q.kind
            ),
        );
        let question = add(&mut segments, f.description.clone());
        add(&mut segments, "\nALLOWED OPTIONS:\n".into());
        let mut options = Vec::with_capacity(f.choices.len());
        for (j, c) in f.choices.iter().enumerate() {
            add(&mut segments, format!("OPTION {}: ", j + 1));
            let mut semantics = vec![("option_id".to_owned(), Json::String(c.value.text()))];
            if let Some(d) = c.description.as_ref().filter(|d| !d.is_null()) {
                semantics.push(("description".to_owned(), d.clone()));
            }
            let raw = python_json_compact_sorted(&Json::Object(semantics)).map_err(render_err)?;
            options.push(add(&mut segments, raw));
            add(&mut segments, "\n".into());
        }
        add(&mut segments, "END FIELD\n".into());
        seg_fields.push(SegmentField {
            kind: q.kind.head_id(),
            question,
            options,
        });
        // The head scores options in model (sorted) order; the answer lists them in request
        // order, so keep the choices in request order and remember the mapping.
        if !f.order.is_empty() {
            let mut ordered = f.choices.clone();
            for (j, &index) in f.order.iter().enumerate() {
                ordered[index] = f.choices[j].clone();
            }
            f.choices = ordered;
        }
        fields.push(f);
    }
    add(
        &mut segments,
        "\n<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\nJOINT SCHEMA DECISIONS:"
            .into(),
    );
    Ok(Compiled {
        encoding: Encoding::Clef,
        fields,
        input: ScoreInput::Clef {
            segments,
            fields: seg_fields,
            image_position: 1,
        },
        images: req.images.clone(),
    })
}

fn clef_field(name: &str, q: &Question) -> std::result::Result<CompiledField, String> {
    if name.trim().is_empty() {
        return Err("field name must not be empty".into());
    }
    let description = match &q.instructions {
        None | Some(Json::Null) => name.to_owned(),
        Some(Json::String(s)) if s.is_empty() => name.to_owned(),
        Some(v) => clef_content(v).map_err(|e| format!("instructions: {e}"))?,
    };
    let mut f = CompiledField {
        name: name.into(),
        kind: q.kind.clone(),
        description,
        choices: Vec::new(),
        options: Vec::new(),
        order: Vec::new(),
    };
    let criteria = q.criteria.as_ref().filter(|c| !c.is_null());
    let add = |f: &mut CompiledField, value: ChoiceValue, description: Option<Json>| {
        f.choices.push(Choice {
            code: String::new(),
            value,
            description,
        });
    };
    match &q.kind {
        QuestionType::Noul => {
            let mut yes = Json::from("The proposition is true or the answer is yes.");
            let mut no = Json::from("The proposition is false or the answer is no.");
            if let Some(c) = criteria {
                let members = c.as_object().ok_or("noul criteria must be an object")?;
                for (key, value) in members {
                    match key.as_str() {
                        "true" => yes = value.clone(),
                        "false" => no = value.clone(),
                        other => return Err(format!("unknown noul criterion {other:?}")),
                    }
                }
            }
            add(&mut f, ChoiceValue::Bool(true), Some(yes));
            add(&mut f, ChoiceValue::Bool(false), Some(no));
        }
        QuestionType::Choice => {
            let members = criteria
                .and_then(Json::as_object)
                .ok_or("choice criteria must be an object")?;
            let mut sorted: Vec<(usize, &(String, Json))> = members.iter().enumerate().collect();
            sorted.sort_by(|a, b| a.1.0.as_bytes().cmp(b.1.0.as_bytes()));
            for (original, (key, value)) in sorted {
                if key.trim().is_empty() {
                    return Err("choice keys must not be empty".into());
                }
                add(&mut f, ChoiceValue::Key(key.clone()), Some(value.clone()));
                f.order.push(original);
            }
        }
        QuestionType::Score => {
            let levels = criteria
                .and_then(Json::as_array)
                .ok_or("score criteria must be an array")?;
            for (i, level) in levels.iter().enumerate() {
                add(&mut f, ChoiceValue::Key(i.to_string()), Some(level.clone()));
            }
        }
        QuestionType::Other(_) => return Err("type must be choice, noul, or score".into()),
    }
    if !(2..=MAX_LETTER_CANDIDATES).contains(&f.choices.len()) {
        return Err(format!(
            "criteria must contain 2–{MAX_LETTER_CANDIDATES} candidates"
        ));
    }
    Ok(f)
}

// ---------------------------------------------------------------------------------------
// Strands

/// Strands shows objects and arrays as JSON indented by two spaces, strings trimmed.
fn strands_content(v: &Json) -> std::result::Result<String, String> {
    match v {
        Json::Object(_) | Json::Array(_) => python_json(v, 2).map_err(|e| e.to_string()),
        other => content(other).map(|s| s.trim().to_owned()),
    }
}

fn compile_strands(req: &SystemOneRequest) -> Result<Compiled> {
    let state = req
        .state
        .as_ref()
        .and_then(|s| strands_content(s).ok())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            DecisionError::invalid("state must be a nonempty string, object, or array")
        })?;
    let mut fields = Vec::with_capacity(req.questions.len());
    let mut rows = Vec::with_capacity(req.questions.len());
    for (name, q) in req.questions.iter() {
        let f = strands_field(name, q).map_err(|e| question_error(name, e))?;
        let header = match q.kind {
            QuestionType::Choice => "Select exactly one option.",
            QuestionType::Score => {
                "Rate the state against the ordered levels below (lowest first)."
            }
            _ => "Decide whether the statement is true of the state.",
        };
        let mut prompt = format!(
            "<question type=\"{}\">\n{header}\n{}\n<options>\n",
            q.kind, f.description
        );
        let mut options = Vec::with_capacity(f.choices.len());
        for (i, c) in f.choices.iter().enumerate() {
            let start = prompt.len();
            prompt.push_str(&format!("{}. {}", i + 1, c.value.text()));
            let description = c
                .description
                .as_ref()
                .and_then(Json::as_str)
                .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            if !description.is_empty() {
                prompt.push_str(" — ");
                prompt.push_str(&description);
            }
            options.push([start, prompt.len()]);
            prompt.push('\n');
        }
        prompt.push_str("</options>\n</question>\n<answer>");
        rows.push(PointerRow {
            prefix: format!("<state>\n{state}\n</state>\n"),
            prompt,
            kind: q.kind.head_id(),
            options,
        });
        fields.push(f);
    }
    Ok(Compiled {
        encoding: Encoding::Strands,
        fields,
        input: ScoreInput::Strands(rows),
        images: Vec::new(),
    })
}

fn strands_field(name: &str, q: &Question) -> std::result::Result<CompiledField, String> {
    if name.trim().is_empty() {
        return Err("field name must not be empty".into());
    }
    let description = q
        .instructions
        .as_ref()
        .and_then(|i| strands_content(i).ok())
        .filter(|d| !d.trim().is_empty())
        .ok_or("instructions must be a nonempty string, object, or array")?;
    let mut f = CompiledField {
        name: name.into(),
        kind: q.kind.clone(),
        description,
        choices: Vec::new(),
        options: Vec::new(),
        order: Vec::new(),
    };
    let add = |f: &mut CompiledField, value: ChoiceValue, description: &str| {
        f.choices.push(Choice {
            code: String::new(),
            value,
            description: Some(Json::String(description.trim().to_owned())),
        });
    };
    let mut limit = MAX_HEAD_OPTIONS;
    match &q.kind {
        QuestionType::Noul => {
            let mut no = "the statement does not hold for this state".to_owned();
            let mut yes = "the statement holds for this state".to_owned();
            if let Some(c) = q.criteria.as_ref().filter(|c| !c.is_null()) {
                let members = c
                    .as_object()
                    .ok_or("noul criteria must be an object of true/false descriptions")?;
                for (key, value) in members {
                    let text = value
                        .as_str()
                        .ok_or("noul criteria must contain only true/false string descriptions")?;
                    match key.as_str() {
                        "false" => no = text.to_owned(),
                        "true" => yes = text.to_owned(),
                        _ => {
                            return Err(
                                "noul criteria must contain only true/false string descriptions"
                                    .into(),
                            );
                        }
                    }
                }
            }
            add(&mut f, ChoiceValue::Bool(false), &no);
            add(&mut f, ChoiceValue::Bool(true), &yes);
        }
        QuestionType::Choice => {
            let members = q
                .criteria
                .as_ref()
                .and_then(Json::as_object)
                .ok_or("choice criteria must map option keys to descriptions or null")?;
            for (key, value) in members {
                if key.trim().is_empty() {
                    return Err("choice keys must not be empty".into());
                }
                let description = match value {
                    Json::Null => "",
                    Json::String(s) => s,
                    _ => {
                        return Err(
                            "choice criteria must map option keys to descriptions or null".into(),
                        );
                    }
                };
                add(&mut f, ChoiceValue::Key(key.clone()), description);
            }
        }
        QuestionType::Score => {
            limit = MAX_SCORE_LEVELS;
            let levels = q
                .criteria
                .as_ref()
                .and_then(Json::as_array)
                .ok_or("score criteria must be an array of descriptions")?;
            for (i, level) in levels.iter().enumerate() {
                let text = level.as_str().ok_or("score descriptions must be strings")?;
                add(&mut f, ChoiceValue::Key(i.to_string()), text);
            }
        }
        QuestionType::Other(_) => return Err("type must be choice, noul, or score".into()),
    }
    if !(2..=limit).contains(&f.choices.len()) {
        return Err(format!("criteria must contain 2–{limit} candidates"));
    }
    Ok(f)
}

// ---------------------------------------------------------------------------------------
// Answers

fn argmax(p: &[f64]) -> usize {
    // First maximum wins ties, so answers do not depend on float noise order.
    let mut best = 0;
    for (i, v) in p.iter().enumerate() {
        if *v > p[best] {
            best = i;
        }
    }
    best
}

impl Compiled {
    /// Number of candidates the scorer must return for each question, model order.
    pub fn candidate_counts(&self) -> Vec<usize> {
        self.fields.iter().map(|f| f.choices.len()).collect()
    }

    /// Turns candidate scores into the System One response: softmax over each question's
    /// candidates, the most probable choice, the expected score level and the confidence
    /// (1 minus normalised entropy; Kev's own measure, rounded to four decimals, for Kev).
    pub fn answer(&self, model: &str, out: &ScoreOutput) -> Result<SystemOneResponse> {
        if out.logits.len() != self.fields.len() {
            return Err(DecisionError::Protocol(format!(
                "scorer returned {} rows for {} questions",
                out.logits.len(),
                self.fields.len()
            )));
        }
        let kev = self.encoding == Encoding::Kev;
        let round = |v: f64| if kev { round_kev(v) } else { v };
        let mut answers = Vec::with_capacity(self.fields.len());
        for (f, logits) in self.fields.iter().zip(&out.logits) {
            if logits.len() != f.choices.len() {
                return Err(DecisionError::Protocol(format!(
                    "scorer returned {} candidates for {:?}, expected {}",
                    logits.len(),
                    f.name,
                    f.choices.len()
                )));
            }
            if logits.iter().any(|l| !l.is_finite()) {
                return Err(DecisionError::Protocol(format!(
                    "scorer returned a non-finite logit for {:?}",
                    f.name
                )));
            }
            let mut logits = logits.clone();
            if !f.order.is_empty() {
                let mut ordered = vec![0.0; logits.len()];
                for (j, &index) in f.order.iter().enumerate() {
                    ordered[index] = logits[j];
                }
                logits = ordered;
            }
            let peak = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mut p: Vec<f64> = logits.iter().map(|l| (l - peak).exp()).collect();
            let sum: f64 = p.iter().sum();
            let (mut entropy, mut score) = (0.0, 0.0);
            for (j, pj) in p.iter_mut().enumerate() {
                *pj /= sum;
                score += j as f64 * *pj;
                if *pj > 0.0 {
                    entropy -= *pj * pj.ln();
                }
            }
            let answer = match f.kind {
                QuestionType::Noul => {
                    let yes = f
                        .choices
                        .iter()
                        .position(|c| c.value == ChoiceValue::Bool(true))
                        .unwrap_or(1);
                    Answer::Noul {
                        noul: round(p[yes]),
                    }
                }
                _ => {
                    let probabilities: Vec<(String, f64)> = f
                        .choices
                        .iter()
                        .zip(&p)
                        .map(|(c, pj)| (c.value.text(), round(*pj)))
                        .collect();
                    let confidence = if kev {
                        round_kev(kev_confidence(&p, &f.kind))
                    } else {
                        (1.0 - entropy / (p.len() as f64).ln()).clamp(0.0, 1.0)
                    };
                    if f.kind == QuestionType::Choice {
                        Answer::Choice {
                            choice: f.choices[argmax(&p)].value.text(),
                            probabilities,
                            confidence,
                        }
                    } else {
                        Answer::Score {
                            score: round(score),
                            legend: f
                                .choices
                                .iter()
                                .map(|c| {
                                    (c.value.text(), c.description.clone().unwrap_or(Json::Null))
                                })
                                .collect(),
                            probabilities,
                            confidence,
                        }
                    }
                }
            };
            answers.push((f.name.clone(), answer));
        }
        Ok(SystemOneResponse {
            model: model.to_owned(),
            answers: Answers(answers),
            usage: Usage {
                input_tokens: out.input_tokens,
                output_tokens: out.output_tokens,
            },
            prompt_eval_cached_count: out.cached_tokens,
        })
    }
}
