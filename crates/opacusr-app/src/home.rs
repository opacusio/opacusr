//! The main window (AnyDesk-style): the «История подключений» list IS
//! the main content — it fills the window and is visible the moment the
//! app opens. There is no separate home screen anymore: the compact
//! connect card sits to the left of the history (above it on narrow
//! windows), and every host-side option lives in the settings drawer
//! (`crate::settings` — Хост, Безопасность).
//!
//! Layout rules:
//! 1. Wide window: connect card (fixed 400 px) on the left, the
//!    history fills the rest of the width AND the full height — no
//!    dead margins, no dead band.
//! 2. Narrow window (open drawer, small screen, mobile portrait): the
//!    card stacks above the history in one scrolling column.
//! 3. The mono ID is scaled so «123 456 789» never wraps.

use egui::{Color32, Ui};

use opacusr_client::addressbook::{fmt_duration, fmt_ts, Outcome};

use crate::app::App;
use crate::theme;

/// Below this available width the connect card and the history stack
/// vertically (drawer open, small window, mobile portrait).
const STACK_BELOW: f32 = 860.0;

/// Connect-card width on wide screens.
const CARD_W: f32 = 400.0;

/// The main layout: the connect card and the history list, both always
/// visible — the window opens straight onto the history.
pub fn home_ui(ctx: &egui::Context, app: &mut App) {
    egui::CentralPanel::default().show(ctx, |ui| {
        if ui.available_width() >= STACK_BELOW {
            // Wide: card top-left, the history fills the rest.
            ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                ui.vertical(|ui| {
                    ui.set_min_width(CARD_W);
                    ui.set_max_width(CARD_W);
                    connect_card(ui, app);
                });
                ui.add_space(12.0);
                history_card(ui, app);
            });
        } else {
            // Narrow: one scrolling column, history below the card.
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.set_min_width(ui.available_width());
                        connect_card(ui, app);
                        ui.add_space(12.0);
                        history_card_narrow(ui, app);
                    });
                });
        }
    });
}

/// The compact connect card: partner ID + password + the violet CTA,
/// the on-demand 2FA field — and the «Этот компьютер» strip beneath
/// (own ID, agent status, start/stop). TeamViewer parity without the
/// big home screen: password, 2FA and service controls are in Настройки.
fn connect_card(ui: &mut Ui, app: &mut App) {
    theme::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        theme::section(ui, "Подключиться к компьютеру");
        ui.add_space(8.0);

        ui.label(egui::RichText::new("ID партнёра").weak().small());
        let id_resp = ui.add_sized(
            [ui.available_width(), 36.0],
            egui::TextEdit::singleline(&mut app.peer_id_input)
                .font(egui::TextStyle::Monospace)
                .hint_text("123 456 789 или ip:port"),
        );
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Пароль").weak().small());
        let pw_resp = ui.add_sized(
            [ui.available_width(), 30.0],
            egui::TextEdit::singleline(&mut app.password_input)
                .password(true)
                .hint_text("пароль компьютера партнёра"),
        );
        // An empty-password connect attempt pops the keyboard onto this
        // field at once — the fix is one keystroke away.
        if app.focus_password {
            pw_resp.request_focus();
            app.focus_password = false;
        }
        ui.add_space(10.0);

        // Connect: violet CTA, full width, TeamViewer-style.
        let connect_w = ui.available_width();
        let connect_clicked = ui
            .add_sized(
                [connect_w, 38.0],
                egui::Button::new(theme::bold("Подключиться").size(17.0).color(Color32::WHITE))
                    .fill(theme::ACCENT)
                    .rounding(egui::Rounding::same(10.0)),
            )
            .clicked();
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if connect_clicked || (enter && (id_resp.lost_focus() || pw_resp.lost_focus())) {
            app.connect_clicked(None);
        }

        ui.add_space(6.0);
        // The single on-demand extra: the host's 2FA code.
        egui::CollapsingHeader::new(
            egui::RichText::new("Код 2FA (если хост требует)").color(theme::MUTED),
        )
        .default_open(false)
        .show(ui, |ui| {
            ui.add_sized(
                [ui.available_width(), 26.0],
                egui::TextEdit::singleline(&mut app.totp_input)
                    .hint_text("6 цифр из приложения-аутентификатора"),
            );
        });

        ui.add_space(4.0);
        // Status under the fields, left-aligned — it belongs to them.
        // Errors are gold, not muted gray: on a phone a weak small line
        // after a tap reads as «ничего не произошло».
        if app.status_is_error {
            ui.label(
                egui::RichText::new(&app.status_message)
                    .color(theme::GOLD)
                    .size(13.0),
            );
        } else {
            ui.label(egui::RichText::new(&app.status_message).weak().small());
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        own_machine_strip(ui, app);
    });
}

