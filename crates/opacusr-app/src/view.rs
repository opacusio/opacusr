//! The live session UI: remote screen view with full input capture, plus
//! toggleable side panels for chat, file transfer and the remote terminal.

use egui::{Color32, Context, Sense};

use opacusr_client::session::Command;
use opacusr_proto::message::mods;
use opacusr_proto::message::{MouseButtonId, OpKey};

use crate::app::SessionHandle;
use crate::theme;
use opacusr_agent::config::AgentConfig;

/// Session screen: central remote view + toggleable right sidebar.
/// The chrome-less «⛶» mode hides the toolbar and the sidebar so the
/// remote view fills the whole window; on Android it also hides the
/// system bars (sticky immersive) and the session holds landscape
/// rotation (see `android_sysui`).
pub fn session_ui(ctx: &Context, s: &mut SessionHandle, cfg: &mut AgentConfig) {
    // Rects of every chrome surface (toolbar, sidebar, keys strip,
    // floating overlays). The input layer refuses pointer/touch events
    // that start inside them: raw touch events bypass egui's widget
    // hit-testing, so a tap on a floating button used to ALSO click the
    // remote desktop at that point.
    let mut overlay: Vec<egui::Rect> = Vec::new();

    // ---- toolbar ---------------------------------------------------------
    if !s.fullscreen {
        let mut toolbar_rect = egui::Rect::NOTHING;
        egui::TopBottomPanel::top("session_toolbar").show(ctx, |ui| {
            session_toolbar(ui, s, cfg);
            toolbar_rect = ui.min_rect();
        });
        overlay.push(toolbar_rect);
    }

    // ---- right sidebar -----------------------------------------------------
    // Works in chrome-less mode too now: the floating quickbar menu
    // toggles the panels without leaving fullscreen.
    let sidebar_open =
        s.panel_chat || s.panel_files || s.panel_term || s.panel_stats || s.panel_tunnel;
    if sidebar_open {
        let mut side_rect = egui::Rect::NOTHING;
        egui::SidePanel::right("session_sidebar")
            .min_width(280.0)
            .default_width(320.0)
            .show(ctx, |ui| {
                if s.panel_stats {
                    stats_panel(ui, s);
                    ui.separator();
                }
                if s.panel_chat {
                    chat_panel(ui, s);
                    ui.separator();
                }
                if s.panel_files {
                    files_panel(ui, s);
                    ui.separator();
                }
                if s.panel_term {
                    terminal_panel(ui, s);
                    ui.separator();
                }
                if s.panel_tunnel {
                    tunnel_panel(ui, s);
                }
                side_rect = ui.min_rect();
            });
        overlay.push(side_rect);
    }

    // ---- control-keys strip (toggled from the Android call menu) --------
    // A soft IME cannot produce Ctrl/Alt/F1–F12/arrows — remote control
    // from a phone needs them on screen. In chrome-less mode it floats
    // over the video instead (see fullscreen_overlays).
    if s.panel_keys && !s.fullscreen {
        let mut keys_rect = egui::Rect::NOTHING;
        egui::TopBottomPanel::bottom("session_keys_strip").show(ctx, |ui| {
            keys_strip(ui, s);
            keys_rect = ui.min_rect();
        });
        overlay.push(keys_rect);
    }

    // ---- remote view -----------------------------------------------------
    egui::CentralPanel::default().show(ctx, |ui| {
        session_view(ui, s, &overlay);
    });

    // Chrome-less mode: floating controls over the video — quickbar
    // (keyboard · Ctrl/Alt/Shift · menu), floating keys strip, exit
    // button and view-only badge. Without them a fullscreen phone user
    // could not type or press Ctrl at all.
    if s.fullscreen {
        fullscreen_overlays(ctx, s, &mut overlay);
    }
}

/// Android toolbar: ONE short row (status · RTT · ⌨ · ⛶ · ⏹ · ⋯) so the
/// video gets the whole screen — the wrapped desktop row consumed two
/// lines and the 💬/📁 sidebar ate the rest («увеличь окно, спрячь чат
/// и передачу файлов в меню вызова»). Everything else lives in the ⋯
/// call menu.
#[cfg(target_os = "android")]
fn session_toolbar(ui: &mut egui::Ui, s: &mut SessionHandle, cfg: &mut AgentConfig) {
    let _ = cfg;
    ui.horizontal_wrapped(|ui| {
        ui.style_mut().spacing.button_padding = egui::vec2(8.0, 3.0);
        if s.view_only {
            ui.label(
                egui::RichText::new("👁")
                    .color(Color32::from_rgb(255, 170, 60))
                    .strong(),
            )
            .on_hover_text("Сессия только для просмотра");
            ui.separator();
        }
        ui.label(egui::RichText::new(&s.status).weak().small());
        ui.label(egui::RichText::new(format!(" {} мс", s.rtt)).weak().small());
        // Soft keyboard: the IME types text (chat, terminal, the remote
        // screen); the ⋯ menu holds the CONTROL keys strip for
        // Ctrl/Alt/arrows/F1–F12 that no soft keyboard can produce.
        if ui
            .button(if s.soft_keyboard_on { "⌨ ✓" } else { "⌨" })
            .on_hover_text("Экранная клавиатура")
            .clicked()
        {
            s.soft_keyboard_on = !s.soft_keyboard_on;
            crate::android_sysui::set_keyboard(s.soft_keyboard_on);
        }
        if ui.button("⛶").on_hover_text("Во весь экран").clicked() {
            set_chromeless(s, true);
        }
        // The call menu — chat, files, terminal, stats, tunnels, the
        // control-keys strip and session controls.
        ui.menu_button("⋯", |ui| {
            ui.set_min_width(220.0);
            if ui
                .button(if s.paused {
                    "▶ Продолжить"
                } else {
                    "⏸ Пауза"
                })
                .clicked()
            {
                s.paused = !s.paused;
                let _ = s.commands.send(Command::Pause(s.paused));
            }
            // Screen selector (multi-monitor hosts).
            if s.screens.len() > 1 {
                let current = s
                    .screens
                    .iter()
                    .find(|sc| sc.id == s.selected_screen)
                    .cloned()
                    .unwrap_or_else(|| s.screens[0].clone());
                egui::ComboBox::from_id_salt("screen_select_menu")
                    .selected_text(format!("🖥 {} ({}×{})", current.name, current.w, current.h))
                    .show_ui(ui, |ui| {
                        for sc in &s.screens {
                            ui.selectable_value(
                                &mut s.selected_screen,
                                sc.id,
                                format!("{} ({}×{})", sc.name, sc.w, sc.h),
                            );
                        }
                    });
                if s.selected_screen != current.id {
                    let _ = s.commands.send(Command::SelectScreen(s.selected_screen));
                }
            }
            if ui
                .toggle_value(&mut s.clipboard_sync, "📋 Буфер обмена")
                .changed()
            {
                let _ = s.commands.send(Command::ClipboardSync(s.clipboard_sync));
            }
            if s.audio_available && ui.toggle_value(&mut s.audio_on, "🔊 Звук").changed() {
                let _ = s.commands.send(Command::Audio(s.audio_on));
            }
            if ui.button("🖨 Печать кадра").clicked() {
                print_current_frame(s);
            }
            ui.separator();
            // Panels: tapping one closes the menu so the video is
            // visible again (the sidebar takes its width).
            for (label, flag, avail) in [
                ("💬 Чат", &mut s.panel_chat, true),
                ("📁 Файлы", &mut s.panel_files, true),
                (">_ Терминал", &mut s.panel_term, true),
                ("⌨ Клавиши (Ctrl/F1/…)", &mut s.panel_keys, true),
                ("📊 Статистика", &mut s.panel_stats, true),
                ("🔗 Туннели", &mut s.panel_tunnel, s.tunnel_available),
            ] {
                if !avail {
                    continue;
                }
                if ui.toggle_value(flag, label).clicked() {
                    ui.close_menu();
                }
            }
        });
        if ui.button("⏹").on_hover_text("Отключиться").clicked() {
            s.disconnect_requested = true;
        }
    });
}

