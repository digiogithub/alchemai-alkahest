//! Recent inference requests for the runtime monitor (ALK-US-0098): a bounded ring of what
//! each request did, never what it said.
//!
//! [`feed_layer`] wraps every inference route (Ollama, OpenAI, Anthropic, decisions; `POST`
//! only, so listings and probes stay out). It opens a [`RequestRecord`] with the time, route,
//! caller (key id and name, or local / anonymous) and makes it the request's current entry
//! through a task-local, which [`crate::generation::acquire`] and [`crate::service::UsageSink`]
//! fill in with the model and token counts. The record is finished when the response body is
//! done or dropped, so a streamed generation reports its whole duration, and is then pushed to
//! the ring and broadcast (the `request` events of `/alkahest/v1/events`). Prompts, messages
//! and outputs are never captured.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::auth::{Caller, RequestContext};
use crate::dialect::Family;
use crate::service::now_rfc3339;
use crate::state::AppState;

/// Finished requests kept for `GET /alkahest/v1/requests`.
pub const FEED_CAPACITY: usize = 500;

/// One inference request as the monitor shows it. Contains no request or response content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRecord {
    /// Increasing id.
    pub id: u64,
    /// When the request arrived, RFC 3339.
    pub at: String,
    /// HTTP method.
    pub method: String,
    /// Route path (no query string).
    pub route: String,
    /// API family (`ollama`, `openai`, `anthropic`, `decisions`).
    pub family: String,
    /// Model, once the handler resolved it.
    pub model: Option<String>,
    /// Caller kind: `key`, `local` or `anonymous`.
    pub caller: String,
    /// Public id of the API key, for key callers.
    pub key_id: Option<String>,
    /// Name of the API key, for key callers.
    pub key_name: Option<String>,
    /// Prompt tokens reported by the engine.
    pub prompt_tokens: u64,
    /// Generated tokens reported by the engine.
    pub completion_tokens: u64,
    /// HTTP status of the response.
    pub status: u16,
    /// Time from arrival to the end of the response body, milliseconds.
    pub duration_ms: u64,
    /// The client went away before the response body ended.
    pub aborted: bool,
}

/// The ring of recent requests and its broadcast channel.
#[derive(Debug)]
pub struct RequestFeed {
    ring: Mutex<VecDeque<RequestRecord>>,
    next_id: AtomicU64,
    tx: broadcast::Sender<RequestRecord>,
}

impl Default for RequestFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestFeed {
    /// An empty feed.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            ring: Mutex::new(VecDeque::with_capacity(FEED_CAPACITY)),
            next_id: AtomicU64::new(1),
            tx,
        }
    }

    /// Finished requests, oldest first, at most `limit`, optionally only those that arrived
    /// within `within` of now.
    pub fn recent(&self, limit: usize, within: Option<Duration>) -> Vec<RequestRecord> {
        let ring = self.ring.lock();
        let cutoff =
            within.and_then(|w| time::OffsetDateTime::now_utc().checked_sub(w.try_into().ok()?));
        let rows: Vec<RequestRecord> = ring
            .iter()
            .filter(|r| match cutoff {
                None => true,
                Some(c) => time::OffsetDateTime::parse(
                    &r.at,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok_and(|t| t >= c),
            })
            .cloned()
            .collect();
        rows[rows.len().saturating_sub(limit)..].to_vec()
    }

    /// Every finished request as it completes.
    pub fn subscribe(&self) -> broadcast::Receiver<RequestRecord> {
        self.tx.subscribe()
    }

    fn push(&self, r: RequestRecord) {
        {
            let mut ring = self.ring.lock();
            if ring.len() >= FEED_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(r.clone());
        }
        let _ = self.tx.send(r);
    }
}

/// The record of the request being handled, shared by the middleware, the handler and the
/// response body.
#[derive(Debug)]
pub struct FeedEntry {
    record: Mutex<RequestRecord>,
    started: Instant,
    feed: Arc<RequestFeed>,
    done: std::sync::atomic::AtomicBool,
}

impl FeedEntry {
    /// Notes the model the request uses.
    pub fn set_model(&self, model: &str) {
        let mut r = self.record.lock();
        if r.model.is_none() {
            r.model = Some(model.to_string());
        }
    }

    /// Adds token counts.
    pub fn add_tokens(&self, prompt: u64, completion: u64) {
        let mut r = self.record.lock();
        r.prompt_tokens = r.prompt_tokens.saturating_add(prompt);
        r.completion_tokens = r.completion_tokens.saturating_add(completion);
    }

