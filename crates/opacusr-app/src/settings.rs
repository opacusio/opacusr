//! The settings window, hidden away from the home screen the same way
//! TeamViewer hides «Дополнительно» behind the gear icon: a separate
//! window with a left navigation column and one section per pane.
//! Everything still applies immediately (no OK/Cancel round trip).

use egui::{Context, Ui};

use opacusr_agent::config::Permission;

use crate::app::App;
use crate::theme;

/// Left-nav sections of the settings window (TeamViewer «Options» order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Connect,
    Security,
    Host,
    P2p,
    About,
}

impl SettingsTab {
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Connect,
        Self::Security,
        Self::Host,
        Self::P2p,
        Self::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::General => "Общие",
            Self::Connect => "Подключение",
            Self::Security => "Безопасность",
            Self::Host => "Хост",
            Self::P2p => "Децентрализованный",
            Self::About => "О программе",
        }
    }
}

/// The «Настройки» drawer (TeamViewer options) — docked to the right
/// edge, full height, resizable. One tool drawer is open at a time;
/// Esc or the «✕» button closes it.
pub fn settings_panel(ctx: &Context, app: &mut App) {
    if !app.show_settings {
        return;
    }
    egui::SidePanel::right("settings_drawer")
        .resizable(true)
        .width_range(520.0..=820.0)
        .default_width(640.0)
        .frame(theme::drawer_frame())
        .show(ctx, |ui| {
            if theme::drawer_header(ui, "Настройки") {
                app.show_settings = false;
            }
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                // ---- left nav (TeamViewer options list) ----
                let tab = app.settings_nav;
                egui::Frame::default()
                    .fill(theme::INSET)
                    .rounding(egui::Rounding::same(10.0))
                    .inner_margin(egui::Margin::same(6.0))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.set_min_width(140.0);
                            for t in SettingsTab::ALL {
                                let selected = t == tab;
                                if ui
                                    .add(egui::SelectableLabel::new(
                                        selected,
                                        egui::RichText::new(t.title()).color(if selected {
                                            theme::TEXT
                                        } else {
                                            theme::MUTED
                                        }),
                                    ))
                                    .clicked()
                                {
                                    app.settings_nav = t;
                                }
                            }
                        });
                    });
                ui.separator();
                // ---- right pane ----
                ui.vertical(|ui| {
                    ui.set_min_width(ui.available_width() - 8.0);
                    match app.settings_nav {
                        SettingsTab::General => general_tab(ui, app),
                        SettingsTab::Connect => connect_tab(ui, app),
                        SettingsTab::Security => security_tab(ui, app),
                        SettingsTab::Host => host_tab(ui, app),
                        SettingsTab::P2p => p2p_tab(ui, app),
                        SettingsTab::About => about_tab(ui),
                    }
                });
            });
        });
}

fn general_tab(ui: &mut Ui, app: &mut App) {
    ui.label(theme::bold("Общие").size(18.0));
    ui.add_space(8.0);
    egui::Grid::new("set_general")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Имя в сети");
            ui.vertical(|ui| {
                ui.add_sized(
                    [360.0, 24.0],
                    egui::TextEdit::singleline(&mut app.display_name)
                        .hint_text(theme::machine_name()),
                );
                crate::hint(
                    ui,
                    "как этот компьютер видят партнёры; по умолчанию — имя машины",
                );
            });
            ui.end_row();

            ui.label("Папка загрузок");
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(app.config.download_dir.display().to_string())
                            .weak()
                            .small(),
                    );
                    if ui.button("Выбрать…").clicked() {
                        if let Some(dir) = crate::pick_folder() {
                            app.config.download_dir = dir;
                            match app.config.save() {
                                Ok(p) => {
                                    app.status_message =
                                        format!("Папка сохранена ({}).", p.display())
                                }
                                Err(e) => app.status_message = format!("Не сохранить: {e}"),
                            }
                        }
                    }
                });
                crate::hint(ui, "куда сохраняются файлы, присланные удалённой стороной");
            });
            ui.end_row();
        });
}