/// Desktop toolbar row of the session screen (hidden by the «⛶» mode).
#[cfg(not(target_os = "android"))]
fn session_toolbar(ui: &mut egui::Ui, s: &mut SessionHandle, cfg: &mut AgentConfig) {
    // Wrapped, not plain horizontal: on a phone the row is wider than
    // the screen and `horizontal` silently clipped half the buttons
    // off-screen — unreachable and untappable.
    ui.horizontal_wrapped(|ui| {
        if s.view_only {
            ui.label(
                egui::RichText::new("👁 только просмотр")
                    .color(Color32::from_rgb(255, 170, 60))
                    .strong(),
            );
            ui.separator();
        }
        ui.label(egui::RichText::new(&s.status).weak().small());
        ui.separator();
        if ui
            .button(if s.paused { "▶ Продолжить" } else { "⏸ Пауза" })
            .clicked()
        {
            s.paused = !s.paused;
            let _ = s.commands.send(Command::Pause(s.paused));
        }
        // Screen selector (multi-monitor hosts).
        if s.screens.len() > 1 {
            ui.separator();
            let current = s
                .screens
                .iter()
                .find(|sc| sc.id == s.selected_screen)
                .cloned()
                .unwrap_or_else(|| s.screens[0].clone());
            egui::ComboBox::from_id_salt("screen_select")
                .selected_text(format!("🖥 {} ({}×{})", current.name, current.w, current.h))
                .show_ui(ui, |ui| {
                    for sc in &s.screens {
                        ui.selectable_value(
                            &mut s.selected_screen,
                            sc.id,
                            format!("{} ({}×{})", sc.name, sc.w, sc.h),
                        );
                    }
                });
            if s.selected_screen != current.id {
                let _ = s.commands.send(Command::SelectScreen(s.selected_screen));
            }
        }
        ui.separator();
        if ui.toggle_value(&mut s.clipboard_sync, "📋 Буфер").changed() {
            let _ = s.commands.send(Command::ClipboardSync(s.clipboard_sync));
        }
        if ui
            .button("🖨")
            .on_hover_text("Печать кадра удалённого экрана на местном принтере")
            .clicked()
        {
            print_current_frame(s);
        }
        if s.audio_available && ui.toggle_value(&mut s.audio_on, "🔊").changed() {
            let _ = s.commands.send(Command::Audio(s.audio_on));
        }
        ui.separator();
        // Quality sliders need desktop screen estate: on a phone
        // they filled the wrapped toolbar and pushed the real
        // controls off-screen; the adaptive tier badge below still
        // reports what the host streams at.
        #[cfg(not(target_os = "android"))]
        {
            let mut fps = cfg.fps as f32;
            let mut q = cfg.jpeg_quality as f32;
            let mut sc = cfg.scale_percent as f32;
            let mut changed = false;
            changed |= ui
                .add(egui::Slider::new(&mut fps, 1.0..=60.0).text("fps"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut q, 20.0..=95.0).text("jpeg"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut sc, 25.0..=100.0).text("scale %"))
                .changed();
            if changed {
                cfg.fps = fps as u8;
                cfg.jpeg_quality = q as u8;
                cfg.scale_percent = sc as u16;
                let _ = s.commands.send(Command::Quality {
                    fps: cfg.fps,
                    jpeg_quality: cfg.jpeg_quality,
                    scale_percent: cfg.scale_percent,
                });
            }
        }
        #[cfg(target_os = "android")]
        {
            let _ = cfg;
        }
        ui.separator();
        ui.label(format!("{} мс", s.rtt));
        // Adaptive-quality tier: what the host's controller is currently
        // streaming at (hidden while a manual quality setting is in
        // force — the sliders above are authoritative then).
        if s.quality_adaptive {
            if let Some(tier) = s.quality_tier {
                ui.separator();
                let (text, color) = tier_badge(tier);
                ui.label(egui::RichText::new(text).color(color).strong())
                    .on_hover_text(match s.quality_params {
                        Some((fps, q, sc)) => format!(
                            "Адаптивное качество, уровень {tier}/3 — {fps} fps, JPEG {q}, масштаб {sc}%"
                        ),
                        None => format!("Адаптивное качество, уровень {tier}/3"),
                    });
            }
        }
        if s.recording {
            ui.separator();
            ui.label(
                egui::RichText::new("● ЗАПИСЬ")
                    .color(egui::Color32::from_rgb(230, 80, 80))
                    .strong(),
            )
            .on_hover_text("Хост записывает эту сессию в видеофайл.");
        }
        ui.separator();
        ui.toggle_value(&mut s.panel_chat, "💬")
            .on_hover_text("Чат");
        ui.toggle_value(&mut s.panel_files, "📁")
            .on_hover_text("Передача файлов");
        ui.toggle_value(&mut s.panel_term, ">_")
            .on_hover_text("Терминал хоста");
        ui.toggle_value(&mut s.panel_stats, "📊")
            .on_hover_text("Статистика");
        let mut tunnel_btn = ui
            .toggle_value(&mut s.panel_tunnel, "🔗")
            .on_hover_text("TCP-туннели (перенаправление портов)");
        tunnel_btn.enabled = s.tunnel_available;
        if s.panel_tunnel && !s.tunnel_available {
            s.panel_tunnel = false;
        }
        // Chrome-less fullscreen: hide toolbar + sidebar so the
        // remote view fills the window (Android: system bars too).
        if ui
            .button("⛶")
            .on_hover_text("Во весь экран (F11 / ⛶ — вернуться)")
            .clicked()
        {
            set_chromeless(s, true);
        }
        // Disconnect — lives here since the desktop lost its egui top
        // bar (the native title row is the only header now); the flag
        // is picked up by App::update right after session_ui returns.
        // Ctrl+Shift+D still works.
        if ui
            .button("⏹")
            .on_hover_text("Отключиться (Ctrl+Shift+D)")
            .clicked()
        {
            s.disconnect_requested = true;
        }
    });
}

