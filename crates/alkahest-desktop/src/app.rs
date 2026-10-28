//! Application start-up: gpui-kit, fonts, tokio bridge, settings, theme, the connection
//! manager and the main window.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use alkahest_client::Client;

use crate::connection::{ConnectionHandle, ConnectionManager};
use crate::launcher::LocalLauncher;
use crate::settings::{DesktopSettings, KeyringKeyStore, migrate_legacy_key, resolve_api_key};
use crate::theme::tokens::size;
use crate::tls;
use futures::StreamExt as _;

use crate::tray::{self, TrayAction, TrayHandle, TrayModel};
use crate::ui::{
    self, AnyWindowHandle, App, AppContext as _, Bounds, Entity, Global, TitlebarOptions,
    WeakEntity, WindowBounds, WindowOptions, px,
};
use crate::views::shell::Shell;
use crate::views::style::Theme;
use crate::{DesktopError, LaunchOptions, fonts, tokio_bridge};

/// Window title and the Linux `app_id` (matches the `.desktop` file of the packages).
const TITLE: &str = "Alkahest";
const APP_ID: &str = "es.digio.alkahest";

/// How long `--smoke-test` waits for the first frame before failing.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Exit status of a smoke test that saw no frame (the CLI's "runtime error" is 1; 3 keeps it
/// apart in CI logs).
const SMOKE_TIMEOUT_EXIT: i32 = 3;

/// Runs the desktop app until its window closes (or, with `smoke_test`, until the first frame
/// is on screen).
pub fn run(options: LaunchOptions) -> Result<(), DesktopError> {
    let failure: Rc<RefCell<Option<DesktopError>>> = Rc::new(RefCell::new(None));
    let failure_in_app = failure.clone();
    ui::application().run(move |cx| {
        if let Err(err) = start(cx, &options) {
            tracing::error!("desktop start-up failed: {err}");
            *failure_in_app.borrow_mut() = Some(err);
            cx.quit();
        }
    });
    match failure.borrow_mut().take() {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// App-wide state: the launch options (to reopen the window), the main window and the tray.
struct AppState {
    options: LaunchOptions,
    window: Option<(AnyWindowHandle, WeakEntity<Shell>)>,
    tray: Option<TrayHandle>,
}

impl Global for AppState {}

fn start(cx: &mut App, options: &LaunchOptions) -> Result<(), DesktopError> {
    let started = Instant::now();
    ui::init(cx);
    fonts::register(cx);
    tokio_bridge::init(cx)?;
    let settings = load_settings(options);
    Theme::install(cx, settings.theme, cx.window_appearance());
    cx.set_global(AppState {
        options: options.clone(),
        window: None,
        tray: None,
    });
    let handle = open_window(cx)?;

    // The tray keeps the app alive when the window closes; the smoke test runs without one,
    // and `ALKAHEST_DESKTOP_TRAY=0` turns it off (desktops without a tray host, kiosks).
    if !options.smoke_test && tray_wanted(&|k| std::env::var(k).ok()) {
        start_tray(cx);
    }
    // An entry from an older version opens only the window; bring it up to date so the next
    // login also starts the server.
    if let Some(exe) = options
        .serve_program
        .as_deref()
        .filter(|_| !options.smoke_test)
    {
        match crate::autostart::refresh(exe, &crate::autostart::process_env) {
            Ok(true) => tracing::info!("updated the start-at-login entry"),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "cannot update the start-at-login entry"),
        }
    }
    // Without a registered tray, closing the window ends the app.
    cx.on_window_closed(|cx, _| {
        let tray_active = cx
            .try_global::<AppState>()
            .and_then(|s| s.tray.as_ref())
            .is_some_and(TrayHandle::is_active);
        if cx.windows().is_empty() && !tray_active {
            cx.quit();
        }
    })
    .detach();

    if options.smoke_test {
        handle
            .update(cx, |_, window, cx| {
                // Watchdog: a compositor that never delivers frames (no output, no GPU) must
                // fail the smoke test instead of hanging CI.
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(SMOKE_TIMEOUT).await;
                    eprintln!(
                        "alkahest desktop: {}",
                        DesktopError::SmokeTimeout(SMOKE_TIMEOUT.as_secs())
                    );
                    std::process::exit(SMOKE_TIMEOUT_EXIT);
                })
                .detach();
                // Runs right after the first frame has been drawn. (A second frame would need
                // something to change: a static window does not draw again by itself.)
                window.on_next_frame(move |_, cx| {
                    println!(
                        "alkahest desktop: first frame rendered in {} ms",
                        started.elapsed().as_millis()
                    );
                    cx.quit();
                });
            })
            .map_err(|e| DesktopError::Window(format!("{e:#}")))?;
    }
    Ok(())
}

