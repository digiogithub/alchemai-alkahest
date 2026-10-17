//! GGUF headers of files on the Hub, read with HTTP range requests (ALK-US-0104), so a
//! quantisation can be sized before it is downloaded.
//!
//! The header (metadata and tensor table) sits at the start of the file and is usually a few
//! MB (the tokenizer vocabulary dominates). [`HfClient::gguf_header`] asks for the first
//! [`FIRST_RANGE`] bytes and grows the range fourfold while the parser runs out of bytes,
//! up to [`MAX_HEADER`]. Split models (`-00001-of-00003.gguf`) carry their own tensor table
//! in every shard; [`HfClient::gguf_header_split`] reads each and merges the tensors so the
//! weight sizes cover the whole model.

use std::io::{Cursor, ErrorKind};

use alkahest_gguf::{Gguf, GgufError};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT_ENCODING, RANGE};
use url::Url;

use crate::client::HfClient;
use crate::error::{HfError, Result};

/// Bytes asked for first.
pub const FIRST_RANGE: u64 = 2 << 20;

/// Largest header read before giving up (a 256k-token vocabulary is ~10 MB).
pub const MAX_HEADER: u64 = 128 << 20;

impl HfClient {
    /// Bytes `start..start + len` of `file` (fewer at the end of the file). A server that
    /// ignores `Range` answers the whole file; only the requested prefix is kept, and the
    /// body is read no further than needed.
    pub async fn file_range(
        &self,
        repo: &str,
        revision: Option<&str>,
        file: &str,
        start: u64,
        len: u64,
    ) -> Result<Vec<u8>> {
        let url = Url::parse(&self.resolve_url(repo, revision, file)?)
            .map_err(|e| HfError::InvalidUrl(e.to_string()))?;
        let end = start.saturating_add(len).saturating_sub(1);
        let mut req = self
            .http()
            .get(url.clone())
            .header(RANGE, format!("bytes={start}-{end}"))
            // Byte offsets must refer to the file, not a compressed transfer.
            .header(ACCEPT_ENCODING, "identity");
        if let Some(t) = self.bearer_for(url.as_str()) {
            req = req.bearer_auth(t);
        }
        let mut resp = crate::client::map_status(req.send().await?, file)?;
        let whole = resp.status() != StatusCode::PARTIAL_CONTENT;
        let skip = if whole { start } else { 0 };
        let want = usize::try_from(len).unwrap_or(usize::MAX);
        let mut out = Vec::with_capacity(want.min(16 << 20));
        let mut seen: u64 = 0;
        while let Some(chunk) = resp.chunk().await? {
            let c = chunk.len() as u64;
            let from = skip.saturating_sub(seen).min(c);
            seen += c;
            out.extend_from_slice(&chunk[usize::try_from(from).unwrap_or(0)..]);
            if out.len() >= want {
                out.truncate(want);
                break;
            }
        }
        Ok(out)
    }

    /// The GGUF header of one file on the Hub.
    pub async fn gguf_header(
        &self,
        repo: &str,
        revision: Option<&str>,
        file: &str,
    ) -> Result<Gguf> {
        let mut len = FIRST_RANGE;
        loop {
            let bytes = self.file_range(repo, revision, file, 0, len).await?;
            let got = bytes.len() as u64;
            match Gguf::read(Cursor::new(bytes)) {
                Ok(g) => return Ok(g),
                Err(GgufError::Io(e)) if e.kind() == ErrorKind::UnexpectedEof && got == len => {
                    if len >= MAX_HEADER {
                        return Err(HfError::InvalidGguf(format!(
                            "{file}: the header is larger than {} MB",
                            MAX_HEADER >> 20
                        )));
                    }
                    len = len.saturating_mul(4).min(MAX_HEADER);
                }
                Err(e) => return Err(HfError::InvalidGguf(format!("{file}: {e}"))),
            }
        }
    }

    /// The header of a model given as its files in shard order: the first shard's metadata
    /// with the tensors of every shard (one file: just its header).
    pub async fn gguf_header_split(
        &self,
        repo: &str,
        revision: Option<&str>,
        files: &[&str],
    ) -> Result<Gguf> {
        let Some((first, rest)) = files.split_first() else {
            return Err(HfError::NotFound(format!("{repo}: no GGUF file")));
        };
        let mut g = self.gguf_header(repo, revision, first).await?;
        for f in rest {
            let shard = self.gguf_header(repo, revision, f).await?;
            g.tensors.extend(shard.tensors);
        }
        Ok(g)
    }
}