fn connect_tab(ui: &mut Ui, app: &mut App) {
    ui.label(theme::bold("Подключение").size(18.0));
    ui.add_space(8.0);
    egui::Grid::new("set_connect")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Сигнальный сервер");
            ui.vertical(|ui| {
                let url_resp = ui.add_sized(
                    [360.0, 24.0],
                    egui::TextEdit::singleline(&mut app.server_url)
                        .hint_text("ws://IP_компьютера:8080/signal — см. подсказку"),
                );
                // Persist on change: before 0.11.13 this field lived only
                // in memory and silently reverted to the loopback default
                // after a restart — on a phone that read as «host
                // unavailable» again and again.
                if url_resp.changed() {
                    app.config.server_url = app.server_url.clone();
                    let _ = app.config.save();
                }
                crate::hint(
                    ui,
                    "должен быть ОДИН на телефон и компьютер; по умолчанию каждое устройство держит свой локальный сервер и видит только само себя. В одной Wi-Fi сети: ws://IP_компьютера:8080/signal — адрес показан на компьютере в карточке «Этот компьютер»",
                );
            });
            ui.end_row();

            ui.label("Прокси");
            ui.vertical(|ui| {
                ui.add_sized(
                    [360.0, 24.0],
                    egui::TextEdit::singleline(&mut app.config.proxy).hint_text(
                        "socks5://user:pass@host:port или http://host:port — пусто = напрямую",
                    ),
                );
                crate::hint(
                    ui,
                    "маршрутизирует сигналинг и ретранслятор; ICE/UDP идёт напрямую",
                );
            });
            ui.end_row();

            ui.label("Транспорт");
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut app.transport_choice,
                        opacusr_client::session::TransportChoice::Auto,
                        "Авто (WebRTC → ретранслятор)",
                    );
                    ui.selectable_value(
                        &mut app.transport_choice,
                        opacusr_client::session::TransportChoice::WebRtcOnly,
                        "WebRTC",
                    );
                    ui.selectable_value(
                        &mut app.transport_choice,
                        opacusr_client::session::TransportChoice::RelayOnly,
                        "TCP-ретранслятор",
                    );
                });
                crate::hint(ui, "«Авто» пробует P2P и откатывается на ретранслятор");
            });
            ui.end_row();
        });
}

