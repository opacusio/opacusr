//! TeamViewer-style home screen: one compact card pair — «Разрешить
//! управление этим компьютером» (host: ID + password) and «Управлять
//! удалённым компьютером» (viewer: partner ID + connect). Every option
//! lives in the settings window (`crate::settings`); this screen shows
//! only what TeamViewer shows.

use egui::{Color32, Ui};

use crate::app::App;
use crate::theme;

/// The TeamViewer-like home layout: two cards side by side, centered,
/// with the encryption footnote at the bottom.
pub fn home_ui(ctx: &egui::Context, app: &mut App) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            let w = ui.available_width();
            ui.add_space(((w - 860.0) / 2.0).max(0.0));
            ui.vertical(|ui| {
                ui.set_max_width(860.0_f32.min(w));
                ui.columns(2, |cols| {
                    host_card(&mut cols[0], app);
                    viewer_card(&mut cols[1], app);
                });
                ui.add_space(14.0);
                ui.vertical_centered(|ui| {
                    crate::hint(
                        ui,
                        "Трафик шифруется сквозно (Argon2id + ChaCha20-Poly1305) поверх \
                         WebRTC/DTLS; при заблокированном P2P — TCP-ретранслятор.",
                    );
                });
            });
        });
    });
}

/// Разрешить управление этим компьютером — the left TeamViewer card:
/// the big ID (click to copy), the access password and the agent
/// start/stop control.
fn host_card(ui: &mut Ui, app: &mut App) {
    let running = app.host.is_some();
    theme::card(ui, |ui| {
        // Match the viewer card's height so the pair looks even.
        ui.set_min_height(256.0);
        ui.add_space(2.0);
        theme::section(ui, "Разрешить управление этим компьютером");
        ui.add_space(12.0);

        // ---- ID + status (TeamViewer hero block) ----
        ui.vertical_centered(|ui| {
            if running {
                match app.host.as_ref().and_then(|h| h.my_id) {
                    Some(id) => {
                        ui.label(egui::RichText::new("Ваш ID").weak().small());
                        let resp = ui
                            .add(
                                egui::Button::new(
                                    theme::mono(id.to_string()).size(34.0).color(theme::TEXT),
                                )
                                .fill(theme::CARD)
                                .stroke(egui::Stroke::new(1.0_f32, theme::CARD_STROKE))
                                .rounding(egui::Rounding::same(10.0))
                                .min_size([220.0, 46.0].into()),
                            )
                            .on_hover_text("Нажмите, чтобы скопировать ID");
                        if resp.clicked() {
                            ui.output_mut(|o| o.copied_text = id.to_string());
                            app.status_message = "ID скопирован в буфер обмена.".into();
                        }
                    }
                    None => {
                        ui.label(
                            egui::RichText::new("регистрация на сервере…")
                                .weak()
                                .size(18.0),
                        );
                    }
                }
            } else {
                ui.label(egui::RichText::new("Агент не запущен").weak().size(18.0));
                ui.label(
                    egui::RichText::new(
                        "Запустите — другие устройства смогут подключаться к этому ПК.",
                    )
                    .weak()
                    .small(),
                );
            }
        });
        ui.add_space(10.0);

        // ---- password block ----
        password_block(ui, app);
        ui.add_space(10.0);

        // ---- status + start/stop ----
        ui.horizontal(|ui| {
            if running {
                let session_active = app.host.as_ref().is_some_and(|h| h.session_active);
                if session_active {
                    ui.label(theme::dot("● сессия активна", theme::TEAL));
                } else if app.host.as_ref().is_some_and(|h| h.my_id.is_some()) {
                    ui.label(theme::dot("● готов к подключениям", theme::TEAL));
                } else {
                    ui.label(theme::dot("● подключение к серверу…", theme::GOLD));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("⏹ Остановить")
                        .on_hover_text("Остановить агент этого компьютера")
                        .clicked()
                    {
                        app.host_stop_clicked();
                    }
                });
            } else {
                ui.label(theme::dot("● офлайн", theme::MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::accent_button(ui, "▶ Запустить агент").clicked() {
                        app.host_start_clicked();
                    }
                });
            }
        });

        // ---- active session: push a file to the viewer ----
        if running && app.host.as_ref().is_some_and(|h| h.session_active) {
            ui.add_space(8.0);
            if ui
                .button("⬆ Отправить файл зрителю")
                .on_hover_text("Зритель получит файл в свою папку загрузок")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    if let Some(tx) = app.host.as_ref().and_then(|h| h.commands.as_ref()) {
                        let _ = tx.send(opacusr_agent::session::HostCommand::SendFile(path));
                    }
                }
            }
        }
    });
}

