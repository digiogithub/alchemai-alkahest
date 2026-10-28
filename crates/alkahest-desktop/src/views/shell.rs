//! The window shell: 232 px left rail (wordmark, navigation with live counters, service card)
//! and the content area of the selected screen (`design/README.md`, *Shell*), plus the
//! connection states of screen 17 that are not owned by one screen: first run (17.3), server
//! stopped (17.4), connection lost (17.5) and unauthorized (17.6).
//!
//! The screens are built by [`Shell::build_screens`] from one client; "Connect" in Settings
//! (another server URL or API key) rebuilds the client, the connection manager and the
//! screens with [`Shell::reconnect`].

use std::time::{Duration, Instant};

use alkahest_client::Client;
use tokio::sync::{mpsc, watch};

use crate::LaunchOptions;
use crate::connection::{CardAction, Command, ConnectionHandle, ConnectionState, Snapshot};
use crate::nav::{Counter, NavCounters, Screen, ServiceCard, Tone, display_address};
use crate::settings::{ApiKeyStore, DesktopSettings, KeyringKeyStore};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::tray::{TrayAction, TrayModel};
use crate::ui::input::{Input, InputState};
use crate::ui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Task, Window, div, h_flex, px, svg_icon, v_flex,
};
use crate::views::catalogs::{CatalogsEvent, CatalogsView};
use crate::views::discover::{DiscoverEvent, DiscoverView};
use crate::views::engines::EnginesView;
use crate::views::icons;
use crate::views::keys::{KeysEvent, KeysView};
use crate::views::library::{LibraryEvent, LibraryView};
use crate::views::running::{RunningEvent, RunningView};
use crate::views::settings::{SettingsEvent, SettingsView};
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{ButtonKind, button, field_frame, mono_caption, warning_banner};

/// The screens of one connection.
struct Screens {
    library: Entity<LibraryView>,
    running: Entity<RunningView>,
    discover: Entity<DiscoverView>,
    engines: Entity<EnginesView>,
    keys: Entity<KeysView>,
    settings: Entity<SettingsView>,
    catalogs: Entity<CatalogsView>,
}

/// Root view of the main window.
pub struct Shell {
    options: LaunchOptions,
    screen: Screen,
    counters: NavCounters,
    service: ServiceCard,
    card_action: Option<CardAction>,
    snapshot: Option<Snapshot>,
    commands: mpsc::UnboundedSender<Command>,
    screens: Screens,
    /// A server answered at least once since the app (or the last reconnect) started.
    ever_connected: bool,
    key_input: Entity<InputState>,
    follow: Option<Task<()>>,
    /// The client of the current connection (tray actions).
    client: Client,
    /// Name of the active connection ([`crate::settings::LOCAL`] for the configured server).
    connection: String,
    /// Re-renders once a second while a reconnect countdown is on screen.
    _ticker: Task<()>,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell")
            .field("screen", &self.screen)
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl Shell {
    /// A shell showing the Library, with unknown counters and the "connecting" card for
    /// `server_url` of connection `connection`. `commands` reaches the connection manager;
    /// `client` is what the screens use for their own requests; `options` let the shell
    /// reconnect.
    pub fn new(
        options: LaunchOptions,
        server_url: &str,
        connection: String,
        commands: mpsc::UnboundedSender<Command>,
        client: Client,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let screens = Self::build_screens(&options, client.clone(), window, cx);
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |shell, cx| {
                    if shell.snapshot.as_ref().is_some_and(|s| s.retry.is_some()) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("alk_\u{2026}")
                .masked(true)
        });
        Self {
            options,
            screen: Screen::Library,
            counters: NavCounters::default(),
            service: ServiceCard::connecting(server_url),
            card_action: None,
            snapshot: None,
            commands,
            screens,
            ever_connected: false,
            key_input,
            follow: None,
            client,
            connection,
            _ticker: ticker,
        }
    }

