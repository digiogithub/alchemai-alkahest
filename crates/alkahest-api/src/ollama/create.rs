//! `HEAD`/`POST /api/blobs/:digest` and `POST /api/create` (ALK-US-0087).
//!
//! The Ollama CLI implements `ollama create` client side: it parses the Modelfile, uploads
//! local files with `POST /api/blobs/sha256:<hex>` (after a `HEAD` to skip known blobs) and
//! sends a structured create request (`model`, `from` or `files`, `template`, `system`,
//! `parameters`, `license`). Older clients send the Modelfile text as `modelfile`; both are
//! accepted. Blobs land in the Alkahest store (never the read-only Ollama store), streamed
//! and verified against the digest in the path.
//!
//! Supported Modelfile subset: `FROM` (a model, or an uploaded GGUF blob), `PARAMETER`,
//! `TEMPLATE` (stored, never executed: ADR 0001), `SYSTEM`, `LICENSE`. `ADAPTER`, `MESSAGE`,
//! `REQUIRES`, `RENDERER`, `PARSER`, quantisation and safetensors imports answer 400.

use std::io::Write;
use std::str::FromStr;

use alkahest_store::{CreateLayers, Digest, StoreError};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::generate::parse;
use super::ndjson;
use crate::dialect::Family;
use crate::service::{self, HttpError, blocking, respond};
use crate::state::AppState;

fn parse_digest(raw: &str) -> Result<Digest, HttpError> {
    Digest::from_str(raw).map_err(|_| HttpError::bad_request(format!("invalid digest {raw:?}")))
}

/// `HEAD /api/blobs/:digest`: 200 when the Alkahest store has the blob, 404 otherwise.
pub(super) async fn head_blob(State(state): State<AppState>, Path(raw): Path<String>) -> Response {
    let r = async {
        let digest = parse_digest(&raw)?;
        let catalog = service::catalog(&state)?;
        if catalog.alkahest().has_blob(&digest) {
            Ok(StatusCode::OK.into_response())
        } else {
            Ok(StatusCode::NOT_FOUND.into_response())
        }
    }
    .await;
    respond(&state, Family::Ollama, r)
}

/// `POST /api/blobs/:digest`: streams the body into the Alkahest store, verifying it hashes
/// to the digest of the path (201; 200 when the blob was already there; 400 on mismatch).
/// The router exempts this route from the request body limit: GGUF files are gigabytes.
pub(super) async fn post_blob(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    body: Body,
) -> Response {
    let r = async {
        let digest = parse_digest(&raw)?;
        let catalog = service::catalog(&state)?;
        if catalog.alkahest().has_blob(&digest) {
            return Ok(StatusCode::OK.into_response());
        }
        let store = catalog.alkahest().clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Option<Bytes>>(8);
        // File I/O and hashing on the blocking pool; `None` means the upload broke off, and
        // dropping the uncommitted writer deletes its temp file.
        let writer = tokio::task::spawn_blocking(move || -> Result<Digest, HttpError> {
            let mut w = store
                .blob_writer(Some(digest))
                .map_err(|e| HttpError::from_store(&e))?;
            while let Some(chunk) = rx.blocking_recv() {
                let Some(chunk) = chunk else {
                    return Err(HttpError::bad_request("the upload was interrupted"));
                };
                w.write_all(&chunk).map_err(|e| {
                    HttpError::new(
                        crate::dialect::ErrorKind::Internal,
                        format!("cannot write the blob: {e}"),
                    )
                })?;
            }
            w.commit().map_err(|e| match e {
                StoreError::DigestMismatch { expected, actual } => HttpError::bad_request(format!(
                    "digest mismatch, expected {expected:?}, got {actual:?}"
                )),
                other => HttpError::from_store(&other),
            })
        });
        let mut data = body.into_data_stream();
        while let Some(chunk) = data.next().await {
            let item = chunk.ok();
            let broken = item.is_none();
            if tx.send(item).await.is_err() || broken {
                break;
            }
        }
        drop(tx);
        writer.await.map_err(|e| {
            HttpError::new(
                crate::dialect::ErrorKind::Internal,
                format!("blob writer failed: {e}"),
            )
        })??;
        Ok(StatusCode::CREATED.into_response())
    }
    .await;
    respond(&state, Family::Ollama, r)
}