/// Whether the tray should start: on unless `ALKAHEST_DESKTOP_TRAY` is `0`, `false` or `off`.
fn tray_wanted(env: &dyn Fn(&str) -> Option<String>) -> bool {
    !env("ALKAHEST_DESKTOP_TRAY").is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// Opens the main window with a new connection and shell (at start, and from the tray after
/// the window was closed).
fn open_window(cx: &mut App) -> Result<AnyWindowHandle, DesktopError> {
    let options = cx.global::<AppState>().options.clone();
    let Connected {
        name,
        url: server_url,
        handle: connection,
        client,
    } = connect(cx, &options)?;
    let bounds = Bounds::centered(
        None,
        ui::size(px(size::DEFAULT_WINDOW.0), px(size::DEFAULT_WINDOW.1)),
        cx,
    );
    let window_options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(ui::size(px(size::MIN_WINDOW.0), px(size::MIN_WINDOW.1))),
        titlebar: Some(TitlebarOptions {
            title: Some(TITLE.into()),
            ..Default::default()
        }),
        app_id: Some(APP_ID.into()),
        ..Default::default()
    };
    let url_for_shell = server_url.clone();
    let (handle, shell) = ui::open_main_window(window_options, cx, move |window, cx| {
        window
            .observe_window_appearance(|window, cx| Theme::follow_appearance(window, cx))
            .detach();
        let ConnectionHandle {
            commands,
            snapshots,
        } = connection;
        let shell =
            cx.new(|cx| Shell::new(options, &url_for_shell, name, commands, client, window, cx));
        Shell::follow(&shell, snapshots, cx);
        shell
    })
    .map_err(DesktopError::Window)?;
    cx.global_mut::<AppState>().window = Some((handle, shell.downgrade()));
    tracing::info!(server = %server_url, "desktop window opened");
    Ok(handle)
}

/// Starts the tray and handles its clicks on the GPUI thread.
fn start_tray(cx: &mut App) {
    let (actions_tx, mut actions) = futures::channel::mpsc::unbounded::<TrayAction>();
    let handle = tray::spawn(TrayModel::default(), actions_tx);
    cx.global_mut::<AppState>().tray = Some(handle);
    cx.spawn(async move |cx| {
        while let Some(action) = actions.next().await {
            cx.update(|cx| on_tray_action(action, cx));
        }
    })
    .detach();
}

/// The live shell and its window, if the window is open.
fn live_window(cx: &App) -> Option<(AnyWindowHandle, Entity<Shell>)> {
    let (handle, weak) = cx.global::<AppState>().window.clone()?;
    let shell = weak.upgrade()?;
    Some((handle, shell))
}