    fn build_screens(
        options: &LaunchOptions,
        client: Client,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Screens {
        let library = cx.new(|cx| LibraryView::new(client.clone(), window, cx));
        // Shared by Discover, Engines and Settings: one listing and one event feed.
        let catalogs = cx.new(|cx| CatalogsView::new(client.clone(), window, cx));
        let discover = cx.new(|cx| DiscoverView::new(client.clone(), catalogs.clone(), window, cx));
        let running = cx.new(|cx| RunningView::new(client.clone(), library.clone(), cx));
        let engines = cx.new(|cx| EnginesView::new(client.clone(), catalogs.clone(), cx));
        let keys = cx.new(|cx| KeysView::new(client.clone(), window, cx));
        let settings = cx.new(|cx| {
            SettingsView::new(
                client,
                options.settings_file.clone(),
                !options.smoke_test,
                options.api_key.is_some(),
                options.serve_program.clone(),
                catalogs.clone(),
                window,
                cx,
            )
        });
        cx.subscribe(&catalogs, |this, _, event: &CatalogsEvent, cx| {
            if *event == CatalogsEvent::Open {
                this.screens
                    .settings
                    .update(cx, |view, cx| view.show_catalogs(cx));
                this.select(Screen::Settings, cx);
            }
        })
        .detach();
        cx.subscribe(
            &discover,
            |this, _, event: &DiscoverEvent, cx| match event {
                // A finished download: the Library lists the new model right away.
                DiscoverEvent::ModelsChanged => {
                    this.screens.library.update(cx, |view, cx| view.reload(cx))
                }
            },
        )
        .detach();
        // Active downloads and install jobs feed the rail counters.
        cx.observe(&discover, |_, _, cx| cx.notify()).detach();
        cx.observe(&engines, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&running, |this, _, event: &RunningEvent, cx| match event {
            RunningEvent::Navigate(screen) => this.select(*screen, cx),
        })
        .detach();
        cx.subscribe(&library, |this, _, event: &LibraryEvent, cx| match event {
            LibraryEvent::Navigate(screen) => this.select(*screen, cx),
        })
        .detach();
        cx.subscribe(&keys, |this, _, event: &KeysEvent, _| match event {
            KeysEvent::RestartServer { pid } => {
                let _ = this.commands.send(Command::RestartServer { pid: *pid });
            }
        })
        .detach();
        cx.subscribe_in(
            &settings,
            window,
            |this, _, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::RestartServer { pid } => {
                    let _ = this.commands.send(Command::RestartServer { pid: *pid });
                }
                SettingsEvent::Reconnect => this.reconnect(window, cx),
                SettingsEvent::Navigate(screen) => this.select(*screen, cx),
            },
        )
        .detach();
        // The Library's own count (fresh after a delete) feeds the rail counter.
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Screens {
            library,
            running,
            discover,
            engines,
            keys,
            settings,
            catalogs,
        }
    }

    /// Re-renders `shell` with every snapshot the connection manager publishes, until the
    /// shell or the manager goes away. `watch` channels work on any executor, so this runs on
    /// GPUI's foreground executor without the tokio bridge.
    pub fn follow(shell: &Entity<Self>, snapshots: watch::Receiver<Snapshot>, cx: &mut App) {
        shell.update(cx, |this, cx| this.start_following(snapshots, cx));
    }

    fn start_following(
        &mut self,
        mut snapshots: watch::Receiver<Snapshot>,
        cx: &mut Context<Self>,
    ) {
        self.follow = Some(cx.spawn(async move |this, cx| {
            loop {
                let snapshot = snapshots.borrow_and_update().clone();
                if this
                    .update(cx, |shell, cx| shell.apply(&snapshot, cx))
                    .is_err()
                {
                    return;
                }
                if snapshots.changed().await.is_err() {
                    return;
                }
            }
        }));
    }