/// Floating controls over the video in chrome-less mode. Without them
/// a fullscreen user had NOTHING but an exit button: no keyboard, no
/// Ctrl, no chat — «нет вызова полноценной клавиатуры». Now:
///   • quickbar (top-center): soft keyboard (Android) · sticky
///     Ctrl/Alt/Shift · ⋯ menu (pause, keys strip, chat, files, stats,
///     disconnect);
///   • the control-keys strip floats over the video when enabled;
///   • view-only badge (top-left) + exit button (top-right).
/// Every surface's rect is pushed into `overlay` so the input layer
/// refuses touches that start on them (raw touches bypass egui's
/// widget hit-testing).
fn fullscreen_overlays(ctx: &Context, s: &mut SessionHandle, overlay: &mut Vec<egui::Rect>) {
    let mut badge_rect = egui::Rect::NOTHING;
    egui::Area::new(egui::Id::new("session_fs_badge"))
        .anchor(egui::Align2::LEFT_TOP, [8.0, 8.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            if s.view_only {
                ui.label(
                    egui::RichText::new("👁 только просмотр")
                        .color(Color32::from_rgb(255, 170, 60))
                        .strong(),
                );
            }
            badge_rect = ui.min_rect();
        });
    overlay.push(badge_rect);

    let mut exit_rect = egui::Rect::NOTHING;
    egui::Area::new(egui::Id::new("session_fs_exit"))
        .anchor(egui::Align2::RIGHT_TOP, [-8.0, 8.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            if ui
                .button("⛶")
                .on_hover_text("Выйти из полноэкранного режима (F11)")
                .clicked()
            {
                set_chromeless(s, false);
            }
            exit_rect = ui.min_rect();
        });
    overlay.push(exit_rect);

    // Floating control-keys strip (⌨ Клавиши from the quickbar menu).
    if s.panel_keys {
        let mut keys_rect = egui::Rect::NOTHING;
        egui::Area::new(egui::Id::new("session_fs_keys"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -64.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        keys_strip(ui, s);
                    });
                });
                keys_rect = ui.min_rect();
            });
        overlay.push(keys_rect);
    }

    // Floating quickbar — the fullscreen session controls.
    let mut bar_rect = egui::Rect::NOTHING;
    egui::Area::new(egui::Id::new("session_fs_quickbar"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 8.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.style_mut().spacing.button_padding = egui::vec2(8.0, 4.0);
                    // Soft keyboard (Android only — the hook is a no-op
                    // elsewhere; desktop has a physical keyboard).
                    #[cfg(target_os = "android")]
                    if ui
                        .button(if s.soft_keyboard_on { "⌨ ✓" } else { "⌨" })
                        .on_hover_text("Экранная клавиатура")
                        .clicked()
                    {
                        s.soft_keyboard_on = !s.soft_keyboard_on;
                        crate::android_sysui::set_keyboard(s.soft_keyboard_on);
                    }
                    sticky_mod_button(ui, s, "Ctrl", mods::CTRL);
                    sticky_mod_button(ui, s, "Alt", mods::ALT);
                    sticky_mod_button(ui, s, "Shift", mods::SHIFT);
                    ui.menu_button("⋯", |ui| {
                        ui.set_min_width(220.0);
                        if ui
                            .button(if s.paused {
                                "▶ Продолжить"
                            } else {
                                "⏸ Пауза"
                            })
                            .clicked()
                        {
                            s.paused = !s.paused;
                            let _ = s.commands.send(Command::Pause(s.paused));
                        }
                        for (label, flag) in [
                            ("⌨ Клавиши (Ctrl/F1/…)", &mut s.panel_keys),
                            ("💬 Чат", &mut s.panel_chat),
                            ("📁 Файлы", &mut s.panel_files),
                            ("📊 Статистика", &mut s.panel_stats),
                        ] {
                            if ui.toggle_value(flag, label).clicked() {
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        if ui.button("⏹ Отключиться").clicked() {
                            s.disconnect_requested = true;
                        }
                    });
                });
            });
            bar_rect = ui.min_rect();
        });
    overlay.push(bar_rect);
}

/// Toggle the chrome-less viewer mode. On Android this also hides the
/// system bars (sticky immersive); leaving the mode hides the soft
/// keyboard along with everything else.
fn set_chromeless(s: &mut SessionHandle, on: bool) {
    if s.fullscreen == on {
        return;
    }
    s.fullscreen = on;
    if !on && s.soft_keyboard_on {
        s.soft_keyboard_on = false;
        crate::android_sysui::set_keyboard(false);
    }
    crate::android_sysui::set_immersive(on);
}

