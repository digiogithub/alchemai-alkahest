//! Logit scoring through llama-server (spec ALK-SP-0007.R3): answers come from the
//! engine's next-token probabilities over the candidate letters, never from sampled text.
//!
//! Each letter-scored question is rendered with the model's chat template
//! (`/apply-template`, thinking disabled), tokenised (`/tokenize`), and read from one
//! next-token distribution (`/completion`, `n_predict: 1`). Every candidate gets the same
//! logit bias and the samplers keep exactly the top `k = candidates` tokens at temperature
//! 1, so the returned `top_probs` are the softmax over the candidates' original logits (a
//! shared offset cancels out). Models that llama-server serves natively (`decision.type`,
//! `systemone` template) are forwarded to its `/v1/systemone`.

use futures::stream::{self, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use crate::compile::{
    ChatMessage, Compiled, PromptRow, ScoreInput, ScoreOutput, compile, validate_limits,
};
use crate::encoding::{DecisionBackend, DecisionPlan};
use crate::error::{DecisionError, Result};
use crate::json::Json;
use crate::types::{Answers, Questions, SystemOneRequest, SystemOneResponse, Usage};

/// Bias that lifts every candidate above all other tokens; retried with
/// [`RETRY_BIAS`] when another token still outranks a candidate.
const SCORE_BIAS: f64 = 100.0;
/// Second, larger bias.
const RETRY_BIAS: f64 = 1000.0;

/// Log-probability given to a candidate llama-server omitted because its probability
/// underflowed `f32` (the smallest positive subnormal).
fn floor_logit() -> f64 {
    f64::from(f32::from_bits(1)).ln()
}

/// How rows are sent to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoringOptions {
    /// Questions scored at the same time. `1` (default) keeps answers bit-for-bit
    /// repeatable: llama-server batches concurrent requests together and different batch
    /// compositions change float results. Higher values trade that for throughput.
    pub concurrency: usize,
    /// Let llama-server reuse cached prompt prefixes. Off by default because llama.cpp does
    /// not guarantee identical logits between cached and fresh evaluation.
    pub cache_prompt: bool,
}

impl Default for ScoringOptions {
    fn default() -> Self {
        Self {
            concurrency: 1,
            cache_prompt: false,
        }
    }
}

/// Answers System One requests with one running llama-server.
#[derive(Debug, Clone)]
pub struct DecisionEngine {
    http: reqwest::Client,
    base_url: String,
    options: ScoringOptions,
}

#[derive(Serialize)]
struct ApplyTemplate<'a> {
    messages: &'a [ChatMessage],
    chat_template_kwargs: TemplateKwargs,
}

#[derive(Serialize)]
struct TemplateKwargs {
    enable_thinking: bool,
}

#[derive(Deserialize)]
struct Prompt {
    prompt: String,
}

#[derive(Serialize)]
struct Tokenize<'a> {
    content: &'a str,
    add_special: bool,
    parse_special: bool,
}

#[derive(Deserialize)]
struct Tokens {
    tokens: Vec<i64>,
}

/// The part of llama-server's `/completion` request scoring uses.
#[derive(Serialize)]
struct CompletionRequest<'a> {
    prompt: &'a [i64],
    stream: bool,
    cache_prompt: bool,
    n_predict: u32,
    n_probs: usize,
    post_sampling_probs: bool,
    samplers: [&'static str; 2],
    top_k: usize,
    temperature: f64,
    logit_bias: Vec<(i64, f64)>,
}

/// The part of llama-server's `/completion` response scoring reads.
#[derive(Deserialize)]
struct CompletionResponse {
    #[serde(default)]
    tokens_predicted: u64,
    #[serde(default)]
    tokens_evaluated: u64,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    completion_probabilities: Vec<Distribution>,
}

#[derive(Deserialize)]
struct Distribution {
    #[serde(default)]
    top_probs: Vec<TokenProb>,
}

#[derive(Deserialize)]
struct TokenProb {
    id: Option<i64>,
    prob: Option<f64>,
}

#[derive(Serialize)]
struct NativeRequest<'a> {
    state: &'a Json,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
    questions: &'a Questions,
}

#[derive(Deserialize)]
struct NativeResponse {
    answers: Answers,
    #[serde(default)]
    usage: Usage,
}

/// One scored row: candidate logits and token counts.
struct RowScore {
    logits: Vec<f64>,
    input_tokens: u64,
    output_tokens: u64,
}

