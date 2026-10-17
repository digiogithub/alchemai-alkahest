//! Screen 16, Settings (ALK-US-0081 / ALK-T-0218, ALK-US-0095).
//!
//! Server settings (folders, Ollama store reuse, model loading, logging) are edited as a
//! draft and saved to the server's `config.toml`; env-locked fields are read-only with a
//! lock note. The side column holds the app's own settings: the server this app talks to and
//! its API key (OS keyring, never a file), the theme (applies at once, `desktop.toml`), and
//! the configuration sources. Network settings stay on screen 14 (linked).

use std::path::PathBuf;

use alkahest_client::admin::ServerSettingsDoc;
use alkahest_client::{Client, ClientError};
use serde_json::Value;

use crate::connection::{ConnectionState, Snapshot};
use crate::launcher::is_local_url;
use crate::nav::Screen;
use crate::settings::{ApiKeyStore, DesktopSettings, KeyringKeyStore};
use crate::settings_screen::{
    ConnectionDraft, KEEP_ALIVE_PRESETS, LOG_FORMATS, LOG_LEVELS, LOG_ROTATIONS, ServerDraft,
    auto_text, key_status,
};
use crate::theme::ThemePreference;
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::input::{Input, InputEvent, InputState};
use crate::ui::{
    AnyElement, AppContext as _, Context, Div, Entity, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, h_flex, px, v_flex,
};
use crate::views::icons;
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, card, checkbox, choice_chip, dot, empty_block, field_frame, field_label,
    mono_caption, notice_line, segment, segmented,
};

/// Width of the side column.
const SIDE_WIDTH: f32 = 420.0;

/// Server fields edited through text inputs.
const TEXT_FIELDS: [&str; 7] = [
    "models_dir",
    "engines_dir",
    "ollama_path",
    "keep_alive",
    "max_loaded",
    "num_parallel",
    "max_queue",
];

/// What the Settings screen asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEvent {
    /// Restart the local server so saved settings apply.
    RestartServer {
        /// Server process id.
        pid: Option<u32>,
    },
    /// The server URL or the API key changed: rebuild the client and reconnect.
    Reconnect,
    /// Open another screen (network settings live on API keys).
    Navigate(Screen),
}

