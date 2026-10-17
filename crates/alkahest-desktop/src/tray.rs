//! System tray (ALK-US-0073): `ksni` (StatusNotifierItem over D-Bus) on Linux, `tray-icon` on
//! macOS and Windows. Pattern from Pando Assistant's `shell/tray.rs`.
//!
//! The tray menu is not in the design canvas (`design/README.md`, "Not designed yet"), so it
//! is a minimal native menu: open the window, the server state and loaded models, stop all
//! models, start or stop the local server, quit. The icon is `design/brand/mark-tray.svg`
//! rasterised at run time (bone on Linux and Windows panels, a template image on macOS).
//!
//! The model, the menu and the icon pixels are pure and unit tested; the backends (with the
//! `gui` feature) only forward clicks as [`TrayAction`]s.

/// What the tray shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrayModel {
    /// The server answers.
    pub running: bool,
    /// A start or stop is in progress.
    pub busy: bool,
    /// Loaded models.
    pub loaded: usize,
    /// This app can start and stop the server (local server, launcher available).
    pub manages_server: bool,
}

/// A click in the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Show (or reopen) the main window.
    Open,
    /// Unload every model (`POST /alkahest/v1/unload {"all": true}`).
    StopAllModels,
    /// Start the local server.
    StartServer,
    /// Stop the local server.
    StopServer,
    /// Quit the app (the server keeps running, as a service would).
    Quit,
}

/// One entry of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A clickable item, or a disabled status line without an action.
    Item {
        /// Label.
        label: String,
        /// Action on click; `None` is a disabled line.
        action: Option<TrayAction>,
    },
    /// A separator line.
    Separator,
}

/// The menu for `model`.
pub fn menu(model: &TrayModel) -> Vec<Entry> {
    let item = |label: &str, action: Option<TrayAction>| Entry::Item {
        label: label.to_string(),
        action,
    };
    let mut out = vec![
        item("Open Alkahest", Some(TrayAction::Open)),
        Entry::Separator,
        item(&status_line(model), None),
    ];
    out.push(item(
        "Stop all models",
        (model.running && model.loaded > 0).then_some(TrayAction::StopAllModels),
    ));
    if model.manages_server {
        if model.running {
            out.push(item(
                "Stop server",
                (!model.busy).then_some(TrayAction::StopServer),
            ));
        } else {
            out.push(item(
                "Start server",
                (!model.busy).then_some(TrayAction::StartServer),
            ));
        }
    }
    out.push(Entry::Separator);
    out.push(item("Quit", Some(TrayAction::Quit)));
    out
}

/// `Server running · 2 models loaded`, `Server not running`, `Starting…`.
pub fn status_line(model: &TrayModel) -> String {
    if model.busy {
        return "Server starting or stopping\u{2026}".into();
    }
    if !model.running {
        return "Server not running".into();
    }
    match model.loaded {
        0 => "Server running \u{b7} nothing loaded".into(),
        1 => "Server running \u{b7} 1 model loaded".into(),
        n => format!("Server running \u{b7} {n} models loaded"),
    }
}

/// Tooltip text.
pub fn tooltip(model: &TrayModel) -> String {
    format!("Alkahest \u{b7} {}", status_line(model))
}

/// `design/brand/mark-tray.svg` (stroke 8, `currentColor`).
pub const MARK_TRAY_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="16" height="16" fill="none" stroke="currentColor" stroke-width="8" stroke-linejoin="round" stroke-linecap="round"><path d="M6 12 H58"/><path d="M13 12 L32 52 L51 12"/><circle cx="32" cy="28" r="8" fill="currentColor" stroke="none"/></svg>"#;

/// RGBA pixels (straight alpha) of the tray mark at `size` px in colour `rgb`, or `None` if
/// the SVG cannot be rasterised.
#[cfg(feature = "gui")]
pub fn icon_rgba(size: u32, rgb: [u8; 3]) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    let color = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    let svg = MARK_TRAY_SVG.replace("currentColor", &color);
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / tree.size().width();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied alpha; the tray APIs want straight alpha.
    Some(
        pixmap
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue(), c.alpha()]
            })
            .collect(),
    )
}

/// Handle to push model updates to the running tray.
#[cfg(feature = "gui")]
#[derive(Debug, Clone)]
pub struct TrayHandle {
    tx: futures::channel::mpsc::UnboundedSender<TrayModel>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "gui")]
impl TrayHandle {
    fn new(tx: futures::channel::mpsc::UnboundedSender<TrayModel>) -> Self {
        Self {
            tx,
            active: std::sync::Arc::default(),
        }
    }

    /// Tells the tray to redraw. Cheap; coalesced by the backend.
    pub fn update(&self, model: TrayModel) {
        let _ = self.tx.unbounded_send(model);
    }

