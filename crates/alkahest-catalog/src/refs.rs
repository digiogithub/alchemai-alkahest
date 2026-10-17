//! Ref resolution over git smart HTTP without git (ALK-SP-0011.R11): protocol v2 `ls-refs`
//! with `ref-prefix`, falling back to the v0 `info/refs` advertisement.
//!
//! Protocol references: <https://git-scm.com/docs/protocol-v2> (`ls-refs`) and
//! <https://git-scm.com/docs/http-protocol> (smart HTTP, v0 advertisement), both pkt-line
//! framed (<https://git-scm.com/docs/protocol-common>).

use reqwest::Method;

use alkahest_core::config::is_commit_id;

use crate::error::{CatalogError, Result};
use crate::hosts::redact_url;
use crate::http::{AuthStyle, Credential, HttpClient};

/// Bound of a ref advertisement (a v0 advertisement of a big repository lists every PR ref).
const REFS_LIMIT: u64 = 16 * 1024 * 1024;

/// One pkt-line with `payload`.
fn pkt(payload: &str) -> String {
    format!("{:04x}{payload}", payload.len() + 4)
}

/// The v2 `ls-refs` request body for `reference`.
pub(crate) fn ls_refs_request(reference: &str) -> String {
    let mut body = String::new();
    body.push_str(&pkt("command=ls-refs\n"));
    body.push_str(&pkt(&format!(
        "agent=alkahest/{}\n",
        env!("CARGO_PKG_VERSION")
    )));
    body.push_str("0001");
    body.push_str(&pkt("peel\n"));
    body.push_str(&pkt(&format!("ref-prefix refs/heads/{reference}\n")));
    body.push_str(&pkt(&format!("ref-prefix refs/tags/{reference}\n")));
    body.push_str("0000");
    body
}

/// Splits a pkt-line stream into payloads; flush/delimiter packets become `None`.
fn pkt_lines(data: &[u8]) -> std::result::Result<Vec<Option<String>>, String> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        if rest.len() < 4 {
            return Err("truncated pkt-line".into());
        }
        let len_text = std::str::from_utf8(&rest[..4]).map_err(|_| "bad pkt-line length")?;
        let len = usize::from_str_radix(len_text, 16).map_err(|_| "bad pkt-line length")?;
        if len < 4 {
            out.push(None);
            rest = &rest[4..];
            continue;
        }
        if rest.len() < len {
            return Err("truncated pkt-line".into());
        }
        let payload = String::from_utf8_lossy(&rest[4..len]).into_owned();
        out.push(Some(payload));
        rest = &rest[len..];
    }
    Ok(out)
}

/// A ref and its commit: `(name, oid, peeled oid)`.
type RefLine = (String, String, Option<String>);

fn is_oid(s: &str) -> bool {
    is_commit_id(s)
}

/// Parses a v2 `ls-refs` answer: `<oid> <name>[ symref-target:..][ peeled:<oid>]`.
pub(crate) fn parse_v2(data: &[u8]) -> std::result::Result<Vec<RefLine>, String> {
    let mut refs = Vec::new();
    for line in pkt_lines(data)?.into_iter().flatten() {
        let line = line.trim_end_matches('\n');
        let mut parts = line.split(' ');
        let (Some(oid), Some(name)) = (parts.next(), parts.next()) else {
            return Err(format!("unexpected line `{line}`"));
        };
        if !is_oid(oid) {
            return Err(format!("unexpected line `{line}`"));
        }
        let peeled = parts
            .find_map(|a| a.strip_prefix("peeled:"))
            .filter(|p| is_oid(p))
            .map(str::to_string);
        refs.push((name.to_string(), oid.to_string(), peeled));
    }
    Ok(refs)
}

/// Parses a v0 advertisement (`# service=git-upload-pack`, flush, `<oid> <name>\0caps`,
/// `<oid> <name>`, peeled tags as `<name>^{}`).
pub(crate) fn parse_v0(data: &[u8]) -> std::result::Result<Vec<RefLine>, String> {
    let mut refs: Vec<RefLine> = Vec::new();
    for line in pkt_lines(data)?.into_iter().flatten() {
        if line.starts_with('#') {
            continue;
        }
        let line = line
            .split('\0')
            .next()
            .unwrap_or_default()
            .trim_end_matches('\n');
        let Some((oid, name)) = line.split_once(' ') else {
            return Err(format!("unexpected line `{line}`"));
        };
        if !is_oid(oid) {
            return Err(format!("unexpected line `{line}`"));
        }
        if let Some(base) = name.strip_suffix("^{}") {
            if let Some(r) = refs.iter_mut().find(|r| r.0 == base) {
                r.2 = Some(oid.to_string());
            }
            continue;
        }
        refs.push((name.to_string(), oid.to_string(), None));
    }
    Ok(refs)
}