/// The central remote view: the fitted video, the cursor overlay and
/// the input capture (toolbar/sidebar are laid out around it).
///
/// The video is CENTERED in the panel: allocating the fitted rect
/// directly left it pinned to the top-left corner with the whole
/// empty remainder to the right/bottom — on a phone in landscape this
/// read as «видео не на весь экран» even when the mode was right.
/// The input rect spans the FULL panel so taps outside the video
/// still clamp into the remote screen edges.
fn session_view(ui: &mut egui::Ui, s: &mut SessionHandle, overlay: &[egui::Rect]) {
    let available = ui.available_size();
    let Some(tex) = &s.texture else {
        // Still no frame: say what we ARE doing and give the two
        // known causes after a grace period (Android host = capture
        // not implemented yet; locked Windows screen = blank frames).
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.label(
                    egui::RichText::new("Ожидание первого кадра…")
                        .weak()
                        .size(16.0),
                );
                if !s.status.is_empty() {
                    ui.add_space(4.0);
                    ui.label(theme::mono(s.status.clone()).weak().small());
                }
                let waiting = s.opened_at.elapsed().as_secs();
                if waiting >= 10 {
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(
                            "Если хост — телефон: разблокируйте его и подтвердите\n\
                                 трансляцию (диалог OpacusR на экране телефона).\n\
                                 Если хост — компьютер: проверьте, что на нём экран разблокирован\n\
                                 и OpacusR работает.",
                        )
                        .weak()
                        .small(),
                    );
                }
            });
        });
        return;
    };
    let (rw, rh) = (s.screen.0 as f32, s.screen.1 as f32);
    let fit = fit_size(available, rw, rh);
    // The whole panel is the input surface; the video sits centered in it.
    let (panel, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
    response.request_focus();
    let rect = egui::Rect::from_center_size(panel.center(), fit);
    ui.painter().image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
    // Frame border for clarity.
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0_f32, Color32::from_gray(90)),
    );

    // Remote cursor overlay: the host's pointer, drawn on top of the
    // video (capture APIs drop the cursor, so it travels separately).
    // The hotspot sits exactly on the reported position; the overlay
    // is hidden while the pointer is outside the streamed monitor or
    // logically hidden on the host.
    if let Some((cx, cy, visible, shape_id)) = s.cursor_pos {
        if visible && rw > 0.0 {
            match s.cursor_shapes.get(&shape_id) {
                Some(cur) => {
                    // Never let the pointer shrink below ~14 screen px: on a
                    // shrunken/letterboxed phone view the raw scale made the
                    // arrow a few pixels tall — invisible (report 0.12.1:
                    // «не видно курсор» on a dark desktop).
                    let scale = (rect.width() / rw).max(14.0 / cur.size.0.max(1.0));
                    let top_left = egui::pos2(
                        rect.min.x + cx as f32 * scale - cur.hotspot.0 * scale,
                        rect.min.y + cy as f32 * scale - cur.hotspot.1 * scale,
                    );
                    let crect = egui::Rect::from_min_size(
                        top_left,
                        egui::vec2(cur.size.0 * scale, cur.size.1 * scale),
                    );
                    if crect.intersects(rect) {
                        ui.painter().image(
                            cur.texture.id(),
                            crect,
                            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                None => {
                    // The position arrived but its shape never did (a
                    // one-shot host-side shape failure used to silence
                    // the cursor for the WHOLE session). Draw a simple
                    // marker so the pointer is never invisible again.
                    let vs = rect.width() / rw;
                    let p = egui::pos2(rect.min.x + cx as f32 * vs, rect.min.y + cy as f32 * vs);
                    if rect.contains(p) {
                        ui.painter().circle(
                            p,
                            6.0,
                            Color32::from_rgba_unmultiplied(230, 160, 40, 200),
                            egui::Stroke::new(1.5_f32, Color32::WHITE),
                        );
                    }
                }
            }
        }
    }

    capture_input(ui, &response, rect, s, overlay);
}

/// Print the latest remote frame (the desktop toolbar button and the
/// Android call menu share this).
fn print_current_frame(s: &mut SessionHandle) {
    match &s.last_frame {
        Some((w, h, rgba)) => {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            match crate::print::print_png(*w, *h, rgba, secs) {
                Ok(path) => s.status = format!("printed: {}", path.display()),
                Err(e) => s.status = format!("print failed: {e}"),
            }
        }
        None => s.status = "Печать: кадр ещё не получен".into(),
    }
}

/// Toolbar label + color for an adaptive-quality tier (0 = full quality…
/// 3 = very constrained link).
fn tier_badge(tier: u8) -> (&'static str, Color32) {
    match tier {
        0 => ("⚡ лучшее", Color32::from_rgb(120, 220, 120)),
        1 => ("⚡ уровень 1", Color32::from_rgb(235, 210, 90)),
        2 => ("⚡ уровень 2", Color32::from_rgb(240, 160, 70)),
        _ => ("⚡ уровень 3", Color32::from_rgb(235, 100, 90)),
    }
}

/// Buffer raw wheel pixel deltas and forward whole 8-px notches. The
/// old per-event `-(dx as i32) / 8` truncated any sub-8-px swipe to a
/// zero-delta no-op — slow trackpad / gentle phone scrolls looked dead.
fn send_wheel(s: &mut SessionHandle, pos: (u32, u32), delta: egui::Vec2) {
    s.wheel_acc += delta;
    let dx = (s.wheel_acc.x / 8.0).trunc();
    let dy = (s.wheel_acc.y / 8.0).trunc();
    if dx != 0.0 || dy != 0.0 {
        s.wheel_acc -= egui::vec2(dx * 8.0, dy * 8.0);
        let _ = s.commands.send(Command::MouseWheel {
            x: pos.0,
            y: pos.1,
            dx: dx as i32,
            dy: dy as i32,
        });
    }
}

/// Scale the remote screen into the available rect preserving aspect.
fn fit_size(available: egui::Vec2, rw: f32, rh: f32) -> egui::Vec2 {
    if rw <= 0.0 || rh <= 0.0 {
        return available;
    }
    let scale = (available.x / rw).min(available.y / rh);
    egui::vec2(rw * scale, rh * scale)
}

/// Map egui pointer/keyboard events onto remote-desktop commands.
/// In view-only sessions nothing is sent — the host ignores control
/// traffic anyway, and not sending it saves bandwidth and surprises.
fn capture_input(
    _ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    s: &mut SessionHandle,
    overlay: &[egui::Rect],
) {
    if s.view_only {
        return;
    }
    let (sw, sh) = (s.screen.0 as f32, s.screen.1 as f32);

    // While a popup (⋯ menu, screen selector) is open, egui owns every
    // pointer/touch event — nothing goes to the remote desktop.
    let popup_open = response.ctx.memory(|m| m.any_popup_open());
    // Chrome surfaces (toolbar, sidebar, keys strip, floating overlays):
    // raw touches bypass egui's widget hit-testing, so a tap on a
    // floating button would otherwise ALSO click the remote desktop at
    // that point.
    let on_chrome = |p: egui::Pos2| overlay.iter().any(|r| r.contains(p));

    // Pointer → remote coordinates.
    let remote_pos = |p: egui::Pos2| -> Option<(u32, u32)> {
        if !rect.contains(p) || sw <= 0.0 || sh <= 0.0 {
            return None;
        }
        let rel = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        let rely = ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
        Some(((rel * sw) as u32, (rely * sh) as u32))
    };

    // Hover/drag movement.
    let pointer = response.ctx.input(|i| i.pointer.hover_pos());
    if let Some((x, y)) = pointer.filter(|p| !on_chrome(*p)).and_then(remote_pos) {
        if s.last_mouse != Some((x, y)) {
            s.last_mouse = Some((x, y));
            let _ = s.commands.send(Command::MouseMove { x, y });
        }
    }

    // ---- Android touch layer (compiled everywhere) ------------------------
    // egui-winit synthesizes pointer events from the FIRST touch (so a
    // tap already clicks), but nothing else worked on a phone: no right
    // click, no scroll, and a held touch still clicked. This layer owns
    // touch input end to end:
    //   quick tap      → left click at the touch point
    //   long-press     → right click at the touch point
    //   drag           → move with the left button pressed
    //   second finger  → two-finger scroll
    // The egui-synthesized pointer clicks are suppressed around every
    // touch interaction (cooldown latch below) so taps don't double.
    // Compiled on ALL platforms (not cfg-android): touch-only events
    // never arrive without a touchscreen, so desktop behavior is
    // unchanged — but the host build type-checks the code (the Android
    // cross-check CI job runs AFTER push, which once let a bad method
    // name slip through).
    {
        let now = std::time::Instant::now();
        let touch_events: Vec<egui::Event> = response.ctx.input(|i| {
            i.events
                .iter()
                .filter(|ev| matches!(ev, egui::Event::Touch { .. }))
                .cloned()
                .collect()
        });
        if !touch_events.is_empty() {
            // egui reports the synthesized click in the frame AFTER the
            // touch ends; keep suppressing pointer clicks past it.
            s.touch_cooldown_until = Some(now + std::time::Duration::from_millis(150));
        }
        // A popup menu owns the whole screen while open.
        if popup_open {
            return;
        }
        for ev in touch_events {
            let egui::Event::Touch { id, phase, pos, .. } = ev else {
                continue;
            };
            match phase {
                egui::TouchPhase::Start => {
                    // A touch that begins on a UI button must not
                    // classify into a remote tap/scroll gesture.
                    if on_chrome(pos) {
                        continue;
                    }
                    if s.touch_pending.is_none() && s.touch_drag.is_none() {
                        // Defer the press: quick release = tap (left),
                        // held release = long-press (right).
                        s.touch_pending = Some((id, pos, now));
                    } else if s.touch_scroll.is_none() {
                        // Second finger during an interaction → scroll.
                        s.touch_scroll = Some((id, pos));
                    }
                }
                egui::TouchPhase::Move => {
                    if let Some((pid, ppos, _t0)) = s.touch_pending {
                        if id == pid && pos.distance(ppos) > 14.0 {
                            // A drag: press the left button at the touch
                            // origin, then follow the finger.
                            if let Some((x, y)) = remote_pos(ppos) {
                                let _ = s.commands.send(Command::MouseButton {
                                    x,
                                    y,
                                    button: MouseButtonId::Left,
                                    down: true,
                                });
                            }
                            s.touch_drag = Some(pid);
                            s.touch_pending = None;
                        }
                    }
                    if s.touch_drag == Some(id) {
                        if let Some((x, y)) = remote_pos(pos) {
                            if s.last_mouse != Some((x, y)) {
                                s.last_mouse = Some((x, y));
                                let _ = s.commands.send(Command::MouseMove { x, y });
                            }
                        }
                    }
                    if let Some((sid, spos)) = s.touch_scroll {
                        if id == sid {
                            if let Some((x, y)) = remote_pos(pos) {
                                // Fractional deltas accumulate in
                                // wheel_acc (see send_wheel) instead of
                                // truncating to «wheel: 0» per move.
                                send_wheel(
                                    s,
                                    (x, y),
                                    egui::vec2(-(pos.x - spos.x), -(pos.y - spos.y)),
                                );
                            }
                            s.touch_scroll = Some((sid, pos));
                        }
                    }
                }
                egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                    if let Some((pid, _ppos, t0)) = s.touch_pending {
                        if id == pid {
                            // A tap only fires on a clean END; a Cancel
                            // (system stole the gesture) sends nothing.
                            if phase == egui::TouchPhase::End {
                                let held = t0.elapsed();
                                let button = if held < std::time::Duration::from_millis(500) {
                                    MouseButtonId::Left
                                } else {
                                    MouseButtonId::Right
                                };
                                if let Some((x, y)) = remote_pos(pos) {
                                    let _ = s.commands.send(Command::MouseMove { x, y });
                                    let _ = s.commands.send(Command::MouseButton {
                                        x,
                                        y,
                                        button,
                                        down: true,
                                    });
                                    let _ = s.commands.send(Command::MouseButton {
                                        x,
                                        y,
                                        button,
                                        down: false,
                                    });
                                }
                            }
                            s.touch_pending = None;
                        }
                    }
                    if s.touch_drag == Some(id) {
                        // Release even on Cancel: a stuck left button on
                        // the host would keep dragging forever.
                        if let Some((x, y)) = remote_pos(pos) {
                            let _ = s.commands.send(Command::MouseButton {
                                x,
                                y,
                                button: MouseButtonId::Left,
                                down: false,
                            });
                        }
                        s.touch_drag = None;
                    }
                    if let Some((sid, _)) = s.touch_scroll {
                        if id == sid {
                            s.touch_scroll = None;
                        }
                    }
                }
            }
        }
    }

    // Buttons — suppressed while the touch layer owns the interaction.
    let touch_owned = s.touch_pending.is_some()
        || s.touch_drag.is_some()
        || s.touch_scroll.is_some()
        || s.touch_cooldown_until
            .is_some_and(|t| std::time::Instant::now() < t);
    if !touch_owned {
        let buttons = [
            (egui::PointerButton::Primary, MouseButtonId::Left),
            (egui::PointerButton::Secondary, MouseButtonId::Right),
            (egui::PointerButton::Middle, MouseButtonId::Middle),
        ];
        for (pb, mb) in buttons {
            let pressed = response.ctx.input(|i| i.pointer.button_clicked(pb));
            let released = response.ctx.input(|i| i.pointer.button_released(pb));
            if let Some((x, y)) = pointer
                .filter(|p| !popup_open && !on_chrome(*p))
                .and_then(remote_pos)
            {
                if pressed {
                    let _ = s.commands.send(Command::MouseButton {
                        x,
                        y,
                        button: mb,
                        down: true,
                    });
                }
                if released {
                    let _ = s.commands.send(Command::MouseButton {
                        x,
                        y,
                        button: mb,
                        down: false,
                    });
                }
            }
        }
    }

    // Wheel: fractional deltas accumulate in `wheel_acc` and go out as
    // whole 8-px notches — the old per-event `-(x as i32) / 8` turned
    // every slow trackpad/phone swipe into a zero-delta no-op.
    let scroll = response.ctx.input(|i| i.raw_scroll_delta);
    if scroll != egui::Vec2::ZERO {
        if let Some((x, y)) = pointer
            .filter(|p| !popup_open && !on_chrome(*p))
            .and_then(remote_pos)
        {
            send_wheel(s, (x, y), egui::vec2(-scroll.x, -scroll.y));
        }
    }

    // Keyboard: desktop forwards while the view has focus; on Android
    // the soft keyboard is summoned via the ⌨ button and may hold the
    // focus itself — forward while it is open even if egui's focus
    // landed elsewhere, otherwise typed text is silently dropped. While
    // a popup menu is open the keys belong to the menu, not the host.
    #[allow(unused_mut)]
    let mut keyboard_live = response.has_focus() && !popup_open;
    #[cfg(target_os = "android")]
    if s.soft_keyboard_on && !popup_open {
        keyboard_live = true;
    }
    if !keyboard_live {
        return;
    }
    let events = response.ctx.input(|i| i.events.clone());
    // Printable keys ride BOTH an Event::Key (Space) and an Event::Text
    // (" ") on some backends — dedupe so hosts do not get double spaces.
    let text_seen_space = events
        .iter()
        .any(|ev| matches!(ev, egui::Event::Text(t) if t.contains(' ')));
    for ev in events {
        match ev {
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } => {
                if key == egui::Key::Space && text_seen_space && modifiers.is_none() {
                    continue;
                }
                if let Some(opkey) = map_key(key) {
                    // Releases carry the SAME modifiers as the press:
                    // the host releases its sticky modifiers only when
                    // the release carries the bits — sending 0 here left
                    // a physical-keyboard Ctrl held down on the host
                    // forever (every later tap behaved as Ctrl+tap).
                    let _ = s.commands.send(Command::KeyEvent {
                        key: opkey,
                        mods: map_mods(&modifiers),
                        down: pressed,
                    });
                }
            }
            egui::Event::Text(text) => {
                let _ = s.commands.send(Command::TextInput { text });
            }
            // Real IME commits (Gboard swipe/autocorrect, desktop CJK
            // input methods) arrive as Ime events, not Text — forward
            // the committed string, drop the composition chatter.
            egui::Event::Ime(egui::ImeEvent::Commit(text)) if !text.is_empty() => {
                let _ = s.commands.send(Command::TextInput { text });
            }
            _ => {}
        }
    }
}

