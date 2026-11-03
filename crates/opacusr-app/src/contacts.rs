//! AnyDesk-style «Контакты» and «История» windows: a search field on
//! top, sectioned device lists (favorites, address book, LAN-discovered),
//! round colored avatars with hover actions — and a simple history log
//! window. The home screen stays TeamViewer-quiet; everything device-
//! related lives here.

use egui::{Color32, Context, Ui, Vec2};

use opacusr_client::addressbook::{fmt_duration, fmt_ts, Outcome};

use crate::app::App;
use crate::theme;

// ---------------------------------------------------------------------
// Contacts window
// ---------------------------------------------------------------------

/// The «Контакты» window (AnyDesk device panel).
pub fn contacts_window(ctx: &Context, app: &mut App) {
    if !app.show_contacts {
        return;
    }
    let mut open = true;
    egui::Window::new(theme::bold("Контакты"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(330.0)
        .default_height(440.0)
        .default_pos([56.0, 96.0])
        .show(ctx, |ui| {
            // Search, AnyDesk-style, always on top of the list.
            ui.add_sized(
                [ui.available_width(), 26.0],
                egui::TextEdit::singleline(&mut app.contacts_search)
                    .hint_text("🔍  Поиск устройств…"),
            );
            ui.add_space(6.0);

            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    let q = app.contacts_search.trim().to_lowercase();
                    let matches = |label: &str, target: &str| {
                        q.is_empty()
                            || label.to_lowercase().contains(&q)
                            || target.to_lowercase().contains(&q)
                    };

                    // ---- Избранные ----
                    let favorites: Vec<_> = app
                        .book
                        .sorted_contacts()
                        .into_iter()
                        .filter(|c| c.favorite)
                        .collect();
                    section_header(ui, "Избранные", favorites.len());
                    if favorites.is_empty() {
                        empty_note(ui, "Добавьте ★ к контакту, чтобы закрепить его здесь.");
                    }
                    for c in favorites {
                        if !matches(&c.label, &c.target) {
                            continue;
                        }
                        contact_row(ui, app, &c);
                    }

                    // ---- Адресная книга ----
                    let others: Vec<_> = app
                        .book
                        .sorted_contacts()
                        .into_iter()
                        .filter(|c| !c.favorite)
                        .collect();
                    ui.add_space(4.0);
                    section_header(ui, "Все контакты", others.len());
                    if others.is_empty() {
                        empty_note(ui, "Сохранённых контактов ещё нет.");
                    }
                    for c in others {
                        if !matches(&c.label, &c.target) {
                            continue;
                        }
                        contact_row(ui, app, &c);
                    }

                    // ---- Найденные в сети (mDNS) ----
                    ui.add_space(4.0);
                    let hosts = app.discovery.snapshot();
                    let hosts: Vec<_> = hosts
                        .into_iter()
                        .filter(|h| {
                            q.is_empty()
                                || h.label.to_lowercase().contains(&q)
                                || h.signal_url.to_lowercase().contains(&q)
                        })
                        .collect();
                    section_header(ui, "Найденные в этой сети", hosts.len());
                    if hosts.is_empty() {
                        empty_note(
                            ui,
                            "Запущенные агенты в локальной сети появятся здесь автоматически.",
                        );
                    }
                    for host in hosts {
                        lan_row(ui, app, &host);
                    }
                    ui.add_space(6.0);
                });

            ui.separator();
            ui.add_space(4.0);
            egui::CollapsingHeader::new(
                egui::RichText::new("+ Добавить контакт").color(theme::MUTED),
            )
            .default_open(app.add_contact_open)
            .show(ui, |ui| {
                add_contact_form(ui, app);
            });
        });
    if !open {
        app.show_contacts = false;
    }
}