    /// Whether a tray icon is actually shown (the desktop may have no tray host); without
    /// one, closing the window must quit the app.
    pub fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set_active(&self, on: bool) {
        self.active.store(on, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Bone (`brand.BONE`), readable on the dark panels of Linux and Windows.
#[cfg(feature = "gui")]
const BONE: [u8; 3] = [0xE8, 0xE6, 0xE1];

#[cfg(all(feature = "gui", target_os = "linux"))]
mod linux {
    use futures::StreamExt as _;

    use super::*;

    struct AlkahestTray {
        model: TrayModel,
        actions: futures::channel::mpsc::UnboundedSender<TrayAction>,
    }

    impl AlkahestTray {
        fn send(&self, a: TrayAction) {
            let _ = self.actions.unbounded_send(a);
        }
    }

    impl ksni::Tray for AlkahestTray {
        fn id(&self) -> String {
            "es.digio.alkahest".into()
        }
        fn title(&self) -> String {
            "Alkahest".into()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: tooltip(&self.model),
                ..Default::default()
            }
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            // SNI wants ARGB32 in network byte order.
            [22_u32, 32, 64]
                .into_iter()
                .filter_map(|size| {
                    let rgba = icon_rgba(size, BONE)?;
                    let data = rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|p| [p[3], p[0], p[1], p[2]])
                        .collect();
                    Some(ksni::Icon {
                        width: i32::try_from(size).ok()?,
                        height: i32::try_from(size).ok()?,
                        data,
                    })
                })
                .collect()
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayAction::Open);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            menu(&self.model)
                .into_iter()
                .map(|e| match e {
                    Entry::Separator => ksni::MenuItem::Separator,
                    Entry::Item { label, action } => StandardItem {
                        label,
                        enabled: action.is_some(),
                        activate: Box::new(move |t: &mut Self| {
                            if let Some(a) = action {
                                t.send(a);
                            }
                        }),
                        ..Default::default()
                    }
                    .into(),
                })
                .collect()
        }
    }

    /// Registers the tray on its own thread (the StatusNotifierWatcher may appear late, so it
    /// retries a few times and gives up quietly). Clicks arrive on `actions`.
    pub fn spawn(
        model: TrayModel,
        actions: futures::channel::mpsc::UnboundedSender<TrayAction>,
    ) -> TrayHandle {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<TrayModel>();
        let handle_out = TrayHandle::new(tx);
        let flag = handle_out.clone();
        let spawned = std::thread::Builder::new()
            .name("alkahest-tray".into())
            .spawn(move || {
                futures::executor::block_on(async move {
                    use ksni::TrayMethods as _;
                    let mut handle = None;
                    for attempt in 0..5_u64 {
                        let tray = AlkahestTray {
                            model,
                            actions: actions.clone(),
                        };
                        match tray.spawn().await {
                            Ok(h) => {
                                handle = Some(h);
                                break;
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "tray registration failed (attempt {attempt}): {e}"
                                );
                                std::thread::sleep(std::time::Duration::from_secs(2 + attempt * 2));
                            }
                        }
                    }
                    let Some(handle) = handle else {
                        tracing::info!("no system tray available; running without one");
                        return;
                    };
                    flag.set_active(true);
                    while let Some(mut m) = rx.next().await {
                        // Coalesce bursts.
                        while let Ok(next) = rx.try_recv() {
                            m = next;
                        }
                        if handle.update(|t| t.model = m).await.is_none() {
                            break;
                        }
                    }
                    flag.set_active(false);
                });
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start the tray thread: {e}");
        }
        handle_out
    }
}

#[cfg(all(feature = "gui", target_os = "linux"))]
pub use linux::spawn;

#[cfg(all(feature = "gui", any(target_os = "macos", target_os = "windows")))]
mod desktop {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use super::*;

    /// Menu item ids and their actions; shared with the menu event handler (any thread).
    type Ids = Arc<Mutex<Vec<(MenuId, TrayAction)>>>;

    /// The tray owned by the GPUI main thread (required on macOS). The menu is rebuilt on
    /// every model change.
    struct DesktopTray {
        icon: TrayIcon,
        updates: futures::channel::mpsc::UnboundedReceiver<TrayModel>,
        ids: Ids,
    }

    thread_local! {
        static TRAY: RefCell<Option<DesktopTray>> = const { RefCell::new(None) };
    }

    fn build_menu(model: &TrayModel, ids: &Ids) -> Menu {
        let menu = Menu::new();
        let mut list = Vec::new();
        for e in super::menu(model) {
            match e {
                Entry::Separator => {
                    let _ = menu.append(&PredefinedMenuItem::separator());
                }
                Entry::Item { label, action } => {
                    let it = MenuItem::new(label, action.is_some(), None);
                    if let Some(a) = action {
                        list.push((it.id().clone(), a));
                    }
                    let _ = menu.append(&it);
                }
            }
        }
        if let Ok(mut v) = ids.lock() {
            *v = list;
        }
        menu
    }