/// «Этот компьютер»: own ID (click to copy), the agent/service status
/// line, the unattended-password reveal and the agent start/stop
/// control when no Windows service hosts the machine.
fn own_machine_strip(ui: &mut Ui, app: &mut App) {
    theme::section(ui, "Этот компьютер");
    ui.add_space(6.0);
    let running_id = app.host.as_ref().and_then(|h| h.my_id);
    let shown = running_id.or(app.config.last_peer_id);
    match shown {
        Some(id) => {
            let online = running_id.is_some();
            let text = spaced_id(id);
            // Scale the mono digits to the column: «123 456 789» is 11
            // glyphs, a mono glyph is ~0.6 em — keep it on ONE line no
            // matter how narrow the column is.
            let col_w = ui.available_width();
            let size = ((col_w - 26.0) / 6.9).clamp(15.0, 26.0);
            let resp = ui
                .add(
                    egui::Button::new(theme::mono(&text).size(size).color(if online {
                        theme::TEXT
                    } else {
                        theme::MUTED
                    }))
                    .fill(theme::INSET)
                    .stroke(egui::Stroke::new(1.0_f32, theme::CARD_STROKE))
                    .rounding(egui::Rounding::same(10.0))
                    .min_size([col_w.max(120.0), 40.0].into()),
                )
                .on_hover_text("Нажмите, чтобы скопировать ID");
            if resp.clicked() {
                ui.output_mut(|o| o.copied_text = id.to_string());
                app.status_message = "ID скопирован в буфер обмена.".into();
            }
            if online {
                ui.label(theme::dot("● ID активен", theme::TEAL));
            } else if app.service_hosts {
                ui.label(theme::dot(
                    "● служба работает · машина доступна",
                    theme::TEAL,
                ));
            } else {
                ui.label(theme::dot("● офлайн · запустите агента", theme::GOLD));
            }
        }
        None => {
            ui.label(
                egui::RichText::new("регистрация на сервере…")
                    .weak()
                    .size(18.0),
            );
            ui.label(
                egui::RichText::new("ID появится после первого подключения к серверу")
                    .weak()
                    .small(),
            );
            // The endpoint loop reports every failed attempt into
            // `host.notices`, but that log lives in Настройки → Диагностика
            // — a host that cannot reach its signaling endpoint (Android
            // without the INTERNET permission, dead server, wrong port)
            // sat on «регистрация на сервере…» forever with no visible
            // reason. Surface the LAST failure right here.
            if let Some(host) = &app.host {
                if let Some(last) = host.notices.last() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(last.clone())
                            .weak()
                            .small()
                            .color(theme::GOLD),
                    );
                }
            }
        }
    }

    // Unattended password parity (the old home card showed it): dots +
    // reveal toggle; changing it lives in Настройки → Безопасность.
    if app.config.is_unattended() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let shown = app.reveal_password;
            let text = if shown {
                app.unattended_display.clone().unwrap_or_default()
            } else {
                "•".repeat(8)
            };
            ui.label(theme::mono(text).size(16.0).strong());
            if ui
                .small_button(if shown { "🙈" } else { "👁" })
                .on_hover_text(if shown {
                    "Скрыть"
                } else {
                    "Показать"
                })
                .clicked()
            {
                app.reveal_password = !shown;
            }
            ui.label(theme::dot("доступ без подтверждения", theme::MUTED));
        });
    }

    // Agent start/stop — only where the service does not host already
    // (the service IS the agent; a duplicate would register twice).
    if !app.service_hosts {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if app.host.is_some() {
                if ui
                    .small_button("⏹ Остановить агента")
                    .on_hover_text("Остановить агент этого компьютера")
                    .clicked()
                {
                    app.host_stop_clicked();
                }
            } else if ui
                .small_button("▶ Запустить агента")
                .on_hover_text("Машина станет доступной по ID")
                .clicked()
            {
                app.host_start_clicked();
            }
            let send_file = app.host.as_ref().is_some_and(|h| h.session_active);
            if send_file
                && ui
                    .small_button("⬆ Файл зрителю")
                    .on_hover_text("Зритель получит файл в свою папку загрузок")
                    .clicked()
            {
                if let Some(path) = crate::pick_file() {
                    if let Some(tx) = app.host.as_ref().and_then(|h| h.commands.as_ref()) {
                        let _ = tx.send(opacusr_agent::session::HostCommand::SendFile(path));
                    }
                }
            }
        });
    }

    // The missing «что вводить на телефоне?» answer: this machine's LAN
    // address both as a direct ip:port target and as the embedded
    // signaling URL — a click copies either into the clipboard. Without
    // it the peer has to run ipconfig and the «host unavailable» error
    // on the phone stays unexplained.
    if let Some(ip) = theme::lan_ipv4() {
        let (direct, signaling) = peer_setup_hints(
            Some(&ip),
            &app.server_url,
            app.config.signal_listen_port,
            app.config.lan_port,
        );
        ui.add_space(6.0);
        if let Some(d) = &direct {
            ui.horizontal(|ui| {
                ui.label(theme::dot("Wi-Fi / LAN:", theme::MUTED).small());
                copy_chip(ui, app, d);
            });
        }
        if let Some(s) = &signaling {
            ui.horizontal(|ui| {
                ui.label(theme::dot("сигналинг:", theme::MUTED).small());
                copy_chip(ui, app, s);
            });
        }
        if !app.config.allow_lan {
            crate::hint(
                ui,
                "прямые сессии включаются: Настройки → Хост → «Прямые LAN-подключения»",
            );
        }
        crate::hint(
            ui,
            "адрес сервера вводится на другом устройстве: Настройки → Подключение",
        );
    }

    ui.add_space(6.0);
    crate::hint(ui, "Пароль доступа, 2FA и служба — в Настройках.");
}