/// Ollama `CreateRequest` (the fields Alkahest understands; others are refused below).
#[derive(Debug, Default, Deserialize)]
struct CreateRequest {
    #[serde(default, alias = "name")]
    model: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    files: Map<String, Value>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    parameters: Option<Map<String, Value>>,
    #[serde(default)]
    license: Option<Value>,
    #[serde(default)]
    modelfile: Option<String>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    quantize: Option<String>,
    #[serde(default)]
    adapters: Map<String, Value>,
    #[serde(default)]
    messages: Vec<Value>,
}

/// `POST /api/create`.
pub(super) async fn create(State(state): State<AppState>, body: Bytes) -> Response {
    let r = create_inner(&state, body).await;
    respond(&state, Family::Ollama, r)
}

async fn create_inner(state: &AppState, body: Bytes) -> Result<Response, HttpError> {
    let req: CreateRequest = parse(&body)?;
    if req.model.trim().is_empty() {
        return Err(HttpError::bad_request("model is required"));
    }
    if req.quantize.as_deref().is_some_and(|q| !q.is_empty()) {
        return Err(HttpError::bad_request(
            "quantizing is not supported; pull or upload an already quantized GGUF",
        ));
    }
    if !req.adapters.is_empty() {
        return Err(HttpError::bad_request("LoRA adapters are not supported"));
    }
    if !req.messages.is_empty() {
        return Err(HttpError::bad_request(
            "seed messages (MESSAGE) are not supported",
        ));
    }
    let plan = CreatePlan::from_request(&req)?;
    let stream = req.stream.unwrap_or(true);
    let catalog = service::catalog(state)?;
    let model = req.model.clone();
    let (base_label, report) = blocking(move || {
        let label = match (&plan.blob, &plan.from) {
            (Some(d), _) => d.to_string(),
            (None, Some(f)) => f.clone(),
            (None, None) => String::new(),
        };
        let r = catalog
            .create(&model, plan.from.as_deref(), plan.blob, &plan.layers)
            .map_err(|e| match e {
                StoreError::MissingBlob(d) => HttpError::bad_request(format!(
                    "blob {d} is not on the server; upload it with POST /api/blobs/{d}"
                )),
                other => HttpError::from_store(&other),
            })?;
        Ok((label, r.1))
    })
    .await?;
    let mut lines = vec![json!({"status": format!("using {base_label}")})];
    if report.blobs_copied > 0 {
        lines.push(json!({"status": format!(
            "copied {} blob(s) from the Ollama store", report.blobs_copied
        )}));
    }
    lines.push(json!({"status": "writing manifest"}));
    lines.push(json!({"status": "success"}));
    if stream {
        Ok(ndjson(futures::stream::iter(lines)))
    } else {
        Ok(Json(json!({"status": "success"})).into_response())
    }
}

/// What a create request resolved to.
#[derive(Debug, Default, PartialEq)]
struct CreatePlan {
    from: Option<String>,
    blob: Option<Digest>,
    layers: CreateLayers,
}

impl CreatePlan {
    /// Merges the legacy `modelfile` text with the structured fields (which win).
    fn from_request(req: &CreateRequest) -> Result<Self, HttpError> {
        let mf = match req.modelfile.as_deref() {
            Some(text) if !text.trim().is_empty() => parse_modelfile(text)?,
            _ => Modelfile::default(),
        };
        let mut plan = CreatePlan::default();
        // Base: uploaded files, then `from`, then the Modelfile's FROM.
        if let Some(digest) = gguf_file(&req.files)? {
            plan.blob = Some(digest);
        } else if let Some(from) = req.from.as_deref().filter(|f| !f.trim().is_empty()) {
            plan.set_from(from)?;
        } else if let Some(from) = mf.from.as_deref() {
            plan.set_from(from)?;
        } else {
            return Err(HttpError::bad_request(
                "neither 'from' nor 'files' was specified",
            ));
        }
        plan.layers.template = req.template.clone().or(mf.template);
        plan.layers.system = req.system.clone().or(mf.system);
        plan.layers.parameters = match &req.parameters {
            Some(p) if !p.is_empty() => Some(p.clone()),
            _ if !mf.parameters.is_empty() => Some(parameters_json(&mf.parameters)),
            _ => None,
        };
        plan.layers.license = match &req.license {
            Some(Value::String(s)) if !s.is_empty() => Some(vec![s.clone()]),
            Some(Value::Array(a)) if !a.is_empty() => Some(
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            ),
            _ if !mf.license.is_empty() => Some(mf.license),
            _ => None,
        };
        Ok(plan)
    }