fn on_tray_action(action: TrayAction, cx: &mut App) {
    if action == TrayAction::Quit {
        cx.quit();
        return;
    }
    let window = match live_window(cx) {
        Some(w) => w,
        None => {
            if let Err(e) = open_window(cx) {
                tracing::warn!("cannot reopen the window: {e}");
                return;
            }
            match live_window(cx) {
                Some(w) => w,
                None => return,
            }
        }
    };
    let (handle, shell) = window;
    match action {
        TrayAction::Open => {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
        other => shell.update(cx, |shell, cx| shell.tray_action(other, cx)),
    }
}

/// Pushes a new tray model (the shell calls this with every snapshot).
pub(crate) fn update_tray(model: TrayModel, cx: &App) {
    if let Some(t) = cx.try_global::<AppState>().and_then(|s| s.tray.as_ref()) {
        t.update(model);
        tray::apply_pending();
    }
}

/// The desktop settings; an unreadable file is logged and the defaults are used, so a typo
/// never keeps the app from opening.
fn load_settings(options: &LaunchOptions) -> DesktopSettings {
    let Some(path) = &options.settings_file else {
        return DesktopSettings::default();
    };
    DesktopSettings::load(path).unwrap_or_else(|err| {
        tracing::warn!("{err}; using the default desktop settings");
        DesktopSettings::default()
    })
}

/// What [`connect`] set up: the active connection's name and URL, the manager's handle and
/// the client the screens send their own requests with.
pub(crate) struct Connected {
    /// Name of the active connection ([`crate::settings::LOCAL`] for the configured server).
    pub name: String,
    /// Base URL of the server.
    pub url: String,
    /// The connection manager's ends.
    pub handle: ConnectionHandle,
    /// Client for the screens.
    pub client: Client,
}

/// Builds the client for the active connection (desktop settings and keyring read again, so
/// a reconnect picks up another connection or key) and starts the connection manager on the
/// tokio runtime. It stops by itself when its handle goes away (window closed, reconnect).
pub(crate) fn connect(cx: &mut App, options: &LaunchOptions) -> Result<Connected, DesktopError> {
    let settings = load_settings(options);
    if !options.smoke_test {
        migrate_settings(&settings, options);
    }
    let active = settings.active_connection(&options.server_url);
    // The smoke test must never block on a keyring unlock prompt.
    let api_key = if options.smoke_test {
        options.api_key.clone()
    } else {
        resolve_api_key(
            options.api_key.clone(),
            &KeyringKeyStore::for_connection(&active.name),
        )
    };
    let mut client =
        Client::new(&active.url).map_err(|e| DesktopError::ServerUrl(e.to_string()))?;
    let env_ca = std::env::var(alkahest_client::CA_CERT_ENV).ok();
    if let Some(ca) = tls::resolve_ca_cert(
        &active.url,
        env_ca.as_deref(),
        active.ca_cert.as_deref(),
        options.generated_ca.as_deref(),
        active.local,
    ) {
        // An unreadable CA is logged, not fatal: the window must open so the Settings screen
        // can fix the path; the connection then reports an untrusted certificate.
        match client.clone().with_ca_cert_file(&ca) {
            Ok(c) => client = c,
            Err(e) => tracing::warn!("{e}; connecting without it"),
        }
    }
    let client = client.with_api_key(api_key);
    // Only the built-in local connection may start a server: a saved connection is another
    // server even when its URL is loopback (an SSH tunnel), and starting `alkahest serve`
    // from this user's configuration would not bring it back. Nor is a server started next to
    // a service that already runs one (ALK-US-0144).
    let mut external_service = None;
    let launcher = options
        .serve_program
        .clone()
        .filter(|_| active.local)
        .map(|program| {
            LocalLauncher::new(program, options.serve_args.clone())
                .with_stderr_log(options.serve_stderr_log.clone())
        })
        .filter(|l| {
            external_service.clone_from(&l.external_service);
            l.external_service.is_none()
        });
    let url = client.base_url().to_string();
    let (manager, handle) = ConnectionManager::new(client.clone(), launcher, url.clone());
    let manager = manager
        .with_external_service(external_service)
        .with_auto_start(options.start_server && !options.smoke_test);
    tokio_bridge::handle(cx).spawn(manager.run());
    tracing::info!(connection = %active.name, server = %url, "connecting");
    Ok(Connected {
        name: active.name,
        url,
        handle,
        client,
    })
}

/// Saves a settings file written before connections existed in the new form, and moves the
/// single legacy keyring key to the connection it belonged to: the migrated remote, or the
/// local server when no connection was ever saved. With saved connections and no migration
/// the owner of a leftover legacy key is unknown, and it stays where it is.
fn migrate_settings(settings: &DesktopSettings, options: &LaunchOptions) {
    let to = settings.migrated().or_else(|| {
        settings
            .connections
            .is_empty()
            .then_some(crate::settings::LOCAL)
    });
    if let Some(to) = to {
        migrate_legacy_key(
            &KeyringKeyStore::legacy(),
            &KeyringKeyStore::for_connection(to),
        );
    }
    if settings.migrated().is_some()
        && let Some(path) = &options.settings_file
    {
        match settings.save(path) {
            Ok(()) => tracing::info!(?to, "moved the server URL to a saved connection"),
            Err(e) => tracing::warn!("{e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_is_on_unless_turned_off() {
        assert!(tray_wanted(&|_| None));
        assert!(!tray_wanted(&|_| Some("0".into())));
        assert!(!tray_wanted(&|_| Some("Off".into())));
        assert!(tray_wanted(&|_| Some("1".into())));
    }
}
