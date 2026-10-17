//! Application start-up: gpui-kit, fonts, tokio bridge, theme and the main window.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::theme::ThemePreference;
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
    Theme::install(cx, ThemePreference::default(), cx.window_appearance());

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
    let server_url = options.server_url.clone();
    let (handle, _shell) = ui::open_main_window(window_options, cx, move |window, cx| {
        window
            .observe_window_appearance(|window, cx| Theme::follow_appearance(window, cx))
            .detach();
        cx.new(|_| Shell::new(&server_url))
    })
    .map_err(DesktopError::Window)?;
    tracing::info!(server = %options.server_url, "desktop window opened");

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
