//! The floating consent dialog for incoming sessions. The host card
//! itself moved to `home` (TeamViewer-style ID/password block) and the
//! host policies to `settings` (the hidden options window).

use egui::Context;

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
                    // MediaProjection consent pops up AFTER «Полный
                    // доступ» on the first PC connection (v0.13.0+ the
                    // phone CAN host). Say so BEFORE the user picks a
                    // button, so the extra dialog is not a surprise.
                    // v0.16: the phone also accepts remote input via
                    // the accessibility shim — but only if the user has
                    // enabled it; otherwise "Полный доступ" degrades to
                    // view-only, so say that too (honest, AnyDesk-style).
                    #[cfg(target_os = "android")]
                    {
                        let input_ready = opacusr_input::android_bridge::input_service_ready();
                        ui.label(
                            egui::RichText::new(if input_ready {
                                "После принятия появится системный диалог трансляции — \
                                 подтвердите его, чтобы передать экран. Удалённое \
                                 управление включено (сервис специальных возможностей)."
                            } else {
                                "Экран телефона увидят только в режиме просмотра: после \
                                 принятия появится системный диалог трансляции. Для удалённого \
                                 управления включите «OpacusR» в настройках специальных \
                                 возможностей телефона."
                            })
                            .color(theme::ACCENT),
                        );
                    }
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