/// One address-book row: avatar + label over target + hover actions
/// (connect, rename, favorite, remove) — AnyDesk list style.
fn contact_row(ui: &mut Ui, app: &mut App, c: &opacusr_client::addressbook::Contact) {
    let renaming = app.renaming_id.as_deref() == Some(c.id.as_str());
    ui.horizontal(|ui| {
        avatar(ui, &c.label, 30.0);
        ui.vertical(|ui| {
            ui.set_min_width(ui.available_width() - 4.0);
            if renaming {
                // Inline rename editor: ✔/Enter applies, ✖ cancels.
                let resp = ui.add_sized(
                    [ui.available_width(), 22.0],
                    egui::TextEdit::singleline(&mut app.rename_label),
                );
                if !app.rename_focused {
                    resp.request_focus();
                    app.rename_focused = true;
                }
                let confirmed = (resp.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    || ui.button("✔").on_hover_text("Сохранить имя").clicked();
                if ui.button("✖").on_hover_text("Отмена").clicked() {
                    app.renaming_id = None;
                }
                if confirmed {
                    let name = app.rename_label.trim().to_string();
                    if app.book.rename(&c.id, &name) {
                        app.status_message = "Контакт переименован.".into();
                        let _ = app.book.save();
                    } else {
                        app.status_message = "Имя не может быть пустым.".into();
                    }
                    app.renaming_id = None;
                }
            } else {
                ui.label(egui::RichText::new(&c.label).strong());
                ui.label(theme::mono(c.target.clone()).weak().small());
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("▶").on_hover_text("Подключиться").clicked() {
                if let Some(ep) = c.endpoint.clone() {
                    app.server_url = ep;
                }
                app.peer_id_input = c.target.clone();
                let label = c.label.clone();
                app.connect_clicked(Some(label));
            }
            if renaming {
                return;
            }
            if ui
                .button(if c.favorite { "★" } else { "☆" })
                .on_hover_text("Избранное")
                .clicked()
            {
                app.book.set_favorite(&c.id, !c.favorite);
                let _ = app.book.save();
            }
            if ui.button("✏").on_hover_text("Переименовать").clicked() {
                app.renaming_id = Some(c.id.clone());
                app.rename_label = c.label.clone();
                app.rename_focused = false;
            }
            if ui.button("✖").on_hover_text("Удалить").clicked() {
                app.book.remove(&c.id);
                let _ = app.book.save();
            }
        });
    });
    ui.add_space(2.0);
}

/// One LAN-discovered row: avatar + name over the signal URL.
/// Click fills the connect fields, ▶ (or double click) connects.
fn lan_row(ui: &mut Ui, app: &mut App, host: &crate::discovery::DiscoveredHost) {
    ui.horizontal(|ui| {
        avatar(ui, &host.label, 30.0);
        ui.vertical(|ui| {
            let w = ui.available_width() - 4.0;
            ui.set_min_width(w);
            let resp = ui
                .vertical(|ui| {
                    ui.label(egui::RichText::new(&host.label).strong());
                    ui.label(theme::mono(host.signal_url.clone()).weak().small());
                })
                .response;
            if resp.clicked() {
                app.server_url = host.signal_url.clone();
                if app.peer_id_input.trim().is_empty() {
                    if let Some(target) = &host.lan_target {
                        app.peer_id_input = target.clone();
                    }
                }
                app.status_message = format!("Выбран найденный хост {}.", host.label);
            }
            if resp.double_clicked() {
                app.server_url = host.signal_url.clone();
                if let Some(target) = &host.lan_target {
                    app.peer_id_input = target.clone();
                }
                app.connect_clicked(Some(host.label.clone()));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("▶").on_hover_text("Подключиться").clicked() {
                app.server_url = host.signal_url.clone();
                if let Some(target) = &host.lan_target {
                    app.peer_id_input = target.clone();
                }
                app.connect_clicked(Some(host.label.clone()));
            }
        });
    });
    ui.add_space(2.0);
}

/// The compact «add contact» form (AnyDesk address-book new entry).
fn add_contact_form(ui: &mut Ui, app: &mut App) {
    egui::Grid::new("book_add_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Название");
            ui.add_sized(
                [240.0, 24.0],
                egui::TextEdit::singleline(&mut app.new_label).hint_text("Мой ноутбук"),
            );
            ui.end_row();
            ui.label("Адрес");
            ui.add_sized(
                [240.0, 24.0],
                egui::TextEdit::singleline(&mut app.new_target)
                    .hint_text("9-значный ID или ip:port"),
            );
            ui.end_row();
            ui.label("Свой сервер");
            ui.add_sized(
                [240.0, 24.0],
                egui::TextEdit::singleline(&mut app.new_endpoint).hint_text(
                    "необязат. — сигнальный URL, который пробуется первым (wss://…/signal)",
                ),
            );
            ui.end_row();
        });
    if ui.button("💾 Сохранить контакт").clicked() {
        let target = app.new_target.replace([' ', '-'], "");
        if target.is_empty() {
            app.status_message = "Контакту нужен адрес: 9-значный ID или ip:port".into();
        } else {
            let label = {
                let l = app.new_label.trim();
                if l.is_empty() {
                    target.clone()
                } else {
                    l.to_string()
                }
            };
            let endpoint = {
                let e = app.new_endpoint.trim();
                if e.is_empty() {
                    None
                } else {
                    Some(e.to_string())
                }
            };
            app.book.upsert(label, target, endpoint);
            app.new_label.clear();
            app.new_target.clear();
            app.new_endpoint.clear();
            app.status_message = "Контакт сохранён.".into();
            let _ = app.book.save();
        }
    }
}

// ---------------------------------------------------------------------
// History window
// ---------------------------------------------------------------------

/// The «История подключений» window (AnyDesk history log).
pub fn history_window(ctx: &Context, app: &mut App) {
    if !app.show_history {
        return;
    }
    let mut open = true;
    egui::Window::new(theme::bold("История подключений"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(540.0)
        .default_height(380.0)
        .default_pos([80.0, 140.0])
        .show(ctx, |ui| {
            if app.book.history().is_empty() {
                empty_note(ui, "Истории подключений пока нет.");
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        for h in app.book.history() {
                            history_row(ui, h);
                        }
                    });
            }
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(egui::RichText::new("Очистить").color(theme::DANGER))
                    .clicked()
                {
                    app.book.clear_history();
                    let _ = app.book.save();
                }
                ui.label(
                    egui::RichText::new("последние 100 попыток подключений")
                        .weak()
                        .small(),
                );
            });
        });
    if !open {
        app.show_history = false;
    }
}

