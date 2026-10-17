//! Application start-up: gpui-kit, fonts, tokio bridge, settings, theme, the connection
//! manager and the main window.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use alkahest_client::Client;

use crate::connection::{ConnectionHandle, ConnectionManager};
use crate::launcher::{LocalLauncher, is_local_url};
use crate::settings::{DesktopSettings, KeyringKeyStore, resolve_api_key};
use crate::theme::tokens::size;
use crate::ui::{
    self, App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px,
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

fn start(cx: &mut App, options: &LaunchOptions) -> Result<(), DesktopError> {
    let started = Instant::now();
    ui::init(cx);
    fonts::register(cx);
    tokio_bridge::init(cx)?;
    let settings = load_settings(options);
    Theme::install(cx, settings.theme, cx.window_appearance());
    let (server_url, connection, client) = start_connection(cx, options, &settings)?;

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
    let (handle, _shell) = ui::open_main_window(window_options, cx, move |window, cx| {
        window
            .observe_window_appearance(|window, cx| Theme::follow_appearance(window, cx))
            .detach();
        let ConnectionHandle {
            commands,
            snapshots,
        } = connection;
        let shell = cx.new(|cx| Shell::new(&url_for_shell, commands, client, window, cx));
        Shell::follow(&shell, snapshots, cx);
        shell
    })
    .map_err(DesktopError::Window)?;
    tracing::info!(server = %server_url, "desktop window opened");

    // Closing the window ends the app (no tray yet: ALK-US-0073).
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
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

/// Builds the client for the effective server and starts the connection manager on the tokio
/// runtime. It stops by itself when the window (holding the handle) goes away. The client is
/// returned too: the screens send their own requests (model list, actions) with it.
fn start_connection(
    cx: &mut App,
    options: &LaunchOptions,
    settings: &DesktopSettings,
) -> Result<(String, ConnectionHandle, Client), DesktopError> {
    let server_url = settings
        .effective_server_url(&options.server_url)
        .to_string();
    // The smoke test must never block on a keyring unlock prompt.
    let api_key = if options.smoke_test {
        options.api_key.clone()
    } else {
        resolve_api_key(options.api_key.clone(), &KeyringKeyStore)
    };
    let client = Client::new(&server_url)
        .map_err(|e| DesktopError::ServerUrl(e.to_string()))?
        .with_api_key(api_key);
    // Starting a server only helps when the server is on this machine.
    let launcher = options
        .serve_program
        .clone()
        .filter(|_| is_local_url(&server_url))
        .map(|program| LocalLauncher::new(program, options.serve_args.clone()));
    let url = client.base_url().to_string();
    let (manager, handle) = ConnectionManager::new(client.clone(), launcher, url.clone());
    tokio_bridge::handle(cx).spawn(manager.run());
    Ok((url, handle, client))
}
