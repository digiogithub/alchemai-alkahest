//! Screen 14, API keys & network (ALK-US-0071, ALK-SP-0008.R7; security-tagged).
//!
//! Exposure toggle (`127.0.0.1` / `0.0.0.0`), port, CORS origins and the plain-HTTP
//! warning; keys table with Revoke (confirmation 17.12); "New key" with the secret shown
//! once and a copy button; endpoint family switches; Hugging Face token in the server
//! keyring. Network changes are saved for the next start: a banner says a restart is needed
//! and, for a local server this app manages, offers to restart it.
//!
//! The logic (rows, drafts, the one-time secret, typed revoke confirmation) is
//! [`crate::keys`]; this view draws it and sends the requests.

use std::future::Future;

use alkahest_client::admin::HfTokenStatus;
use alkahest_client::{Client, ClientError};
use time::OffsetDateTime;

use crate::connection::{ConnectionState, Snapshot};
use crate::keys::{KeyRow, KeysState, curl_example, key_rows, network_warnings};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::input::{Input, InputEvent, InputState};
use crate::ui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Div, Entity, EventEmitter, Focusable as _,
    FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, h_flex, px, v_flex,
};
use crate::views::icons;
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, card, card_title, checkbox, column_label, dialog, field_frame, field_label,
    mono_caption, notice_line, tag, warning_banner,
};

/// Width of the side column (created key, endpoints, HF token).
const SIDE_WIDTH: f32 = 420.0;

/// What the screen asks of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysEvent {
    /// Restart the local server so saved settings apply (`pid` from `/alkahest/v1/info`).
    RestartServer {
        /// Server process id.
        pid: Option<u32>,
    },
}

/// The API keys & network view.
pub struct KeysView {
    client: Client,
    server_url: String,
    connected: bool,
    manages_server: bool,
    server_pid: Option<u32>,
    state: KeysState,
    hf: Option<HfTokenStatus>,
    hf_editing: bool,
    port: Entity<InputState>,
    cors: Entity<InputState>,
    new_name: Entity<InputState>,
    revoke_typed: Entity<InputState>,
    hf_token: Entity<InputState>,
    pending_reload: bool,
}

impl std::fmt::Debug for KeysView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `state` carries a redacted secret; the inputs are kit entities.
        f.debug_struct("KeysView")
            .field("connected", &self.connected)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<KeysEvent> for KeysView {}

