//! Screen 16b, Settings › Catalogs (ALK-US-0122, ALK-SP-0011), and the catalog state other
//! screens read: the listing, the running sync jobs and the sync-failed banner (17.17).
//!
//! One [`CatalogsView`] lives per connection. It reads `GET /alkahest/v1/catalogs`, follows the
//! `catalog_sync` jobs and the `catalog` event on the server's event stream, and draws the
//! Catalogs tab: one card per catalog in layer order (Sync now, Edit…, Remove, enable
//! toggle), the snapshot limits, the recipe layers and the private catalog tokens, with the
//! Add catalog (17.15), Edit, token and Remove catalog (17.16) dialogs. Discover and Engines
//! hold the entity to read the listing, show the banner and start syncs. Catalog actions apply
//! at once; there is no Save.
//!
//! Tokens are typed into masked fields, read once when sent (the field is cleared at that
//! moment) and never shown, stored or logged by the app; the server keeps them in its keyring.
//! The rules live in [`crate::catalogs`] (GPUI-free, unit tested).

use std::collections::HashSet;
use std::time::Duration;

use alkahest_client::admin::Job;
use alkahest_client::{CatalogEvent, CatalogListing, CatalogPatch, Client, ClientError};
use futures::StreamExt as _;
use time::OffsetDateTime;

use crate::catalogs::{
    AddForm, BannerState, EditForm, Field, FieldError, LIMIT_KEYS, RemoveAsk, Status, SyncJobs,
    enabled_count, facts_line, failed, failure_note, field_of_error, format_iec, limit_of_error,
    limits_patch, patch_needs_sync, recipe_layers, source_parts, status, sync_scope, token_chip,
};
use crate::connection::{ConnectionState, Snapshot};
use crate::discover::Fetch;
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::input::{Input, InputEvent, InputState};
use crate::ui::{
    AnyElement, AppContext as _, Context, Div, Entity, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Task, Window, div, h_flex, px, v_flex,
};
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, card, checkbox, dialog, dialog_sized, dismissable, field_frame,
    field_label, meter, mono_caption, notice_line, section_tab, status_pill, tag,
};

/// Pause before re-subscribing to the event stream after it broke.
const FEED_RETRY: Duration = Duration::from_secs(3);
/// Width of the side column (Recipe layers, Private catalogs).
const SIDE_WIDTH: f32 = 360.0;
/// Width of the Add catalog dialog (17.15).
const ADD_WIDTH: f32 = 480.0;

/// What the catalog state asks of the shell and the screens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogsEvent {
    /// Settings: the General tab was clicked.
    ShowGeneral,
    /// Show Settings › Catalogs (from the Discover pill, a banner or an empty state).
    Open,
    /// A sync finished or a catalog changed: catalog searches and engine recipes may differ.
    Changed,
}

enum Feed {
    Jobs(Vec<Job>),
    Job(Box<Job>),
    Catalog(CatalogEvent),
}

struct AddDialog {
    name: Entity<InputState>,
    url: Entity<InputState>,
    r#ref: Entity<InputState>,
    subdir: Entity<InputState>,
    interval: Entity<InputState>,
    pin: Entity<InputState>,
    token: Entity<InputState>,
    error: Option<FieldError>,
    /// The server refused the name: a token of another host is stored under it (409).
    conflict: Option<String>,
    busy: bool,
}

struct EditDialog {
    name: String,
    r#ref: Entity<InputState>,
    subdir: Entity<InputState>,
    interval: Entity<InputState>,
    pin: Entity<InputState>,
    error: Option<FieldError>,
    busy: bool,
}

struct TokenDialog {
    name: String,
    input: Entity<InputState>,
    replace: bool,
    error: Option<String>,
    busy: bool,
}

/// Outcome of "Add & sync".
enum AddOutcome {
    Added {
        name: String,
        job: Option<Box<Job>>,
        token_error: Option<String>,
    },
    Conflict(String),
    Failed(String),
}

/// Settings › Catalogs and the shared catalog state of one connection.
pub struct CatalogsView {
    client: Client,
    connected: bool,
    listing: Fetch<CatalogListing>,
    sync: SyncJobs,
    banner: BannerState,
    notice: Option<(bool, String)>,
    feed: Option<Task<()>>,
    add: Option<AddDialog>,
    edit: Option<EditDialog>,
    remove: Option<RemoveAsk>,
    token: Option<TokenDialog>,
    limits_open: bool,
    limit_inputs: [Entity<InputState>; 3],
    limits_error: Option<(usize, String)>,
    limits_busy: bool,
    /// The limit fields are filled from the next listing (needs the window: done in render).
    fill_limits: bool,
    warnings_open: HashSet<String>,
    /// Catalogs with a request in flight (toggle, remove).
    pending: HashSet<String>,
}

impl std::fmt::Debug for CatalogsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogsView")
            .field("connected", &self.connected)
            .field("catalogs", &self.listing().map(|l| l.catalogs.len()))
            .finish_non_exhaustive()
    }
}

impl EventEmitter<CatalogsEvent> for CatalogsView {}

