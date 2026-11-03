//! System-tray icon (TeamViewer/AnyDesk model: the app lives in the
//! tray, the window is optional). Windows and macOS only — on Linux the
//! desktop-tray landscape is fragmented (AppIndicator/GNOME extensions),
//! and there the window alone is the supported UI.
//!
//! Tray/menu events arrive through muda's event handlers (setting a
//! handler disables the global channel, so the handlers are the single
//! delivery path): each event is translated to a [`TrayAction`] and
//! pushed into an app-owned channel, and the UI is woken with a repaint
//! so hidden-window clicks are processed immediately.

use std::sync::{mpsc::Sender, Arc};

use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

/// What the user did in the tray (translated to app actions by `App`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Hide,
    StartAgent,
    StopAgent,
    Quit,
}

/// Stable menu-item ids (matched in the process-lifetime handler).
const ID_SHOW: &str = "opacusr.show";
const ID_HIDE: &str = "opacusr.hide";
const ID_START: &str = "opacusr.start";
const ID_STOP: &str = "opacusr.stop";
const ID_QUIT: &str = "opacusr.quit";

fn action_of_id(id: &str) -> Option<TrayAction> {
    match id {
        ID_SHOW => Some(TrayAction::Show),
        ID_HIDE => Some(TrayAction::Hide),
        ID_START => Some(TrayAction::StartAgent),
        ID_STOP => Some(TrayAction::StopAgent),
        ID_QUIT => Some(TrayAction::Quit),
        _ => None,
    }
}

/// The live tray icon. Dropping it removes the icon.
pub struct Tray {
    _tray: TrayIcon,
}

impl Tray {
    /// Create the tray icon on the main thread (winit pumps its
    /// messages). `actions` receives everything the user picks; `wake`
    /// must request an egui repaint. Returns `None` if the tray is
    /// unavailable — the app keeps working, just window-bound.
    pub fn create(
        actions: Sender<TrayAction>,
        wake: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Option<Self> {
        let (menu, _items) = {
            let menu = Menu::new();
            let items = (
                MenuItem::with_id(MenuId::new(ID_SHOW), "Показать OpacusR", true, None),
                MenuItem::with_id(MenuId::new(ID_HIDE), "Скрыть в трей", true, None),
                MenuItem::with_id(MenuId::new(ID_START), "Запустить агента", true, None),
                MenuItem::with_id(MenuId::new(ID_STOP), "Остановить агента", true, None),
                MenuItem::with_id(MenuId::new(ID_QUIT), "Выход", true, None),
            );
            let _ = menu.append_items(&[
                &items.0,
                &items.1,
                &PredefinedMenuItem::separator(),
                &items.2,
                &items.3,
                &PredefinedMenuItem::separator(),
                &items.4,
            ]);
            (menu, items)
        };

        // Menu picks → action + wake. The handler lives for the whole
        // process, so it matches on the stable ids above.
        tray_icon::menu::MenuEvent::set_event_handler(Some({
            let (actions, wake) = (actions.clone(), wake.clone());
            move |ev: tray_icon::menu::MenuEvent| {
                if let Some(action) = action_of_id(ev.id().0.as_str()) {
                    let _ = actions.send(action);
                    wake();
                }
            }
        }));

        let builder = TrayIconBuilder::new()
            .with_tooltip("OpacusR — защищённый доступ к рабочему столу")
            .with_icon(icon())
            .with_menu(Box::new(menu));
        // TeamViewer: left click opens the window, the menu sits behind
        // the right click.
        #[cfg(any(windows, target_os = "macos"))]
        let builder = builder.with_menu_on_left_click(false);
        let tray = match builder.build() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("tray icon unavailable: {e}");
                return None;
            }
        };

        // Left click on the icon itself also opens the window.
        tray_icon::TrayIconEvent::set_event_handler(Some({
            let (actions, wake) = (actions.clone(), wake.clone());
            move |ev| {
                use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
                let clicked = matches!(
                    ev,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                );
                if clicked {
                    let _ = actions.send(TrayAction::Show);
                    wake();
                }
            }
        }));

        Some(Self { _tray: tray })
    }
}

/// The app icon as RGBA (violet ring + white dot on transparent dark
/// roundel) — the same mark as the window icon, 32 px for tray DPI.
fn icon() -> tray_icon::Icon {
    let size = 32usize;
    let mut rgba = vec![0u8; size * size * 4];
    let c = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            let (i, o, bg) = (c * 0.95, c * 0.55, c * 0.98);
            let px = if r < o {
                [255, 255, 255, 255] // white core dot
            } else if r < i {
                [124u8, 92, 255, 255] // violet ring (#7C5CFC)
            } else if r < bg {
                [10, 10, 20, 255] // dark roundel
            } else {
                [0, 0, 0, 0] // transparent corner
            };
            let idx = (y * size + x) * 4;
            rgba[idx..idx + 4].copy_from_slice(&px);
        }
    }
    tray_icon::Icon::from_rgba(rgba, size as u32, size as u32)
        .expect("tray icon rgba is well-formed")
}