fn security_tab(ui: &mut Ui, app: &mut App) {
    ui.label(theme::bold("Безопасность").size(18.0));
    ui.add_space(8.0);
    egui::Grid::new("set_security")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Пароль доступа");
            ui.vertical(|ui| {
                if app.config.is_unattended() {
                    let storage = if app.config.password_in_keychain {
                        "хранится в связке ключей ОС"
                    } else {
                        "хранится в конфиге (0600)"
                    };
                    ui.horizontal(|ui| {
                        ui.label(theme::dot(format!("● задан ({storage})"), theme::TEAL));
                        if ui.small_button("Убрать").clicked() {
                            let _ = opacusr_agent::keychain::delete_password();
                            app.config.password = None;
                            app.config.password_in_keychain = false;
                            app.unattended_display = None;
                            match app.config.save() {
                                Ok(p) => {
                                    app.status_message =
                                        format!("Пароль убран (сохранено {}).", p.display())
                                }
                                Err(e) => app.status_message = format!("Не сохранить: {e}"),
                            }
                        }
                    });
                    crate::hint(
                        ui,
                        "подключающийся вводит этот пароль — вручную подтверждать сессию не нужно",
                    );
                } else {
                    ui.horizontal(|ui| {
                        let resp = ui.add_sized(
                            [180.0, 24.0],
                            egui::TextEdit::singleline(&mut app.host_password_input)
                                .password(true)
                                .hint_text("минимум 4 символа"),
                        );
                        if ui.button("Задать").clicked()
                            || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            app.save_host_password();
                            app.refresh_unattended_display();
                        }
                    });
                    crate::hint(
                        ui,
                        "пароль будет запрашиваться у подключающихся — вручную подтверждать сессию не нужно; хранится в связке ключей ОС (или в конфиге 0600, если связки нет)",
                    );
                }
            });
            ui.end_row();

            ui.label("Двухфакторная (TOTP)");
            ui.vertical(|ui| {
                if let Some(secret) = app.config.totp_secret.clone() {
                    ui.horizontal(|ui| {
                        ui.label(theme::dot("● включена", theme::TEAL));
                        if ui.small_button("Копировать ключ").clicked() {
                            ui.output_mut(|o| o.copied_text = secret.clone());
                            app.status_message =
                                "Base32-ключ скопирован — добавьте его в приложение-аутентификатор."
                                    .into();
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
                    });
                } else if ui.button("Включить 2FA…").clicked() {
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
                crate::hint(ui, "зрители должны будут вводить 6-значный код");
            });
            ui.end_row();

            ui.label("Доступ по умолчанию");
            ui.vertical(|ui| {
                let mut perm = Permission::parse(&app.config.default_permission);
                egui::ComboBox::from_id_salt("default_perm")
                    .selected_text(match perm {
                        Permission::Full => "полный контроль",
                        Permission::ViewOnly => "только просмотр",
                        Permission::Deny => "запретить",
                    })
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut perm, Permission::Full, "полный контроль");
                        ui.selectable_value(&mut perm, Permission::ViewOnly, "только просмотр");
                        ui.selectable_value(&mut perm, Permission::Deny, "запретить");
                    });
                if perm.as_str() != app.config.default_permission {
                    app.config.default_permission = perm.as_str().to_string();
                    let _ = app.config.save();
                }
                crate::hint(ui, "для подключений без присмотра, не попавших в ACL");
            });
            ui.end_row();

            ui.label("Запись сессий");
            ui.vertical(|ui| {
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
                        egui::RichText::new(app.config.record_dir.display().to_string())
                            .weak()
                            .small(),
                    );
                }
            });
            ui.end_row();

            ui.label("Блокировка ввода");
            ui.vertical(|ui| {
                ui.checkbox(
                    &mut app.config.lock_input,
                    "блокировать локальные клавиатуру/мышь во время управления",
                );
                crate::hint(
                    ui,
                    "как в TeamViewer: удалённый ввод проходит, локальный подавляется. \
                     На Windows Ctrl+Alt+Del остаётся доступным. Сессии «только просмотр» \
                     не блокируют.",
                );
            });
            ui.end_row();

            ui.label("Буфер обмена");
            ui.checkbox(
                &mut app.config.clipboard_sync,
                "делиться буфером с подключёнными зрителями",
            );
            ui.end_row();
        });
}