impl CatalogsView {
    /// The catalog state of a connection through `client`.
    pub fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let limit_inputs = [0, 1, 2].map(|_| cx.new(|cx| InputState::new(window, cx)));
        for input in &limit_inputs {
            cx.subscribe_in(input, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.apply_limits(cx);
                }
            })
            .detach();
        }
        Self {
            client,
            connected: false,
            listing: Fetch::Idle,
            sync: SyncJobs::default(),
            banner: BannerState::default(),
            notice: None,
            feed: None,
            add: None,
            edit: None,
            remove: None,
            token: None,
            limits_open: false,
            limit_inputs,
            limits_error: None,
            limits_busy: false,
            fill_limits: true,
            warnings_open: HashSet::new(),
            pending: HashSet::new(),
        }
    }

    /// The catalogs, once read.
    pub fn listing(&self) -> Option<&CatalogListing> {
        self.listing.ready()
    }

    /// Whether any sync runs.
    pub fn any_syncing(&self) -> bool {
        self.sync.any_running()
    }

    /// Follows a connection snapshot: on connect, reads the catalogs and subscribes to the job
    /// and catalog events; on disconnect, drops the subscription.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let connected = matches!(snapshot.state, ConnectionState::Connected(_));
        if connected == self.connected {
            return;
        }
        self.connected = connected;
        if connected {
            self.reload(cx);
            self.follow(cx);
        } else {
            self.feed = None;
        }
        cx.notify();
    }

    fn call<R: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        fut: impl std::future::Future<Output = Result<R, ClientError>> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<R, String>, &mut Context<Self>) + 'static,
    ) {
        let task = tokio_bridge::spawn(cx, fut);
        cx.spawn(async move |this, cx| {
            let r = match task.await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(e) => Err(format!("the request was cancelled: {e}")),
            };
            let _ = this.update(cx, |v, cx| {
                done(v, r, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the catalog listing again.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.listing == Fetch::Idle {
            self.listing = Fetch::Loading;
        }
        let c = self.client.clone();
        self.call(cx, async move { c.catalogs().await }, |v, r, _| match r {
            Ok(l) => {
                v.banner.on_listing(&l);
                v.listing = Fetch::Ready(l);
            }
            Err(e) => {
                // Keep the last listing on a transient failure.
                if v.listing.ready().is_none() {
                    v.listing = Fetch::Failed(e);
                }
            }
        });
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Feed>();
        let pump = tokio_bridge::spawn(cx, async move {
            loop {
                if let Ok(jobs) = client.jobs().await
                    && tx.send(Feed::Jobs(jobs)).is_err()
                {
                    return;
                }
                if let Ok(mut events) = client.events().await {
                    while let Some(ev) = events.next().await {
                        let Ok(ev) = ev else { break };
                        let msg = if let Some(Ok(job)) = ev.job() {
                            if sync_scope(&job).is_none() {
                                continue;
                            }
                            Feed::Job(Box::new(job))
                        } else if let Some(Ok(cat)) = ev.catalog() {
                            Feed::Catalog(cat)
                        } else {
                            continue;
                        };
                        if tx.send(msg).is_err() {
                            return;
                        }
                    }
                }
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(FEED_RETRY).await;
            }
        });
        self.feed = Some(cx.spawn(async move |this, cx| {
            let _pump = pump;
            while let Some(msg) = rx.recv().await {
                if this.update(cx, |v, cx| v.on_feed(msg, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn on_feed(&mut self, msg: Feed, cx: &mut Context<Self>) {
        match msg {
            Feed::Jobs(jobs) => self.sync.replace_all(&jobs),
            Feed::Job(job) => self.track(&job, cx),
            Feed::Catalog(ev) => {
                self.banner.on_event(&ev);
                self.reload(cx);
            }
        }
        cx.notify();
    }

    fn track(&mut self, job: &Job, cx: &mut Context<Self>) {
        if self.sync.upsert(job) {
            if job.state == "failed"
                && let Some(e) = &job.error
            {
                self.notice = Some((true, format!("Sync failed: {e}")));
            } else if job.state == "completed"
                && self
                    .notice
                    .as_ref()
                    .is_some_and(|(_, n)| n.ends_with("syncing\u{2026}"))
            {
                self.notice = Some((false, "Sync finished.".into()));
            }
            self.reload(cx);
            cx.emit(CatalogsEvent::Changed);
        }
    }

    /// Starts a sync of every enabled catalog ("Sync all").
    pub fn sync_all(&mut self, cx: &mut Context<Self>) {
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.sync_catalogs().await },
            |v, r, cx| match r {
                Ok(started) => v.track(&started.job, cx),
                Err(e) => v.notice = Some((true, format!("Cannot start the sync: {e}"))),
            },
        );
    }

    /// Starts a sync of one catalog ("Sync now", "Retry").
    pub fn sync_one(&mut self, name: String, cx: &mut Context<Self>) {
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.sync_catalog(&name).await },
            |v, r, cx| match r {
                Ok(started) => v.track(&started.job, cx),
                Err(e) => v.notice = Some((true, format!("Cannot start the sync: {e}"))),
            },
        );
    }

    /// Asks the shell to show Settings › Catalogs.
    pub fn request_open(&mut self, cx: &mut Context<Self>) {
        cx.emit(CatalogsEvent::Open);
    }

    /// Hides the sync-failed banner (17.17); the error stays on the card and the tab dot.
    pub fn dismiss_banner(&mut self, cx: &mut Context<Self>) {
        self.banner.dismiss();
        cx.notify();
    }

    fn set_enabled(&mut self, name: String, on: bool, never_fetched: bool, cx: &mut Context<Self>) {
        if !self.pending.insert(name.clone()) {
            return;
        }
        let c = self.client.clone();
        let n = name.clone();
        let patch = CatalogPatch {
            enabled: Some(on),
            ..CatalogPatch::default()
        };
        self.call(
            cx,
            async move { c.update_catalog(&n, &patch).await },
            move |v, r, cx| {
                v.pending.remove(&name);
                match r {
                    Ok(_) => {
                        v.reload(cx);
                        cx.emit(CatalogsEvent::Changed);
                        if on && never_fetched {
                            v.sync_one(name, cx);
                        }
                    }
                    Err(e) => v.notice = Some((true, format!("Cannot change {name}: {e}"))),
                }
            },
        );
    }

    // ------------------------------------------------------------------------------- dialogs

    fn text_input(
        window: &mut Window,
        cx: &mut Context<Self>,
        value: &str,
        hint: &str,
    ) -> Entity<InputState> {
        let (value, hint) = (value.to_string(), hint.to_string());
        cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(hint);
            s.set_value(value, window, cx);
            s
        })
    }

    /// Opens the Add catalog dialog (17.15) with the name field focused.
    pub fn open_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = Self::text_input(window, cx, "", "acme");
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("only for private repositories")
                .masked(true)
        });
        name.update(cx, |s, cx| s.focus(window, cx));
        self.add = Some(AddDialog {
            url: Self::text_input(
                window,
                cx,
                "",
                "https://git.example.com/org/alkahest-catalog",
            ),
            r#ref: Self::text_input(window, cx, "", "main"),
            subdir: Self::text_input(window, cx, "", "."),
            interval: Self::text_input(window, cx, "", "24h"),
            pin: Self::text_input(window, cx, "", "full 40-character commit id"),
            name,
            token,
            error: None,
            conflict: None,
            busy: false,
        });
        cx.notify();
    }

    fn close_add(&mut self, cx: &mut Context<Self>) {
        // Dropping the dialog drops its inputs, the token field among them.
        self.add = None;
        cx.notify();
    }

    /// "Add & sync": adds the catalog, stores the token (if one was typed) and starts the
    /// first sync. `replace_token` answers a 409 about a token of another host.
    fn submit_add(&mut self, replace_token: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.add.as_mut() else { return };
        if d.busy {
            return;
        }
        let read = |e: &Entity<InputState>, cx: &Context<Self>| e.read(cx).value().to_string();
        let form = AddForm {
            name: read(&d.name, cx),
            url: read(&d.url, cx),
            r#ref: read(&d.r#ref, cx),
            subdir: read(&d.subdir, cx),
            interval: read(&d.interval, cx),
            pin: read(&d.pin, cx),
        };
        let req = match form.request(replace_token) {
            Ok(r) => r,
            Err(e) => {
                d.error = Some(e);
                cx.notify();
                return;
            }
        };
        // The token leaves the field now: it travels only inside the request below.
        let token = read(&d.token, cx).trim().to_string();
        d.token.update(cx, |s, cx| s.set_value("", window, cx));
        d.busy = true;
        d.error = None;
        let c = self.client.clone();
        let task = tokio_bridge::spawn(cx, async move {
            let added = match c.add_catalog(&req).await {
                Ok(a) => a,
                Err(ClientError::Api {
                    status: 409,
                    message,
                }) => return AddOutcome::Conflict(message),
                Err(e) => return AddOutcome::Failed(e.to_string()),
            };
            let name = added.catalog.name.clone();
            let mut token_error = None;
            if !token.is_empty()
                && let Err(e) = c.set_catalog_token(&name, &token).await
            {
                token_error = Some(e.to_string());
            }
            drop(token);
            let job = c.sync_catalog(&name).await.ok().map(|s| Box::new(s.job));
            AddOutcome::Added {
                name,
                job,
                token_error,
            }
        });
        cx.spawn(async move |this, cx| {
            let outcome = task
                .await
                .unwrap_or_else(|e| AddOutcome::Failed(format!("the request was cancelled: {e}")));
            let _ = this.update(cx, |v, cx| {
                match outcome {
                    AddOutcome::Added {
                        name,
                        job,
                        token_error,
                    } => {
                        v.add = None;
                        v.notice = Some(match token_error {
                            Some(e) => (
                                true,
                                format!(
                                    "Catalog {name} was added, but its token was not stored: {e}"
                                ),
                            ),
                            None => (false, format!("Catalog {name} added; syncing\u{2026}")),
                        });
                        if let Some(j) = job {
                            v.track(&j, cx);
                        }
                        v.reload(cx);
                        cx.emit(CatalogsEvent::Changed);
                    }
                    AddOutcome::Conflict(message) => {
                        if let Some(d) = v.add.as_mut() {
                            d.busy = false;
                            d.conflict = Some(message);
                        }
                    }
                    AddOutcome::Failed(message) => {
                        if let Some(d) = v.add.as_mut() {
                            d.busy = false;
                            d.error = Some(FieldError {
                                field: field_of_error(&message),
                                message,
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_edit(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = self
            .listing()
            .and_then(|l| l.catalogs.iter().find(|c| c.name == name))
            .cloned()
        else {
            return;
        };
        let f = EditForm::from_info(&info);
        self.edit = Some(EditDialog {
            name: info.name.clone(),
            r#ref: Self::text_input(window, cx, &f.r#ref, "main"),
            subdir: Self::text_input(window, cx, &f.subdir, "."),
            interval: Self::text_input(window, cx, &f.interval, "24h"),
            pin: Self::text_input(window, cx, &f.pin, "blank = follow the ref"),
            error: None,
            busy: false,
        });
        cx.notify();
    }

    fn submit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.edit.as_mut() else { return };
        let Some(info) = self
            .listing
            .ready()
            .and_then(|l| l.catalogs.iter().find(|c| c.name == d.name))
            .cloned()
        else {
            return;
        };
        let read = |e: &Entity<InputState>| e.read(cx).value().to_string();
        let form = EditForm {
            r#ref: read(&d.r#ref),
            subdir: read(&d.subdir),
            interval: read(&d.interval),
            pin: read(&d.pin),
        };
        let patch = match form.patch(&info) {
            Ok(p) => p,
            Err(e) => {
                d.error = Some(e);
                cx.notify();
                return;
            }
        };
        if patch == CatalogPatch::default() {
            self.edit = None;
            cx.notify();
            return;
        }
        d.busy = true;
        let name = d.name.clone();
        let sync = patch_needs_sync(&patch) && info.enabled;
        let c = self.client.clone();
        let n = name.clone();
        self.call(
            cx,
            async move { c.update_catalog(&n, &patch).await },
            move |v, r, cx| match r {
                Ok(_) => {
                    v.edit = None;
                    v.notice = Some((false, format!("Catalog {name} changed.")));
                    v.reload(cx);
                    cx.emit(CatalogsEvent::Changed);
                    // The server does not sync after an edit of ref, subdir or pin.
                    if sync {
                        v.sync_one(name, cx);
                    }
                }
                Err(e) => {
                    if let Some(d) = v.edit.as_mut() {
                        d.busy = false;
                        d.error = Some(FieldError {
                            field: field_of_error(&e),
                            message: e,
                        });
                    }
                }
            },
        );
    }

    /// Opens the token dialog of `name` (Replace… in Private catalogs, "Set token…" of the
    /// banner).
    pub fn open_token(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let replace = self
            .listing()
            .and_then(|l| l.catalogs.iter().find(|c| c.name == name))
            .is_some_and(|c| c.token_source == "keyring");
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("access token")
                .masked(true)
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        cx.subscribe_in(&input, window, |this, _, e: &InputEvent, window, cx| {
            if matches!(e, InputEvent::PressEnter { .. }) {
                this.submit_token(window, cx);
            }
        })
        .detach();
        self.token = Some(TokenDialog {
            name: name.to_string(),
            input,
            replace,
            error: None,
            busy: false,
        });
        cx.notify();
    }

    fn submit_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.token.as_mut() else { return };
        if d.busy {
            return;
        }
        let token = d.input.read(cx).value().trim().to_string();
        if token.is_empty() {
            d.error = Some("Type the token first.".into());
            cx.notify();
            return;
        }
        // Sent once; the field is cleared before the request leaves.
        d.input.update(cx, |s, cx| s.set_value("", window, cx));
        d.busy = true;
        d.error = None;
        let name = d.name.clone();
        let c = self.client.clone();
        let n = name.clone();
        self.call(
            cx,
            async move { c.set_catalog_token(&n, &token).await },
            move |v, r, cx| match r {
                Ok(status) => {
                    v.token = None;
                    v.notice = Some((
                        false,
                        if status.shadowed_by_env {
                            format!(
                                "Token of {name} stored in the keyring; an environment variable \
                                 still takes precedence."
                            )
                        } else {
                            format!("Token of {name} stored in the server's keyring.")
                        },
                    ));
                    v.reload(cx);
                    // A failed private sync is usually waiting for this token.
                    v.sync_one(name, cx);
                }
                Err(e) => {
                    if let Some(d) = v.token.as_mut() {
                        d.busy = false;
                        d.error = Some(format!("The token was not stored: {e}"));
                    }
                }
            },
        );
        cx.notify();
    }

    fn clear_token(&mut self, name: String, cx: &mut Context<Self>) {
        let c = self.client.clone();
        let n = name.clone();
        self.call(
            cx,
            async move { c.delete_catalog_token(&n).await },
            move |v, r, cx| match r {
                Ok(_) => {
                    v.notice = Some((false, format!("Token of {name} deleted.")));
                    v.reload(cx);
                }
                Err(e) => v.notice = Some((true, format!("Cannot delete the token: {e}"))),
            },
        );
    }

    fn confirm_remove(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = self.remove.take() else {
            return;
        };
        let c = self.client.clone();
        let (name, delete_token) = (ask.name.clone(), ask.delete_token);
        self.pending.insert(name.clone());
        self.call(
            cx,
            async move { c.remove_catalog(&name, delete_token).await },
            move |v, r, cx| {
                v.pending.remove(&ask.name);
                match r {
                    Ok(done) => {
                        v.notice = Some((
                            false,
                            if done.result == "disabled" {
                                format!("Catalog {} disabled.", ask.name)
                            } else if done.token_deleted {
                                format!("Catalog {} removed with its token.", ask.name)
                            } else {
                                format!("Catalog {} removed.", ask.name)
                            },
                        ));
                        v.reload(cx);
                        cx.emit(CatalogsEvent::Changed);
                    }
                    Err(e) => {
                        v.notice = Some((true, format!("Cannot remove {}: {e}", ask.name)));
                    }
                }
            },
        );
    }

    fn apply_limits(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.listing() else { return };
        if self.limits_busy {
            return;
        }
        let texts: Vec<String> = self
            .limit_inputs
            .iter()
            .map(|i| i.read(cx).value().to_string())
            .collect();
        let change = match limits_patch(
            [&texts[0], &texts[1], &texts[2]],
            &l.limits,
            &l.default_limits,
        ) {
            Ok(ch) => ch,
            Err(e) => {
                self.limits_error = Some(e);
                cx.notify();
                return;
            }
        };
        if change.iter().all(Option::is_none) {
            self.limits_error = None;
            cx.notify();
            return;
        }
        self.limits_busy = true;
        self.limits_error = None;
        let c = self.client.clone();
        let [a, b, d] = change;
        self.call(
            cx,
            async move { c.set_catalog_limits(a, b, d).await },
            |v, r, cx| {
                v.limits_busy = false;
                match r {
                    Ok(_) => {
                        v.notice = Some((
                            false,
                            "Snapshot limits saved; the next sync uses them.".into(),
                        ));
                        v.fill_limits = true;
                        v.reload(cx);
                    }
                    Err(e) => v.limits_error = Some((limit_of_error(&e).unwrap_or(0), e)),
                }
            },
        );
    }

    // ------------------------------------------------------------------------------- drawing

    /// The sync-failed banner (17.17) for Discover › Catalog and Engines; `None` when there is
    /// nothing new to report.
    pub fn banner_element(
        &self,
        id: &'static str,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let b = self.banner.current.clone()?;
        let info = self
            .listing()
            .and_then(|l| l.catalogs.iter().find(|c| c.name == b.catalog));
        let detail = b.detail(info, OffsetDateTime::now_utc());
        let mut actions = h_flex().gap(px(space::S2)).items_center().flex_shrink_0();
        if b.auth {
            let name = b.catalog.clone();
            actions = actions.child(
                button(
                    SharedString::from(format!("{id}-banner-token")),
                    "Set token\u{2026}",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    !self.connected,
                    p,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_token(&name, window, cx);
                    cx.emit(CatalogsEvent::Open);
                })),
            );
        }
        let name = b.catalog.clone();
        actions = actions
            .child(
                button(
                    SharedString::from(format!("{id}-banner-retry")),
                    "Retry",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    !self.connected,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.banner.dismiss();
                    this.sync_one(name.clone(), cx);
                })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{id}-banner-dismiss")))
                    .px(px(space::S2))
                    .cursor_pointer()
                    .text_color(c(p.accent_text))
                    .child("\u{00d7}")
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_banner(cx))),
            );
        Some(
            h_flex()
                .flex_shrink_0()
                .items_start()
                .gap(px(space::S3))
                .px(px(14.0))
                .py(px(space::S3))
                .rounded(px(radius::LG))
                .bg(c(p.accent_subtle))
                .border_1()
                .border_color(c(p.accent_border))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(c(p.accent_text))
                        .child("\u{26a0}"),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.0))
                        .child(
                            div()
                                .type_style(text::SMALL)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(c(p.accent_text))
                                .child(b.title()),
                        )
                        .child(
                            div()
                                .type_style(text::MONO)
                                .text_size(px(text::CAPTION.size))
                                .text_color(c(p.danger_text))
                                .child(SharedString::from(b.error.clone())),
                        )
                        .child(
                            div()
                                .type_style(text::CAPTION)
                                .text_color(c(p.text_secondary))
                                .child(detail),
                        ),
                )
                .child(actions)
                .into_any_element(),
        )
    }

    fn header(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let file = self
            .listing()
            .map(|l| l.file.clone())
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| "config.toml".into());
        let can_sync = self.connected
            && self.listing().is_some_and(|l| enabled_count(l) > 0)
            && !self.sync.all_running();
        let mut sync_all = button(
            "catalogs-sync-all",
            if self.sync.all_running() {
                "Syncing\u{2026}"
            } else {
                "Sync all"
            },
            ButtonKind::Secondary,
            size::CONTROL,
            !can_sync,
            p,
        );
        if can_sync {
            sync_all = sync_all.on_click(cx.listener(|this, _, _, cx| this.sync_all(cx)));
        }
        let mut add = button(
            "catalogs-add",
            "+  Add catalog",
            ButtonKind::Primary,
            size::CONTROL,
            !self.connected,
            p,
        );
        if self.connected {
            add = add.on_click(cx.listener(|this, _, window, cx| this.open_add(window, cx)));
        }
        h_flex()
            .items_center()
            .gap(px(space::S3))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(space::S1))
                    .child(div().type_style(text::H1).child("Settings"))
                    .child(
                        h_flex()
                            .overflow_hidden()
                            .gap(px(space::S1))
                            .type_style(text::SMALL)
                            .text_color(c(p.text_muted))
                            .child("Catalogs write")
                            .child(div().type_style(text::MONO).child("[[catalogs]]"))
                            .child("in")
                            .child(div().type_style(text::MONO).child(file))
                            .child("\u{b7} snapshots stay in the server's data folder"),
                    ),
            )
            .child(sync_all)
            .child(add)
    }

    fn tabs(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let (count, alert) = self
            .listing()
            .map(|l| (Some(l.catalogs.len()), !failed(l).is_empty()))
            .unwrap_or((None, false));
        h_flex()
            .gap(px(space::S2))
            .border_b_1()
            .border_color(c(p.line_default))
            .child(
                section_tab("settings-tab-general", "General", None, false, false, p)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CatalogsEvent::ShowGeneral))),
            )
            .child(section_tab(
                "settings-tab-catalogs",
                "Catalogs",
                count,
                alert,
                true,
                p,
            ))
    }

    fn status_el(st: Status, p: &Palette) -> Div {
        let (fg, bg, d) = match st {
            Status::UpToDate => (p.ok_text, p.ok_subtle, p.ok_default),
            Status::Syncing => (p.info_text, p.info_subtle, p.info_default),
            Status::Failed => (p.danger_text, p.danger_subtle, p.danger_default),
            Status::Disabled | Status::NotSynced => (p.text_muted, p.muted_subtle, p.muted_default),
        };
        status_pill(st.label(), c(fg), c(bg), c(d))
    }

    fn catalog_card(
        &self,
        i: usize,
        info: &alkahest_client::CatalogInfo,
        now: OffsetDateTime,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let progress = self.sync.progress(&info.name, info.enabled);
        let st = status(info, progress.is_some());
        let busy = self.pending.contains(&info.name);
        let mut title = h_flex()
            .flex_1()
            .min_w_0()
            .gap(px(space::S2))
            .items_center()
            .child(mono_caption(info.position.to_string(), p).w(px(18.0)))
            .child(
                div()
                    .type_style(text::MONO)
                    .text_size(px(text::H2.size))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(SharedString::from(info.name.clone())),
            );
        if let Some(chip) = token_chip(info) {
            title = title.child(
                div()
                    .flex_shrink_0()
                    .px(px(space::S2))
                    .py(px(1.0))
                    .rounded(px(radius::FULL))
                    .bg(c(p.muted_subtle))
                    .text_color(c(p.text_secondary))
                    .type_style(text::CAPTION)
                    .child(format!("\u{1f512}\u{fe0e} {chip}")),
            );
        }
        title = title.child(Self::status_el(st, p));
        let name = info.name.clone();
        let can_sync = self.connected && info.enabled && progress.is_none();
        let mut sync = button(
            ("catalog-sync", i),
            "Sync now",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            !can_sync,
            p,
        );
        if can_sync {
            let n = name.clone();
            sync = sync.on_click(cx.listener(move |this, _, _, cx| this.sync_one(n.clone(), cx)));
        }
        let n = name.clone();
        let edit = button(
            ("catalog-edit", i),
            "Edit\u{2026}",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            !self.connected,
            p,
        )
        .on_click(cx.listener(move |this, _, window, cx| this.open_edit(&n, window, cx)));
        let mut actions = h_flex()
            .gap(px(space::S2))
            .items_center()
            .flex_shrink_0()
            .child(sync)
            .child(edit);
        // `official` is only ever disabled (`catalog rm official`): no Remove.
        if !info.official {
            let ask = RemoveAsk::new(info);
            actions = actions.child(
                button(
                    ("catalog-remove", i),
                    "Remove",
                    ButtonKind::Danger,
                    size::CONTROL_SMALL,
                    !self.connected || busy,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.remove = Some(ask.clone());
                    cx.notify();
                })),
            );
        }
        let on = info.enabled;
        let never = info.commit.is_none();
        let mut toggle = h_flex()
            .id(("catalog-enable", i))
            .gap(px(space::S2))
            .items_center()
            .child(checkbox(("catalog-enable-box", i), on, p))
            .child(
                div()
                    .type_style(text::SMALL)
                    .child(if on { "On" } else { "Off" }),
            );
        if self.connected && !busy {
            let n = name.clone();
            toggle = toggle
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_enabled(n.clone(), !on, never, cx);
                }));
        } else {
            toggle = toggle.opacity(0.5);
        }
        actions = actions.child(toggle);
        // The URL gives way (truncated) so ref, subdir and commit stay readable.
        let mut source = h_flex().gap(px(space::S4)).items_center().min_w_0();
        for (k, part) in source_parts(info).into_iter().enumerate() {
            let el = div()
                .type_style(text::MONO)
                .text_size(px(text::CAPTION.size))
                .whitespace_nowrap();
            source = source.child(if k == 0 {
                el.min_w_0()
                    .truncate()
                    .text_color(c(p.text_primary))
                    .child(part)
            } else {
                el.flex_shrink_0()
                    .text_color(c(p.text_secondary))
                    .child(part)
            });
        }
        let mut body = v_flex()
            .min_w_0()
            .pl(px(26.0))
            .gap(px(space::S2))
            .child(source)
            .child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .child(facts_line(info, now)),
            );
        if let Some(prog) = &progress {
            body = body
                .child(meter(
                    prog.fraction.unwrap_or(0.0) as f32,
                    c(p.info_default),
                    4.0,
                    p,
                ))
                .child(
                    div()
                        .type_style(text::MONO)
                        .text_size(px(text::CAPTION.size))
                        .text_color(c(p.info_text))
                        .child(SharedString::from(prog.step.clone())),
                );
        }
        if let (Some(err), true) = (&info.last_error, info.enabled) {
            body = body.child(
                v_flex()
                    .p(px(space::S3))
                    .gap(px(space::S1))
                    .rounded(px(radius::MD))
                    .bg(c(p.bg_code))
                    .child(
                        div()
                            .type_style(text::CODE)
                            .text_color(c(p.danger_text))
                            .child(SharedString::from(err.clone())),
                    )
                    .child(
                        div()
                            .type_style(text::CAPTION)
                            .text_color(c(p.text_secondary))
                            .child(failure_note(info, now)),
                    ),
            );
        }
        if let Some(e) = &info.load_error {
            body = body.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.danger_text))
                    .child(format!("The snapshot on disk cannot be used: {e}")),
            );
        }
        if !info.warnings.is_empty() {
            let open = self.warnings_open.contains(&info.name);
            let n = name.clone();
            body = body.child(
                div()
                    .id(("catalog-warnings", i))
                    .type_style(text::SMALL)
                    .text_color(c(p.accent_text))
                    .cursor_pointer()
                    .child(format!(
                        "{} {} warning{}",
                        if open { "\u{25be}" } else { "\u{25b8}" },
                        info.warnings.len(),
                        if info.warnings.len() == 1 { "" } else { "s" }
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.warnings_open.remove(&n) {
                            this.warnings_open.insert(n.clone());
                        }
                        cx.notify();
                    })),
            );
            if open {
                for w in &info.warnings {
                    body = body.child(mono_caption(w.clone(), p));
                }
            }
        }
        let mut el = card(p)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(space::S3))
                    .child(title)
                    .child(actions),
            )
            .child(body);
        if !info.enabled {
            el = el.opacity(0.72);
        }
        el
    }

    fn limits_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let open = self.limits_open;
        let head = div()
            .id("catalog-limits-toggle")
            .type_style(text::BODY)
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .child(format!(
                "{} Advanced \u{b7} snapshot limits",
                if open { "\u{25be}" } else { "\u{25b8}" }
            ))
            .on_click(cx.listener(|this, _, _, cx| {
                this.limits_open = !this.limits_open;
                cx.notify();
            }));
        let mut el = card(p).child(head);
        if !open {
            return el;
        }
        let Some(l) = self.listing() else { return el };
        let defaults = [
            l.default_limits.max_snapshot_bytes,
            l.default_limits.max_file_bytes,
            l.default_limits.max_archive_bytes,
        ];
        let labels = ["Max snapshot size", "Max file size", "Max archive size"];
        let mut row = h_flex().gap(px(space::S3)).items_start();
        for k in 0..3 {
            let err = self.limits_error.as_ref().filter(|(i, _)| *i == k);
            let mut col = v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .child(field_label(labels[k], p))
                .child(
                    field_frame(err.is_some(), p).type_style(text::MONO).child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.limit_inputs[k]).appearance(false)),
                    ),
                )
                .child(mono_caption(
                    format!(
                        "{} \u{b7} default {}",
                        LIMIT_KEYS[k],
                        format_iec(defaults[k])
                    ),
                    p,
                ));
            if let Some((_, e)) = err {
                col = col.child(
                    div()
                        .type_style(text::CAPTION)
                        .text_color(c(p.danger_text))
                        .child(SharedString::from(e.clone())),
                );
            }
            row = row.child(col);
        }
        let mut apply = button(
            "catalog-limits-apply",
            if self.limits_busy {
                "Applying\u{2026}"
            } else {
                "Apply limits"
            },
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            self.limits_busy || !self.connected,
            p,
        );
        if !self.limits_busy && self.connected {
            apply = apply.on_click(cx.listener(|this, _, _, cx| this.apply_limits(cx)));
        }
        let auto = if l.auto_sync {
            "Due catalogs refresh in the background; ALKAHEST_CATALOG_AUTO_SYNC=0 turns that off."
        } else {
            "The background refresh is off (ALKAHEST_CATALOG_AUTO_SYNC=0)."
        };
        el = el.child(row).child(
            h_flex()
                .gap(px(space::S3))
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .type_style(text::CAPTION)
                        .text_color(c(p.text_muted))
                        .child(format!(
                            "Limits protect against oversized or hostile catalogs; a sync that \
                             exceeds one fails and keeps the previous snapshot. Saved in \
                             [catalog_limits]; a blank field restores the default. {auto}"
                        )),
                )
                .child(apply),
        );
        el
    }

    fn layers_card(&self, l: &CatalogListing, p: &Palette) -> Div {
        let mut el = card(p)
            .child(div().type_style(text::TITLE).child("Recipe layers"))
            .child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_secondary))
                    .child(
                        "Engines read recipes from these layers; the highest one that has a recipe \
                         wins. Engines shows each recipe's origin.",
                    ),
            );
        for (i, (layer, disabled)) in recipe_layers(l).into_iter().enumerate() {
            let mut row = h_flex()
                .gap(px(space::S2))
                .type_style(text::MONO)
                .text_size(px(text::CAPTION.size))
                .child(
                    div()
                        .w(px(20.0))
                        .text_color(c(p.text_muted))
                        .child(format!("{}.", i + 1)),
                )
                .child(div().child(layer));
            if disabled {
                row = row.child(
                    div()
                        .type_style(text::CAPTION)
                        .text_color(c(p.text_muted))
                        .child("\u{b7} disabled"),
                );
            }
            el = el.child(row);
        }
        el
    }

    fn tokens_card(&self, l: &CatalogListing, p: &Palette, cx: &mut Context<Self>) -> Div {
        let service = if l.keyring_service.is_empty() {
            "alkahest".to_string()
        } else {
            l.keyring_service.clone()
        };
        let env = if l.token_env.is_empty() {
            "ALKAHEST_CATALOG_TOKEN_<NAME>".to_string()
        } else {
            l.token_env.clone()
        };
        let mut el = card(p)
            .child(div().type_style(text::TITLE).child("Private catalogs"))
            .child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_secondary))
                    .child(format!(
                        "Tokens live in the OS keyring ({service} / catalog:<name>) or in {env}. \
                         They are never shown, logged or written to config.toml, and are sent \
                         only to the repository's host after it asks for credentials."
                    )),
            );
        for (i, info) in l.catalogs.iter().filter(|c| !c.official).enumerate() {
            let state = match info.token_source.as_str() {
                "keyring" => "token set",
                "env" => "token from environment",
                "mismatch" => "token of another host",
                _ => "no token",
            };
            let mut row = h_flex()
                .gap(px(space::S2))
                .items_center()
                .pt(px(space::S2))
                .border_t_1()
                .border_color(c(p.line_subtle))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(space::S2))
                        .child(
                            div()
                                .type_style(text::SMALL)
                                .child(SharedString::from(info.name.clone())),
                        )
                        .child(mono_caption(state, p)),
                );
            if info.token_source != "env" {
                let n = info.name.clone();
                row = row.child(
                    button(
                        ("catalog-token", i),
                        if info.token_source == "keyring" {
                            "Replace\u{2026}"
                        } else {
                            "Set token\u{2026}"
                        },
                        ButtonKind::Secondary,
                        size::CONTROL_SMALL,
                        !self.connected,
                        p,
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_token(&n, window, cx)),
                    ),
                );
            }
            if info.token_source == "mismatch" {
                let n = info.name.clone();
                row = row.child(
                    button(
                        ("catalog-token-clear", i),
                        "Delete",
                        ButtonKind::Danger,
                        size::CONTROL_SMALL,
                        !self.connected,
                        p,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.clear_token(n.clone(), cx))),
                );
            }
            el = el.child(row);
        }
        el
    }

    fn input_field(
        label: &str,
        input: &Entity<InputState>,
        hint: Option<&str>,
        error: Option<&str>,
        mono: bool,
        mask: bool,
        p: &Palette,
    ) -> Div {
        let mut frame = field_frame(error.is_some(), p);
        if mono {
            frame = frame.type_style(text::MONO);
        }
        let mut inp = Input::new(input).appearance(false);
        if mask {
            inp = inp.mask_toggle();
        }
        let mut col = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(space::S1))
            .child(field_label(label.to_string(), p))
            .child(frame.child(div().flex_1().min_w_0().child(inp)));
        if let Some(e) = error {
            col = col.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.danger_text))
                    .child(SharedString::from(e.to_string())),
            );
        } else if let Some(h) = hint {
            col = col.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .child(SharedString::from(h.to_string())),
            );
        }
        col
    }

    fn add_dialog(
        &self,
        window: &mut Window,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let d = self.add.as_ref()?;
        let err = |f: Field| {
            d.error
                .as_ref()
                .filter(|e| e.field == Some(f))
                .map(|e| e.message.as_str())
        };
        let typed_name = d.name.read(cx).value().to_string();
        let keyring_user = if typed_name.trim().is_empty() {
            "catalog:<name>".to_string()
        } else {
            format!("catalog:{}", typed_name.trim())
        };
        let mut extra = v_flex()
            .gap(px(space::S3))
            .child(Self::input_field(
                "Name",
                &d.name,
                Some("Lower-case letters, digits, - _ . \u{b7} used in model names with --catalog"),
                err(Field::Name),
                true,
                false,
                p,
            ))
            .child(Self::input_field(
                "Repository URL",
                &d.url,
                Some("https://, or file:// for a folder on the server machine. No credentials in the URL."),
                err(Field::Url),
                true,
                false,
                p,
            ))
            .child(
                h_flex()
                    .gap(px(space::S3))
                    .items_start()
                    .child(Self::input_field("Ref (branch or tag)", &d.r#ref, None, err(Field::Ref), true, false, p))
                    .child(Self::input_field("Subdir", &d.subdir, None, err(Field::Subdir), true, false, p))
                    .child(Self::input_field("Refresh every", &d.interval, None, err(Field::Interval), true, false, p)),
            )
            .child(Self::input_field(
                "Pin to commit (optional)",
                &d.pin,
                Some("A pinned catalog is never refreshed."),
                err(Field::Pin),
                true,
                false,
                p,
            ))
            .child(Self::input_field(
                "Access token (private repositories only)",
                &d.token,
                None,
                err(Field::Token),
                false,
                true,
                p,
            ))
            .child(
                h_flex()
                    .gap(px(space::S2))
                    .items_start()
                    .p(px(space::S3))
                    .rounded(px(radius::MD))
                    .border_1()
                    .border_color(c(p.line_default))
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_secondary))
                    .child("\u{1f512}\u{fe0e}")
                    .child(div().flex_1().min_w_0().child(format!(
                        "The token goes to the OS keyring (alkahest / {keyring_user}), never to \
                         config.toml, and is not shown again. It is sent only to the \
                         repository's host."
                    ))),
            )
            .child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .child("Added after the existing catalogs, so its entries win over theirs."),
            );
        if let Some(e) = d.error.as_ref().filter(|e| e.field.is_none()) {
            extra = extra.child(notice_line(e.message.clone(), true, p));
        }
        if let Some(conflict) = &d.conflict {
            extra = extra.child(notice_line(
                format!(
                    "{conflict}. Replacing deletes that token; type this catalog's token above \
                     if it needs one."
                ),
                true,
                p,
            ));
        }
        let cancel = button(
            "catalog-add-cancel",
            "Cancel",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| this.close_add(cx)));
        let replace = d.conflict.is_some();
        let mut confirm = button(
            "catalog-add-confirm",
            match (d.busy, replace) {
                (true, _) => "Adding\u{2026}",
                (false, true) => "Replace token & add",
                (false, false) => "Add & sync",
            },
            ButtonKind::Primary,
            size::CONTROL_SMALL,
            d.busy,
            p,
        );
        if !d.busy {
            confirm = confirm.on_click(cx.listener(move |this, _, window, cx| {
                this.submit_add(replace, window, cx);
            }));
        }
        let scrim = dialog_sized(
            ADD_WIDTH,
            "Add catalog",
            None,
            "A git repository of engine recipes and models.",
            Some(extra.into_any_element()),
            vec![cancel, confirm],
            p,
        );
        Some(
            dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| this.close_add(cx)),
                window,
                cx,
            )
            .into_any_element(),
        )
    }

    fn edit_dialog(
        &self,
        window: &mut Window,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let d = self.edit.as_ref()?;
        let err = |f: Field| {
            d.error
                .as_ref()
                .filter(|e| e.field == Some(f))
                .map(|e| e.message.as_str())
        };
        let mut extra = v_flex()
            .gap(px(space::S3))
            .child(
                h_flex()
                    .gap(px(space::S3))
                    .items_start()
                    .child(Self::input_field(
                        "Ref (branch or tag)",
                        &d.r#ref,
                        None,
                        err(Field::Ref),
                        true,
                        false,
                        p,
                    ))
                    .child(Self::input_field(
                        "Subdir",
                        &d.subdir,
                        None,
                        err(Field::Subdir),
                        true,
                        false,
                        p,
                    ))
                    .child(Self::input_field(
                        "Refresh every",
                        &d.interval,
                        None,
                        err(Field::Interval),
                        true,
                        false,
                        p,
                    )),
            )
            .child(Self::input_field(
                "Pin to commit",
                &d.pin,
                Some("Blank follows the ref; a pinned catalog is never refreshed."),
                err(Field::Pin),
                true,
                false,
                p,
            ));
        if let Some(e) = d.error.as_ref().filter(|e| e.field.is_none()) {
            extra = extra.child(notice_line(e.message.clone(), true, p));
        }
        let cancel = button(
            "catalog-edit-cancel",
            "Cancel",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.edit = None;
            cx.notify();
        }));
        let mut save = button(
            "catalog-edit-save",
            if d.busy { "Saving\u{2026}" } else { "Save" },
            ButtonKind::Primary,
            size::CONTROL_SMALL,
            d.busy,
            p,
        );
        if !d.busy {
            save = save.on_click(cx.listener(|this, _, _, cx| this.submit_edit(cx)));
        }
        let scrim = dialog_sized(
            ADD_WIDTH,
            format!("Edit catalog {}", d.name),
            None,
            "Applies at once. A changed ref, subdir or pin starts a sync.",
            Some(extra.into_any_element()),
            vec![cancel, save],
            p,
        );
        Some(
            dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| {
                    this.edit = None;
                    cx.notify();
                }),
                window,
                cx,
            )
            .into_any_element(),
        )
    }

    fn token_dialog(
        &self,
        window: &mut Window,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let d = self.token.as_ref()?;
        let extra = Self::input_field(
            "Access token",
            &d.input,
            Some("Kept in the server's keyring, never in a file; not shown again."),
            d.error.as_deref(),
            false,
            true,
            p,
        );
        let cancel = button(
            "catalog-token-cancel",
            "Cancel",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.token = None;
            cx.notify();
        }));
        let mut save = button(
            "catalog-token-save",
            match (d.busy, d.replace) {
                (true, _) => "Saving\u{2026}",
                (false, true) => "Replace token",
                (false, false) => "Save token",
            },
            ButtonKind::Primary,
            size::CONTROL_SMALL,
            d.busy,
            p,
        );
        if !d.busy {
            save = save.on_click(cx.listener(|this, _, window, cx| this.submit_token(window, cx)));
        }
        let scrim = dialog(
            format!("Token of catalog {}", d.name),
            Some(SharedString::from(format!("alkahest / catalog:{}", d.name))),
            "Sent only to the repository's host when it asks for credentials. A sync starts once \
             it is stored.",
            Some(extra.into_any_element()),
            vec![cancel, save],
            p,
        );
        Some(
            dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| {
                    this.token = None;
                    cx.notify();
                }),
                window,
                cx,
            )
            .into_any_element(),
        )
    }

    fn remove_dialog(
        &self,
        window: &mut Window,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ask = self.remove.as_ref()?;
        let cancel = button(
            "catalog-remove-cancel",
            "Cancel",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .border_color(c(p.accent_default))
        .on_click(cx.listener(|this, _, _, cx| {
            this.remove = None;
            cx.notify();
        }));
        let mut buttons = vec![cancel];
        if ask.enabled {
            let n = ask.name.clone();
            buttons.push(
                button(
                    "catalog-remove-disable",
                    "Disable instead",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    false,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.remove = None;
                    this.set_enabled(n.clone(), false, false, cx);
                })),
            );
        }
        buttons.push(
            button(
                "catalog-remove-confirm",
                "Remove catalog",
                ButtonKind::Danger,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.confirm_remove(cx))),
        );
        let extra = ask.has_token.then(|| {
            let on = ask.delete_token;
            h_flex()
                .id("catalog-remove-token")
                .gap(px(space::S2))
                .items_center()
                .cursor_pointer()
                .child(checkbox("catalog-remove-token-box", on, p))
                .child(
                    div()
                        .type_style(text::SMALL)
                        .child("Also delete its token from the keyring"),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(a) = this.remove.as_mut() {
                        a.delete_token = !on;
                    }
                    cx.notify();
                }))
                .into_any_element()
        });
        let scrim = dialog(
            format!("Remove catalog {}?", ask.name),
            Some(SharedString::from(ask.facts.clone())),
            ask.consequence(),
            extra,
            buttons,
            p,
        );
        Some(
            dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| {
                    this.remove = None;
                    cx.notify();
                }),
                window,
                cx,
            )
            .into_any_element(),
        )
    }

    /// The content of the Catalogs tab below the tabs.
    fn body(&self, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let now = OffsetDateTime::now_utc();
        let mut main = v_flex().flex_1().min_w_0().gap(px(space::S4)).child(
            div()
                .type_style(text::SMALL)
                .text_color(c(p.text_secondary))
                .child(
                    "Catalogs are git repositories of engine recipes and models, so new engines \
                     and models arrive without a new Alkahest release. Listed lowest to highest: \
                     when two catalogs define the same entry, the later one wins.",
                ),
        );
        let mut side = v_flex()
            .w(px(SIDE_WIDTH))
            .flex_shrink_0()
            .gap(px(space::S4));
        match &self.listing {
            Fetch::Ready(l) => {
                if l.catalogs.is_empty() {
                    main = main.child(notice_line(
                        "No catalog is configured. Add one to get engine recipes and models from \
                         a git repository.",
                        false,
                        p,
                    ));
                }
                for (i, info) in l.catalogs.iter().enumerate() {
                    main = main.child(self.catalog_card(i, info, now, p, cx));
                }
                main = main.child(self.limits_card(p, cx));
                side = side
                    .child(self.layers_card(l, p))
                    .child(self.tokens_card(l, p, cx));
            }
            Fetch::Failed(e) => {
                main = main.child(notice_line(
                    format!("Cannot read the catalogs: {e}"),
                    true,
                    p,
                ));
            }
            Fetch::Loading => {
                main = main.child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_muted))
                        .child("Reading the catalogs\u{2026}"),
                );
            }
            Fetch::Idle => {
                main = main.child(notice_line(
                    "Catalogs live on the server. Start or connect to one first.",
                    false,
                    p,
                ));
            }
        }
        div()
            .id("catalogs-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .items_start()
                    .gap(px(space::S5))
                    .child(main)
                    .child(side),
            )
    }
}