impl DecisionEngine {
    /// An engine at `base_url` (`http://127.0.0.1:41234`, the scheduler lease's base URL)
    /// with its own HTTP client and default options.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_client(reqwest::Client::new(), base_url)
    }

    /// An engine using a shared HTTP client (timeouts and pools are the caller's).
    pub fn with_client(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            http,
            base_url,
            options: ScoringOptions::default(),
        }
    }

    /// The same engine with other scoring options.
    pub fn with_options(mut self, options: ScoringOptions) -> Self {
        self.options = options;
        self
    }

    /// The engine base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Answers `req` following `plan` (from [`crate::DecisionMetadata::plan`]).
    ///
    /// Native models are validated and forwarded; prompt models are compiled, scored and
    /// answered. Encodings that need a model-native head (kev, clef, strands) are
    /// [`DecisionError::Unsupported`] here, since llama-server exposes those heads only
    /// through its own `/v1/systemone`.
    pub async fn decide(
        &self,
        req: &SystemOneRequest,
        plan: &DecisionPlan,
    ) -> Result<SystemOneResponse> {
        match plan.backend {
            DecisionBackend::Native => self.native(req).await,
            DecisionBackend::Prompt(encoding) => {
                if !encoding.scores_letters() {
                    return Err(DecisionError::Unsupported(format!(
                        "the {encoding} encoding needs a model-native decision head; llama-server \
                         serves it only for GGUFs with decision metadata"
                    )));
                }
                let compiled = compile(req, encoding)?;
                let out = self.score(&compiled, plan.system.as_deref()).await?;
                compiled.answer(&req.model, &out)
            }
        }
    }

    /// Forwards a validated request to llama-server's native `/v1/systemone`.
    pub async fn native(&self, req: &SystemOneRequest) -> Result<SystemOneResponse> {
        if req.model.trim().is_empty() {
            return Err(DecisionError::invalid("model is required"));
        }
        validate_limits(req)?;
        let body = NativeRequest {
            state: req.state.as_ref().unwrap_or(&Json::Null),
            images: req.images.iter().map(|i| image_data_url(i)).collect(),
            questions: &req.questions,
        };
        let resp: NativeResponse = match self.post("/v1/systemone", &body).await {
            Err(DecisionError::EngineStatus { message, .. })
                if message.contains("physical batch size") =>
            {
                // Native heads read the whole prompt in one batch sized to the context, so
                // outgrowing it means the prompt is too long.
                return Err(DecisionError::invalid(
                    "the request is too long for the model's context",
                ));
            }
            other => other?,
        };
        // Answers follow question order whatever order the engine wrote them in.
        let mut answers = resp.answers;
        answers.0.sort_by_key(|(id, _)| {
            req.questions
                .iter()
                .position(|(q, _)| q == id)
                .unwrap_or(usize::MAX)
        });
        Ok(SystemOneResponse {
            model: req.model.clone(),
            answers,
            usage: resp.usage,
            prompt_eval_cached_count: None,
        })
    }

    /// Scores every question of a letter-encoded request (generic, tev1), rows in order.
    ///
    /// `system` is prepended as a system message before the chat template is applied.
    pub async fn score(&self, compiled: &Compiled, system: Option<&str>) -> Result<ScoreOutput> {
        let ScoreInput::Prompts(rows) = &compiled.input else {
            return Err(DecisionError::Unsupported(format!(
                "the {} encoding is not scored by candidate letters",
                compiled.encoding
            )));
        };
        // `buffered` keeps results in row order whatever the completion order.
        let scores: Vec<RowScore> =
            stream::iter(rows.iter().map(|row| self.score_row(row, system)))
                .buffered(self.options.concurrency.max(1))
                .try_collect()
                .await?;
        let mut out = ScoreOutput::default();
        for s in scores {
            out.input_tokens += s.input_tokens;
            out.output_tokens += s.output_tokens;
            out.logits.push(s.logits);
        }
        Ok(out)
    }

    async fn score_row(&self, row: &PromptRow, system: Option<&str>) -> Result<RowScore> {
        let mut messages = Vec::with_capacity(row.messages.len() + 1);
        if let Some(s) = system.filter(|s| !s.is_empty()) {
            messages.push(ChatMessage {
                role: "system".into(),
                content: s.to_owned(),
            });
        }
        messages.extend(row.messages.iter().cloned());
        let prompt: Prompt = self
            .post(
                "/apply-template",
                &ApplyTemplate {
                    messages: &messages,
                    chat_template_kwargs: TemplateKwargs {
                        enable_thinking: false,
                    },
                },
            )
            .await?;
        let tokens = self.tokenize(&prompt.prompt, true).await?;
        if tokens.is_empty() {
            return Err(DecisionError::Protocol(
                "the rendered prompt has no tokens".into(),
            ));
        }
        let mut candidates: Vec<i64> = Vec::with_capacity(row.candidates.len());
        for c in &row.candidates {
            let joined = self
                .tokenize(&format!("{}{c}", prompt.prompt), true)
                .await?;
            let literal = self.tokenize(c, false).await?;
            let id = match (joined.split_last(), literal.as_slice()) {
                (Some((last, head)), [only]) if head == tokens.as_slice() && last == only => *last,
                _ => {
                    return Err(DecisionError::invalid(format!(
                        "candidate {c:?} must append exactly one ordinary token to the prompt"
                    )));
                }
            };
            if id < 0 || candidates.contains(&id) {
                return Err(DecisionError::invalid(
                    "invalid or duplicate candidate tokens",
                ));
            }
            candidates.push(id);
        }

        let mut output_tokens = 0;
        for bias in [SCORE_BIAS, RETRY_BIAS] {
            let req = CompletionRequest {
                prompt: &tokens,
                stream: false,
                cache_prompt: self.options.cache_prompt,
                n_predict: 1,
                n_probs: candidates.len(),
                post_sampling_probs: true,
                samplers: ["top_k", "temperature"],
                top_k: candidates.len(),
                temperature: 1.0,
                logit_bias: candidates.iter().map(|&id| (id, bias)).collect(),
            };
            let resp: CompletionResponse = self.post("/completion", &req).await?;
            if resp.truncated {
                return Err(DecisionError::invalid(format!(
                    "the prompt ({} tokens) does not fit the model's context",
                    tokens.len()
                )));
            }
            if resp.tokens_evaluated != tokens.len() as u64
                || resp.completion_probabilities.len() != 1
            {
                return Err(DecisionError::Protocol(
                    "scoring did not evaluate the complete prompt and return one distribution"
                        .into(),
                ));
            }
            output_tokens += resp.tokens_predicted;
            let mut logits = vec![floor_logit(); candidates.len()];
            let mut seen = vec![false; candidates.len()];
            let mut outranked = false;
            let top = &resp.completion_probabilities[0].top_probs;
            if top.is_empty() {
                return Err(DecisionError::Protocol(
                    "empty candidate distribution".into(),
                ));
            }
            for p in top {
                let (Some(id), Some(prob)) = (p.id, p.prob) else {
                    return Err(DecisionError::Protocol(
                        "candidate probability without id or value".into(),
                    ));
                };
                if !(prob > 0.0 && prob <= 1.0) {
                    return Err(DecisionError::Protocol(format!(
                        "invalid candidate probability {prob}"
                    )));
                }
                let Some(j) = candidates.iter().position(|&c| c == id) else {
                    outranked = true;
                    break;
                };
                if std::mem::replace(&mut seen[j], true) {
                    return Err(DecisionError::Protocol(
                        "a candidate was returned twice".into(),
                    ));
                }
                logits[j] = prob.ln().max(floor_logit());
            }
            if !outranked {
                return Ok(RowScore {
                    logits,
                    input_tokens: tokens.len() as u64,
                    output_tokens,
                });
            }
        }
        Err(DecisionError::Protocol(
            "candidates were outranked by other tokens".into(),
        ))
    }

    async fn tokenize(&self, content: &str, parse_special: bool) -> Result<Vec<i64>> {
        let t: Tokens = self
            .post(
                "/tokenize",
                &Tokenize {
                    content,
                    add_special: false,
                    parse_special,
                },
            )
            .await?;
        Ok(t.tokens)
    }

    async fn post<B: Serialize + ?Sized, R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| DecisionError::Transport(format!("POST {path}: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| DecisionError::Transport(format!("POST {path}: {e}")))?;
        if !status.is_success() {
            return Err(DecisionError::EngineStatus {
                status: status.as_u16(),
                message: error_message(&bytes),
            });
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| DecisionError::Protocol(format!("POST {path}: {e}")))
    }
}