/// The access password, exactly as visible as TeamViewer's «Пароль»:
/// dots + reveal toggle when an unattended password is set, a quiet
/// inline form otherwise (attended mode confirms every session by hand).
fn password_block(ui: &mut Ui, app: &mut App) {
    ui.separator();
    ui.add_space(8.0);
    if app.config.is_unattended() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Пароль доступа").weak().small());
            let shown = app.reveal_password;
            let text = if shown {
                app.unattended_display.clone().unwrap_or_default()
            } else {
                "•".repeat(8)
            };
            ui.label(theme::mono(text).size(20.0).strong());
            if ui
                .button(if shown { "🙈" } else { "👁" })
                .on_hover_text(if shown {
                    "Скрыть"
                } else {
                    "Показать"
                })
                .clicked()
            {
                app.reveal_password = !shown;
            }
        });
        ui.horizontal(|ui| {
            if ui
                .button("Сменить")
                .on_hover_text("Задать другой пароль доступа")
                .clicked()
            {
                app.password_editing = true;
                app.host_password_input.clear();
            }
            if ui
                .button("Убрать")
                .on_hover_text("Вернуть ручное подтверждение каждого подключения")
                .clicked()
            {
                let _ = opacusr_agent::keychain::delete_password();
                app.config.password = None;
                app.config.password_in_keychain = false;
                app.unattended_display = None;
                match app.config.save() {
                    Ok(p) => {
                        app.status_message = format!("Пароль убран (сохранено {}).", p.display())
                    }
                    Err(e) => app.status_message = format!("Не сохранить: {e}"),
                }
            }
            ui.label(
                egui::RichText::new("доступ без подтверждения")
                    .weak()
                    .small(),
            );
        });
        // Inline «Сменить» editor.
        if app.password_editing {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let resp = ui.add_sized(
                    [150.0, 26.0],
                    egui::TextEdit::singleline(&mut app.host_password_input)
                        .password(true)
                        .hint_text("новый пароль (мин. 4)"),
                );
                if ui.button("✔").on_hover_text("Сохранить").clicked()
                    || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    app.save_host_password();
                    app.refresh_unattended_display();
                    app.password_editing = false;
                }
                if ui.button("✖").on_hover_text("Отмена").clicked() {
                    app.password_editing = false;
                    app.host_password_input.clear();
                }
            });
        }
    } else {
        ui.label(
            egui::RichText::new("Каждое подключение подтверждается вручную.")
                .weak()
                .small(),
        );
        ui.horizontal(|ui| {
            let resp = ui.add_sized(
                [150.0, 26.0],
                egui::TextEdit::singleline(&mut app.host_password_input)
                    .password(true)
                    .hint_text("пароль доступа (мин. 4)"),
            );
            if ui
                .button("Задать")
                .on_hover_text("Включить доступ без ручного подтверждения")
                .clicked()
                || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                app.save_host_password();
                app.refresh_unattended_display();
            }
        });
    }
}

/// Управлять удалённым компьютером — the right TeamViewer card: partner
/// ID, password, the violet Connect CTA. The 2FA code is the one extra
/// field (revealed on demand); everything else is in the settings window.
fn viewer_card(ui: &mut Ui, app: &mut App) {
    theme::card(ui, |ui| {
        ui.add_space(2.0);
        theme::section(ui, "Управлять удалённым компьютером");
        ui.add_space(12.0);

        ui.label(egui::RichText::new("ID партнёра").weak().small());
        let id_resp = ui.add_sized(
            [ui.available_width(), 40.0],
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
        ui.add_space(10.0);

        // Connect: violet CTA, full width, TeamViewer-style.
        let connect_w = ui.available_width();
        let connect_clicked = ui
            .add_sized(
                [connect_w, 40.0],
                egui::Button::new(theme::bold("Подключиться").size(17.0).color(Color32::WHITE))
                    .fill(theme::ACCENT)
                    .rounding(egui::Rounding::same(10.0)),
            )
            .clicked();
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if connect_clicked || (enter && (id_resp.lost_focus() || pw_resp.lost_focus())) {
            app.connect_clicked(None);
        }

        ui.add_space(8.0);
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

        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(&app.status_message).weak().small());
        });
    });
}
