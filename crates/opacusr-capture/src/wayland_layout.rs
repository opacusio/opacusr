//! Wayland output-layout probe (wl_output + xdg-output).
//!
//! A short-lived `wayland-client` connection that enumerates the
//! compositor's outputs once per portal negotiation: logical positions
//! and sizes (fractional-scaling aware via xdg-output) plus the native
//! device-pixel mode — everything [`crate::source::OutputMapping`] needs
//! to translate frame pixels of a streamed monitor onto the desktop box
//! an uinput absolute pointer is mapped over.
//!
//! Pure-Rust wire implementation (no `wayland-sys`), so this compiles
//! and runs anywhere the `pipewire` feature builds; a failed connection
//! (no `WAYLAND_DISPLAY`, headless CI) yields an empty layout and the
//! caller keeps the single-monitor mapping.

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};

use crate::source::OutputInfo;

/// The v4-only events we consume (`name`/`description`); binding higher
/// is fine but we never ask for events above this.
const WL_OUTPUT_VERSION: u32 = 4;
const XDG_MANAGER_VERSION: u32 = 2;

#[derive(Debug, Default)]
struct RawOutput {
    // wl_output
    mode_w: u32,
    mode_h: u32,
    rotated: bool,
    scale: u32,
    model: String,
    // shared slots (xdg_output overrides wl_output when present)
    x: i32,
    y: i32,
    logical_w: u32,
    logical_h: u32,
    name: String,
    description: String,
}

impl RawOutput {
    /// Fold the raw events into an [`OutputInfo`]: transform-normalize
    /// the device size, fall back to `mode / wl_scale` for the logical
    /// size when the compositor has no xdg-output manager (integer
    /// scaling only there), and pick the best available name.
    fn finalize(&self) -> Option<OutputInfo> {
        let (device_w, device_h) = if self.rotated {
            (self.mode_h, self.mode_w)
        } else {
            (self.mode_w, self.mode_h)
        };
        if device_w == 0 || device_h == 0 {
            return None;
        }
        let scale = self.scale.max(1);
        let (logical_w, logical_h) = if self.logical_w > 0 && self.logical_h > 0 {
            (self.logical_w, self.logical_h)
        } else {
            (device_w / scale, device_h / scale)
        };
        if logical_w == 0 || logical_h == 0 {
            return None;
        }
        let name = if self.name.is_empty() {
            self.model.clone()
        } else {
            self.name.clone()
        };
        Some(OutputInfo {
            x: self.x,
            y: self.y,
            logical_w,
            logical_h,
            device_w,
            device_h,
            name,
            description: self.description.clone(),
        })
    }
}

/// Is this `wl_output.mode` event the currently enabled mode?
fn mode_is_current(flags: WEnum<wl_output::Mode>) -> bool {
    match flags {
        // `Mode` is combinable on the wire (`Current | Preferred`), so the
        // generated bitflags accept every raw value — the Unknown arm only
        // fires for compositors breaking the protocol.
        WEnum::Value(m) => m.contains(wl_output::Mode::Current),
        WEnum::Unknown(raw) => raw & 1 != 0, // bit 0 = Current
    }
}

/// Does this `wl_output.transform` rotate the panel by 90°/270° (so the
/// native mode's width/height swap against the logical one)?
fn transform_rotates(t: WEnum<wl_output::Transform>) -> bool {
    match t {
        WEnum::Value(t) => matches!(
            t,
            wl_output::Transform::_90
                | wl_output::Transform::_270
                | wl_output::Transform::Flipped90
                | wl_output::Transform::Flipped270
        ),
        WEnum::Unknown(_) => false,
    }
}