impl KeysView {
    /// A keys screen sending its requests through `client`.
    pub fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("11435"));
        let cors = cx.new(|cx| InputState::new(window, cx).placeholder("https://app.lan"));
        let new_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("who uses it: n8n, laptop, ci"));
        let revoke_typed = cx.new(|cx| InputState::new(window, cx));
        let hf_token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("hf_\u{2026}")
                .masked(true)
        });
        cx.subscribe_in(&port, window, |this, input, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change)
                && let Some(d) = this.state.draft.as_mut()
            {
                d.port = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        cx.subscribe_in(&cors, window, |this, input, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change)
                && let Some(d) = this.state.draft.as_mut()
            {
                d.cors = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        cx.subscribe_in(
            &new_name,
            window,
            |this, input, e: &InputEvent, _, cx| match e {
                InputEvent::Change => {
                    this.state.new_name = input.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.create_key(cx),
                _ => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &revoke_typed,
            window,
            |this, input, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Change)
                    && let Some(r) = this.state.revoke.as_mut()
                {
                    r.typed = input.read(cx).value().to_string();
                    cx.notify();
                }
            },
        )
        .detach();
        Self {
            client,
            server_url: String::new(),
            connected: false,
            manages_server: false,
            server_pid: None,
            state: KeysState::default(),
            hf: None,
            hf_editing: false,
            port,
            cors,
            new_name,
            revoke_typed,
            hf_token,
            pending_reload: false,
        }
    }

    /// Follows a connection snapshot; (re)loads the data when the server comes up.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let connected = matches!(snapshot.state, ConnectionState::Connected(_));
        self.server_url.clone_from(&snapshot.server_url);
        self.manages_server = snapshot.manages_server;
        if connected != self.connected {
            self.connected = connected;
            // Fetched on the next render, which has the window the inputs need.
            self.pending_reload = connected;
            cx.notify();
        }
    }

    /// Leaving the screen: the one-time secret is dropped for good.
    pub fn on_hidden(&mut self, cx: &mut Context<Self>) {
        if self.state.created.is_some() {
            self.state.forget_secret();
            cx.notify();
        }
    }

    /// Runs `fut` on the tokio runtime and hands the result to `done` on the UI thread.
    fn call<R: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        fut: impl Future<Output = Result<R, ClientError>> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<R, String>, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
    ) {
        let task = tokio_bridge::spawn(cx, fut);
        cx.spawn_in(window, async move |this, cx| {
            let r = match task.await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(e) => Err(format!("the request was cancelled: {e}")),
            };
            let _ = this.update_in(cx, |view, window, cx| {
                done(view, r, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetches keys, network settings, HF token status and the server pid.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.keys().await },
            |v, r, _, _| match r {
                Ok(keys) => v.state.keys = keys,
                Err(e) => v.state.notice = Some((true, format!("Cannot list keys: {e}"))),
            },
            window,
        );
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.network_settings().await },
            |v, r, window, cx| match r {
                Ok(doc) => v.show_network(doc, window, cx),
                Err(e) => {
                    v.state.notice = Some((true, format!("Cannot read network settings: {e}")));
                }
            },
            window,
        );
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.hf_token_status(false).await },
            |v, r, _, _| v.hf = r.ok(),
            window,
        );
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.server_info(false).await },
            |v, r, _, _| v.server_pid = r.ok().map(|i| i.pid).filter(|p| *p > 0),
            window,
        );
    }

    fn show_network(
        &mut self,
        doc: alkahest_client::admin::NetworkSettingsDoc,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.set_network(doc);
        if let Some(d) = self.state.draft.clone() {
            self.port
                .update(cx, |s, cx| s.set_value(d.port.clone(), window, cx));
            self.cors
                .update(cx, |s, cx| s.set_value(d.cors.clone(), window, cx));
        }
    }

    fn save_network(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(doc), Some(draft)) = (&self.state.network, &self.state.draft) else {
            return;
        };
        let patch = match draft.patch(&doc.saved) {
            Ok(Some(p)) => p,
            Ok(None) => return,
            Err(e) => {
                self.state.notice = Some((true, e));
                cx.notify();
                return;
            }
        };
        self.state.busy = true;
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.update_network_settings(&patch).await },
            |v, r, window, cx| {
                v.state.busy = false;
                match r {
                    Ok(doc) => {
                        v.state.notice = Some((
                            false,
                            "Network settings saved for the next start.".to_string(),
                        ));
                        v.show_network(doc, window, cx);
                    }
                    Err(e) => v.state.notice = Some((true, e)),
                }
            },
            window,
        );
    }

    fn toggle_endpoint(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.state.draft.as_mut() {
            match key {
                "ollama" => d.ollama = !d.ollama,
                "openai" => d.openai = !d.openai,
                "anthropic" => d.anthropic = !d.anthropic,
                _ => d.decisions = !d.decisions,
            }
        }
        self.save_network(window, cx);
    }

    fn create_key(&mut self, cx: &mut Context<Self>) {
        let req = match self.state.new_key() {
            Ok(r) => r,
            Err(e) => {
                self.state.notice = Some((true, e));
                cx.notify();
                return;
            }
        };
        self.state.busy = true;
        cx.notify();
        let c = self.client.clone();
        let task = tokio_bridge::spawn(cx, async move { c.create_key(&req).await });
        cx.spawn(async move |this, cx| {
            let r = match task.await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let _ = this.update(cx, |v, cx| {
                v.state.busy = false;
                match r {
                    Ok(created) => {
                        v.state.on_created(&created.record.name, &created.key);
                        v.state.keys.push(created.record);
                        v.state.notice = None;
                    }
                    Err(e) => v.state.notice = Some((true, e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_new_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.forget_secret();
        self.state.creating = true;
        self.new_name
            .update(cx, |s, cx| s.set_value("", window, cx));
        // Type the name right away.
        let focus = self.new_name.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn copy_secret(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = &self.state.created {
            cx.write_to_clipboard(ClipboardItem::new_string(s.expose().to_string()));
            self.state.copied = true;
            cx.notify();
        }
    }

    fn ask_revoke(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.state.ask_revoke(&id, OffsetDateTime::now_utc());
        self.revoke_typed
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn revoke(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ask) = self.state.revoke.take().filter(|a| a.can_confirm()) else {
            return;
        };
        let c = self.client.clone();
        let id = ask.id.clone();
        self.call(
            cx,
            async move { c.revoke_key(&id).await },
            move |v, r, _, _| match r {
                Ok(_) => {
                    v.state.keys.retain(|k| k.id != ask.id);
                    v.state.notice = Some((
                        false,
                        format!("Revoked {}: clients using it now get 401.", ask.name),
                    ));
                }
                Err(e) => v.state.notice = Some((true, format!("Cannot revoke: {e}"))),
            },
            window,
        );
    }

    fn save_hf_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.hf_token.read(cx).value().trim().to_string();
        if token.is_empty() {
            return;
        }
        self.hf_token
            .update(cx, |s, cx| s.set_value("", window, cx));
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.set_hf_token(&token, true).await },
            |v, r, _, _| match r {
                Ok(s) => {
                    v.hf = Some(s);
                    v.hf_editing = false;
                }
                Err(e) => v.state.notice = Some((true, format!("Hugging Face token: {e}"))),
            },
            window,
        );
    }

    fn header(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let mut new_key = button(
            "keys-new",
            "+  New key",
            ButtonKind::Primary,
            size::CONTROL,
            !self.connected,
            p,
        );
        if self.connected {
            new_key =
                new_key.on_click(cx.listener(|this, _, window, cx| this.open_new_key(window, cx)));
        }
        h_flex()
            .items_center()
            .gap(px(space::S3))
            .child(
                v_flex()
                    .mr_auto()
                    .gap(px(space::S1))
                    .child(div().type_style(text::H1).child("API keys & network"))
                    .child(
                        div()
                            .type_style(text::SMALL)
                            .text_color(c(p.text_muted))
                            .child("localhost never needs a key. Every other client does."),
                    ),
            )
            .child(new_key)
    }

    fn network_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let Some(draft) = self.state.draft.clone() else {
            return card(p).child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_muted))
                    .child("Loading network settings\u{2026}"),
            );
        };
        let doc = self.state.network.clone().unwrap_or_default();
        let toggle = h_flex()
            .id("keys-allow-remote")
            .gap(px(space::S2))
            .items_center()
            .cursor_pointer()
            .child(checkbox("keys-allow-remote-box", draft.allow_remote, p))
            .child(div().type_style(text::SMALL).child(if draft.allow_remote {
                "On"
            } else {
                "Off"
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                if let Some(d) = this.state.draft.as_mut() {
                    d.allow_remote = !d.allow_remote;
                }
                this.save_network(window, cx);
            }));
        let field = |label: &str, child: AnyElement| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .child(field_label(label.to_string(), p))
                .child(child)
        };
        let host = if draft.allow_remote {
            crate::keys::ANY_HOST
        } else {
            crate::keys::LOOPBACK_HOST
        };
        let mut body = card(p)
            .child(
                h_flex()
                    .items_start()
                    .gap(px(space::S3))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(space::S1))
                            .child(
                                div()
                                    .type_style(text::TITLE)
                                    .child("Allow access from other machines"),
                            )
                            .child(
                                div()
                                    .type_style(text::SMALL)
                                    .text_color(c(p.text_muted))
                                    .child(
                                        "Binds the API on all interfaces. Requests without a \
                                         valid key get 401.",
                                    ),
                            ),
                    )
                    .child(toggle),
            )
            .child(
                h_flex()
                    .gap(px(space::S3))
                    .child(field(
                        "Bind address",
                        field_frame(false, p)
                            .type_style(text::MONO)
                            .text_color(c(p.text_secondary))
                            .child(host)
                            .into_any_element(),
                    ))
                    .child(field(
                        "Port",
                        field_frame(false, p)
                            .type_style(text::MONO)
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.port).appearance(false)),
                            )
                            .into_any_element(),
                    ))
                    .child(field(
                        "Allowed origins (CORS)",
                        field_frame(false, p)
                            .type_style(text::MONO)
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.cors).appearance(false)),
                            )
                            .into_any_element(),
                    )),
            );
        let dirty = draft
            .patch(&doc.saved)
            .is_ok_and(|p| p.is_some_and(|p| p.port.is_some() || p.cors_origins.is_some()));
        if dirty {
            body = body.child(
                h_flex().justify_end().child(
                    button(
                        "keys-save-network",
                        "Save port and origins",
                        ButtonKind::Secondary,
                        size::CONTROL_SMALL,
                        self.state.busy,
                        p,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.save_network(window, cx))),
                ),
            );
        }
        for w in network_warnings(&doc) {
            body = body.child(warning_banner(w, p));
        }
        if doc.restart_required {
            let mut row = h_flex()
                .items_center()
                .gap(px(space::S3))
                .px(px(14.0))
                .py(px(space::S2))
                .rounded(px(radius::LG))
                .bg(c(p.info_subtle))
                .text_color(c(p.info_text))
                .type_style(text::SMALL)
                .child(div().flex_1().min_w_0().child(format!(
                    "Saved to {}. The server still runs with {}:{}; restart it to apply.",
                    doc.file, doc.running.host, doc.running.port
                )));
            if self.manages_server {
                let pid = self.server_pid;
                row = row.child(
                    button(
                        "keys-restart",
                        "Restart server",
                        ButtonKind::Secondary,
                        size::CONTROL_SMALL,
                        false,
                        p,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(KeysEvent::RestartServer { pid });
                    })),
                );
            } else {
                row = row.child(mono_caption("alkahest service restart", p));
            }
            body = body.child(row);
        }
        body
    }

    fn keys_card(&self, rows: &[KeyRow], p: &Palette, cx: &mut Context<Self>) -> Div {
        const NAME: f32 = 170.0;
        const KEY: f32 = 130.0;
        const USED: f32 = 90.0;
        const COUNT: f32 = 80.0;
        let header = h_flex()
            .px(px(size::CARD_PADDING))
            .py(px(space::S3))
            .gap(px(space::S3))
            .border_b_1()
            .border_color(c(p.line_subtle))
            .child(column_label("Name", p).w(px(NAME)))
            .child(column_label("Key", p).w(px(KEY)))
            .child(column_label("Scope", p).flex_1())
            .child(column_label("Last used", p).w(px(USED)))
            .child(column_label("Requests", p).w(px(COUNT)))
            .child(div().w(px(80.0)));
        let mut list = v_flex().child(header);
        if rows.is_empty() {
            list = list.child(
                div()
                    .px(px(size::CARD_PADDING))
                    .py(px(space::S5))
                    .type_style(text::SMALL)
                    .text_color(c(p.text_muted))
                    .child(
                        "No keys yet. Local clients need none; create one for every remote \
                         client so each can be revoked on its own.",
                    ),
            );
        }
        for (i, r) in rows.iter().enumerate() {
            let mut scopes = h_flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .gap(px(space::S1))
                .flex_wrap();
            for s in &r.scopes {
                scopes = scopes.child(tag(s.clone(), p));
            }
            let id = r.id.clone();
            list = list.child(
                h_flex()
                    .px(px(size::CARD_PADDING))
                    .py(px(space::S3))
                    .gap(px(space::S3))
                    .items_center()
                    .border_b_1()
                    .border_color(c(p.line_subtle))
                    .child(
                        v_flex()
                            .w(px(NAME))
                            .child(
                                div()
                                    .type_style(text::SMALL)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(SharedString::from(r.name.clone())),
                            )
                            .child(
                                div()
                                    .type_style(text::CAPTION)
                                    .text_color(c(p.text_muted))
                                    .child(SharedString::from(r.created.clone())),
                            ),
                    )
                    .child(
                        div()
                            .w(px(KEY))
                            .type_style(text::MONO)
                            .text_size(px(text::CAPTION.size))
                            .text_color(c(p.text_secondary))
                            .child(SharedString::from(r.prefix.clone())),
                    )
                    .child(scopes)
                    .child(
                        div()
                            .w(px(USED))
                            .type_style(text::SMALL)
                            .text_color(c(if r.live { p.ok_text } else { p.text_secondary }))
                            .child(SharedString::from(r.last_used.clone())),
                    )
                    .child(
                        div()
                            .w(px(COUNT))
                            .type_style(text::MONO)
                            .child(crate::present::group_digits(r.requests)),
                    )
                    .child(
                        button(
                            ("keys-revoke", i),
                            "Revoke",
                            ButtonKind::Danger,
                            size::CONTROL_SMALL,
                            false,
                            p,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.ask_revoke(id.clone(), window, cx);
                            },
                        )),
                    ),
            );
        }
        v_flex()
            .flex_shrink_0()
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .child(
                div()
                    .p(px(size::CARD_PADDING))
                    .border_b_1()
                    .border_color(c(p.line_subtle))
                    .child(card_title(
                        "Keys",
                        Some("stored hashed \u{b7} shown once at creation".into()),
                        p,
                    )),
            )
            .child(list)
    }

    fn created_card(&self, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        if let Some(secret) = &self.state.created {
            let copy_label = if self.state.copied { "Copied" } else { "Copy" };
            return Some(
                card(p)
                    .border_color(c(p.ok_default))
                    .child(
                        h_flex()
                            .gap(px(space::S2))
                            .items_center()
                            .type_style(text::SMALL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(p.ok_text))
                            .child(crate::views::widgets::dot(c(p.ok_default), 7.0))
                            .child(format!("Key created \u{b7} {}", secret.name)),
                    )
                    .child(
                        h_flex()
                            .gap(px(space::S2))
                            .items_center()
                            .p(px(space::S3))
                            .rounded(px(radius::MD))
                            .border_1()
                            .border_color(c(p.accent_default))
                            .bg(c(p.bg_code))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .type_style(text::MONO)
                                    .text_color(c(p.accent_text))
                                    .child(SharedString::from(secret.expose().to_string())),
                            )
                            .child(
                                button(
                                    "keys-copy",
                                    copy_label,
                                    ButtonKind::Secondary,
                                    size::CONTROL_SMALL,
                                    false,
                                    p,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.copy_secret(cx))),
                            ),
                    )
                    .child(
                        div()
                            .type_style(text::SMALL)
                            .text_color(c(p.text_secondary))
                            .child(
                                "Copy it now. Alkahest keeps only a hash and cannot show this \
                                 key again.",
                            ),
                    )
                    .child(
                        h_flex().justify_end().child(
                            button(
                                "keys-done",
                                "Done",
                                ButtonKind::Secondary,
                                size::CONTROL_SMALL,
                                false,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.forget_secret();
                                cx.notify();
                            })),
                        ),
                    ),
            );
        }
        if !self.state.creating {
            return None;
        }
        Some(
            card(p)
                .border_color(c(p.accent_border))
                .child(div().type_style(text::TITLE).child("New key"))
                .child(field_label("Name", p))
                .child(
                    field_frame(false, p).child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.new_name).appearance(false)),
                    ),
                )
                .child(
                    h_flex()
                        .id("keys-new-admin")
                        .gap(px(space::S2))
                        .items_center()
                        .cursor_pointer()
                        .child(checkbox("keys-new-admin-box", self.state.new_admin, p))
                        .child(
                            div()
                                .type_style(text::SMALL)
                                .child("Admin scope (manage models, keys and settings)"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.new_admin = !this.state.new_admin;
                            cx.notify();
                        })),
                )
                .child(
                    h_flex()
                        .gap(px(space::S2))
                        .justify_end()
                        .child(
                            button(
                                "keys-new-cancel",
                                "Cancel",
                                ButtonKind::Secondary,
                                size::CONTROL_SMALL,
                                false,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.creating = false;
                                cx.notify();
                            })),
                        )
                        .child(
                            button(
                                "keys-new-create",
                                "Create key",
                                ButtonKind::Secondary,
                                size::CONTROL_SMALL,
                                self.state.busy || self.state.new_name.trim().is_empty(),
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.create_key(cx))),
                        ),
                ),
        )
    }

    fn endpoints_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let mut el = card(p).child(div().type_style(text::TITLE).child("Endpoints"));
        let Some(d) = self.state.draft.clone() else {
            return el;
        };
        let rows: [(&'static str, &'static str, &'static str, bool); 4] = [
            ("ollama", "Ollama native", "/api/*", d.ollama),
            (
                "openai",
                "OpenAI compatible",
                "/v1/chat/completions \u{b7} /v1/embeddings \u{b7} \u{2026}",
                d.openai,
            ),
            (
                "anthropic",
                "Anthropic Messages",
                "/v1/messages",
                d.anthropic,
            ),
            (
                "decisions",
                "Decisions",
                "/v1/systemone \u{b7} /v1/decisions",
                d.decisions,
            ),
        ];
        for (key, label, paths, on) in rows {
            el = el.child(
                h_flex()
                    .id(SharedString::from(format!("keys-endpoint-{key}")))
                    .items_center()
                    .gap(px(space::S3))
                    .py(px(space::S1))
                    .border_b_1()
                    .border_color(c(p.line_subtle))
                    .cursor_pointer()
                    .child(
                        v_flex()
                            .flex_1()
                            .child(
                                div()
                                    .type_style(text::SMALL)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(label),
                            )
                            .child(mono_caption(paths, p)),
                    )
                    .child(checkbox(
                        SharedString::from(format!("keys-ep-box-{key}")),
                        on,
                        p,
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_endpoint(key, window, cx);
                    })),
            );
        }
        if let Some(doc) = &self.state.network {
            el = el.child(
                div()
                    .type_style(text::MONO)
                    .text_size(px(text::CAPTION.size))
                    .text_color(c(p.text_secondary))
                    .child(curl_example(doc, &self.server_url)),
            );
        }
        el
    }

    fn hf_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let status = match &self.hf {
            Some(s) if s.present => format!(
                "{} \u{b7} used for gated repos and faster pulls{}",
                if s.source == "env" {
                    "from HF_TOKEN in the server environment"
                } else {
                    "in the server's system keyring"
                },
                if s.shadowed_by_env {
                    " \u{b7} HF_TOKEN overrides it"
                } else {
                    ""
                }
            ),
            Some(_) => "not set \u{b7} needed for gated repos (Llama, Gemma)".to_string(),
            None => "status unknown".to_string(),
        };
        let mut el = card(p).child(
            h_flex()
                .items_center()
                .gap(px(space::S3))
                .child(
                    v_flex()
                        .flex_1()
                        .child(div().type_style(text::TITLE).child("Hugging Face token"))
                        .child(
                            div()
                                .type_style(text::CAPTION)
                                .text_color(c(p.text_muted))
                                .child(status),
                        ),
                )
                .child(
                    button(
                        "keys-hf-replace",
                        if self.hf.as_ref().is_some_and(|s| s.present) {
                            "Replace"
                        } else {
                            "Add"
                        },
                        ButtonKind::Secondary,
                        size::CONTROL_SMALL,
                        !self.connected,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hf_editing = !this.hf_editing;
                        cx.notify();
                    })),
                ),
        );
        if self.hf_editing {
            el = el
                .child(
                    field_frame(false, p).child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.hf_token).appearance(false).mask_toggle()),
                    ),
                )
                .child(
                    h_flex().justify_end().child(
                        button(
                            "keys-hf-save",
                            "Save to keyring",
                            ButtonKind::Secondary,
                            size::CONTROL_SMALL,
                            false,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.save_hf_token(window, cx)),
                        ),
                    ),
                );
        }
        el
    }

    fn revoke_dialog(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.state.revoke.as_ref()?;
        let extra = ask.needs_typing.then(|| {
            v_flex()
                .gap(px(6.0))
                .child(
                    h_flex()
                        .gap(px(space::S1))
                        .type_style(text::SMALL)
                        .text_color(c(p.text_secondary))
                        .child("Type")
                        .child(
                            div()
                                .type_style(text::MONO)
                                .child(SharedString::from(ask.name.clone())),
                        )
                        .child("to confirm"),
                )
                .child(
                    field_frame(false, p).type_style(text::MONO).child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.revoke_typed).appearance(false)),
                    ),
                )
                .into_any_element()
        });
        let cancel = button(
            "revoke-cancel",
            "Cancel",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .border_color(c(p.accent_default))
        .on_click(cx.listener(|this, _, _, cx| {
            this.state.revoke = None;
            cx.notify();
        }));
        let can = ask.can_confirm();
        let mut confirm = button(
            "revoke-confirm",
            "Revoke key",
            ButtonKind::Danger,
            size::CONTROL_SMALL,
            !can,
            p,
        );
        if can {
            confirm = confirm.on_click(cx.listener(|this, _, window, cx| this.revoke(window, cx)));
        }
        Some(
            dialog(
                format!("Revoke {}?", ask.name),
                Some(SharedString::from(ask.facts.clone())),
                "Clients using this key get 401 from the next request. This cannot be undone; \
                 create a new key to restore access.",
                extra,
                vec![cancel, confirm],
                p,
            )
            .into_any_element(),
        )
    }
}