    /// Rebuilds the client from the (just saved) desktop settings and keyring, starts a new
    /// connection manager and new screens. The old manager stops when its command channel
    /// closes.
    pub fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match crate::app::connect(cx, &self.options) {
            Ok(crate::app::Connected {
                name,
                url,
                handle:
                    ConnectionHandle {
                        commands,
                        snapshots,
                    },
                client,
            }) => {
                self.connection = name;
                self.commands = commands;
                self.client = client.clone();
                self.screens = Self::build_screens(&self.options, client, window, cx);
                self.service = ServiceCard::connecting(&url);
                self.card_action = None;
                self.snapshot = None;
                self.ever_connected = false;
                self.start_following(snapshots, cx);
            }
            Err(e) => tracing::warn!("reconnect failed: {e}"),
        }
        cx.notify();
    }

    /// Switches the content area to `screen`.
    pub fn select(&mut self, screen: Screen, cx: &mut Context<Self>) {
        if self.screen != screen {
            if self.screen == Screen::ApiKeys {
                // A key secret shown once must not survive leaving the screen.
                self.screens.keys.update(cx, |v, cx| v.on_hidden(cx));
            }
            self.screen = screen;
            cx.notify();
        }
    }

    /// Carries out a tray menu click (ALK-US-0073).
    pub fn tray_action(&mut self, action: TrayAction, cx: &mut Context<Self>) {
        match action {
            TrayAction::StartServer => {
                let _ = self.commands.send(Command::StartServer);
            }
            TrayAction::StopServer => {
                // The server's pid (for a server no service unit manages) comes from the
                // admin info route.
                let client = self.client.clone();
                let commands = self.commands.clone();
                tokio_bridge::spawn(cx, async move {
                    let pid = client.server_info(false).await.ok().map(|i| i.pid);
                    let _ = commands.send(Command::StopServer { pid });
                })
                .detach();
            }
            TrayAction::StopAllModels => {
                let client = self.client.clone();
                tokio_bridge::spawn(cx, async move {
                    if let Err(e) = client.unload_all().await {
                        tracing::warn!("tray: stop all models failed: {e}");
                    }
                })
                .detach();
            }
            TrayAction::Open | TrayAction::Quit => {}
        }
    }

    /// Shows a connection snapshot: service card, its button and the rail counters.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        crate::app::update_tray(
            TrayModel {
                running: matches!(snapshot.state, ConnectionState::Connected(_)),
                busy: matches!(
                    snapshot.state,
                    ConnectionState::Starting { .. } | ConnectionState::Stopping { .. }
                ),
                loaded: snapshot.counters().loaded.unwrap_or(0),
                manages_server: snapshot.manages_server,
            },
            cx,
        );
        let s = &self.screens;
        s.library.update(cx, |view, cx| view.apply(snapshot, cx));
        s.running.update(cx, |view, cx| view.apply(snapshot, cx));
        s.discover.update(cx, |view, cx| view.apply(snapshot, cx));
        s.engines.update(cx, |view, cx| view.apply(snapshot, cx));
        s.keys.update(cx, |view, cx| view.apply(snapshot, cx));
        s.settings.update(cx, |view, cx| view.apply(snapshot, cx));
        s.catalogs.update(cx, |view, cx| view.apply(snapshot, cx));
        if matches!(snapshot.state, ConnectionState::Connected(_)) {
            self.ever_connected = true;
        }
        let service = snapshot.service_card();
        let action = snapshot.card_action();
        let counters = snapshot.counters();
        if self.snapshot.as_ref() != Some(snapshot) {
            self.snapshot = Some(snapshot.clone());
            cx.notify();
        }
        if self.service != service || self.card_action != action || self.counters != counters {
            self.service = service;
            self.card_action = action;
            self.counters = counters;
            cx.notify();
        }
    }

    /// Sends a service-card action to the connection manager.
    fn act(&mut self, action: CardAction) {
        let cmd = match action {
            CardAction::StartServer => Command::StartServer,
            CardAction::Retry => Command::Retry,
        };
        // A closed channel means the manager stopped; there is nothing left to tell.
        let _ = self.commands.send(cmd);
    }

    /// Saves the key typed in the unauthorized panel to the keyring and reconnects.
    fn connect_with_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).value().trim().to_string();
        if key.is_empty() || self.options.smoke_test {
            return;
        }
        if let Err(e) = KeyringKeyStore::for_connection(&self.connection).set(&key) {
            tracing::warn!("cannot store the API key: {e}");
            return;
        }
        self.key_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.reconnect(window, cx);
    }

    /// Switches to the built-in local connection (the saved ones are kept) and reconnects.
    fn use_local_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = &self.options.settings_file {
            let mut s = DesktopSettings::load(path).unwrap_or_default();
            s.active = None;
            if let Err(e) = s.save(path) {
                tracing::warn!("{e}");
                return;
            }
        }
        self.reconnect(window, cx);
    }

    fn rail(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let first_run = self.first_run();
        let mut rail = v_flex()
            .w(px(size::SIDEBAR))
            .flex_shrink_0()
            .h_full()
            .px(px(14.0))
            .py(px(18.0))
            .gap(px(space::S1))
            .bg(c(p.bg_sidebar))
            .border_r_1()
            .border_color(c(p.line_default))
            .child(wordmark(p));
        for screen in Screen::ALL {
            rail = rail.child(self.nav_item(screen, first_run, p, cx));
        }
        let mut card = service_card(&self.service, p);
        if let Some(action) = self.card_action {
            card = card.child(
                card_button(action, p).on_click(cx.listener(move |this, _, _, _| this.act(action))),
            );
        }
        rail.child(div().flex_1()).child(card)
    }

    fn nav_item(
        &self,
        screen: Screen,
        disabled: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.screen == screen && !disabled;
        let (fg, weight_style) = if active {
            (c(p.accent_text), text::H2)
        } else if disabled {
            (c(p.text_faint), text::BODY)
        } else {
            (c(p.text_secondary), text::BODY)
        };
        let icon = match screen {
            Screen::Library => icons::LIBRARY,
            Screen::Discover => icons::DISCOVER,
            Screen::Running => icons::RUNNING,
            Screen::Engines => icons::ENGINES,
            Screen::ApiKeys => icons::API_KEYS,
            Screen::Settings => icons::SETTINGS,
        };
        let mut item = h_flex()
            .id(screen.id())
            .h(px(size::NAV_ITEM))
            .px(px(space::S3))
            .gap(px(space::S3))
            .items_center()
            .rounded(px(radius::MD))
            .text_color(fg)
            .type_style(weight_style)
            .text_size(px(text::BODY.size))
            .child(svg_icon(icon, size::ICON, fg))
            .child(screen.label());
        // First run: the rail is disabled except Settings (connect to a remote server).
        if !disabled || screen == Screen::Settings {
            item = item
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.select(screen, cx)));
        }
        if active {
            item = item.bg(c(p.accent_subtle));
        } else if !disabled {
            item = item.hover(|s| s.bg(c(p.bg_overlay)));
        }
        if let Some(counter) = self.live_counters(cx).counter(screen) {
            item = item.child(counter_badge(&counter, active, p));
        }
        item
    }

    /// The rail counters: the connection's, with the screens' own counts once they have them
    /// (the Library count changes right after a delete, before the next probe).
    fn live_counters(&self, cx: &App) -> NavCounters {
        let mut counters = self.counters;
        if counters.models.is_some()
            && let Some(n) = self.screens.library.read(cx).model_count()
        {
            counters.models = Some(n);
        }
        if let Some(n) = self.screens.discover.read(cx).active_downloads() {
            counters.downloads = Some(n);
        }
        if let Some(n) = self.screens.engines.read(cx).active_jobs() {
            counters.installs = Some(n);
        }
        counters
    }

    /// First run (17.3): nothing ever answered, the server is local and not running.
    fn first_run(&self) -> bool {
        !self.ever_connected
            && self.snapshot.as_ref().is_some_and(|s| {
                s.manages_server
                    && matches!(
                        s.state,
                        ConnectionState::NotRunning { .. } | ConnectionState::Starting { .. }
                    )
            })
    }

    /// The content of the selected screen.
    fn content(&self) -> AnyElement {
        let s = &self.screens;
        match self.screen {
            Screen::Library => s.library.clone().into_any_element(),
            Screen::Running => s.running.clone().into_any_element(),
            Screen::Discover => s.discover.clone().into_any_element(),
            Screen::Engines => s.engines.clone().into_any_element(),
            Screen::ApiKeys => s.keys.clone().into_any_element(),
            Screen::Settings => s.settings.clone().into_any_element(),
        }
    }

    /// Screen 17.3: three-step checklist while no server ever answered.
    fn first_run_view(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let starting = self
            .snapshot
            .as_ref()
            .is_some_and(|s| matches!(s.state, ConnectionState::Starting { .. }));
        let error = self.snapshot.as_ref().and_then(|s| match &s.state {
            ConnectionState::NotRunning { error } => error.clone(),
            _ => None,
        });
        let address = self
            .snapshot
            .as_ref()
            .map(|s| display_address(&s.server_url))
            .unwrap_or_default();
        let mut start = button(
            "first-run-start",
            if starting {
                "Starting\u{2026}"
            } else {
                "Start server"
            },
            ButtonKind::Primary,
            size::CONTROL,
            starting,
            p,
        );
        if !starting {
            start = start.on_click(cx.listener(|this, _, _, _| {
                let _ = this.commands.send(Command::StartServer);
            }));
        }
        let mut list = v_flex().max_w(px(860.0)).gap(px(space::S3)).child(step(
            1,
            "Start the local server",
            format!(
                "Runs alkahest serve on {address}. Install it as a service later to start at \
                     login."
            ),
            true,
            Some(start),
            p,
        ));
        if let Some(e) = error {
            list = list.child(mono_caption(e, p).text_color(c(p.danger_text)));
        }
        list = list
            .child(step(
                2,
                "Pick an engine build",
                "The CPU llama.cpp build is bundled. GPU builds for this machine are offered in \
                 Engines once the server runs."
                    .to_string(),
                false,
                None,
                p,
            ))
            .child(step(
                3,
                "Get a model",
                "Models of an existing Ollama store appear read-only on their own; or download \
                 one in Discover."
                    .to_string(),
                false,
                None,
                p,
            ))
            .child(
                h_flex()
                    .gap(px(space::S1))
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child("Using a server on another machine?")
                    .child(
                        div()
                            .id("first-run-remote")
                            .text_color(c(p.accent_text))
                            .cursor_pointer()
                            .child("Connect to a remote server")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ever_connected = true;
                                this.select(Screen::Settings, cx);
                            })),
                    ),
            );
        v_flex()
            .gap(px(space::S5))
            .child(
                v_flex()
                    .gap(px(space::S1))
                    .child(div().type_style(text::H1).child("Welcome to alkahest"))
                    .child(
                        div()
                            .type_style(text::BODY)
                            .text_color(c(p.text_secondary))
                            .child("Three steps to your first answer. Everything runs on this machine."),
                    ),
            )
            .child(list)
    }

    /// The connection banner over the screens (17.4, 17.5, 17.6), if any.
    fn connection_banner(&self, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        let snap = self.snapshot.as_ref()?;
        if let Some(n) = &snap.notice {
            return Some(warning_banner(format!("Cannot stop the server: {n}"), p));
        }
        match &snap.state {
            ConnectionState::NotRunning { error } if snap.manages_server => {
                // 17.4: local server stopped.
                let mut el = banner_frame(p)
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(space::S1))
                            .child(
                                div()
                                    .type_style(text::TITLE)
                                    .child("alkahest is not running"),
                            )
                            .child(
                                div()
                                    .type_style(text::SMALL)
                                    .text_color(c(p.text_secondary))
                                    .child(
                                        "Start it to list models, load them and serve requests.",
                                    ),
                            ),
                    )
                    .child(
                        button(
                            "banner-start",
                            "Start server",
                            ButtonKind::Primary,
                            size::CONTROL,
                            false,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, _| {
                            let _ = this.commands.send(Command::StartServer);
                        })),
                    );
                if let Some(e) = error {
                    el = el.child(mono_caption(e.clone(), p));
                }
                Some(el)
            }
            ConnectionState::Tls { issue, detail } => {
                // Secure connection failed: same frame as 17.5, with the cause and the fix.
                Some(
                    banner_frame(p)
                        .border_color(c(p.accent_border))
                        .bg(c(p.accent_subtle))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(space::S1))
                                .child(
                                    div()
                                        .type_style(text::TITLE)
                                        .text_color(c(p.accent_text))
                                        .child(format!(
                                            "\u{1f512} {} \u{b7} {}",
                                            issue.title(),
                                            display_address(&snap.server_url)
                                        )),
                                )
                                .child(
                                    div()
                                        .type_style(text::SMALL)
                                        .text_color(c(p.text_secondary))
                                        .child(issue.hint()),
                                )
                                .child(
                                    mono_caption(detail.clone(), p).text_color(c(p.danger_text)),
                                ),
                        )
                        .child(
                            button(
                                "banner-tls-settings",
                                "Open settings",
                                ButtonKind::Secondary,
                                size::CONTROL,
                                false,
                                p,
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.select(Screen::Settings, cx)),
                            ),
                        )
                        .child(
                            button(
                                "banner-tls-retry",
                                "Retry now",
                                ButtonKind::Primary,
                                size::CONTROL,
                                false,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, _| {
                                let _ = this.commands.send(Command::Retry);
                            })),
                        ),
                )
            }
            ConnectionState::NotRunning { .. } | ConnectionState::Error(_) => {
                // 17.5: remote server lost (Start not offered).
                let raw = match &snap.state {
                    ConnectionState::Error(e) => e.clone(),
                    _ => format!("no answer from {}", snap.server_url),
                };
                Some(
                    banner_frame(p)
                        .border_color(c(p.accent_border))
                        .bg(c(p.accent_subtle))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(space::S1))
                                .child(
                                    div()
                                        .type_style(text::TITLE)
                                        .text_color(c(p.accent_text))
                                        .child(format!(
                                            "Lost connection to {}",
                                            display_address(&snap.server_url)
                                        )),
                                )
                                .child(
                                    div()
                                        .type_style(text::SMALL)
                                        .text_color(c(p.text_secondary))
                                        .child(match &snap.retry {
                                            Some(r) => r.describe(Instant::now()),
                                            None => {
                                                "Retrying with backoff (0.5 s doubling to 10 s)."
                                                    .to_string()
                                            }
                                        }),
                                )
                                .child(mono_caption(raw, p).text_color(c(p.danger_text))),
                        )
                        .child(
                            button(
                                "banner-change",
                                "Change server",
                                ButtonKind::Secondary,
                                size::CONTROL,
                                false,
                                p,
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.select(Screen::Settings, cx)),
                            ),
                        )
                        .child(
                            button(
                                "banner-retry",
                                "Retry now",
                                ButtonKind::Primary,
                                size::CONTROL,
                                false,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, _| {
                                let _ = this.commands.send(Command::Retry);
                            })),
                        ),
                )
            }
            ConnectionState::Unauthorized => {
                // 17.6: the server wants a key.
                let local_offer = self.connection != crate::settings::LOCAL;
                let mut actions = h_flex().gap(px(space::S2)).items_center();
                if local_offer {
                    actions = actions.child(
                        button(
                            "banner-local",
                            "Use local server",
                            ButtonKind::Secondary,
                            size::CONTROL,
                            false,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.use_local_server(window, cx)),
                        ),
                    );
                }
                actions = actions.child(
                    button(
                        "banner-connect",
                        "Connect",
                        ButtonKind::Primary,
                        size::CONTROL,
                        false,
                        p,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.connect_with_key(window, cx))),
                );
                Some(
                    banner_frame(p)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(space::S2))
                                .child(div().type_style(text::TITLE).child("\u{1f512} This server needs an API key"))
                                .child(
                                    div()
                                        .type_style(text::SMALL)
                                        .text_color(c(p.text_secondary))
                                        .child(format!(
                                            "{} refused the request. Ask its owner for a key with the \
                                             admin scope, or create one there in API keys.",
                                            display_address(&snap.server_url)
                                        )),
                                )
                                .child(
                                    field_frame(false, p)
                                        .border_color(c(p.accent_default))
                                        .child(div().flex_1().child(Input::new(&self.key_input).appearance(false))),
                                )
                                .child(
                                    div()
                                        .type_style(text::CAPTION)
                                        .text_color(c(p.text_muted))
                                        .child("Kept in the system keyring, never in a file."),
                                ),
                        )
                        .child(actions),
                )
            }
            _ => None,
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Theme::get(cx).palette;
        let mut area = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .py(px(size::SCREEN_PADDING.0))
            .px(px(size::SCREEN_PADDING.1))
            .gap(px(space::S5));
        if self.first_run() {
            area = area.child(self.first_run_view(p, cx));
        } else {
            if let Some(b) = self.connection_banner(p, cx) {
                area = area.child(b);
            }
            area = area.child(div().flex_1().min_h_0().child(self.content()));
        }
        h_flex()
            .size_full()
            .bg(c(p.bg_base))
            .text_color(c(p.text_primary))
            .type_style(text::BODY)
            .child(self.rail(p, cx))
            .child(area)
    }
}