    fn icon() -> Option<Icon> {
        let rgb = if cfg!(target_os = "macos") {
            [0, 0, 0]
        } else {
            BONE
        };
        Icon::from_rgba(icon_rgba(32, rgb)?, 32, 32).ok()
    }

    /// Creates the tray on the calling (main) thread; clicks go to `actions`.
    pub fn spawn(
        model: TrayModel,
        actions: futures::channel::mpsc::UnboundedSender<TrayAction>,
    ) -> TrayHandle {
        let (tx, updates) = futures::channel::mpsc::unbounded::<TrayModel>();
        let out = TrayHandle::new(tx);
        let ids: Ids = Arc::new(Mutex::new(Vec::new()));
        let menu = build_menu(&model, &ids);
        let Some(first_icon) = icon() else {
            return out;
        };
        let built = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip(&model))
            .with_icon(first_icon)
            .with_icon_as_template(cfg!(target_os = "macos"))
            .build();
        match built {
            Ok(icon) => {
                let lookup = ids.clone();
                MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                    let found = lookup
                        .lock()
                        .ok()
                        .and_then(|v| v.iter().find(|(id, _)| *id == e.id).map(|(_, a)| *a));
                    if let Some(a) = found {
                        let _ = actions.unbounded_send(a);
                    }
                }));
                TRAY.with(|t| *t.borrow_mut() = Some(DesktopTray { icon, updates, ids }));
                out.set_active(true);
            }
            Err(e) => tracing::warn!("tray unavailable: {e}"),
        }
        out
    }

    /// Applies the latest queued model (call on the GPUI thread after updates).
    pub fn apply_pending() {
        TRAY.with(|t| {
            let mut t = t.borrow_mut();
            let Some(tray) = t.as_mut() else { return };
            let mut latest = None;
            while let Ok(m) = tray.updates.try_recv() {
                latest = Some(m);
            }
            let Some(model) = latest else { return };
            let menu = build_menu(&model, &tray.ids);
            tray.icon.set_menu(Some(Box::new(menu)));
            let _ = tray.icon.set_tooltip(Some(tooltip(&model)));
        });
    }
}

#[cfg(all(feature = "gui", any(target_os = "macos", target_os = "windows")))]
pub use desktop::{apply_pending, spawn};

/// Linux updates reach the tray through its own thread; nothing to apply on the GPUI side.
#[cfg(all(feature = "gui", target_os = "linux"))]
pub fn apply_pending() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(m: &[Entry]) -> Vec<(String, bool)> {
        m.iter()
            .filter_map(|e| match e {
                Entry::Item { label, action } => Some((label.clone(), action.is_some())),
                Entry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn menu_offers_open_status_stop_all_server_and_quit() {
        let running = TrayModel {
            running: true,
            loaded: 2,
            manages_server: true,
            busy: false,
        };
        assert_eq!(
            labels(&menu(&running)),
            [
                ("Open Alkahest".to_string(), true),
                ("Server running \u{b7} 2 models loaded".to_string(), false),
                ("Stop all models".to_string(), true),
                ("Stop server".to_string(), true),
                ("Quit".to_string(), true),
            ]
        );
        let stopped = TrayModel {
            manages_server: true,
            ..TrayModel::default()
        };
        let l = labels(&menu(&stopped));
        assert_eq!(l[1].0, "Server not running");
        assert!(!l[2].1, "nothing to stop");
        assert_eq!(l[3], ("Start server".to_string(), true));
        let remote = TrayModel {
            running: true,
            ..TrayModel::default()
        };
        assert!(
            !labels(&menu(&remote))
                .iter()
                .any(|(l, _)| l.contains("server") && l.starts_with("St")),
            "a remote server is never started or stopped from here"
        );
    }

    #[test]
    fn tooltip_names_the_state() {
        assert_eq!(
            tooltip(&TrayModel {
                running: true,
                loaded: 1,
                ..TrayModel::default()
            }),
            "Alkahest \u{b7} Server running \u{b7} 1 model loaded"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_mark_rasterises_with_a_transparent_background() {
        let px = icon_rgba(32, [255, 255, 255]).unwrap_or_default();
        assert_eq!(px.len(), 32 * 32 * 4);
        assert_eq!(px[3], 0, "corner is transparent");
        // The solid core (circle at 32,28 of 64) is opaque.
        let core = ((14 * 32 + 16) * 4) as usize;
        assert!(px[core + 3] > 200, "core alpha {}", px[core + 3]);
    }
}
