//! The capture abstraction implemented by every backend.

use crate::{CaptureError, Frame};
use opacusr_proto::message::OutputMapping;

/// Default streaming frame rate.
pub const CAPTURE_FPS_DEFAULT: u8 = 15;

/// One selectable monitor of the host (multi-monitor support).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenDesc {
    /// Stable within one capture instance; used with [`ScreenCapture::select_screen`].
    pub id: u32,
    pub name: String,
    pub w: u32,
    pub h: u32,
}

/// One monitor of the compositor's output layout (native Wayland).
/// Logical geometry is the compositor's global desktop space; device
/// geometry is the streamed-frame space (upright, transform-normalized
/// device pixels). Filled by the Wayland layout probe, consumed by
/// [`output_mapping_from_layout`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputInfo {
    /// Logical top-left in the desktop space.
    pub x: i32,
    pub y: i32,
    /// Logical size (compositor space, fractional scaling accounted for).
    pub logical_w: u32,
    pub logical_h: u32,
    /// Device-pixel size of the captured frame (native mode, upright).
    pub device_w: u32,
    pub device_h: u32,
    /// Short connector-ish name (`xdg_output.name` / `wl_output.name`,
    /// e.g. `DP-1`) or the monitor model as a fallback.
    pub name: String,
    /// Human-readable description (vendor + model), may be empty.
    pub description: String,
}

/// Logical bounding box of a whole output layout — the `[0, 1]` space of
/// an absolute-pointer device maps over exactly this rect, so remote
/// input must be translated into it (see [`OutputMapping`]).
pub fn output_layout_bbox(outputs: &[OutputInfo]) -> (i32, i32, u32, u32) {
    if outputs.is_empty() {
        return (0, 0, 0, 0);
    }
    let min_x = outputs.iter().map(|o| o.x).min().unwrap_or(0);
    let min_y = outputs.iter().map(|o| o.y).min().unwrap_or(0);
    let max_x = outputs
        .iter()
        .map(|o| o.x.saturating_add(o.logical_w as i32))
        .max()
        .unwrap_or(0);
    let max_y = outputs
        .iter()
        .map(|o| o.y.saturating_add(o.logical_h as i32))
        .max()
        .unwrap_or(0);
    (
        min_x,
        min_y,
        max_x.saturating_sub(min_x).max(0) as u32,
        max_y.saturating_sub(min_y).max(0) as u32,
    )
}

/// Build the absolute-pointer mapping for the output at `idx` of the
/// probed layout. `None` on a degenerate layout (probe failed, zero
/// sizes) — callers then keep the single-monitor mapping.
pub fn output_mapping_from_layout(outputs: &[OutputInfo], idx: usize) -> Option<OutputMapping> {
    let o = outputs.get(idx)?;
    if o.logical_w == 0 || o.logical_h == 0 {
        return None;
    }
    let (bbox_x, bbox_y, bbox_w, bbox_h) = output_layout_bbox(outputs);
    if bbox_w == 0 || bbox_h == 0 {
        return None;
    }
    Some(OutputMapping {
        out_x: o.x,
        out_y: o.y,
        logical_w: o.logical_w,
        logical_h: o.logical_h,
        bbox_x,
        bbox_y,
        bbox_w,
        bbox_h,
    })
}

/// A live screen source. `capture()` returns the current frame of the
/// *selected* monitor; backends may cache resources between calls.
pub trait ScreenCapture: Send {
    /// Grab one RGBA frame.
    fn capture(&mut self) -> Result<Frame, CaptureError>;
    /// Current screen size (queried fresh — screens can be reconfigured).
    fn size(&mut self) -> Result<(u32, u32), CaptureError>;
    /// Monitors available for streaming. The first entry is the default.
    fn screens(&mut self) -> Vec<ScreenDesc> {
        match self.size() {
            Ok((w, h)) => vec![ScreenDesc {
                id: 0,
                name: "Screen".into(),
                w,
                h,
            }],
            Err(_) => Vec::new(),
        }
    }
    /// Switch the streamed monitor. `Ok(true)` = selection changed,
    /// `Ok(false)` = already selected; errors on unknown ids.
    fn select_screen(&mut self, id: u32) -> Result<bool, CaptureError> {
        if id == 0 {
            Ok(false)
        } else {
            Err(CaptureError::Unsupported)
        }
    }
    /// Layout of the streamed output inside the compositor's desktop, for
    /// absolute-pointer injection (native Wayland: the compositor maps an
    /// uinput pointer over the whole desktop, so frame coordinates need
    /// the per-output translation). `None` = no layout info available —
    /// the single-monitor mapping applies.
    fn output_mapping(&mut self) -> Option<OutputMapping> {
        None
    }
    /// Top-left of the captured region in global screen coordinates.
    /// Cursor positions ([`crate::cursor::CursorPosition`]) are global;
    /// the host subtracts this origin so the viewer gets coordinates
    /// relative to the streamed frame. Single-monitor captures are `(0,0)`.
    fn origin(&mut self) -> (i32, i32) {
        (0, 0)
    }
    /// Human-readable backend name for logs.
    fn backend(&self) -> &'static str {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(x: i32, y: i32, lw: u32, lh: u32) -> OutputInfo {
        OutputInfo {
            x,
            y,
            logical_w: lw,
            logical_h: lh,
            device_w: lw * 2, // Retina-ish default for the tests
            device_h: lh * 2,
            name: String::new(),
            description: String::new(),
        }
    }

    #[test]
    fn bbox_covers_side_by_side_layouts() {
        // 1920x1080 + 2560x1440 stacked right, different heights.
        let outs = [out(0, 0, 1920, 1080), out(1920, 0, 2560, 1440)];
        assert_eq!(output_layout_bbox(&outs), (0, 0, 4480, 1440));
    }

    #[test]
    fn bbox_handles_negative_and_gap_positions() {
        // Left monitor sits above/left with a gap and negative origin.
        let outs = [out(-1920, -600, 1920, 1080), out(0, 480, 1920, 1080)];
        assert_eq!(output_layout_bbox(&outs), (-1920, -600, 3840, 2160));
    }

    #[test]
    fn bbox_of_empty_layout_is_zero() {
        assert_eq!(output_layout_bbox(&[]), (0, 0, 0, 0));
    }

    #[test]
    fn mapping_carries_output_and_bbox_geometry() {
        let outs = [out(0, 0, 1920, 1080), out(1920, 0, 2560, 1440)];
        let m = output_mapping_from_layout(&outs, 1).expect("mapping");
        assert_eq!(m.out_x, 1920);
        assert_eq!(m.logical_w, 2560);
        assert_eq!(m.bbox_w, 4480);
        assert_eq!(m.bbox_h, 1440);
    }

    #[test]
    fn mapping_rejects_degenerate_layouts() {
        assert!(output_mapping_from_layout(&[], 0).is_none());
        // Zero logical size (probe did not see real geometry).
        let outs = [OutputInfo::default()];
        assert!(output_mapping_from_layout(&outs, 0).is_none());
    }
}