/// One checklist step of 17.3.
fn step(
    n: u8,
    title: &str,
    body: String,
    active: bool,
    action: Option<Stateful<Div>>,
    p: &Palette,
) -> Div {
    let mut el = h_flex()
        .items_center()
        .gap(px(space::S4))
        .p(px(size::CARD_PADDING))
        .rounded(px(radius::XL))
        .border_1()
        .border_color(c(if active {
            p.accent_default
        } else {
            p.line_default
        }))
        .bg(c(p.bg_raised))
        .child(
            div()
                .size(px(28.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(c(if active {
                    p.accent_subtle
                } else {
                    p.muted_subtle
                }))
                .text_color(c(if active { p.accent_text } else { p.text_muted }))
                .type_style(text::SMALL)
                .font_weight(FontWeight::SEMIBOLD)
                .child(n.to_string()),
        )
        .child(
            v_flex()
                .flex_1()
                .gap(px(space::S1))
                .child(
                    div()
                        .type_style(text::SMALL)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_secondary))
                        .child(body),
                ),
        );
    if let Some(a) = action {
        el = el.child(a);
    }
    el
}

fn banner_frame(p: &Palette) -> Div {
    h_flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(space::S4))
        .p(px(size::CARD_PADDING))
        .rounded(px(radius::XL))
        .border_1()
        .border_color(c(p.line_default))
        .bg(c(p.bg_raised))
}