impl Render for CatalogsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.fill_limits
            && let Some(l) = self.listing.ready()
        {
            self.fill_limits = false;
            let values = [
                l.limits.max_snapshot_bytes,
                l.limits.max_file_bytes,
                l.limits.max_archive_bytes,
            ];
            for (input, v) in self.limit_inputs.iter().zip(values) {
                let text = if v > 0 { format_iec(v) } else { String::new() };
                input.update(cx, |s, cx| s.set_value(text, window, cx));
            }
        }
        let p = Theme::get(cx).palette;
        let mut content = v_flex()
            .size_full()
            .gap(px(space::S5))
            .child(self.header(p, cx))
            .child(self.tabs(p, cx));
        if let Some((danger, msg)) = self.notice.clone() {
            content = content.child(notice_line(msg, danger, p));
        }
        content = content.child(self.body(p, cx));
        let mut root = div().relative().size_full().child(content);
        for d in [
            self.remove_dialog(window, p, cx),
            self.edit_dialog(window, p, cx),
            self.add_dialog(window, p, cx),
            self.token_dialog(window, p, cx),
        ]
        .into_iter()
        .flatten()
        {
            root = root.child(d);
        }
        root
    }
}

/// A small catalog name chip (result cards, Discover detail).
pub fn catalog_chip(name: &str, p: &Palette) -> Div {
    tag(name.to_string(), p)
}