    /// A `FROM` value: a blob digest, or a model reference. Local paths cannot be read from
    /// the client's machine, so they are refused with the upload recipe.
    fn set_from(&mut self, from: &str) -> Result<(), HttpError> {
        let from = from.trim();
        let digest_like = from.strip_prefix('@').unwrap_or(from);
        if digest_like.starts_with("sha256:") || digest_like.starts_with("sha256-") {
            self.blob = Some(parse_digest(digest_like)?);
            return Ok(());
        }
        let lower = from.to_ascii_lowercase();
        if from.starts_with(['.', '/', '~', '\\'])
            || lower.ends_with(".gguf")
            || lower.ends_with(".safetensors")
        {
            return Err(HttpError::bad_request(format!(
                "FROM {from}: local files must be uploaded first (POST /api/blobs/sha256:<hex>) \
                 and passed in 'files'"
            )));
        }
        self.from = Some(from.to_string());
        Ok(())
    }
}

/// The GGUF blob among the uploaded `files` (`{"model.gguf": "sha256:..."}`). Several GGUF
/// files (a projector, split weights) and safetensors are not supported yet.
fn gguf_file(files: &Map<String, Value>) -> Result<Option<Digest>, HttpError> {
    if files.is_empty() {
        return Ok(None);
    }
    let mut ggufs = Vec::new();
    for (name, digest) in files {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".safetensors") || lower.ends_with(".json") {
            return Err(HttpError::bad_request(
                "importing safetensors is not supported; upload a GGUF file",
            ));
        }
        let Some(d) = digest.as_str() else {
            return Err(HttpError::bad_request(format!("invalid digest for {name}")));
        };
        ggufs.push(parse_digest(d)?);
    }
    match ggufs.as_slice() {
        [one] => Ok(Some(*one)),
        _ => Err(HttpError::bad_request(
            "models made of several files (projector, split GGUF) cannot be created yet",
        )),
    }
}

/// The supported subset of a Modelfile.
#[derive(Debug, Default, PartialEq)]
struct Modelfile {
    from: Option<String>,
    parameters: Vec<(String, String)>,
    template: Option<String>,
    system: Option<String>,
    license: Vec<String>,
}

/// Parses Modelfile text: one instruction per line (case-insensitive), `#` comments,
/// values either bare to the end of the line, `"quoted"`, or `"""triple quoted"""` across
/// lines.
fn parse_modelfile(text: &str) -> Result<Modelfile, HttpError> {
    let mut mf = Modelfile::default();
    let mut rest = text;
    while !rest.is_empty() {
        let (line, after) = rest.split_once('\n').unwrap_or((rest, ""));
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            rest = after;
            continue;
        }
        let (cmd, arg) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        let arg = arg.trim_start();
        let (value, next) = if let Some(open) = arg.strip_prefix("\"\"\"") {
            // The closing quotes may be on this line or a later one.
            let offset = text.len() - rest.len() + (line.len() - open.len());
            let body = &text[offset..];
            let end = body.find("\"\"\"").ok_or_else(|| {
                HttpError::bad_request(format!("unterminated \"\"\" in {cmd} of the Modelfile"))
            })?;
            let tail = &body[end + 3..];
            let next = tail.split_once('\n').map_or("", |(_, n)| n);
            (body[..end].to_string(), next)
        } else {
            let v = arg.trim_end();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            (v.to_string(), after)
        };
        match cmd.to_ascii_uppercase().as_str() {
            "FROM" => mf.from = Some(value.trim().to_string()),
            "PARAMETER" => {
                let (k, v) = value
                    .trim()
                    .split_once(char::is_whitespace)
                    .ok_or_else(|| {
                        HttpError::bad_request(format!("PARAMETER {value} has no value"))
                    })?;
                let v = v.trim();
                let v = v
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(v);
                mf.parameters.push((k.to_ascii_lowercase(), v.to_string()));
            }
            "TEMPLATE" => mf.template = Some(value),
            "SYSTEM" => mf.system = Some(value),
            "LICENSE" => mf.license.push(value),
            other @ ("ADAPTER" | "MESSAGE" | "REQUIRES" | "RENDERER" | "PARSER") => {
                return Err(HttpError::bad_request(format!(
                    "the Modelfile instruction {other} is not supported"
                )));
            }
            other => {
                return Err(HttpError::bad_request(format!(
                    "unknown Modelfile instruction {other}"
                )));
            }
        }
        rest = next;
    }
    Ok(mf)
}