/// Mark and lower-case wordmark at the top of the rail.
fn wordmark(p: &Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(10.0))
        .pt(px(6.0))
        .px(px(10.0))
        .pb(px(space::S5))
        .child(svg_icon(icons::MARK_SMALL, 26.0, c(p.accent_default)))
        .child(div().type_style(text::WORDMARK).child("alkahest"))
}

fn counter_badge(counter: &Counter, active: bool, p: &Palette) -> impl IntoElement {
    let base = h_flex()
        .ml_auto()
        .items_center()
        .gap(px(6.0))
        .type_style(text::MONO)
        .text_size(px(text::CAPTION.size));
    match counter {
        Counter::Plain(n) => {
            let fg = if active { p.accent_text } else { p.text_muted };
            base.text_color(c(fg)).child(n.clone())
        }
        Counter::Downloads(n) => base.text_color(c(p.info_text)).child(n.clone()),
        Counter::Live(n) => base
            .text_color(c(p.ok_text))
            .child(dot(c(p.ok_default), 7.0))
            .child(n.clone()),
    }
}

fn dot(color: Hsla, diameter: f32) -> Div {
    div()
        .size(px(diameter))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
}

fn tone_colors(tone: Tone, p: &Palette) -> (Hsla, Hsla) {
    let (dot, text) = match tone {
        Tone::Ok => (p.ok_default, p.ok_text),
        Tone::Info => (p.info_default, p.info_text),
        Tone::Attention => (p.accent_default, p.accent_text),
        Tone::Danger => (p.danger_default, p.danger_text),
        Tone::Muted => (p.muted_default, p.text_muted),
    };
    (c(dot), c(text))
}

