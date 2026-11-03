//! The host part of the merged home screen (Rev-3 mockup): the «Этот
//! компьютер» card — run the agent service, show the assigned ID, manage
//! the unattended password / 2FA — plus the floating consent dialog for
//! incoming sessions.

use egui::{Context, Ui};

use opacusr_agent::config::Permission;
use opacusr_agent::service::SessionDecision;

use crate::app::App;
use crate::theme;

/// Floating consent dialog for an incoming session (mockup screen 2:
/// a popup over the desktop, centered). Drawn every frame while a
/// request is pending; visible even during a viewer session.
pub fn consent_window(ctx: &Context, app: &mut App) {
    if let Some(host) = &mut app.host {
        if let Some((_id, name, ans_tx)) = host.consent.take() {
            let mut decision: Option<SessionDecision> = None;
            egui::Window::new(theme::bold("Входящая сессия").strong())
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{name} хочет подключиться к этому компьютеру."
                        ))
                        .size(15.0),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Проверьте, что узнаёте этого пользователя, перед тем как разрешить.",
                        )
                        .weak()
                        .small(),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new("✔ Полный доступ").strong())
                                    .fill(theme::ACCENT)
                                    .min_size([130.0, 32.0].into()),
                            )
                            .clicked()
                        {
                            decision = Some(SessionDecision::Full);
                        }
                        if ui.button("👁 Только просмотр").clicked() {
                            decision = Some(SessionDecision::ViewOnly);
                        }
                        if ui
                            .button(egui::RichText::new("✖ Отклонить").color(theme::DANGER))
                            .clicked()
                        {
                            decision = Some(SessionDecision::Reject);
                        }
                    });
                });
            if let Some(d) = decision {
                let _ = ans_tx.send(d);
            } else {
                // Still open — put it back for the next frame.
                if let Some(h) = &mut app.host {
                    h.consent = Some((_id, name, ans_tx));
                }
            }
        }
    }
}

