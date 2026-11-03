//! OpacusR Rev-3 design system (the approved UI mockup): dark navy
//! background, violet accent, teal status, card layout, Carlito type.
//!
//! Everything the UI needs for the mockup look lives here: colors,
//! fonts, the global `install` hook, plus small helpers (`card`,
//! `section`, `accent_button`) shared by the home screen and the host
//! panel.

use egui::{Context, FontData, FontDefinitions, Rounding, Stroke, Ui, Vec2};

// ---------------------------------------------------------------------
// Palette (from the Rev 3 mockup, download/opacusr-design/)
// ---------------------------------------------------------------------

/// App background — very dark navy.
pub const BG: egui::Color32 = egui::Color32::from_rgb(0x0A, 0x0A, 0x14);
/// Card background.
pub const CARD: egui::Color32 = egui::Color32::from_rgb(0x15, 0x15, 0x2A);
/// Card / window border.
pub const CARD_STROKE: egui::Color32 = egui::Color32::from_rgb(0x27, 0x27, 0x4C);
/// Input-field background (slightly darker than the card).
pub const INSET: egui::Color32 = egui::Color32::from_rgb(0x0D, 0x0D, 0x1E);
/// Hovered-control background.
pub const HOVER: egui::Color32 = egui::Color32::from_rgb(0x20, 0x20, 0x40);
/// Primary accent (buttons, selection).
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x7C, 0x5C, 0xFC);
/// Accent used for pressed states.
pub const ACCENT_DIM: egui::Color32 = egui::Color32::from_rgb(0x64, 0x4B, 0xD4);
/// Online / success status.
pub const TEAL: egui::Color32 = egui::Color32::from_rgb(0x2D, 0xD4, 0xBF);
/// Destructive / errors.
pub const DANGER: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x5C, 0x72);
/// Warnings / notice highlights.
pub const GOLD: egui::Color32 = egui::Color32::from_rgb(0xF5, 0xC2, 0x6B);
/// Primary text.
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xE8, 0xF4);
/// Secondary text.
pub const MUTED: egui::Color32 = egui::Color32::from_rgb(0x92, 0x92, 0xB4);

// ---------------------------------------------------------------------
// Global install: fonts + visuals
// ---------------------------------------------------------------------

/// Carlito (SIL OFL 1.1 — license shipped next to the TTFs): the mockup
/// typeface. Prepended to the proportional family; egui's bundled mono
/// font stays for IDs and code.
static CARLITO_REG: &[u8] = include_bytes!("../assets/fonts/Carlito-Regular.ttf");
static CARLITO_BOLD: &[u8] = include_bytes!("../assets/fonts/Carlito-Bold.ttf");

/// Apply the Rev-3 theme to the egui context: Carlito fonts + dark
/// palette + rounded widgets. Called once at startup.
pub fn install(ctx: &Context) {
    install_fonts(ctx);
    install_visuals(ctx);
}

fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("carlito".into(), FontData::from_static(CARLITO_REG));
    fonts
        .font_data
        .insert("carlito-bold".into(), FontData::from_static(CARLITO_BOLD));
    // Prepend so Carlito renders everything; bundled fonts stay as
    // glyph fallback (emoji, icons) and keep serving the Monospace
    // family untouched. egui 0.29 has no weight-aware font selection
    // (strong() only brightens the color), so bold Carlito is exposed
    // as a separate family picked via `theme::bold`.
    {
        let family = egui::FontFamily::Proportional;
        let list = fonts.families.entry(family).or_default();
        list.insert(0, "carlito".into());
        list.insert(1, "carlito-bold".into());
    }
    fonts.families.insert(
        egui::FontFamily::Name("carlito-bold".into()),
        vec![
            "carlito-bold".into(),
            "carlito".into(),
            "Ubuntu-Light".into(),
            "NotoEmoji-Regular".into(),
            "emoji-icon-font".into(),
        ],
    );
    ctx.set_fonts(fonts);
}