fn history_row(ui: &mut Ui, h: &opacusr_client::addressbook::HistoryEntry) {
    ui.horizontal(|ui| {
        // AnyDesk history rows: time, device, result mark, duration.
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

// ---------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------

/// A muted section header with an entry count (AnyDesk list groups).
fn section_header(ui: &mut Ui, title: &str, count: usize) {
    ui.horizontal(|ui| {
        ui.label(
            theme::bold(title.to_uppercase())
                .small()
                .color(theme::MUTED),
        );
        ui.label(
            egui::RichText::new(format!("{count}"))
                .weak()
                .small()
                .color(theme::MUTED),
        );
    });
}

fn empty_note(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).weak().small());
    ui.add_space(2.0);
}

/// Round colored avatar with the first letter of the name (AnyDesk
/// device icons). The color is derived from the name so a device keeps
/// the same color across lists.
fn avatar(ui: &mut Ui, name: &str, size: f32) {
    let letter = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let palette = [
        theme::ACCENT,
        theme::TEAL,
        Color32::from_rgb(0xE0, 0x7A, 0x5F), // terracotta
        Color32::from_rgb(0x5F, 0x9E, 0xE0), // steel blue
        theme::GOLD,
        Color32::from_rgb(0x9E, 0x5F, 0xE0), // amethyst
    ];
    let idx = name
        .bytes()
        .fold(0usize, |acc, b| (acc * 31 + b as usize) % 997);
    let fill = palette[idx % palette.len()];
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let center = rect.center();
    let radius = size / 2.0;
    ui.painter().circle_filled(center, radius, fill);
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        letter,
        egui::FontId::proportional(size * 0.5),
        Color32::WHITE,
    );
}