fn host_tab(ui: &mut Ui, app: &mut App) {
    ui.label(theme::bold("Хост").size(18.0));
    ui.add_space(8.0);
    egui::Grid::new("set_host")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Стриминг");
            ui.vertical(|ui| {
                ui.add(egui::Slider::new(&mut app.fps, 1.0..=60.0).text("fps"));
                ui.add(egui::Slider::new(&mut app.quality, 20.0..=95.0).text("качество"));
                ui.add(egui::Slider::new(&mut app.scale, 25.0..=100.0).text("масштаб %"));
            });
            ui.end_row();

            ui.label("Кодек");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("codec")
                    .selected_text(app.config.codec.to_ascii_uppercase())
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for opt in ["auto", "h264", "jpeg"] {
                            ui.selectable_value(&mut app.config.codec, opt.to_string(), opt);
                        }
                    });
                crate::hint(ui, "auto = аппаратный H.264 при возможности, иначе JPEG");
            });
            ui.end_row();

            ui.label("Адаптивное качество");
            ui.checkbox(
                &mut app.config.adaptive_quality,
                "подстраивать fps/качество/масштаб под канал",
            );
            ui.end_row();

            ui.label("Звук");
            ui.checkbox(
                &mut app.config.audio,
                "передавать звук подключённым зрителям",
            );
            ui.end_row();

            ui.label("Автозапуск");
            ui.vertical(|ui| {
                if ui
                    .checkbox(
                        &mut app.config.start_agent_automatically,
                        "запускать агента вместе с OpacusR (машину видно сразу)",
                    )
                    .changed()
                {
                    let _ = app.config.save();
                }
                crate::hint(
                    ui,
                    "агент работает, пока открыт OpacusR; чтобы машина была доступна всегда, \
                     установите службу ниже",
                );
            });
            ui.end_row();

            ui.label("LAN");
            ui.vertical(|ui| {
                if ui
                    .checkbox(
                        &mut app.config.allow_lan,
                        "прямые подключения по локальной сети (без сервера)",
                    )
                    .changed()
                {
                    let _ = app.config.save();
                }
                crate::hint(
                    ui,
                    &format!(
                        "телефон или компьютер в той же Wi-Fi сети подключается по адресу \
                         этого компьютера: в поле ID вводится ip:{} вместо девятизначного ID; \
                         signaling-сервер и интернет не нужны; вступает в силу после \
                         перезапуска агента",
                        app.config.lan_port
                    ),
                );
            });
            ui.end_row();

            // Tray close + service rows only where the tray exists.
            #[cfg(any(windows, target_os = "macos"))]
            {
                ui.label("Окно");
                ui.vertical(|ui| {
                    if ui
                        .checkbox(
                            &mut app.config.close_to_tray,
                            "при закрытии окна сворачивать в трей (работа в фоне)",
                        )
                        .changed()
                    {
                        let _ = app.config.save();
                    }
                    crate::hint(
                        ui,
                        "по умолчанию закрытие окна сворачивает OpacusR в трей — агент и приём \
                         подключений продолжают работать (выход — через иконку в трее); \
                         выключите, чтобы крестик полностью завершал приложение",
                    );
                });
                ui.end_row();

                #[cfg(windows)]
                {
                    ui.label("Служба Windows");
                    ui.vertical(|ui| host_service_ui(ui, app));
                    ui.end_row();
                }
            }
        });

    ui.add_space(10.0);
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
                    "Журнал появится, когда запущен агент этого компьютера. \
                     Без присмотра: сессии принимаются по правильному паролю. \
                     Очно: каждое подключение подтверждается вручную.",
                );
            }
        });
}

/// The «Служба Windows» row (Настройки → Хост): live SCM state plus a
/// one-click elevated install for zip installs (the MSI installs the
/// service itself). Status text mirrors what `services.msc` shows.
#[cfg(windows)]
fn host_service_ui(ui: &mut Ui, app: &mut App) {
    use crate::service_ctl::SvcState;

    let state = app.svc_state_cached();
    let (dot, text, color) = match state {
        SvcState::Running => (
            "●",
            "служба работает — агент стартует при входе в систему без участия OpacusR",
            theme::TEAL,
        ),
        SvcState::StartPending => ("◐", "служба запускается…", theme::MUTED),
        SvcState::StopPending => ("◐", "служба останавливается…", theme::MUTED),
        SvcState::Stopped => (
            "○",
            "служба установлена, но остановлена (запустится при следующей загрузке Windows)",
            theme::GOLD,
        ),
        SvcState::Other => ("○", "служба в нестандартном состоянии", theme::GOLD),
        SvcState::NotInstalled => (
            "○",
            "служба не установлена — машина доступна, пока открыт OpacusR",
            theme::MUTED,
        ),
    };
    ui.label(egui::RichText::new(format!("{dot} {text}")).color(color));

    if state == SvcState::NotInstalled && ui.button("Установить службу (автозапуск)").clicked()
    {
        match crate::service_ctl::install_elevated() {
            Ok(()) => {
                app.status_message = "Служба OpacusRAgent установлена и запущена.".into();
                app.svc_state_refresh();
            }
            Err(e) => {
                app.status_message = format!("Служба не установлена: {e}");
                app.svc_state_refresh();
            }
        }
    }
    if state != SvcState::NotInstalled {
        crate::hint(
            ui,
            "службу устанавливает MSI или кнопка выше; удалить — «opacusr-agent service uninstall» \
             от Администратора",
        );
    } else {
        crate::hint(
            ui,
            "потребуется подтверждение UAC; для подключений без присутствия заранее задайте \
             пароль доступа (Настройки → Безопасность)",
        );
    }
}