/// A mono copy chip: click copies the address into the clipboard — the
/// phone keyboard gets it right, no manual retyping of digits.
fn copy_chip(ui: &mut Ui, app: &mut App, text: &str) {
    let resp = ui
        .add(
            egui::Button::new(theme::mono(text).size(13.0).color(theme::TEXT))
                .fill(theme::INSET)
                .stroke(egui::Stroke::new(1.0_f32, theme::CARD_STROKE))
                .rounding(egui::Rounding::same(8.0)),
        )
        .on_hover_text("Нажмите, чтобы скопировать адрес");
    if resp.clicked() {
        ui.output_mut(|o| o.copied_text = text.to_string());
        app.status_message = "Адрес скопирован в буфер обмена.".into();
        app.status_is_error = false;
    }
}

/// What the OTHER device should use to reach this machine, derived from
/// the LAN IP: (direct `ip:port` target, signaling URL to point the
/// peer's «Сигнальный сервер» setting at). When `server_url` is left at
/// the embedded default (loopback), peers must use this machine's LAN
/// address; a custom server_url is itself what both sides share.
fn peer_setup_hints(
    lan_ip: Option<&str>,
    server_url: &str,
    signal_port: u16,
    lan_port: u16,
) -> (Option<String>, Option<String>) {
    let direct = lan_ip.map(|ip| format!("{ip}:{lan_port}"));
    let loopback = server_url.is_empty()
        || server_url.contains("127.0.0.1")
        || server_url.contains("localhost");
    let signaling = if loopback {
        lan_ip.map(|ip| format!("ws://{ip}:{signal_port}/signal"))
    } else {
        Some(server_url.to_string())
    };
    (direct, signaling)
}

/// The «История подключений» card — the main body of the window: fills
/// the rest of the width and the full height, rows scroll, the caption
/// is pinned to the bottom. No «Очистить» button — the history is not
/// wiped from the UI.
fn history_card(ui: &mut Ui, app: &mut App) {
    theme::card(ui, |ui| {
        // Force vertical layout: the card is placed directly in a
        // horizontal parent on wide screens and would otherwise inherit
        // left-to-right layout (header and rows on one line).
        ui.vertical(|ui| {
            ui.set_min_width(ui.available_width());
            history_header(ui, app);
            ui.add_space(4.0);

            egui::TopBottomPanel::bottom("history_caption")
                .frame(egui::Frame::none())
                .show_inside(ui, |ui| {
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("последние 100 попыток подключений")
                            .weak()
                            .small(),
                    );
                    ui.add_space(2.0);
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show_inside(ui, |ui| {
                    if app.book.history().is_empty() {
                        empty_note(ui);
                    } else {
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                for h in app.book.history() {
                                    history_row(ui, h);
                                }
                            });
                    }
                });
        });
    });
}