/// The commit of `reference`: `refs/heads/<ref>` first, then the peeled commit of
/// `refs/tags/<ref>`.
pub(crate) fn pick(refs: &[RefLine], reference: &str) -> Option<String> {
    let head = format!("refs/heads/{reference}");
    let tag = format!("refs/tags/{reference}");
    refs.iter()
        .find(|r| r.0 == head)
        .map(|r| r.1.clone())
        .or_else(|| {
            refs.iter()
                .find(|r| r.0 == tag)
                .map(|r| r.2.clone().unwrap_or_else(|| r.1.clone()))
        })
}

/// Resolves `reference` on the repository at `url` to a full commit id. A full commit id is
/// returned as is, without a request.
///
/// # Errors
///
/// [`CatalogError::RefNotFound`], or the transport error of the last attempt.
pub(crate) async fn resolve(
    http: &HttpClient,
    catalog: &str,
    url: &str,
    reference: &str,
    cred: Option<&Credential>,
) -> Result<String> {
    if is_commit_id(reference) {
        return Ok(reference.to_string());
    }
    let base = url.trim_end_matches('/');
    let v2 = http
        .send(
            Method::POST,
            &format!("{base}/git-upload-pack"),
            cred,
            AuthStyle::Basic,
            &[
                ("Git-Protocol", "version=2"),
                ("Content-Type", "application/x-git-upload-pack-request"),
                ("Accept", "application/x-git-upload-pack-result"),
            ],
            Some(ls_refs_request(reference).as_bytes()),
            REFS_LIMIT,
        )
        .await;
    let v2_refs = match &v2 {
        Ok(body) => parse_v2(body).ok(),
        Err(e) => {
            if matches!(
                e,
                CatalogError::Status {
                    status: 401 | 403,
                    ..
                }
            ) {
                // Authentication fails the same way on v0; do not ask twice.
                return Err(v2
                    .err()
                    .unwrap_or_else(|| CatalogError::Config(String::new())));
            }
            None
        }
    };
    if let Some(refs) = v2_refs {
        return pick(&refs, reference).ok_or_else(|| CatalogError::RefNotFound {
            catalog: catalog.to_string(),
            reference: reference.to_string(),
            url: redact_url(url),
        });
    }
    tracing::debug!(url = %redact_url(url), "ls-refs v2 unavailable; trying the v0 advertisement");
    let adv_url = format!("{base}/info/refs?service=git-upload-pack");
    let body = http
        .get(&adv_url, cred, AuthStyle::Basic, REFS_LIMIT)
        .await?;
    let refs = parse_v0(&body).map_err(|reason| CatalogError::Protocol {
        url: redact_url(&adv_url),
        reason,
    })?;
    pick(&refs, reference).ok_or_else(|| CatalogError::RefNotFound {
        catalog: catalog.to_string(),
        reference: reference.to_string(),
        url: redact_url(url),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29";
    const B: &str = "902161a0000000000000000000000000000000ff";

    #[test]
    fn request_is_pkt_framed() {
        let body = ls_refs_request("main");
        assert!(body.starts_with("0014command=ls-refs\n"));
        assert!(body.contains("00010009peel\n"));
        assert!(body.ends_with("ref-prefix refs/tags/main\n0000"));
    }

    #[test]
    fn v2_answer_parses() {
        let data = format!(
            "{}{}0000",
            pkt(&format!("{A} refs/heads/main\n")),
            pkt(&format!("{B} refs/tags/v1 peeled:{A}\n"))
        );
        let refs = parse_v2(data.as_bytes()).unwrap_or_default();
        assert_eq!(pick(&refs, "main").as_deref(), Some(A));
        assert_eq!(pick(&refs, "v1").as_deref(), Some(A));
        assert_eq!(pick(&refs, "nope"), None);
    }

    #[test]
    fn v0_fallback() {
        let data = format!(
            "{}0000{}{}{}0000",
            pkt("# service=git-upload-pack\n"),
            pkt(&format!(
                "{B} HEAD\0multi_ack symref=HEAD:refs/heads/main\n"
            )),
            pkt(&format!("{A} refs/heads/main-old\n")),
            pkt(&format!("{B} refs/tags/v2\n")),
        ) + &pkt(&format!("{A} refs/tags/v2^{{}}\n"));
        let refs = parse_v0(data.as_bytes()).unwrap_or_default();
        // `main-old` is not `main`; the tag is peeled.
        assert_eq!(pick(&refs, "main"), None);
        assert_eq!(pick(&refs, "v2").as_deref(), Some(A));
    }

    #[test]
    fn a_v0_answer_is_not_mistaken_for_v2() {
        let data = format!("{}0000", pkt("# service=git-upload-pack\n"));
        assert!(parse_v2(data.as_bytes()).is_err());
    }
}