fn install_visuals(ctx: &Context) {
    let mut style = (*ctx.style()).clone();
    {
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = INSET;
        v.faint_bg_color = INSET;
        v.window_stroke = Stroke::new(1.0_f32, CARD_STROKE);
        v.window_rounding = Rounding::same(16.0);
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.0_f32, TEXT);
        v.hyperlink_color = TEAL;
        v.error_fg_color = DANGER;
        v.warn_fg_color = GOLD;

        // Widgets: rounded, quiet, violet when active.
        use egui::style::WidgetVisuals as W;
        let rounding = Rounding::same(10.0);
        v.widgets.noninteractive = W {
            bg_fill: CARD,
            weak_bg_fill: CARD,
            bg_stroke: Stroke::new(1.0_f32, CARD_STROKE),
            fg_stroke: Stroke::new(1.0_f32, TEXT),
            rounding,
            expansion: 0.0,
        };
        v.widgets.inactive = W {
            bg_fill: INSET,
            weak_bg_fill: INSET,
            bg_stroke: Stroke::new(1.0_f32, CARD_STROKE),
            fg_stroke: Stroke::new(1.0_f32, TEXT),
            rounding,
            expansion: 0.0,
        };
        v.widgets.hovered = W {
            bg_fill: HOVER,
            weak_bg_fill: HOVER,
            bg_stroke: Stroke::new(1.0_f32, ACCENT_DIM),
            fg_stroke: Stroke::new(1.0_f32, TEXT),
            rounding,
            expansion: 0.0,
        };
        v.widgets.active = W {
            bg_fill: ACCENT_DIM,
            weak_bg_fill: ACCENT_DIM,
            bg_stroke: Stroke::new(1.0_f32, ACCENT),
            fg_stroke: Stroke::new(1.0_f32, TEXT),
            rounding,
            expansion: 0.0,
        };
        v.widgets.open = v.widgets.hovered;
    };
    ctx.set_style(style);
}

// ---------------------------------------------------------------------
// Small UI helpers shared by the home screen and host panel
// ---------------------------------------------------------------------

/// A mockup-style rounded card around the given content.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::default()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, CARD_STROKE))
        .rounding(Rounding::same(14.0))
        .inner_margin(egui::Margin::same(14.0))
        .outer_margin(egui::Margin::symmetric(0.0, 0.0))
        .show(ui, add)
}

/// Uppercase muted section label (mockup style), real bold face.
pub fn section(ui: &mut Ui, text: &str) {
    ui.label(bold(text.to_uppercase()).small().strong().color(MUTED));
}

/// Bold Carlito (egui 0.29 picks fonts per family, not per weight).
pub fn bold(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).family(egui::FontFamily::Name("carlito-bold".into()))
}

/// The violet call-to-action button.
pub fn accent_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(bold(text).color(TEXT))
            .fill(ACCENT)
            .rounding(Rounding::same(10.0))
            .min_size(Vec2::new(150.0, 34.0)),
    )
}

/// A quiet secondary button.
pub fn ghost_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(TEXT))
            .fill(CARD)
            .stroke(Stroke::new(1.0_f32, CARD_STROKE))
            .rounding(Rounding::same(10.0)),
    )
}

/// Monospace text (IDs, ports, URLs).
pub fn mono(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).monospace()
}

/// Colored status chip text, e.g. `● online`.
pub fn dot(text: impl Into<String>, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text.into()).color(color).small()
}

/// The computer name used as the default network-visible name:
/// `/etc/hostname` on unix, `COMPUTERNAME` on Windows, env user fallback.
pub fn machine_name() -> String {
    #[cfg(unix)]
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("USERNAME"))
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "my-computer".into())
}

#[cfg(test)]
mod tests {
    use super::machine_name;

    #[test]
    fn machine_name_is_never_empty() {
        assert!(!machine_name().is_empty());
    }
}
