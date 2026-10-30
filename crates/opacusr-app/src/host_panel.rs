//! The Host tab: run the agent service, show the assigned ID, manage the
//! unattended password, approve incoming sessions, and watch the log.

use egui::Context;

use crate::app::App;

pub fn host_ui(ctx: &Context, app: &mut App) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(10.0);

        // ---- consent dialog (if pending) ----
        if let Some(host) = &mut app.host {
            if let Some((_id, name, ans_tx)) = host.consent.take() {
                let mut decision = false;
                egui::Window::new("Incoming session")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(format!("{name} wants to view and control this machine.")).size(15.0));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(egui::RichText::new("✔ Allow").strong()).clicked() {
                                decision = true;
                            }
                            if ui.button("✖ Deny").clicked() {
                                decision = false;
                            }
                        });
                    });
                let _ = ans_tx.send(decision);
            }
        }

        let running = app.host.is_some();

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
                if app.config.password.is_some() {
                    ui.label(egui::RichText::new("● set (stored 0600 in the config)").color(egui::Color32::from_rgb(90, 200, 120)));
                    if ui.small_button("Clear").clicked() {
                        app.config.password = None;
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
                ui.add(egui::Slider::new(&mut app.quality, 20.0..=95.0).text("jpeg"));
                ui.add(egui::Slider::new(&mut app.scale, 25.0..=100.0).text("scale %"));
            });
            ui.end_row();
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
