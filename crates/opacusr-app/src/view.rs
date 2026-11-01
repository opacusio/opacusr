//! The live session UI: remote screen view with full input capture, plus
//! toggleable side panels for chat, file transfer and the remote terminal.

use egui::{Color32, Context, Sense};

use opacusr_client::session::Command;
use opacusr_proto::message::mods;
use opacusr_proto::message::{MouseButtonId, OpKey};

use crate::app::SessionHandle;
use opacusr_agent::config::AgentConfig;

/// Session screen: central remote view + toggleable right sidebar.
pub fn session_ui(ctx: &Context, s: &mut SessionHandle, cfg: &mut AgentConfig) {
    // ---- toolbar ---------------------------------------------------------
    egui::TopBottomPanel::top("session_toolbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            if s.view_only {
                ui.label(
                    egui::RichText::new("👁 view only")
                        .color(Color32::from_rgb(255, 170, 60))
                        .strong(),
                );
                ui.separator();
            }
            ui.label(egui::RichText::new(&s.status).weak().small());
            ui.separator();
            if ui
                .button(if s.paused { "▶ Resume" } else { "⏸ Pause" })
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
            if ui.toggle_value(&mut s.clipboard_sync, "📋 sync").changed() {
                let _ = s.commands.send(Command::ClipboardSync(s.clipboard_sync));
            }
            if s.audio_available && ui.toggle_value(&mut s.audio_on, "🔊").changed() {
                let _ = s.commands.send(Command::Audio(s.audio_on));
            }
            ui.separator();
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
            ui.separator();
            ui.label(format!("RTT {} ms", s.rtt));
            ui.separator();
            ui.toggle_value(&mut s.panel_chat, "💬");
            ui.toggle_value(&mut s.panel_files, "📁");
            ui.toggle_value(&mut s.panel_term, "⌨");
        });
    });

    // ---- right sidebar ---------------------------------------------------
    if s.panel_chat || s.panel_files || s.panel_term {
        egui::SidePanel::right("session_sidebar")
            .min_width(280.0)
            .default_width(320.0)
            .show(ctx, |ui| {
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
                }
            });
    }

    // ---- remote view -----------------------------------------------------
    egui::CentralPanel::default().show(ctx, |ui| {
        let available = ui.available_size();
        let Some(tex) = &s.texture else {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new("waiting for the first frame…").weak());
            });
            return;
        };
        let (rw, rh) = (s.screen.0 as f32, s.screen.1 as f32);
        let fit = fit_size(available, rw, rh);
        let (rect, response) = ui.allocate_exact_size(fit, Sense::click_and_drag());
        response.request_focus();
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

        capture_input(ui, &response, rect, s);
    });
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
) {
    if s.view_only {
        return;
    }
    let (sw, sh) = (s.screen.0 as f32, s.screen.1 as f32);

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
    if let Some((x, y)) = pointer.and_then(remote_pos) {
        if s.last_mouse != Some((x, y)) {
            s.last_mouse = Some((x, y));
            let _ = s.commands.send(Command::MouseMove { x, y });
        }
    }

    // Buttons.
    let buttons = [
        (egui::PointerButton::Primary, MouseButtonId::Left),
        (egui::PointerButton::Secondary, MouseButtonId::Right),
        (egui::PointerButton::Middle, MouseButtonId::Middle),
    ];
    for (pb, mb) in buttons {
        let pressed = response.ctx.input(|i| i.pointer.button_clicked(pb));
        let released = response.ctx.input(|i| i.pointer.button_released(pb));
        if let Some((x, y)) = pointer.and_then(remote_pos) {
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

    // Wheel.
    let scroll = response.ctx.input(|i| i.raw_scroll_delta);
    if scroll != egui::Vec2::ZERO {
        if let Some((x, y)) = pointer.and_then(remote_pos) {
            let _ = s.commands.send(Command::MouseWheel {
                x,
                y,
                dx: -scroll.x as i32 / 8,
                dy: -scroll.y as i32 / 8,
            });
        }
    }

    // Keyboard (only while the view has focus).
    if !response.has_focus() {
        return;
    }
    let events = response.ctx.input(|i| i.events.clone());
    for ev in events {
        match ev {
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } => {
                if pressed && !modifiers.is_none() {
                    if let Some(opkey) = map_key(key) {
                        let _ = s.commands.send(Command::KeyEvent {
                            key: opkey,
                            mods: map_mods(&modifiers),
                            down: pressed,
                        });
                    }
                } else if let Some(opkey) = map_key(key) {
                    let _ = s.commands.send(Command::KeyEvent {
                        key: opkey,
                        mods: 0,
                        down: pressed,
                    });
                }
            }
            egui::Event::Text(text) => {
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

fn chat_panel(ui: &mut egui::Ui, s: &mut SessionHandle) {
    ui.heading("💬 Chat");
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .max_height(220.0)
        .show(ui, |ui| {
            for line in &s.chat {
                ui.label(line);
            }
        });
    ui.horizontal(|ui| {
        let send = ui.button("Send");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut s.chat_input)
                .hint_text("message…")
                .desired_width(ui.available_width() - 60.0),
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
    ui.heading("📁 Files");
    if s.view_only {
        crate::hint(
            ui,
            "View-only session: file transfer is disabled by the host.",
        );
        return;
    }
    crate::hint(
        ui,
        "Drag & drop files anywhere on the window to send them to the host.",
    );
    ui.horizontal(|ui| {
        if ui.button("⬆ Send files…").clicked() {
            if let Some(paths) = rfd::FileDialog::new().pick_files() {
                for p in paths {
                    let _ = s.commands.send(Command::PushFile(p));
                }
            }
        }
        if ui.button("📤 Send clipboard → host").clicked() {
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
    ui.heading("⌨ Remote terminal");
    if s.view_only {
        crate::hint(
            ui,
            "View-only session: the terminal is disabled by the host.",
        );
        return;
    }
    if !s.term_open {
        if ui.button("Open shell").clicked() {
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
                .hint_text("command…")
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
    if ui.button("Close shell").clicked() {
        let _ = s.commands.send(Command::TermClose { id: 1 });
        s.term_open = false;
    }
}