/// The «Этот компьютер» card: agent on/off, the big ID, unattended
/// password and 2FA; host-side policies live in a collapsing section.
pub fn host_card(ui: &mut Ui, app: &mut App) {
    theme::card(ui, |ui| {
        theme::section(ui, "Этот компьютер");
        ui.add_space(8.0);

        let running = app.host.is_some();

        // ---- hero row: ID / offline note + start-stop ----
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                if running {
                    if let Some(host) = &app.host {
                        match host.my_id {
                            Some(id) => {
                                ui.label(egui::RichText::new("Ваш OpacusR ID").weak().small());
                                ui.label(theme::mono(format!("{id}")).size(30.0).strong());
                            }
                            None => {
                                ui.label(egui::RichText::new("регистрация на сервере…").weak());
                            }
                        }
                    }
                } else {
                    ui.label(egui::RichText::new("Агент не запущен").weak().size(16.0));
                    ui.label(
                        "Запустите, чтобы другие устройства могли подключаться к этому компьютеру.",
                    );
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if let Some(h) = &app.host {
                        if h.session_active {
                            ui.label(theme::dot("● сессия активна", theme::TEAL));
                        }
                    }
                    if ui.button("⏹ Остановить").clicked() {
                        app.host_stop_clicked();
                    }
                } else if theme::accent_button(ui, "▶ Запустить").clicked() {
                    app.host_start_clicked();
                }
            });
        });

        // ---- active session: send a file to the viewer ----
        if let Some(h) = &app.host {
            if h.session_active {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let enabled = h.commands.is_some();
                    if ui
                        .add_enabled(enabled, egui::Button::new("⬆ Отправить файл зрителю"))
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            if let Some(tx) = app.host.as_ref().and_then(|h| h.commands.as_ref()) {
                                let _ =
                                    tx.send(opacusr_agent::session::HostCommand::SendFile(path));
                            }
                        }
                    }
                    crate::hint(ui, "зритель получит файл в свою папку загрузок");
                });
            }
        }

        ui.add_space(8.0);

        // ---- access rows ----
        egui::Grid::new("host_access_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Пароль без присмотра").strong());
                ui.horizontal(|ui| {
                    if app.config.is_unattended() {
                        let storage = if app.config.password_in_keychain {
                            "хранится в связке ключей ОС"
                        } else {
                            "хранится в конфиге (0600)"
                        };
                        ui.label(theme::dot(format!("● задан ({storage})"), theme::TEAL));
                        if ui.small_button("Убрать").clicked() {
                            let _ = opacusr_agent::keychain::delete_password();
                            app.config.password = None;
                            app.config.password_in_keychain = false;
                            match app.config.save() {
                                Ok(p) => {
                                    app.status_message =
                                        format!("Пароль убран (сохранено {}).", p.display())
                                }
                                Err(e) => app.status_message = format!("Не сохранить: {e}"),
                            }
                        }
                    } else {
                        let resp = ui.add_sized(
                            [220.0, 24.0],
                            egui::TextEdit::singleline(&mut app.host_password_input)
                                .password(true)
                                .hint_text("минимум 4 символа"),
                        );
                        if (ui.button("Задать").clicked() || resp.lost_focus())
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            app.save_host_password();
                        }
                        crate::hint(ui, "включает доступ без подтверждения вручную");
                    }
                });
                ui.end_row();

                ui.label(egui::RichText::new("Двухфакторная (TOTP)").strong());
                ui.horizontal(|ui| {
                    if let Some(secret) = app.config.totp_secret.clone() {
                        ui.label(theme::dot("● включена", theme::TEAL));
                        if ui.small_button("Копировать ключ").clicked() {
                            ui.output_mut(|o| o.copied_text = secret.clone());
                            app.status_message =
                                "Base32-ключ скопирован — добавьте его в приложение-аутентификатор.".into();
                        }
                        if ui.small_button("Отключить").clicked() {
                            app.config.totp_secret = None;
                            match app.config.save() {
                                Ok(p) => {
                                    app.status_message =
                                        format!("2FA отключена (сохранено {}).", p.display())
                                }
                                Err(e) => app.status_message = format!("Не сохранить: {e}"),
                            }
                        }
                    } else {
                        if ui.button("Включить 2FA…").clicked() {
                            let key = opacusr_crypto::totp::generate_secret();
                            ui.output_mut(|o| o.copied_text = key.clone());
                            app.config.totp_secret = Some(key.clone());
                            match app.config.save() {
                                Ok(p) => {
                                    app.status_message = format!(
                                        "2FA включена (сохранено {}). Ключ скопирован в буфер — \
                                         добавьте его в аутентификатор \
                                         (otpauth://totp/OpacusR?secret={key}&issuer=OpacusR).",
                                        p.display()
                                    );
                                }
                                Err(e) => {
                                    app.config.totp_secret = None;
                                    app.status_message = format!("Не сохранить: {e}");
                                }
                            }
                        }
                        ui.label(
                            egui::RichText::new("зрители должны будут вводить 6-значный код")
                                .weak()
                                .small(),
                        );
                    }
                });
                ui.end_row();
            });

        // ---- host policies (secondary, per the mockup) ----
        ui.add_space(4.0);
        egui::CollapsingHeader::new(egui::RichText::new("Настройки хоста").color(theme::MUTED))
            .default_open(false)
            .show(ui, |ui| {
                egui::Grid::new("host_policy_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Запись сессий");
                        ui.horizontal(|ui| {
                            let before = app.config.record;
                            if ui
                                .checkbox(&mut app.config.record, "записывать MP4-видео")
                                .changed()
                            {
                                match app.config.save() {
                                    Ok(p) => {
                                        app.status_message = if app.config.record {
                                            format!(
                                                "Запись сессий включена ({}).",
                                                app.config.record_dir.display()
                                            )
                                        } else {
                                            format!("Запись сессий отключена (сохранено {}).", p.display())
                                        };
                                    }
                                    Err(e) => {
                                        app.config.record = before;
                                        app.status_message = format!("Не сохранить: {e}");
                                    }
                                }
                            }
                            if app.config.record {
                                ui.label(
                                    egui::RichText::new(
                                        app.config.record_dir.display().to_string(),
                                    )
                                    .weak()
                                    .small(),
                                );
                            }
                        });
                        ui.end_row();

                        ui.label("Папка загрузок");
                        ui.label(app.config.download_dir.display().to_string());
                        ui.end_row();

                        ui.label("Стриминг");
                        ui.horizontal(|ui| {
                            ui.add(egui::Slider::new(&mut app.fps, 1.0..=60.0).text("fps"));
                            ui.add(
                                egui::Slider::new(&mut app.quality, 20.0..=95.0).text("качество"),
                            );
                            ui.add(
                                egui::Slider::new(&mut app.scale, 25.0..=100.0).text("масштаб %"),
                            );
                            egui::ComboBox::from_id_salt("codec")
                                .selected_text(app.config.codec.to_ascii_uppercase())
                                .width(90.0)
                                .show_ui(ui, |ui| {
                                    for opt in ["auto", "h264", "jpeg"] {
                                        ui.selectable_value(
                                            &mut app.config.codec,
                                            opt.to_string(),
                                            opt,
                                        );
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("Буфер обмена");
                        ui.checkbox(
                            &mut app.config.clipboard_sync,
                            "делиться буфером с подключёнными зрителями",
                        );
                        ui.end_row();

                        ui.label("Адаптивное качество");
                        ui.checkbox(
                            &mut app.config.adaptive_quality,
                            "подстраивать fps/качество/масштаб под канал",
                        );
                        ui.end_row();

                        ui.label("Доступ по умолчанию");
                        ui.horizontal(|ui| {
                            let mut perm = Permission::parse(&app.config.default_permission);
                            egui::ComboBox::from_id_salt("default_perm")
                                .selected_text(match perm {
                                    Permission::Full => "полный контроль".to_string(),
                                    Permission::ViewOnly => "только просмотр".to_string(),
                                    Permission::Deny => "запретить".to_string(),
                                })
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut perm, Permission::Full, "полный контроль");
                                    ui.selectable_value(&mut perm, Permission::ViewOnly, "только просмотр");
                                    ui.selectable_value(&mut perm, Permission::Deny, "запретить");
                                });
                            if perm.as_str() != app.config.default_permission {
                                app.config.default_permission = perm.as_str().to_string();
                            }
                            crate::hint(ui, "для подключений без присмотра, не попавших в ACL");
                        });
                        ui.end_row();

                        ui.label("Звук");
                        ui.checkbox(
                            &mut app.config.audio,
                            "передавать звук подключённым зрителям",
                        );
                        ui.end_row();

                        ui.label("Блокировка ввода");
                        ui.checkbox(
                            &mut app.config.lock_input,
                            "блокировать локальные клавиатуру/мышь во время управления",
                        );
                        ui.end_row();
                    });
                crate::hint(
                    ui,
                    "как в TeamViewer: удалённый ввод проходит, локальный подавляется. \
                     На Windows Ctrl+Alt+Del остаётся доступным. Сессии «только просмотр» не блокируют.",
                );
            });

        // ---- activity log ----
        egui::CollapsingHeader::new(egui::RichText::new("Активность").color(theme::MUTED))
            .default_open(false)
            .show(ui, |ui| {
                if let Some(host) = &app.host {
                    if host.notices.is_empty() {
                        ui.label(egui::RichText::new("пока событий нет").weak());
                    }
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for line in host.notices.iter().rev() {
                                ui.label(format!("· {line}"));
                            }
                        });
                } else {
                    crate::hint(
                        ui,
                        "Без присмотра: сессии принимаются по правильному паролю, без ручного подтверждения.\n\
                         Очно (без пароля): каждое входящее подключение показывает диалог «Разрешить/Отклонить».",
                    );
                }
            });
    });
}