/// Narrow-window variant: the rows render in the outer scrolling
/// column (no nested scroll areas), the caption follows the rows.
fn history_card_narrow(ui: &mut Ui, app: &mut App) {
    theme::card(ui, |ui| {
        ui.vertical(|ui| {
            history_header(ui, app);
            ui.add_space(4.0);
            if app.book.history().is_empty() {
                empty_note(ui);
            } else {
                for h in app.book.history() {
                    history_row(ui, h);
                }
            }
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(3.0);
            ui.label(
                egui::RichText::new("последние 100 попыток подключений")
                    .weak()
                    .small(),
            );
        });
    });
}

/// Section title + entry count.
fn history_header(ui: &mut Ui, app: &mut App) {
    ui.horizontal(|ui| {
        theme::section(ui, "История подключений");
        ui.label(
            egui::RichText::new(format!("{}", app.book.history().len()))
                .weak()
                .small()
                .color(theme::MUTED),
        );
    });
}

fn empty_note(ui: &mut Ui) {
    ui.label(egui::RichText::new("Истории подключений пока нет.").weak());
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("Каждая попытка подключения появится здесь автоматически.")
            .weak()
            .small()
            .color(theme::MUTED),
    );
}

/// One history row: time, device, result mark, duration (AnyDesk log).
fn history_row(ui: &mut Ui, h: &opacusr_client::addressbook::HistoryEntry) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(fmt_ts(h.ts)).weak().small().monospace());
        match &h.label {
            Some(label) => {
                ui.label(egui::RichText::new(label).strong());
                ui.label(theme::mono(h.target.clone()).weak().small());
            }
            None => {
                ui.label(theme::mono(h.target.clone()).small());
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(d) = h.duration_secs {
                ui.label(egui::RichText::new(fmt_duration(d)).weak().small());
            }
            let dot = match h.outcome {
                Outcome::Connected => theme::dot("●", theme::TEAL),
                Outcome::Failed => theme::dot("●", theme::DANGER),
                Outcome::Unknown => egui::RichText::new("●").weak(),
            };
            let tip = match h.outcome {
                Outcome::Connected => "подключено",
                Outcome::Failed => "не удалось",
                Outcome::Unknown => "в процессе",
            };
            ui.label(dot).on_hover_text(tip);
        });
    });
    ui.add_space(2.0);
}

/// 123456789 → «123 456 789» (TeamViewer-style digit grouping).
fn spaced_id(id: u64) -> String {
    let digits = id.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{peer_setup_hints, spaced_id};

    #[test]
    fn digit_grouping_matches_teamviewer() {
        assert_eq!(spaced_id(123456789), "123 456 789");
        assert_eq!(spaced_id(987654321), "987 654 321");
        assert_eq!(spaced_id(12345678), "12 345 678");
        assert_eq!(spaced_id(123), "123");
    }

    #[test]
    fn loopback_server_url_swaps_to_lan_addresses() {
        let (direct, signaling) = peer_setup_hints(
            Some("192.168.1.5"),
            "ws://127.0.0.1:8080/signal",
            8080,
            54000,
        );
        assert_eq!(direct.as_deref(), Some("192.168.1.5:54000"));
        assert_eq!(signaling.as_deref(), Some("ws://192.168.1.5:8080/signal"));
    }

    #[test]
    fn custom_server_url_is_shared_verbatim() {
        let (direct, signaling) = peer_setup_hints(
            Some("10.0.0.2"),
            "wss://relay.example.com/signal",
            8080,
            54000,
        );
        assert_eq!(direct.as_deref(), Some("10.0.0.2:54000"));
        assert_eq!(signaling.as_deref(), Some("wss://relay.example.com/signal"));
    }

    #[test]
    fn no_lan_ip_means_no_hints() {
        let (direct, signaling) = peer_setup_hints(None, "ws://127.0.0.1:8080/signal", 8080, 54000);
        assert!(direct.is_none());
        assert!(signaling.is_none());
    }
}
