//! Memory estimate of a Hugging Face quant before it is downloaded
//! (`GET /alkahest/v1/hf/estimate`, ALK-US-0104).

use serde_json::Value;

use crate::{Client, Result};

/// Which Hub file to estimate: one file name, or a quantisation of the repository (the
/// server's default preference when both are `None`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HfQuant<'a> {
    /// Repository id (`org/name`).
    pub repo: &'a str,
    /// File name (any shard of a split model).
    pub file: Option<&'a str>,
    /// Quantisation (`Q4_K_M`).
    pub quant: Option<&'a str>,
    /// Revision; `main` when `None`.
    pub revision: Option<&'a str>,
}

impl Client {
    /// `GET /alkahest/v1/hf/estimate`: the same document as [`Client::estimate_memory`]
    /// (`estimate`, `fits`, free VRAM, available RAM) plus `files` and `download_bytes`.
    /// `overrides` are `(name, value)` pairs: `context_length`, `num_parallel`,
    /// `kv_cache_type_k`, `kv_cache_type_v`, `flash_attention`, `gpu_layers`.
    pub async fn hf_estimate(&self, q: HfQuant<'_>, overrides: &[(&str, String)]) -> Result<Value> {
        let mut query: Vec<(&str, String)> = vec![("repo", q.repo.to_string())];
        if let Some(f) = q.file {
            query.push(("file", f.to_string()));
        }
        if let Some(v) = q.quant {
            query.push(("quant", v.to_string()));
        }
        if let Some(r) = q.revision {
            query.push(("revision", r.to_string()));
        }
        query.extend(overrides.iter().map(|(k, v)| (*k, v.clone())));
        self.call_json(
            reqwest::Method::GET,
            "/alkahest/v1/hf/estimate",
            &query,
            None,
        )
        .await
    }
}
