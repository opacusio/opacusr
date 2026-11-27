//! AnyDesk-style «Контакты» drawer: a search field on top, sectioned
//! device lists (favorites, address book, LAN-discovered), round
//! colored avatars with hover actions. Docks to the right edge of the
//! window; the connection history is NOT here anymore — it is the main
//! window content (`crate::home`).

use egui::{Color32, Context, Ui, Vec2};

use crate::app::App;
use crate::theme;

// ---------------------------------------------------------------------
// Contacts drawer
// ---------------------------------------------------------------------

/// The «Контакты» drawer (AnyDesk device panel) — docked to the right
/// edge, full height, resizable. Pinned: search on top, «+ Добавить
/// контакт» at the bottom, the device list scrolls in between.
pub fn contacts_panel(ctx: &Context, app: &mut App) {
    if !app.show_contacts {
        return;
    }
    egui::SidePanel::right("contacts_drawer")
        .resizable(true)
        .width_range(300.0..=520.0)
        .default_width(340.0)
        .frame(theme::drawer_frame())
        .show(ctx, |ui| {
            if theme::drawer_header(ui, "Контакты") {
                app.show_contacts = false;
            }
            ui.add_space(4.0);

            // Search, AnyDesk-style, pinned above the list.
            egui::TopBottomPanel::top("contacts_search")
                .frame(egui::Frame::none())
                .show_inside(ui, |ui| {
                    ui.add_sized(
                        [ui.available_width(), 26.0],
                        egui::TextEdit::singleline(&mut app.contacts_search)
                            .hint_text("🔍  Поиск устройств…"),
                    );
                    ui.add_space(4.0);
                });

            // «Add contact» section, pinned at the bottom of the drawer.
            egui::TopBottomPanel::bottom("contacts_add")
                .frame(egui::Frame::none())
                .show_inside(ui, |ui| {
                    ui.separator();
                    ui.add_space(3.0);
                    egui::CollapsingHeader::new(
                        egui::RichText::new("+ Добавить контакт").color(theme::MUTED),
                    )
                    .default_open(app.add_contact_open)
                    .show(ui, |ui| {
                        add_contact_form(ui, app);
                    });
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show_inside(ui, |ui| {
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
                                        || h.signal_url.as_deref().unwrap_or("").to_lowercase().contains(&q)
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
                });
        });
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
                // The contact's own endpoint (its tunnel URL) rides as a
                // one-shot ladder hint instead of overwriting the global
                // server_url setting (a hidden side effect it used to be).
                app.contact_endpoint_hint = c.endpoint.clone();
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
                    let subtitle = match (&host.signal_url, &host.lan_target) {
                        (Some(url), _) => url.clone(),
                        (None, Some(t)) => t.clone(),
                        (None, None) => String::new(),
                    };
                    if !subtitle.is_empty() {
                        ui.label(theme::mono(subtitle).weak().small());
                    }
                    // The assigned ID when discovery knows it — a tap on
                    // the row fills it too, so «connect by ID» starts
                    // using the direct-first ladder immediately.
                    if let Some(id) = host.peer_id {
                        ui.label(theme::mono(id.to_string()).weak().small());
                    }
                })
                .response;
            if resp.clicked() {
                if let Some(url) = &host.signal_url {
                    app.server_url = url.clone();
                }
                if app.peer_id_input.trim().is_empty() {
                    match &host.lan_target {
                        Some(target) => app.peer_id_input = target.clone(),
                        None => {
                            if let Some(id) = host.peer_id {
                                app.peer_id_input = id.to_string();
                            }
                        }
                    }
                }
                app.status_message = format!("Выбран найденный хост {}.", host.label);
            }
            if resp.double_clicked() {
                if let Some(url) = &host.signal_url {
                    app.server_url = url.clone();
                }
                // The ID is the best target: the connect ladder resolves
                // it to direct-LAN + signaling routes automatically.
                match (&host.peer_id, &host.lan_target) {
                    (Some(id), _) => app.peer_id_input = id.to_string(),
                    (None, Some(target)) => app.peer_id_input = target.clone(),
                    (None, None) => {}
                }
                app.connect_clicked(Some(host.label.clone()));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("▶").on_hover_text("Подключиться").clicked() {
                if let Some(url) = &host.signal_url {
                    app.server_url = url.clone();
                }
                // Same preference as the double click: the ID wins —
                // the ladder resolves it to direct + signaling routes.
                match (&host.peer_id, &host.lan_target) {
                    (Some(id), _) => app.peer_id_input = id.to_string(),
                    (None, Some(target)) => app.peer_id_input = target.clone(),
                    (None, None) => {}
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