fn map_mods(m: &egui::Modifiers) -> u8 {
    let mut out = 0;
    if m.shift {
        out |= mods::SHIFT;
    }
    if m.ctrl {
        out |= mods::CTRL;
    }
    if m.alt {
        out |= mods::ALT;
    }
    if m.mac_cmd || m.command {
        out |= mods::META;
    }
    out
}

fn map_key(key: egui::Key) -> Option<OpKey> {
    use egui::Key as K;
    Some(match key {
        K::Enter => OpKey::Enter,
        K::Escape => OpKey::Escape,
        K::Tab => OpKey::Tab,
        K::Backspace => OpKey::Backspace,
        K::Delete => OpKey::Delete,
        K::Insert => OpKey::Insert,
        K::Home => OpKey::Home,
        K::End => OpKey::End,
        K::PageUp => OpKey::PageUp,
        K::PageDown => OpKey::PageDown,
        K::ArrowUp => OpKey::ArrowUp,
        K::ArrowDown => OpKey::ArrowDown,
        K::ArrowLeft => OpKey::ArrowLeft,
        K::ArrowRight => OpKey::ArrowRight,
        K::Space => OpKey::Space,
        K::F1 => OpKey::F1,
        K::F2 => OpKey::F2,
        K::F3 => OpKey::F3,
        K::F4 => OpKey::F4,
        K::F5 => OpKey::F5,
        K::F6 => OpKey::F6,
        K::F7 => OpKey::F7,
        K::F8 => OpKey::F8,
        K::F9 => OpKey::F9,
        K::F10 => OpKey::F10,
        K::F11 => OpKey::F11,
        K::F12 => OpKey::F12,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

fn stats_panel(ui: &mut egui::Ui, s: &mut SessionHandle) {
    ui.heading("📊 Статистика");
    egui::Grid::new("stats_grid")
        .num_columns(2)
        .spacing([12.0, 3.0])
        .show(ui, |ui| {
            ui.label("Хост");
            let name = if s.host_name.is_empty() {
                "—".to_string()
            } else {
                s.host_name.clone()
            };
            ui.label(name);
            ui.end_row();

            ui.label("Поток");
            ui.label(format!("{}×{} px", s.stream_res.0, s.stream_res.1));
            ui.end_row();

            ui.label("Кодек");
            ui.label(
                s.codec
                    .map(|c| format!("{c:?}"))
                    .unwrap_or_else(|| "—".into()),
            );
            ui.end_row();

            ui.label("Приём");
            ui.label(format!(
                "{:.1} fps (хост подсказывает {} fps)",
                s.recv_fps, s.fps_hint
            ));
            ui.end_row();

            ui.label("Битрейт");
            ui.label(fmt_bitrate(s.bitrate_bps));
            ui.end_row();

            ui.label("Задержка");
            ui.label(format!("{} мс", s.rtt));
            ui.end_row();

            ui.label("Качество");
            if s.quality_adaptive {
                match s.quality_tier {
                    Some(tier) => {
                        let (text, color) = tier_badge(tier);
                        ui.label(
                            egui::RichText::new(format!("{text} ({tier}/3)"))
                                .color(color)
                                .strong(),
                        )
                        .on_hover_text(
                            "Управляется отчётами RTT/джиттера; \
                             уровни: <60 мс лучшее, <120 мс у1, <250 мс у2, ≥250 мс у3",
                        );
                    }
                    None => {
                        ui.label("адаптивное");
                    }
                }
            } else if s.quality_params.is_some() {
                ui.label("вручную").on_hover_text(
                    "Ручная настройка качества отключила адаптивное качество для этой сессии.",
                );
            } else {
                ui.label("—");
            }
            ui.end_row();

            ui.label("Состояние");
            ui.label(if s.paused {
                "⏸ пауза"
            } else {
                "идёт поток"
            });
            ui.end_row();

            ui.label("Запись");
            if s.recording {
                ui.label(
                    egui::RichText::new("● идёт (хост)")
                        .color(egui::Color32::from_rgb(230, 80, 80)),
                );
            } else {
                ui.label("—");
            }
            ui.end_row();
        });
    ui.separator();
    ui.label(
        egui::RichText::new(
            "F11 — во весь экран · Ctrl+Shift+P — пауза · Ctrl+Shift+D — отключиться\n\
             Ctrl+Shift+C/F/T/S — чат / файлы / терминал / статистика",
        )
        .small()
        .weak(),
    );
}

/// Wire bitrate: `300_000` bytes/s → `"2.4 Mbit/s"`, `30_000` → `"240 kbit/s"`.
fn fmt_bitrate(bytes_per_sec: u64) -> String {
    let bits = bytes_per_sec * 8;
    if bits >= 1_000_000 {
        format!("{:.1} Mbit/s", bits as f64 / 1_000_000.0)
    } else {
        format!("{} kbit/s", bits / 1_000)
    }
}

/// Sticky-modifier tap for the on-screen keys strip. `bit` = a bit of
/// [`opacusr_proto::message::mods`].
fn sticky_mod_button(ui: &mut egui::Ui, s: &mut SessionHandle, label: &str, bit: u8) {
    let on = s.sticky_mods & bit != 0;
    let text = egui::RichText::new(label).small();
    let text = if on { text.strong() } else { text };
    let fill = if on {
        theme::TEAL
    } else {
        ui.visuals().widgets.inactive.bg_fill
    };
    if ui
        .add(
            egui::Button::new(text)
                .fill(fill)
                .min_size([34.0, 24.0].into()),
        )
        .clicked()
    {
        s.sticky_mods ^= bit;
    }
}

/// One control key: press+release with the sticky modifiers, then clear
/// them (classic sticky-keys behavior — Ctrl sticky + «C» = Ctrl+C).
fn key_button(ui: &mut egui::Ui, s: &mut SessionHandle, label: &str, key: OpKey) {
    if ui.button(egui::RichText::new(label).small()).clicked() {
        let m = s.sticky_mods;
        let _ = s.commands.send(Command::KeyEvent {
            key,
            mods: m,
            down: true,
        });
        let _ = s.commands.send(Command::KeyEvent {
            key,
            mods: m,
            down: false,
        });
        s.sticky_mods = 0;
    }
}

fn fkey(i: u32) -> OpKey {
    match i {
        1 => OpKey::F1,
        2 => OpKey::F2,
        3 => OpKey::F3,
        4 => OpKey::F4,
        5 => OpKey::F5,
        6 => OpKey::F6,
        7 => OpKey::F7,
        8 => OpKey::F8,
        9 => OpKey::F9,
        10 => OpKey::F10,
        11 => OpKey::F11,
        _ => OpKey::F12,
    }
}

/// On-screen control keys: a phone soft keyboard types letters but has
/// NO Ctrl/Alt/Win, no F1–F12, no arrows — remote control
/// («полноценная клавиатура») needs them. Modifiers are STICKY:
/// tap Ctrl, then tap C.
fn keys_strip(ui: &mut egui::Ui, s: &mut SessionHandle) {
    ui.horizontal_wrapped(|ui| {
        ui.style_mut().spacing.button_padding = egui::vec2(6.0, 3.0);
        ui.style_mut().spacing.item_spacing = egui::vec2(4.0, 4.0);
        sticky_mod_button(ui, s, "Ctrl", mods::CTRL);
        sticky_mod_button(ui, s, "Alt", mods::ALT);
        sticky_mod_button(ui, s, "Shift", mods::SHIFT);
        sticky_mod_button(ui, s, "Win", mods::META);
        ui.separator();
        key_button(ui, s, "Esc", OpKey::Escape);
        key_button(ui, s, "Tab", OpKey::Tab);
        key_button(ui, s, "Enter", OpKey::Enter);
        key_button(ui, s, "Back", OpKey::Backspace);
        key_button(ui, s, "Del", OpKey::Delete);
        key_button(ui, s, "Home", OpKey::Home);
        key_button(ui, s, "End", OpKey::End);
        key_button(ui, s, "PgUp", OpKey::PageUp);
        key_button(ui, s, "PgDn", OpKey::PageDown);
        key_button(ui, s, "←", OpKey::ArrowLeft);
        key_button(ui, s, "↑", OpKey::ArrowUp);
        key_button(ui, s, "↓", OpKey::ArrowDown);
        key_button(ui, s, "→", OpKey::ArrowRight);
        ui.separator();
        for i in 1..=12u32 {
            key_button(ui, s, &format!("F{i}"), fkey(i));
        }
        ui.separator();
        // Latin letters: the only way to send Ctrl+C-style combos from a
        // phone (an IME text commit carries no modifiers).
        for c in b'a'..=b'z' {
            let ch = c as char;
            key_button(ui, s, &ch.to_string(), OpKey::Char(ch));
        }
    });
}

fn chat_panel(ui: &mut egui::Ui, s: &mut SessionHandle) {
    ui.heading("💬 Чат");
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .max_height(220.0)
        .show(ui, |ui| {
            for line in &s.chat {
                ui.label(line);
            }
        });
    ui.horizontal(|ui| {
        let send = ui.button("Отправить");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut s.chat_input)
                .hint_text("сообщение…")
                .desired_width(ui.available_width() - 110.0),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if send.clicked() || enter {
            if !s.chat_input.trim().is_empty() {
                let text = s.chat_input.trim().to_string();
                s.chat.push(format!("[me] {text}"));
                let _ = s.commands.send(Command::Chat(text));
                s.chat_input.clear();
            }
            resp.request_focus();
        }
    });
}

fn files_panel(ui: &mut egui::Ui, s: &mut SessionHandle) {
    ui.heading("📁 Файлы");
    if s.view_only {
        crate::hint(
            ui,
            "Сессия только для просмотра: передача файлов запрещена хостом.",
        );
        return;
    }
    crate::hint(ui, "Перетащите файлы в окно, чтобы отправить их на хост.");
    ui.horizontal(|ui| {
        if ui.button("⬆ Отправить файлы…").clicked() {
            if let Some(paths) = crate::pick_files() {
                for p in paths {
                    let _ = s.commands.send(Command::PushFile(p));
                }
            }
        }
        if ui.button("📤 Буфер обмена → хост").clicked() {
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            if let Ok(mut cb) = arboard::Clipboard::new() {
                if let Ok(text) = cb.get_text() {
                    let _ = s.commands.send(Command::SendClipboard(text));
                }
            }
        }
    });
    // Active uploads.
    for (id, name, done, total) in &s.uploads {
        let frac = if *total > 0 {
            *done as f32 / *total as f32
        } else {
            0.0
        };
        ui.add(egui::ProgressBar::new(frac).text(format!("{name} ({id})")));
    }
}

fn terminal_panel(ui: &mut egui::Ui, s: &mut SessionHandle) {
    ui.heading(">_ Терминал хоста");
    if s.view_only {
        crate::hint(ui, "Сессия только для просмотра: терминал запрещён хостом.");
        return;
    }
    if !s.term_open {
        if ui.button("Открыть оболочку").clicked() {
            let _ = s.commands.send(Command::TermOpen {
                rows: 30,
                cols: 100,
            });
            s.term_out.clear();
        }
        return;
    }
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .max_height(260.0)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut s.term_out)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        });
    ui.horizontal(|ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut s.term_input)
                .hint_text("команда…")
                .desired_width(ui.available_width() - 50.0),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let line = std::mem::take(&mut s.term_input);
            let _ = s.commands.send(Command::TermData {
                id: 1,
                data: format!("{line}\n").into_bytes(),
            });
            resp.request_focus();
        }
        if ui.button("⏎").clicked() {
            let line = std::mem::take(&mut s.term_input);
            let _ = s.commands.send(Command::TermData {
                id: 1,
                data: format!("{line}\n").into_bytes(),
            });
        }
    });
    if ui.button("Закрыть оболочку").clicked() {
        let _ = s.commands.send(Command::TermClose { id: 1 });
        s.term_open = false;
    }
}