/// The service card: state, bind address, service manager.
fn service_card(card: &ServiceCard, p: &Palette) -> Div {
    let (dot_color, title_color) = tone_colors(card.tone, p);
    v_flex()
        .p(px(14.0))
        .gap(px(6.0))
        .border_1()
        .border_color(c(p.line_default))
        .rounded(px(radius::LG))
        .type_style(text::CAPTION)
        .text_color(c(p.text_muted))
        .child(
            h_flex()
                .items_center()
                .gap(px(space::S2))
                .type_style(text::SMALL)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(title_color)
                .child(dot(dot_color, 8.0))
                .child(SharedString::from(card.title.clone())),
        )
        .child(
            div()
                .type_style(text::MONO)
                .text_size(px(text::CAPTION.size))
                .child(SharedString::from(card.address.clone())),
        )
        .child(SharedString::from(card.detail.clone()))
}

/// The secondary button of the service card (`Start server`, `Retry`). Not brass: the one
/// primary action of a view lives in its content area.
fn card_button(action: CardAction, p: &Palette) -> Stateful<Div> {
    h_flex()
        .id("service-card-action")
        .mt(px(space::S1))
        .h(px(size::CONTROL_SMALL))
        .px(px(space::S3))
        .items_center()
        .justify_center()
        .rounded(px(radius::MD))
        .bg(c(p.bg_overlay))
        .border_1()
        .border_color(c(p.line_strong))
        .text_color(c(p.text_primary))
        .type_style(text::SMALL)
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.border_color(c(p.accent_border)))
        .child(action.label())
}