/// The Settings view.
pub struct SettingsView {
    client: Client,
    settings_file: Option<PathBuf>,
    keyring: bool,
    env_key: bool,
    has_key: bool,
    server_url: String,
    connected: bool,
    manages_server: bool,
    server_pid: Option<u32>,
    draft: Option<ServerDraft>,
    load_error: Option<String>,
    conn: ConnectionDraft,
    editing_key: bool,
    saving: bool,
    restart_after_save: bool,
    notice: Option<(bool, String)>,
    inputs: Vec<(&'static str, Entity<InputState>)>,
    url_input: Entity<InputState>,
    key_input: Entity<InputState>,
    pending_reload: bool,
    /// The `alkahest` executable an autostart entry runs; `None` hides the toggle.
    autostart_exe: Option<PathBuf>,
    autostart: bool,
}

impl std::fmt::Debug for SettingsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsView")
            .field("connected", &self.connected)
            .field("draft", &self.draft)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    /// A Settings screen. `settings_file` is `desktop.toml`; `keyring` is false in the smoke
    /// test (no unlock prompts); `env_key` says the key comes from `ALKAHEST_API_KEY`.
    pub fn new(
        client: Client,
        settings_file: Option<PathBuf>,
        keyring: bool,
        env_key: bool,
        autostart_exe: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut inputs = Vec::new();
        for key in TEXT_FIELDS {
            let input = cx.new(|cx| InputState::new(window, cx));
            cx.subscribe_in(&input, window, move |this, input, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Change)
                    && let Some(d) = this.draft.as_mut()
                {
                    d.set_text(key, &input.read(cx).value());
                    cx.notify();
                }
            })
            .detach();
            inputs.push((key, input));
        }
        let url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("http://127.0.0.1:11435 (local server)")
        });
        cx.subscribe_in(&url_input, window, |this, input, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                this.conn.server_url = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("alk_\u{2026}")
                .masked(true)
        });
        cx.subscribe_in(&key_input, window, |this, input, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                this.conn.new_key = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        let saved_url = settings_file
            .as_deref()
            .and_then(|p| DesktopSettings::load(p).ok())
            .and_then(|s| s.server_url)
            .unwrap_or_default();
        url_input.update(cx, |s, cx| s.set_value(saved_url.clone(), window, cx));
        // Asked off the UI thread: a locked keyring may block on an unlock prompt.
        if keyring && !env_key {
            let probe = tokio_bridge::spawn(cx, async {
                tokio::task::spawn_blocking(|| KeyringKeyStore.get().ok().flatten().is_some())
                    .await
                    .unwrap_or(false)
            });
            cx.spawn(async move |this, cx| {
                let has = probe.await.unwrap_or(false);
                let _ = this.update(cx, |v, cx| {
                    v.has_key = has;
                    cx.notify();
                });
            })
            .detach();
        }
        Self {
            client,
            settings_file,
            keyring,
            env_key,
            has_key: false,
            server_url: String::new(),
            connected: false,
            manages_server: false,
            server_pid: None,
            draft: None,
            load_error: None,
            conn: ConnectionDraft {
                server_url: saved_url,
                new_key: String::new(),
            },
            editing_key: false,
            saving: false,
            restart_after_save: false,
            notice: None,
            inputs,
            url_input,
            key_input,
            pending_reload: false,
            autostart: crate::autostart::is_enabled(&crate::autostart::process_env),
            autostart_exe,
        }
    }

    /// Follows a connection snapshot.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let connected = matches!(snapshot.state, ConnectionState::Connected(_));
        self.server_url.clone_from(&snapshot.server_url);
        self.manages_server = snapshot.manages_server;
        if matches!(snapshot.state, ConnectionState::Unauthorized) && !self.editing_key {
            // 401/403: the key form is what the user needs (screen 17.6).
            self.editing_key = true;
            cx.notify();
        }
        if connected != self.connected {
            self.connected = connected;
            self.pending_reload = connected;
            cx.notify();
        }
    }

    fn input(&self, key: &str) -> Option<&Entity<InputState>> {
        self.inputs.iter().find(|(k, _)| *k == key).map(|(_, e)| e)
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let c = self.client.clone();
        let task = tokio_bridge::spawn(cx, async move {
            let (s, i) = tokio::join!(c.server_settings(), c.server_info(false));
            (s, i.ok().map(|i| i.pid))
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok((doc, pid)) = task.await else { return };
            let _ = this.update_in(cx, |v, window, cx| {
                v.server_pid = pid.filter(|p| *p > 0);
                match doc {
                    Ok(doc) => v.show(doc, window, cx),
                    Err(e) => v.load_error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show(&mut self, doc: ServerSettingsDoc, window: &mut Window, cx: &mut Context<Self>) {
        let draft = ServerDraft::new(doc);
        // Unset folders show the folder the server resolved as the placeholder.
        let resolved = |k: &str| {
            draft
                .doc()
                .paths
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        for (key, path_key) in [
            ("models_dir", "models_dir"),
            ("engines_dir", "engines_dir"),
            ("ollama_path", "ollama_store"),
        ] {
            let hint = resolved(path_key);
            if let Some(input) = self.input(key).cloned() {
                input.update(cx, |s, cx| s.set_placeholder(hint, window, cx));
            }
        }
        for (key, input) in &self.inputs {
            let text = match *key {
                "max_loaded" => auto_text(draft.value(key).as_u64().unwrap_or(0), "Auto"),
                "num_parallel" => auto_text(draft.value(key).as_u64().unwrap_or(0), "Auto"),
                _ => draft.text(key),
            };
            input.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        if self.restart_after_save && !draft.doc().restart_required {
            // The server came back with the saved values.
            self.restart_after_save = false;
            self.notice = Some((
                false,
                "Server restarted; the saved settings are live.".into(),
            ));
        }
        // Setting the inputs fired change events against the old draft; start clean.
        self.draft = Some(draft);
        self.load_error = None;
    }

    fn call<R: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        window: &mut Window,
        fut: impl std::future::Future<Output = Result<R, ClientError>> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<R, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let task = tokio_bridge::spawn(cx, fut);
        cx.spawn_in(window, async move |this, cx| {
            let r = match task.await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let _ = this.update_in(cx, |v, window, cx| {
                done(v, r, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &self.draft else { return };
        let patch = match d.patch() {
            Ok(p) => p,
            Err(e) => {
                self.notice = Some((true, e));
                cx.notify();
                return;
            }
        };
        let restart = self.manages_server;
        if patch.is_empty() {
            if restart && d.doc().restart_required {
                cx.emit(SettingsEvent::RestartServer {
                    pid: self.server_pid,
                });
            }
            return;
        }
        self.saving = true;
        self.restart_after_save = restart;
        let c = self.client.clone();
        self.call(
            cx,
            window,
            async move { c.update_server_settings(&patch).await },
            |v, r, window, cx| {
                v.saving = false;
                match r {
                    Ok(doc) => {
                        v.show(doc, window, cx);
                        if v.restart_after_save {
                            v.notice = Some((false, "Saved; restarting the server\u{2026}".into()));
                            cx.emit(SettingsEvent::RestartServer { pid: v.server_pid });
                        } else {
                            v.notice = Some((
                                false,
                                "Saved to the server's config file; restart the server to apply."
                                    .into(),
                            ));
                        }
                    }
                    Err(e) => v.notice = Some((true, e)),
                }
            },
        );
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(doc) = self.draft.as_ref().map(|d| d.doc().clone()) {
            self.show(doc, window, cx);
        }
        cx.notify();
    }

    fn set_theme(&mut self, pref: ThemePreference, window: &mut Window, cx: &mut Context<Self>) {
        Theme::install(cx, pref, window.appearance());
        if let Some(path) = &self.settings_file {
            let mut s = DesktopSettings::load(path).unwrap_or_default();
            s.theme = pref;
            if let Err(e) = s.save(path) {
                self.notice = Some((true, e.to_string()));
            }
        }
        cx.notify();
    }

    /// Saves the server URL (and a typed key) and asks the shell to reconnect.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let url = match self.conn.url() {
            Ok(u) => u,
            Err(e) => {
                self.notice = Some((true, e));
                cx.notify();
                return;
            }
        };
        if let Some(path) = &self.settings_file {
            let mut s = DesktopSettings::load(path).unwrap_or_default();
            s.server_url = url;
            if let Err(e) = s.save(path) {
                self.notice = Some((true, e.to_string()));
                cx.notify();
                return;
            }
        }
        if !self.store_key(cx) {
            return;
        }
        cx.emit(SettingsEvent::Reconnect);
    }

    /// Writes a typed key to the keyring; false when that failed.
    fn store_key(&mut self, cx: &mut Context<Self>) -> bool {
        let key = self.conn.new_key.trim().to_string();
        if key.is_empty() || !self.keyring {
            return true;
        }
        match KeyringKeyStore.set(&key) {
            Ok(()) => {
                self.has_key = true;
                self.editing_key = false;
                self.conn.new_key.clear();
                true
            }
            Err(e) => {
                self.notice = Some((true, e.to_string()));
                cx.notify();
                false
            }
        }
    }

    fn remove_key(&mut self, cx: &mut Context<Self>) {
        if self.keyring && KeyringKeyStore.delete().is_ok() {
            self.has_key = false;
            cx.emit(SettingsEvent::Reconnect);
        }
        cx.notify();
    }

    fn header(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let file = self
            .draft
            .as_ref()
            .map(|d| d.doc().file.clone())
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| "config.toml".into());
        let mut row = h_flex().items_center().gap(px(space::S3)).child(
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
                        .child("Server settings write")
                        .child(div().type_style(text::MONO).child(file))
                        .child("\u{b7} app settings write")
                        .child(div().type_style(text::MONO).child("desktop.toml")),
                ),
        );
        let Some(d) = &self.draft else { return row };
        if let Some(status) = d.status_line() {
            row = row.child(
                h_flex()
                    .gap(px(space::S2))
                    .items_center()
                    .type_style(text::SMALL)
                    .text_color(c(p.accent_text))
                    .child(dot(c(p.accent_default), 7.0))
                    .child(status),
            );
        }
        let dirty = d.dirty();
        let mut discard = button(
            "settings-discard",
            "Discard",
            ButtonKind::Secondary,
            size::CONTROL,
            !dirty,
            p,
        );
        if dirty {
            discard = discard.on_click(cx.listener(|this, _, window, cx| this.discard(window, cx)));
        }
        let can_save = dirty || (self.manages_server && d.doc().restart_required);
        let mut save = button(
            "settings-save",
            d.save_label(self.manages_server),
            ButtonKind::Primary,
            size::CONTROL,
            !can_save || self.saving,
            p,
        );
        if can_save && !self.saving {
            save = save.on_click(cx.listener(|this, _, window, cx| this.save(window, cx)));
        }
        row.child(discard).child(save)
    }

    /// A text field for server setting `key` with its changed tag or lock note.
    fn field(&self, key: &'static str, label: &str, p: &Palette, mono: bool) -> Div {
        let Some(d) = &self.draft else { return div() };
        let locked = d.locked(key).map(str::to_string);
        let changed = d.changed(key);
        let mut head = h_flex()
            .gap(px(space::S2))
            .items_center()
            .child(field_label(label.to_string(), p));
        if changed {
            head = head.child(
                div()
                    .px(px(6.0))
                    .rounded(px(radius::XS))
                    .bg(c(p.accent_subtle))
                    .text_color(c(p.accent_text))
                    .type_style(text::CAPTION)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("changed \u{b7} restart"),
            );
        }
        let mut frame = field_frame(d.error(key).is_some(), p);
        if changed {
            frame = frame.border_color(c(p.accent_default));
        }
        let inner: AnyElement = match (&locked, self.input(key)) {
            (Some(_), _) | (None, None) => div()
                .flex_1()
                .text_color(c(p.text_muted))
                .child(d.text(key))
                .into_any_element(),
            (None, Some(input)) => div()
                .flex_1()
                .min_w_0()
                .child(Input::new(input).appearance(false))
                .into_any_element(),
        };
        if locked.is_some() {
            frame = frame.border_dashed().border_color(c(p.line_dashed));
        }
        if mono {
            frame = frame.type_style(text::MONO);
        }
        let mut col = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(6.0))
            .child(head)
            .child(frame.child(inner));
        if let Some(e) = d.error(key) {
            col = col.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.danger_text))
                    .child(e.to_string()),
            );
        }
        col
    }

    fn storage_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let Some(d) = &self.draft else { return div() };
        let paths = &d.doc().paths;
        let path_of = |k: &str| {
            paths
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let mut el = card(p)
            .child(div().type_style(text::TITLE).child("Storage"))
            .child(self.field("models_dir", "Models folder", p, true));
        if d.changed("models_dir") {
            el = el.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .child(format!(
                        "Was {}. Existing models are not moved; the Library lists the new folder \
                         after the restart.",
                        path_of("models_dir")
                    )),
            );
        } else {
            el = el.child(mono_caption(format!("now {}", path_of("models_dir")), p));
        }
        el = el
            .child(self.field("engines_dir", "Engines folder", p, true))
            .child(mono_caption(format!("now {}", path_of("engines_dir")), p));
        let on = d.value("ollama_enabled").as_bool().unwrap_or(false);
        let locked = d.locked("ollama_enabled").is_some();
        let mut toggle = h_flex()
            .id("settings-ollama")
            .gap(px(space::S2))
            .items_center()
            .child(checkbox("settings-ollama-box", on, p))
            .child(
                div()
                    .type_style(text::SMALL)
                    .child(if on { "On" } else { "Off" }),
            );
        if !locked {
            toggle = toggle
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(d) = this.draft.as_mut() {
                        d.set_value("ollama_enabled", Value::Bool(!on));
                    }
                    cx.notify();
                }));
        }
        let store = path_of("ollama_store");
        el.child(
            h_flex()
                .items_center()
                .gap(px(space::S3))
                .pt(px(space::S2))
                .border_t_1()
                .border_color(c(p.line_subtle))
                .child(
                    v_flex()
                        .flex_1()
                        .child(
                            div()
                                .type_style(text::SMALL)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Use models from the Ollama store"),
                        )
                        .child(mono_caption(
                            if store.is_empty() {
                                "no Ollama store found \u{b7} read-only, Alkahest never writes there"
                                    .to_string()
                            } else {
                                format!("{store} \u{b7} read-only, Alkahest never writes there")
                            },
                            p,
                        )),
                )
                .child(toggle),
        )
        .child(self.field("ollama_path", "Other Ollama store path (blank = detect)", p, true))
    }

    fn loading_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let Some(d) = &self.draft else { return div() };
        let preset = d.keep_alive_preset();
        let locked = d.locked("keep_alive").is_some();
        let mut chips = h_flex().gap(px(space::S2)).flex_wrap().items_center();
        for (i, (label, value)) in KEEP_ALIVE_PRESETS.iter().enumerate() {
            let mut chip = choice_chip(
                ("settings-keep", i),
                *label,
                preset == Some(*value),
                true,
                p,
            );
            if !locked {
                let value = *value;
                chip = chip.on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(d) = this.draft.as_mut() {
                        d.set_value("keep_alive", Value::String(value.into()));
                    }
                    if let Some(input) = this.input("keep_alive").cloned() {
                        input.update(cx, |s, cx| s.set_value(value, window, cx));
                    }
                    cx.notify();
                }));
            }
            chips = chips.child(chip);
        }
        card(p)
            .child(div().type_style(text::TITLE).child("Model loading"))
            .child(field_label("Default keep-alive", p))
            .child(chips)
            .child(div().w(px(220.0)).child(self.field(
                "keep_alive",
                "Custom (1h30m, 0, -1)",
                p,
                true,
            )))
            .child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .child(
                        "Idle time before a model leaves memory. A request's keep_alive and \
                         per-model settings win.",
                    ),
            )
            .child(
                h_flex()
                    .gap(px(space::S3))
                    .child(self.field("max_loaded", "Max loaded models", p, true))
                    .child(self.field("num_parallel", "Parallel per model", p, true))
                    .child(self.field("max_queue", "Queue limit", p, true)),
            )
            .children(self.lock_notes(p))
    }

    fn lock_notes(&self, p: &Palette) -> Vec<Div> {
        let Some(d) = &self.draft else {
            return Vec::new();
        };
        d.doc()
            .locked
            .iter()
            .map(|(field, var)| {
                h_flex()
                    .gap(px(space::S2))
                    .items_center()
                    .px(px(14.0))
                    .py(px(space::S2))
                    .rounded(px(radius::LG))
                    .border_1()
                    .border_color(c(p.line_default))
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child("\u{1f512}")
                    .child(format!(
                        "{field} is set by {var} in the service environment and cannot be \
                         changed here."
                    ))
            })
            .collect()
    }

    fn connection_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let local = is_local_url(&self.server_url);
        let mut el = card(p)
            .child(div().type_style(text::TITLE).child("Server connection"))
            .child(field_label("This app talks to", p))
            .child(
                h_flex()
                    .gap(px(space::S2))
                    .child(
                        field_frame(self.conn.url().is_err(), p)
                            .flex_1()
                            .type_style(text::MONO)
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.url_input).appearance(false)),
                            ),
                    )
                    .child(
                        button(
                            "settings-connect",
                            "Connect",
                            ButtonKind::Secondary,
                            size::CONTROL,
                            false,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                    ),
            )
            .child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .child(format!(
                        "Now {}. A remote server needs an API key, stored in the system keyring. \
                         Bind address, port and CORS live in API keys & network.",
                        crate::nav::display_address(&self.server_url)
                    )),
            )
            .child(
                div()
                    .id("settings-to-keys")
                    .type_style(text::CAPTION)
                    .text_color(c(p.accent_text))
                    .cursor_pointer()
                    .child("Open API keys & network \u{2192}")
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SettingsEvent::Navigate(Screen::ApiKeys));
                    })),
            );
        let status = key_status(local, self.has_key, self.env_key);
        let mut key_row = h_flex()
            .items_center()
            .gap(px(space::S2))
            .pt(px(space::S2))
            .border_t_1()
            .border_color(c(p.line_subtle))
            .child(div().type_style(text::SMALL).child("API key"))
            .child(div().flex_1().child(mono_caption(status, p)));
        if !self.env_key {
            key_row = key_row.child(
                button(
                    "settings-set-key",
                    if self.has_key {
                        "Replace\u{2026}"
                    } else {
                        "Set key\u{2026}"
                    },
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    !self.keyring,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.editing_key = !this.editing_key;
                    cx.notify();
                })),
            );
            if self.has_key {
                key_row = key_row.child(
                    button(
                        "settings-remove-key",
                        "Remove",
                        ButtonKind::Danger,
                        size::CONTROL_SMALL,
                        false,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.remove_key(cx))),
                );
            }
        }
        el = el.child(key_row);
        if self.editing_key && !self.env_key {
            el = el
                .child(
                    field_frame(false, p).child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.key_input).appearance(false).mask_toggle()),
                    ),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(space::S2))
                        .child(
                            div()
                                .flex_1()
                                .type_style(text::CAPTION)
                                .text_color(c(p.text_muted))
                                .child("Kept in the system keyring, never in a file."),
                        )
                        .child(
                            button(
                                "settings-key-connect",
                                "Save & connect",
                                ButtonKind::Secondary,
                                size::CONTROL_SMALL,
                                self.conn.new_key.trim().is_empty(),
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                        ),
                );
        }
        el
    }

    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        let Some(exe) = &self.autostart_exe else {
            return;
        };
        let on = !self.autostart;
        match crate::autostart::set_enabled(on, exe, &crate::autostart::process_env) {
            Ok(()) => self.autostart = on,
            Err(e) => self.notice = Some((true, format!("Start at login: {e}"))),
        }
        cx.notify();
    }

    fn desktop_card(&self, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        self.autostart_exe.as_ref()?;
        let on = self.autostart;
        Some(
            card(p)
                .child(div().type_style(text::TITLE).child("Desktop"))
                .child(
                    h_flex()
                        .id("settings-autostart")
                        .gap(px(space::S2))
                        .items_center()
                        .cursor_pointer()
                        .child(checkbox("settings-autostart-box", on, p))
                        .child(
                            div()
                                .type_style(text::SMALL)
                                .child("Start Alkahest at login"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_autostart(cx))),
                )
                .child(
                    div()
                        .type_style(text::CAPTION)
                        .text_color(c(p.text_muted))
                        .child(
                            "Opens this app at login. With a system tray, closing the window keeps \
                             Alkahest in the tray; Quit is in the tray menu. The server itself \
                             starts with its service (alkahest service install --user).",
                        ),
                ),
        )
    }

    fn appearance_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let current = Theme::get(cx).preference;
        let segs = [
            ("Dark", ThemePreference::Dark),
            ("Light", ThemePreference::Light),
            ("System", ThemePreference::System),
        ]
        .into_iter()
        .map(|(label, pref)| {
            segment(
                SharedString::from(format!("theme-{label}")),
                label,
                current == pref,
                p,
            )
            .on_click(cx.listener(move |this, _, window, cx| this.set_theme(pref, window, cx)))
        })
        .collect();
        card(p)
            .child(div().type_style(text::TITLE).child("Appearance"))
            .child(segmented(segs, p))
            .child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .child("Applies immediately. Saved in desktop.toml."),
            )
    }

    fn choice_row(
        &self,
        field: &'static str,
        options: &'static [&'static str],
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(d) = &self.draft else { return div() };
        let current = d.text(field);
        let locked = d.locked(field).is_some();
        let mut row = h_flex().gap(px(space::S2)).flex_wrap();
        for (i, opt) in options.iter().enumerate() {
            let mut chip = choice_chip(
                SharedString::from(format!("settings-{field}-{i}")),
                *opt,
                current == *opt,
                true,
                p,
            );
            if !locked {
                let opt = *opt;
                chip = chip.on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(d) = this.draft.as_mut() {
                        d.set_value(field, Value::String(opt.into()));
                    }
                    cx.notify();
                }));
            }
            row = row.child(chip);
        }
        if d.changed(field) {
            row = row.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.accent_text))
                    .child("changed \u{b7} restart"),
            );
        }
        if let Some(var) = d.locked(field) {
            row = row.child(mono_caption(format!("\u{1f512} {var}"), p));
        }
        row
    }

    fn logging_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let Some(d) = &self.draft else { return div() };
        let dir = d
            .doc()
            .paths
            .get("log_dir")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        card(p)
            .child(div().type_style(text::TITLE).child("Logging"))
            .child(field_label("Level", p))
            .child(self.choice_row("log_level", &LOG_LEVELS, p, cx))
            .child(field_label("Format", p))
            .child(self.choice_row("log_format", &LOG_FORMATS, p, cx))
            .child(field_label("Rotation", p))
            .child(self.choice_row("log_rotation", &LOG_ROTATIONS, p, cx))
            .child(mono_caption(
                format!("{dir} \u{b7} {}", d.text("log_rotation")),
                p,
            ))
    }

    fn sources_card(&self, p: &Palette) -> Div {
        let Some(d) = &self.draft else { return div() };
        let mut el = v_flex()
            .flex_shrink_0()
            .p(px(size::CARD_PADDING))
            .gap(px(space::S1))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .child(
                div()
                    .type_style(text::LABEL)
                    .text_color(c(p.text_muted))
                    .child("LOADED FROM"),
            );
        let sources = if d.doc().sources.is_empty() {
            vec!["built-in defaults".to_string()]
        } else {
            d.doc().sources.clone()
        };
        for s in sources {
            el = el.child(mono_caption(s, p));
        }
        el
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.pending_reload) {
            self.reload(window, cx);
        }
        let p = Theme::get(cx).palette;
        let mut content = v_flex()
            .size_full()
            .gap(px(space::S5))
            .child(self.header(p, cx));
        if let Some((danger, msg)) = self.notice.clone() {
            content = content.child(notice_line(msg, danger, p));
        }
        let mut main = v_flex().flex_1().min_w_0().gap(px(space::S4));
        if self.draft.is_some() {
            main = main
                .child(self.storage_card(p, cx))
                .child(self.loading_card(p, cx));
        } else if let Some(e) = &self.load_error {
            main = main.child(notice_line(
                format!("Cannot read the server settings: {e}"),
                true,
                p,
            ));
        } else {
            main = main.child(empty_block(
                "settings-disconnected",
                icons::SETTINGS,
                if self.connected {
                    "Loading\u{2026}"
                } else {
                    "Server settings need a server"
                },
                "Folders, model loading and logging are the server's settings. The app settings \
                 on the right work without one.",
                Vec::new(),
                None,
                p,
            ));
        }
        let mut side = v_flex()
            .w(px(SIDE_WIDTH))
            .flex_shrink_0()
            .gap(px(space::S4))
            .child(self.connection_card(p, cx))
            .child(self.appearance_card(p, cx))
            .children(self.desktop_card(p, cx));
        if self.draft.is_some() {
            side = side
                .child(self.logging_card(p, cx))
                .child(self.sources_card(p));
        }
        content.child(
            div()
                .id("settings-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    h_flex()
                        .items_start()
                        .gap(px(space::S5))
                        .child(main)
                        .child(side),
                ),
        )
    }
}