/// Modelfile `PARAMETER` strings as Ollama's params JSON: numbers and booleans typed,
/// `stop` collected into an array (it may repeat).
fn parameters_json(params: &[(String, String)]) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in params {
        if k == "stop" {
            let entry = out.entry(k.clone()).or_insert_with(|| json!([]));
            if let Value::Array(a) = entry {
                a.push(json!(v));
            }
            continue;
        }
        let typed = if let Ok(i) = v.parse::<i64>() {
            json!(i)
        } else if let Ok(f) = v.parse::<f64>() {
            json!(f)
        } else if let Ok(b) = v.parse::<bool>() {
            json!(b)
        } else {
            json!(v)
        };
        out.insert(k.clone(), typed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modelfile_subset_parses() {
        let mf = parse_modelfile(
            "# comment\nFROM llama3.2\nPARAMETER temperature 0.2\nparameter stop \"<|end|>\"\n\
             PARAMETER stop </s>\nPARAMETER num_ctx 4096\nSYSTEM \"\"\"You are\nterse.\"\"\"\n\
             TEMPLATE {{ .Prompt }}\nLICENSE MIT\n",
        )
        .unwrap();
        assert_eq!(mf.from.as_deref(), Some("llama3.2"));
        assert_eq!(mf.system.as_deref(), Some("You are\nterse."));
        assert_eq!(mf.template.as_deref(), Some("{{ .Prompt }}"));
        assert_eq!(mf.license, vec!["MIT".to_string()]);
        let p = parameters_json(&mf.parameters);
        assert_eq!(p["temperature"], json!(0.2));
        assert_eq!(p["num_ctx"], json!(4096));
        assert_eq!(p["stop"], json!(["<|end|>", "</s>"]));
    }

    #[test]
    fn modelfile_refuses_unsupported_instructions() {
        assert!(parse_modelfile("FROM x\nADAPTER ./lora.gguf\n").is_err());
        assert!(parse_modelfile("FROM x\nBOGUS y\n").is_err());
        assert!(parse_modelfile("FROM x\nSYSTEM \"\"\"never closed\n").is_err());
    }

    #[test]
    fn create_plan_prefers_files_and_structured_fields() {
        let d = format!("sha256:{}", "ab".repeat(32));
        let req: CreateRequest = serde_json::from_value(json!({
            "model": "mine", "files": {"m.gguf": d}, "system": "S",
            "modelfile": "FROM llama3.2\nSYSTEM other\nPARAMETER top_k 5",
        }))
        .unwrap();
        let plan = CreatePlan::from_request(&req).unwrap();
        assert_eq!(plan.blob.map(|b| b.to_string()), Some(d));
        assert_eq!(plan.layers.system.as_deref(), Some("S"));
        assert_eq!(plan.layers.parameters.unwrap()["top_k"], json!(5));
        let req: CreateRequest =
            serde_json::from_value(json!({"model": "m", "from": "./x.gguf"})).unwrap();
        assert!(CreatePlan::from_request(&req).is_err());
    }
}