impl Render for KeysView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.pending_reload) {
            self.reload(window, cx);
        }
        let p = Theme::get(cx).palette;
        let mut main = v_flex().flex_1().min_w_0().gap(px(space::S4));
        let mut side = v_flex()
            .w(px(SIDE_WIDTH))
            .flex_shrink_0()
            .gap(px(space::S4));
        if self.connected {
            let rows = key_rows(&self.state.keys, OffsetDateTime::now_utc());
            main = main
                .child(self.network_card(p, cx))
                .child(self.keys_card(&rows, p, cx));
            if let Some(created) = self.created_card(p, cx) {
                side = side.child(created);
            }
            side = side
                .child(self.endpoints_card(p, cx))
                .child(self.hf_card(p, cx));
        } else {
            main = main.child(crate::views::widgets::empty_block(
                "keys-disconnected",
                icons::API_KEYS,
                "Not connected",
                "Keys and network settings live on the server. Start or connect to one first.",
                Vec::new(),
                None,
                p,
            ));
        }
        let mut content = v_flex()
            .size_full()
            .gap(px(space::S5))
            .child(self.header(p, cx));
        if let Some((danger, msg)) = self.state.notice.clone() {
            content = content.child(notice_line(msg, danger, p));
        }
        content = content.child(
            div()
                .id("keys-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    h_flex()
                        .items_start()
                        .gap(px(space::S5))
                        .child(main)
                        .children(self.connected.then_some(side)),
                ),
        );
        let mut root = div().relative().size_full().child(content);
        if let Some(d) = self.revoke_dialog(p, cx) {
            root = root.child(d);
        }
        root
    }
}