fn tunnel_panel(ui: &mut egui::Ui, s: &mut SessionHandle) {
    ui.heading("🔗 TCP-туннели");
    if !s.tunnel_available {
        crate::hint(
            ui,
            "Хост не перенаправляет туннели (политика отключена или сессия только для просмотра).",
        );
        return;
    }

    // Active listeners with stop buttons.
    if s.tunnels.is_empty() {
        ui.weak("нет активных туннелей");
    } else {
        egui::Grid::new("tunnel_list")
            .num_columns(4)
            .show(ui, |ui| {
                for (port, host, tport) in s.tunnels.clone() {
                    ui.monospace(format!("127.0.0.1:{port}"));
                    ui.label("→");
                    ui.monospace(format!("{host}:{tport}"));
                    if ui.button("стоп").clicked() {
                        let _ = s.commands.send(Command::TunnelStop { listen_port: port });
                        s.tunnels.retain(|(p, ..)| *p != port);
                    }
                    ui.end_row();
                }
            });
    }
    ui.separator();

    ui.label("Перенаправить локальный порт на цель на хосте:");
    egui::Grid::new("tunnel_form")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Локальный порт");
            ui.add(
                egui::TextEdit::singleline(&mut s.tun_local_port)
                    .hint_text("8080 (0 = auto)")
                    .desired_width(120.0),
            );
            ui.end_row();
            ui.label("Цель (на хосте)");
            ui.add(
                egui::TextEdit::singleline(&mut s.tun_target_host)
                    .hint_text("127.0.0.1")
                    .desired_width(120.0),
            );
            ui.end_row();
            ui.label("Порт цели");
            ui.add(
                egui::TextEdit::singleline(&mut s.tun_target_port)
                    .hint_text("3000")
                    .desired_width(120.0),
            );
            ui.end_row();
        });

    let local = s.tun_local_port.trim().parse::<u16>().ok();
    let target = s.tun_target_port.trim().parse::<u16>().ok();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                local.is_some() && target.is_some(),
                egui::Button::new("Запустить"),
            )
            .clicked()
        {
            let _ = s.commands.send(Command::TunnelListen {
                listen_port: local.unwrap_or(0),
                target_host: s.tun_target_host.trim().to_string(),
                target_port: target.unwrap_or(0),
            });
            s.tun_local_port.clear();
        }
        if ui.button("Остановить все").clicked() {
            for (port, ..) in s.tunnels.clone() {
                let _ = s.commands.send(Command::TunnelStop { listen_port: port });
            }
            s.tunnels.clear();
        }
    });
    ui.weak("Подключите приложения к 127.0.0.1:<локальный порт>; хост перенаправит их.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SessionHandle;

    /// Headless render smoke test: the session screen (toolbar or the
    /// chrome-less «⛶» mode) must lay out and PAINT in both chrome modes
    /// when a frame texture is present — and while still waiting for one.
    /// Pure egui (`Context::run`), no window system involved.
    #[test]
    fn session_ui_renders_in_both_chrome_modes() {
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = SessionHandle::new(cmd_tx, ev_rx);
        s.screen = (1920, 1080);
        let mut cfg = AgentConfig::default();

        let ctx = egui::Context::default();
        let img = egui::ColorImage::new([64, 48], Color32::from_rgb(40, 40, 60));
        let tex = ctx.load_texture("frame", img, egui::TextureOptions::default());

        for (label, fullscreen, with_tex) in [
            ("waiting, toolbar", false, false),
            ("waiting, chromeless", true, false),
            ("frame, toolbar", false, true),
            ("frame, chromeless", true, true),
        ] {
            s.fullscreen = fullscreen;
            s.texture = with_tex.then(|| tex.clone());
            for _ in 0..3 {
                let out = ctx.run(egui::RawInput::default(), |ctx| {
                    session_ui(ctx, &mut s, &mut cfg);
                });
                assert!(
                    !out.shapes.is_empty(),
                    "{label}: the session screen painted nothing"
                );
            }
        }
    }

    /// The chrome-less overlays (exit button + view-only badge) must
    /// paint: they are the only controls a fullscreen phone user can
    /// reach. Areas report shapes from the SECOND frame on (egui needs
    /// one pass to place anchored widgets), so this drives six passes
    /// and requires output at least once after the first.
    #[test]
    fn chromeless_mode_overlays_paint_view_only_badge() {
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = SessionHandle::new(cmd_tx, ev_rx);
        s.view_only = true;
        s.fullscreen = true;
        let mut cfg = AgentConfig::default();

        let ctx = egui::Context::default();
        let mut ever_painted = false;
        for _pass in 0..6 {
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                session_ui(ctx, &mut s, &mut cfg);
            });
            if !out.shapes.is_empty() {
                ever_painted = true;
            }
        }
        assert!(
            ever_painted,
            "chromeless overlays (exit button / view-only badge) painted nothing in 6 passes"
        );
    }

    /// fit_size preserves the remote aspect ratio and never exceeds the
    /// available area (the phone letterboxing math).
    #[test]
    fn fit_size_preserves_aspect() {
        let av = egui::vec2(400.0, 800.0);
        let fit = fit_size(av, 1920.0, 1080.0);
        assert!(fit.x <= 400.0 + f32::EPSILON && fit.y <= 800.0 + f32::EPSILON);
        assert!((fit.x / fit.y - 1920.0 / 1080.0).abs() < 1e-3);

        let fit2 = fit_size(av, 1080.0, 1920.0);
        assert!(fit2.x <= 400.0 + f32::EPSILON && fit2.y <= 800.0 + f32::EPSILON);
        assert!((fit2.x / fit2.y - 1080.0 / 1920.0).abs() < 1e-3);
    }
}