    fn finish(&self, aborted: bool) {
        if self.done.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut r = self.record.lock().clone();
        r.duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        r.aborted = aborted;
        self.feed.push(r);
    }
}

tokio::task_local! {
    static CURRENT: Arc<FeedEntry>;
}

/// The entry of the request whose handler is running on this task, if it is recorded.
pub fn current() -> Option<Arc<FeedEntry>> {
    CURRENT.try_with(Arc::clone).ok()
}

/// Whether a request is an inference call worth showing.
fn recorded(method: &Method, family: Family, path: &str) -> bool {
    method == Method::POST
        && matches!(
            family,
            Family::Ollama
                | Family::OpenAi
                | Family::Anthropic
                | Family::Decisions
                | Family::SystemOne
        )
        && !path.starts_with("/api/blobs/")
}

fn family_name(f: Family) -> &'static str {
    match f {
        Family::Ollama => "ollama",
        Family::OpenAi => "openai",
        Family::Anthropic => "anthropic",
        Family::Decisions | Family::SystemOne => "decisions",
        Family::Admin => "admin",
        Family::Base | Family::Other => "other",
    }
}

/// Middleware recording inference requests in the state's [`RequestFeed`].
pub async fn feed_layer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let family = Family::of_path(&path);
    if !recorded(req.method(), family, &path) {
        return next.run(req).await;
    }
    let (caller, key_id, key_name) =
        match req.extensions().get::<RequestContext>().map(|c| &c.caller) {
            Some(Caller::Key(p)) => ("key", Some(p.key_id.clone()), Some(p.name.clone())),
            Some(Caller::Anonymous) => ("anonymous", None, None),
            Some(Caller::Local) | None => ("local", None, None),
        };
    let feed = state.requests();
    let entry = Arc::new(FeedEntry {
        record: Mutex::new(RequestRecord {
            id: feed.next_id.fetch_add(1, Ordering::Relaxed),
            at: now_rfc3339(),
            method: req.method().to_string(),
            route: path,
            family: family_name(family).into(),
            caller: caller.into(),
            key_id,
            key_name,
            ..RequestRecord::default()
        }),
        started: Instant::now(),
        feed,
        done: std::sync::atomic::AtomicBool::new(false),
    });
    let resp = CURRENT.scope(Arc::clone(&entry), next.run(req)).await;
    entry.record.lock().status = resp.status().as_u16();
    let (parts, body) = resp.into_parts();
    Response::from_parts(
        parts,
        Body::new(FeedBody {
            inner: body,
            entry: Some(entry),
        }),
    )
}

/// Response body that finishes its [`FeedEntry`] when it ends (or is dropped: aborted).
struct FeedBody {
    inner: Body,
    entry: Option<Arc<FeedEntry>>,
}

impl http_body::Body for FeedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let polled = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(None) = &polled
            && let Some(e) = self.entry.take()
        {
            e.finish(false);
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for FeedBody {
    fn drop(&mut self) {
        if let Some(e) = self.entry.take() {
            // A body that reports its end without a final poll (empty bodies) is complete.
            let aborted = !http_body::Body::is_end_stream(&self.inner);
            e.finish(aborted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_is_bounded_and_filters_by_age() {
        let feed = RequestFeed::new();
        for i in 0..(FEED_CAPACITY as u64 + 3) {
            feed.push(RequestRecord {
                id: i,
                at: now_rfc3339(),
                ..RequestRecord::default()
            });
        }
        let all = feed.recent(usize::MAX, None);
        assert_eq!(all.len(), FEED_CAPACITY);
        assert_eq!(all[0].id, 3);
        assert_eq!(feed.recent(2, None).len(), 2);
        assert_eq!(feed.recent(10, Some(Duration::from_secs(60))).len(), 10);
        feed.push(RequestRecord {
            id: 9999,
            at: "2000-01-01T00:00:00Z".into(),
            ..RequestRecord::default()
        });
        assert!(
            feed.recent(usize::MAX, Some(Duration::from_secs(60)))
                .iter()
                .all(|r| r.id != 9999)
        );
    }

    #[test]
    fn only_inference_posts_are_recorded() {
        assert!(recorded(&Method::POST, Family::Ollama, "/api/chat"));
        assert!(!recorded(&Method::GET, Family::Ollama, "/api/tags"));
        assert!(!recorded(
            &Method::POST,
            Family::Admin,
            "/alkahest/v1/unload"
        ));
        assert!(!recorded(
            &Method::POST,
            Family::Ollama,
            "/api/blobs/sha256:00"
        ));
    }
}