/// The message of a llama-server error body (`{"error":{"message":..}}` or
/// `{"error":".."}`), or the raw text.
fn error_message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return text.trim().to_owned();
    };
    let e = &v["error"];
    e["message"]
        .as_str()
        .or_else(|| e.as_str())
        .map_or_else(|| text.trim().to_owned(), str::to_owned)
}

/// An image as llama-server wants it: a data URL. Ollama clients send bare base64, so the
/// media type is guessed from the base64 of the format's magic bytes.
pub fn image_data_url(image: &str) -> String {
    if image.starts_with("data:") {
        return image.to_owned();
    }
    let mime = [
        ("iVBORw0KGgo", "image/png"),
        ("/9j/", "image/jpeg"),
        ("R0lGOD", "image/gif"),
        ("UklGR", "image/webp"),
    ]
    .iter()
    .find_map(|(prefix, mime)| image.starts_with(prefix).then_some(*mime))
    .unwrap_or("application/octet-stream");
    format!("data:{mime};base64,{image}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls_from_base64_magic() {
        assert_eq!(
            image_data_url("iVBORw0KGgoAAA"),
            "data:image/png;base64,iVBORw0KGgoAAA"
        );
        assert_eq!(
            image_data_url("/9j/4AAQ"),
            "data:image/jpeg;base64,/9j/4AAQ"
        );
        assert_eq!(
            image_data_url("data:image/gif;base64,R0"),
            "data:image/gif;base64,R0"
        );
        assert!(image_data_url("AAAA").starts_with("data:application/octet-stream;base64,"));
    }

    #[test]
    fn engine_error_messages() {
        assert_eq!(
            error_message(
                br#"{"error":{"code":501,"message":"This model is not a decision model"}}"#
            ),
            "This model is not a decision model"
        );
        assert_eq!(error_message(br#"{"error":"bad"}"#), "bad");
        assert_eq!(error_message(b" plain "), "plain");
    }

    #[test]
    fn floor_is_the_smallest_subnormal() {
        assert!((floor_logit() - (-103.279)).abs() < 1e-3);
    }

    #[test]
    fn base_url_trailing_slash_is_dropped() {
        assert_eq!(DecisionEngine::new("http://h:1//").base_url(), "http://h:1");
    }
}