fn about_tab(ui: &mut Ui) {
    ui.label(theme::bold("О программе").size(18.0));
    ui.add_space(8.0);
    ui.label(format!("OpacusR v{}", env!("CARGO_PKG_VERSION")));
    ui.label(
        egui::RichText::new(
            "Самостоятельно размещаемый удалённый рабочий стол с открытым кодом \
             (альтернатива TeamViewer).",
        )
        .weak(),
    );
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);
    crate::hint(
        ui,
        "Шифрование: Argon2id (KDF) + ChaCha20-Poly1305 поверх WebRTC/DTLS; \
         при заблокированном P2P — TCP-ретранслятор.",
    );
    crate::hint(
        ui,
        "Лицензия: AGPL-3.0-or-later. github.com/opacusio/opacusr",
    );
}

/// Настройки → Децентрализованный (эксперимент): embedded rust-libp2p
/// node — connect-by-PeerId without a signaling server.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn p2p_tab(ui: &mut Ui, app: &mut App) {
    ui.label(theme::bold("Децентрализованный (эксперимент)").size(18.0));
    ui.add_space(8.0);
    crate::hint(
        ui,
        "подключение по PeerId без сигнального сервера: rust-libp2p — Kademlia DHT + AutoNAT + \
         Relay v2 + DCUtR. Сейчас по каналу идёт проверка связи (пинг); полноценные сессии — \
         следующий шаг.",
    );
    ui.add_space(4.0);

    let node_up = app.p2p.as_ref().is_some_and(|p| p.handle.is_some());
    // Strings cloned out of the node state up front — the closures below
    // need `&mut App` and cannot hold borrows into `app.p2p`.
    let (peer_id, invite, reach, status, ping_status) = match app.p2p.as_ref() {
        Some(p) => (
            p.peer_id.clone(),
            p.invite_addr.clone(),
            p.reachability.clone(),
            p.status.clone(),
            p.ping_status.clone(),
        ),
        None => (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ),
    };

    egui::Grid::new("set_p2p")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Узел");
            ui.vertical(|ui| {
                let before = app.config.p2p_enabled;
                if ui
                    .checkbox(
                        &mut app.config.p2p_enabled,
                        "запускать узел вместе с OpacusR",
                    )
                    .changed()
                {
                    match app.config.save() {
                        Ok(_) => {
                            if app.config.p2p_enabled {
                                crate::p2p::start_node(app);
                            } else {
                                crate::p2p::stop_node(app);
                            }
                        }
                        Err(e) => {
                            app.config.p2p_enabled = before;
                            app.status_message = format!("Не сохранить: {e}");
                        }
                    }
                }
                ui.label(
                    egui::RichText::new(if node_up {
                        "● работает"
                    } else {
                        "○ остановлен"
                    })
                    .color(if node_up { theme::TEAL } else { theme::MUTED })
                    .small(),
                );
            });
            ui.end_row();

            if node_up && !peer_id.is_empty() {
                ui.label("Мой PeerId");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&peer_id).small().strong());
                        if ui.small_button("Копировать").clicked() {
                            ui.output_mut(|o| o.copied_text = peer_id.clone());
                            app.status_message = "PeerId скопирован.".into();
                        }
                    });
                    crate::hint(
                        ui,
                        "адрес этого устройства в децентрализованном режиме — партнёр вставляет \
                         его в «Проверка связи» на своём компьютере",
                    );
                });
                ui.end_row();

                if !invite.is_empty() {
                    ui.label("Пригласительный адрес");
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&invite).small().weak());
                            if ui.small_button("Копировать").clicked() {
                                ui.output_mut(|o| o.copied_text = invite.clone());
                                app.status_message = "Адрес скопирован.".into();
                            }
                        });
                        crate::hint(
                            ui,
                            "вставьте его в список bootstrap на другой машине этой же сети — она \
                             найдёт эту машину без сигнального сервера",
                        );
                    });
                    ui.end_row();
                }
            }

            ui.label("Bootstrap-узлы");
            ui.vertical(|ui| {
                ui.add_sized(
                    [360.0, 72.0],
                    egui::TextEdit::multiline(&mut app.p2p_bootstrap_edit).hint_text(
                        "по одному multiaddr в строке:\n\
                         /ip4/хост/udp/4001/quic-v1/p2p/12D3KooW…",
                    ),
                );
                if ui.button("Сохранить и перезапустить узел").clicked()
                {
                    let parsed: Vec<String> = app
                        .p2p_bootstrap_edit
                        .lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect();
                    let valid: Vec<String> = parsed
                        .iter()
                        .filter(|l| l.parse::<opacusr_libp2p::Multiaddr>().is_ok())
                        .cloned()
                        .collect();
                    if valid.len() != parsed.len() {
                        app.status_message =
                            "Часть строк не разобрана — проверьте формат multiaddr.".into();
                    }
                    app.config.p2p_bootstrap = valid;
                    let _ = app.config.save();
                    crate::p2p::stop_node(app);
                    if app.config.p2p_enabled {
                        crate::p2p::start_node(app);
                    }
                }
                crate::hint(
                    ui,
                    "пусто = узел сам себе seed (kad-сервер + релей) — этого хватает двум машинам \
                     в одной LAN (пригласительный адрес первой машины — во вторую); для разных \
                     сетей нужен seed с публичным адресом: p2p-node --seed",
                );
            });
            ui.end_row();

            if node_up {
                ui.label("Доступность");
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&reach).weak().small());
                    for a in &app
                        .p2p
                        .as_ref()
                        .map(|p| p.listen_addrs.clone())
                        .unwrap_or_default()
                    {
                        ui.label(egui::RichText::new(a).weak().small());
                    }
                });
                ui.end_row();

                ui.label("Проверка связи");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [360.0, 24.0],
                            egui::TextEdit::singleline(&mut app.p2p_ping_input)
                                .hint_text("PeerId партнёра: 12D3KooW…"),
                        );
                        if ui.button("Пинг").clicked() {
                            crate::p2p::ping_clicked(app);
                        }
                    });
                    if !ping_status.is_empty() {
                        ui.label(egui::RichText::new(&ping_status).weak().small());
                    }
                    crate::hint(
                        ui,
                        "ищет узел в DHT, дозванивается и пингует: покажет время ответа и было ли \
                         соединение прямым или через релей",
                    );
                });
                ui.end_row();

                ui.label("События");
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&status).weak().small());
                });
                ui.end_row();
            }
        });
}

/// Mobile shells have no embedded node — the tab explains why.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn p2p_tab(ui: &mut Ui, _app: &mut App) {
    ui.label(theme::bold("Децентрализованный (эксперимент)").size(18.0));
    ui.add_space(8.0);
    crate::hint(
        ui,
        "на этой платформе узел децентрализованного режима пока не встроен — используйте \
         настольную версию или прямое LAN-подключение по ip:порт",
    );
}
