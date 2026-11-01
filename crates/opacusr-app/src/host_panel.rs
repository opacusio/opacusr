//! The Host tab: run the agent service, show the assigned ID, manage the
//! unattended password, approve incoming sessions, and watch the log.

use egui::Context;

use opacusr_agent::config::Permission;
use opacusr_agent::service::SessionDecision;

use crate::app::App;

pub fn host_ui(ctx: &Context, app: &mut App) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(10.0);

        // ---- consent dialog (if pending) ----
        if let Some(host) = &mut app.host {
            if let Some((_id, name, ans_tx)) = host.consent.take() {
                let mut decision: Option<SessionDecision> = None;
                egui::Window::new("Incoming session")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("{name} wants to connect to this machine."))
                                .size(15.0),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button(egui::RichText::new("✔ Full control").strong())
                                .clicked()
                            {
                                decision = Some(SessionDecision::Full);
                            }
                            if ui.button("👁 View only").clicked() {
                                decision = Some(SessionDecision::ViewOnly);
                            }
                            if ui.button("✖ Deny").clicked() {
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

        let running = app.host.is_some();

        // ---- active session: send a file to the viewer ----
        if let Some(h) = &app.host {
            if h.session_active {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Session active").strong());
                    let enabled = h.commands.is_some();
                    if ui
                        .add_enabled(enabled, egui::Button::new("⬆ Send file to viewer"))
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            if let Some(tx) =
                                app.host.as_ref().and_then(|h| h.commands.as_ref())
                            {
                                let _ = tx.send(
                                    opacusr_agent::session::HostCommand::SendFile(path),
                                );
                            }
                        }
                    }
                    crate::hint(ui, "the viewer receives it into its download folder");
                });
                ui.add_space(6.0);
            }
        }

        // ---- status card ----
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    if running {
                        if let Some(host) = &app.host {
                            match host.my_id {
                                Some(id) => {
                                    ui.label(egui::RichText::new("Your OpacusR ID").weak().small());
                                    ui.label(egui::RichText::new(format!("{id}")).size(34.0).strong());
                                }
                                None => {
                                    ui.label("registering on the server…");
                                }
                            }
                        }
                    } else {
                        ui.label(egui::RichText::new("Agent is offline").weak().size(16.0));
                        ui.label("Start it to let other devices connect to this machine.");
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if running {
                        if ui.button(egui::RichText::new("⏹ Stop agent").size(15.0)).clicked() {
                            app.host_stop_clicked();
                        }
                    } else if ui
                        .add(
                            egui::Button::new(egui::RichText::new("▶ Start agent").size(15.0))
                                .min_size([130.0, 36.0].into()),
                        )
                        .clicked()
                    {
                        app.host_start_clicked();
                    }
                });
            });
        });

        ui.add_space(12.0);

        egui::Grid::new("host_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(egui::RichText::new("Server").strong());
            ui.add_sized([430.0, 24.0], egui::TextEdit::singleline(&mut app.server_url));
            ui.end_row();

            ui.label(egui::RichText::new("Machine name").strong());
            ui.add_sized([430.0, 24.0], egui::TextEdit::singleline(&mut app.display_name));
            ui.end_row();

            ui.label(egui::RichText::new("Unattended password").strong());
            ui.horizontal(|ui| {
                if app.config.is_unattended() {
                    let storage = if app.config.password_in_keychain {
                        "stored in the OS keychain"
                    } else {
                        "stored 0600 in the config"
                    };
                    ui.label(
                        egui::RichText::new(format!("● set ({storage})"))
                            .color(egui::Color32::from_rgb(90, 200, 120)),
                    );
                    if ui.small_button("Clear").clicked() {
                        let _ = opacusr_agent::keychain::delete_password();
                        app.config.password = None;
                        app.config.password_in_keychain = false;
                        match app.config.save() {
                            Ok(p) => app.status_message = format!("Cleared (saved {}).", p.display()),
                            Err(e) => app.status_message = format!("Save failed: {e}"),
                        }
                    }
                } else {
                    let resp = ui.add_sized(
                        [220.0, 24.0],
                        egui::TextEdit::singleline(&mut app.host_password_input)
                            .password(true)
                            .hint_text("min 4 chars"),
                    );
                    if (ui.button("Set").clicked() || resp.lost_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        app.save_host_password();
                    }
                }
            });
            ui.end_row();

            ui.label(egui::RichText::new("Download folder").strong());
            ui.label(app.config.download_dir.display().to_string());
            ui.end_row();

            ui.label(egui::RichText::new("Streaming").strong());
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut app.fps, 1.0..=60.0).text("fps"));
                ui.add(egui::Slider::new(&mut app.quality, 20.0..=95.0).text("quality"));
                ui.add(egui::Slider::new(&mut app.scale, 25.0..=100.0).text("scale %"));
                egui::ComboBox::from_id_salt("codec")
                    .selected_text(app.config.codec.to_ascii_uppercase())
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for opt in ["auto", "h264", "jpeg"] {
                            ui.selectable_value(&mut app.config.codec, opt.to_string(), opt);
                        }
                    });
            });
            ui.end_row();

            ui.label(egui::RichText::new("Clipboard sync").strong());
            ui.checkbox(&mut app.config.clipboard_sync, "share clipboard with connected viewers");
            ui.end_row();

            ui.label(egui::RichText::new("Adaptive quality").strong());
            ui.checkbox(
                &mut app.config.adaptive_quality,
                "adapt fps/quality/scale to the link (manual settings still win)",
            );
            ui.end_row();

            ui.label(egui::RichText::new("Default access").strong());
            ui.horizontal(|ui| {
                let mut perm = Permission::parse(&app.config.default_permission);
                egui::ComboBox::from_id_salt("default_perm")
                    .selected_text(match perm {
                        Permission::Full => "full control".to_string(),
                        Permission::ViewOnly => "view only".to_string(),
                        Permission::Deny => "deny".to_string(),
                    })
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut perm, Permission::Full, "full control");
                        ui.selectable_value(&mut perm, Permission::ViewOnly, "view only");
                        ui.selectable_value(&mut perm, Permission::Deny, "deny");
                    });
                if perm.as_str() != app.config.default_permission {
                    app.config.default_permission = perm.as_str().to_string();
                }
                crate::hint(ui, "for unattended connections not matched by ACL rules");
            });
            ui.end_row();

            ui.label(egui::RichText::new("Audio").strong());
            ui.checkbox(&mut app.config.audio, "offer audio streaming to viewers");
            ui.end_row();

            ui.label(egui::RichText::new("Input lock").strong());
            ui.checkbox(
                &mut app.config.lock_input,
                "lock physical keyboard/mouse while a viewer controls this machine",
            );
            ui.end_row();
            crate::hint(
                ui,
                "TeamViewer-style: remote input passes, local input is swallowed. \
                 Ctrl+Alt+Del stays available on Windows. View-only sessions never lock.",
            );
        });

        ui.add_space(12.0);
        ui.separator();
        ui.label(egui::RichText::new("Activity").strong());
        if let Some(host) = &app.host {
            egui::ScrollArea::vertical().max_height(ui.available_height() - 10.0).show(ui, |ui| {
                if host.notices.is_empty() {
                    ui.label(egui::RichText::new("no events yet").weak());
                }
                for line in host.notices.iter().rev() {
                    ui.label(format!("· {line}"));
                }
            });
        } else {
            crate::hint(
                ui,
                "Unattended mode: sessions are accepted after a correct password, without a human prompt.\n\
                 Attended mode (no password): every incoming session shows an Allow/Deny dialog.",
            );
        }
    });
}