#[derive(Debug, Default)]
struct ProbeState {
    outputs: Vec<RawOutput>,
    /// Bound `wl_output` proxies, indexed like `outputs`.
    handles: Vec<wl_output::WlOutput>,
    manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_output" => {
                    let idx = state.outputs.len();
                    state.outputs.push(RawOutput::default());
                    let out = registry.bind::<wl_output::WlOutput, usize, Self>(
                        name,
                        version.min(WL_OUTPUT_VERSION),
                        qh,
                        idx,
                    );
                    state.handles.push(out);
                }
                "zxdg_output_manager_v1" => {
                    state.manager = Some(
                        registry.bind::<zxdg_output_manager_v1::ZxdgOutputManagerV1, (), Self>(
                            name,
                            version.min(XDG_MANAGER_VERSION),
                            qh,
                            (),
                        ),
                    );
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for ProbeState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.get_mut(*idx) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x,
                y,
                make,
                model,
                transform,
                ..
            } => {
                o.x = x;
                o.y = y;
                o.model = model.clone();
                if o.description.is_empty() && !make.is_empty() {
                    o.description = format!("{make} {model}");
                }
                o.rotated = transform_rotates(transform);
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                if mode_is_current(flags) {
                    o.mode_w = width.max(0) as u32;
                    o.mode_h = height.max(0) as u32;
                }
            }
            wl_output::Event::Scale { factor } => o.scale = factor.max(1) as u32,
            wl_output::Event::Name { name } => o.name = name,
            wl_output::Event::Description { description } => o.description = description,
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for ProbeState {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.get_mut(*idx) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                o.x = x;
                o.y = y;
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                o.logical_w = width.max(0) as u32;
                o.logical_h = height.max(0) as u32;
            }
            zxdg_output_v1::Event::Name { name } => o.name = name,
            zxdg_output_v1::Event::Description { description } => {
                o.description = description;
            }
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for ProbeState {
    fn event(
        _: &mut Self,
        _: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // No manager events (the only one — done — carries no data).
    }
}

/// Enumerate the compositor's outputs. Empty on any failure — callers
/// degrade to the single-monitor mapping.
pub(crate) fn probe_outputs() -> Vec<OutputInfo> {
    let Ok(conn) = Connection::connect_to_env() else {
        tracing::debug!(target: "opacusr", "wayland layout: no compositor connection");
        return Vec::new();
    };
    let mut state = ProbeState::default();
    let mut eq = conn.new_event_queue();
    let qh = eq.handle();
    conn.display().get_registry(&qh, ());
    // Roundtrip 1: globals → bind wl_outputs (+ manager).
    if eq.roundtrip(&mut state).is_err() {
        tracing::debug!(target: "opacusr", "wayland layout: dispatch failed");
        return Vec::new();
    }
    // Attach xdg_outputs now that both the manager and the outputs
    // exist; two more roundtrips settle geometry + logical events.
    if let Some(mgr) = state.manager.clone() {
        for (idx, out) in state.handles.iter().enumerate() {
            mgr.get_xdg_output(out, &qh, idx);
        }
        let _ = eq.roundtrip(&mut state);
        let _ = eq.roundtrip(&mut state);
    }
    let outs: Vec<OutputInfo> = state.outputs.iter().filter_map(|o| o.finalize()).collect();
    tracing::debug!(target: "opacusr", "wayland layout: {} output(s) {:?}", outs.len(),
        outs.iter().map(|o| format!("{} {}x{} @({},{})", o.name, o.logical_w, o.logical_h, o.x, o.y)).collect::<Vec<_>>());
    outs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_normalizes_rotation_and_scale() {
        // Portrait panel: mode 1080x1920 with a 90° transform, scale 2 →
        // upright device size 1920x1080, logical 960x540.
        let raw = RawOutput {
            mode_w: 1080,
            mode_h: 1920,
            rotated: true,
            scale: 2,
            ..RawOutput::default()
        };
        let o = raw.finalize().expect("info");
        assert_eq!((o.device_w, o.device_h), (1920, 1080));
        assert_eq!((o.logical_w, o.logical_h), (960, 540));
    }

    #[test]
    fn finalize_falls_back_to_model_name() {
        let raw = RawOutput {
            mode_w: 1920,
            mode_h: 1080,
            scale: 1,
            model: "U2415".into(),
            ..RawOutput::default()
        };
        let o = raw.finalize().expect("info");
        assert_eq!(o.name, "U2415");
        assert_eq!((o.device_w, o.device_h), (1920, 1080));
    }

    #[test]
    fn finalize_rejects_modeless_outputs() {
        let raw = RawOutput {
            scale: 1,
            ..RawOutput::default()
        };
        assert!(raw.finalize().is_none());
    }

    #[test]
    fn mode_flag_parsing_tolerates_combined_values() {
        // `Current` alone parses through the bitflags path.
        assert!(mode_is_current(WEnum::from(1u32)));
        // Combined `Current | Preferred` (3) still means current.
        assert!(mode_is_current(WEnum::from(3u32)));
        assert!(!mode_is_current(WEnum::from(2u32)));
    }

    #[test]
    fn transform_flag_parsing() {
        assert!(!transform_rotates(WEnum::from(0u32))); // normal
        assert!(transform_rotates(WEnum::from(1u32))); // 90°
        assert!(!transform_rotates(WEnum::from(2u32))); // 180°
        assert!(transform_rotates(WEnum::from(3u32))); // 270°
        assert!(transform_rotates(WEnum::from(5u32))); // flipped 90°
    }
}
