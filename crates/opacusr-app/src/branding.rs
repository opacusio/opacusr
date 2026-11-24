//! Embedded brand assets: the OpacusR ghost logo + action icons.
//!
//! PNGs live in `crates/opacusr-app/assets/branding/` (generated from the
//! user's opacus-logos set by scripts/apply_branding.py) and are decoded
//! once into egui textures on startup. `logo` is the flat ghost on its
//! dark tile (header + «О программе»); the `ui-*` icons come from the
//! same set and are used on the corresponding action buttons.

use egui::{ColorImage, TextureHandle};

pub const LOGO_PNG: &[u8] = include_bytes!("../assets/branding/logo.png");
pub const SEND_PNG: &[u8] = include_bytes!("../assets/branding/ui-send.png");
pub const SHIELD_PNG: &[u8] = include_bytes!("../assets/branding/ui-shield.png");
pub const STEALTH_PNG: &[u8] = include_bytes!("../assets/branding/ui-stealth.png");
pub const SWAP_PNG: &[u8] = include_bytes!("../assets/branding/ui-swap.png");

pub struct Brand {
    pub logo: TextureHandle,
    pub send: TextureHandle,
    pub shield: TextureHandle,
    pub stealth: TextureHandle,
    pub swap: TextureHandle,
}

impl Brand {
    /// Decode every embedded PNG once (startup cost: a few 128px PNGs).
    pub fn load(ctx: &egui::Context) -> Self {
        Self {
            logo: texture(ctx, "brand-logo", LOGO_PNG),
            send: texture(ctx, "brand-send", SEND_PNG),
            shield: texture(ctx, "brand-shield", SHIELD_PNG),
            stealth: texture(ctx, "brand-stealth", STEALTH_PNG),
            swap: texture(ctx, "brand-swap", SWAP_PNG),
        }
    }
}

fn texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> TextureHandle {
    let img = image::load_from_memory(bytes)
        .unwrap_or_else(|e| panic!("embedded brand PNG {name} is broken: {e}"))
        .to_rgba8();
    let [w, h] = [img.width() as usize, img.height() as usize];
    let color = ColorImage::from_rgba_unmultiplied([w, h], img.as_raw());
    ctx.load_texture(name, color, egui::TextureOptions::LINEAR)
}

/// An icon drawn at an exact square size (brand PNGs are all square).
pub fn icon(ui: &mut egui::Ui, tex: &TextureHandle, size: f32) -> egui::Response {
    ui.add(
        egui::Image::new(tex)
            .fit_to_exact_size(egui::Vec2::splat(size))
            .rounding(egui::Rounding::same(6.0)),
    )
}
