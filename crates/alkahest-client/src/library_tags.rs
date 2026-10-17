//! Tags of an Ollama library model (`GET /alkahest/v1/ollama/tags`, ALK-US-0099): the
//! quantisations and sizes the Discover screen's tag picker offers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rest::take;
use crate::{Client, Result};

/// One tag of a library model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryTag {
    /// Pullable reference (`smollm2:135m-instruct-q4_K_M`).
    pub name: String,
    /// The tag alone.
    pub tag: String,
    /// Short manifest digest (12 hex digits); tags sharing it are aliases.
    pub digest: Option<String>,
    /// Download size as the site shows it (`105MB`).
    pub size: Option<String>,
    /// Download size in bytes.
    pub size_bytes: Option<u64>,
    /// Context window as shown (`8K`).
    pub context: Option<String>,
    /// Input modalities (`text`, `image`).
    pub inputs: Vec<String>,
    /// Relative update time (`1 year ago`).
    pub updated: Option<String>,
    /// Quantisation named by the tag (`Q4_K_M`).
    pub quantization: Option<String>,
    /// Parameter size named by the tag (`135m`).
    pub parameters: Option<String>,
}

impl Client {
    /// `GET /alkahest/v1/ollama/tags?name=`: the tags of library model `name`.
    pub async fn ollama_tags(&self, name: &str) -> Result<Vec<LibraryTag>> {
        let v: Value = self
            .call_json(
                reqwest::Method::GET,
                "/alkahest/v1/ollama/tags",
                &[("name", name.to_string())],
                None,
            )
            .await?;
        take(v, "tags")
    }
}