// GUI tests need gpui-kit's `test-support` (feature `gui-test`, the desktop CI job).
#[cfg(all(test, feature = "gui-test"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use alkahest_client::CatalogInfo;

    use super::*;
    use crate::theme::ThemePreference;
    use crate::ui::WindowAppearance;
    use crate::ui::testing::{TestAppContext, gpui_test};

    #[gpui_test]
    fn catalogs_tab_renders_and_dialogs_validate(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::ui::init(cx);
            Theme::install(cx, ThemePreference::Dark, WindowAppearance::Dark);
            tokio_bridge::init(cx).unwrap();
        });
        let client = Client::new("http://127.0.0.1:9").unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| CatalogsView::new(client, window, cx));
        view.update(cx, |v, cx| {
            v.connected = true;
            v.listing = Fetch::Ready(CatalogListing {
                catalogs: vec![
                    CatalogInfo {
                        position: 1,
                        name: "official".into(),
                        official: true,
                        enabled: true,
                        commit: Some("c1c1877b".into()),
                        ..CatalogInfo::default()
                    },
                    CatalogInfo {
                        position: 2,
                        name: "acme".into(),
                        enabled: true,
                        token_present: true,
                        token_source: "keyring".into(),
                        last_error: Some("401 Unauthorized".into()),
                        warnings: vec!["stripped key".into()],
                        ..CatalogInfo::default()
                    },
                ],
                ..CatalogListing::default()
            });
            cx.notify();
        });
        cx.run_until_parked();
        // Add: an invalid name is refused without a request, under the name field.
        view.update_in(cx, |v, window, cx| {
            v.open_add(window, cx);
            v.submit_add(false, window, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            let d = v.add.as_ref().unwrap();
            assert_eq!(d.error.as_ref().unwrap().field, Some(Field::Name));
            assert!(!d.busy);
        });
        // Remove asks about the token of a private catalog.
        view.update(cx, |v, cx| {
            v.add = None;
            let info = v.listing().unwrap().catalogs[1].clone();
            v.remove = Some(RemoveAsk::new(&info));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.remove.as_ref().unwrap().has_token));
        // An empty token is refused locally.
        view.update_in(cx, |v, window, cx| {
            v.remove = None;
            v.open_token("acme", window, cx);
            v.submit_token(window, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            let d = v.token.as_ref().unwrap();
            assert!(d.replace && d.error.is_some() && !d.busy);
        });
    }
}