// GUI tests need gpui-kit's `test-support` (feature `gui-test`, the desktop CI job).
#[cfg(all(test, feature = "gui-test"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use alkahest_client::admin::ApiKey;

    use super::*;
    use crate::theme::ThemePreference;
    use crate::ui::WindowAppearance;
    use crate::ui::testing::{TestAppContext, gpui_test};

    #[gpui_test]
    fn secret_shows_once_and_revoke_asks_first(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::ui::init(cx);
            Theme::install(cx, ThemePreference::Dark, WindowAppearance::Dark);
            tokio_bridge::init(cx).unwrap();
        });
        let client = Client::new("http://127.0.0.1:9").unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| KeysView::new(client, window, cx));
        view.update(cx, |v, cx| {
            v.connected = true;
            v.state.keys = vec![ApiKey {
                id: "abcd".into(),
                name: "laptop".into(),
                status: "active".into(),
                created: "2026-10-01T00:00:00Z".into(),
                ..ApiKey::default()
            }];
            v.state.on_created("ci", "alk_secret_once");
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            assert_eq!(
                v.state.created.as_ref().unwrap().expose(),
                "alk_secret_once"
            );
            v.on_hidden(cx);
            assert!(
                v.state.created.is_none(),
                "leaving the screen drops the secret"
            );
        });
        view.update_in(cx, |v, window, cx| v.ask_revoke("abcd".into(), window, cx));
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            let ask = v.state.revoke.as_ref().unwrap();
            assert_eq!(ask.name, "laptop");
            assert!(ask.can_confirm(), "never used: no typing needed");
        });
    }
}
