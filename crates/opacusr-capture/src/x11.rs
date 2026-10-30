//! Linux/X11 backend: `GetImage` over a RandR monitor rect.
//!
//! Monitors are enumerated with RandR (`get_monitors`); the primary one is
//! streamed by default and [`ScreenCapture::select_screen`] switches the
//! capture rect. Known limits (see roadmap): XShm + Damage for performance;
//! native Wayland sessions need the `pipewire` feature instead — under
//! Wayland the connection attempt fails and callers get
//! [`crate::CaptureError::X11`].

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrExt;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};

use crate::source::{ScreenCapture, ScreenDesc};
use crate::{bgra_to_rgba, CaptureError, Frame};

pub struct X11Capture {
    conn: x11rb::rust_connection::RustConnection,
    screen: usize,
    root: u32,
    /// Capture rect in root-window coordinates: `(x, y, w, h)`.
    rect: (i16, i16, u16, u16),
    selected: u32,
}

impl X11Capture {
    /// Connect to the default display (`$DISPLAY`).
    pub fn connect() -> Result<Self, CaptureError> {
        let (conn, screen) =
            x11rb::connect(None).map_err(|e| CaptureError::X11(format!("connect: {e}")))?;
        let root = conn.setup().roots[screen].root;
        let mut cap = Self {
            conn,
            screen,
            root,
            rect: (0, 0, 0, 0),
            selected: 0,
        };
        // Default to the primary monitor (or the first one); fall back to
        // the whole root window when RandR has nothing to say.
        let screens = cap.query_screens();
        if let Some(first) = screens.first() {
            cap.selected = first.id;
            if let Some((x, y, w, h)) = cap.monitor_rect(first.id) {
                cap.rect = (x, y, w, h);
            } else {
                let (w, h) = cap.root_geometry();
                cap.rect = (0, 0, w as u16, h as u16);
            }
        } else {
            let (w, h) = cap.root_geometry();
            cap.rect = (0, 0, w as u16, h as u16);
        }
        Ok(cap)
    }

    fn root_geometry(&self) -> (u32, u32) {
        let roots = &self.conn.setup().roots[self.screen];
        (roots.width_in_pixels as u32, roots.height_in_pixels as u32)
    }

    /// Query RandR monitors; empty when RandR is unavailable (then the
    /// root window is the single screen).
    fn query_screens(&mut self) -> Vec<ScreenDesc> {
        let reply = match self
            .conn
            .randr_get_monitors(self.root, true)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(r) => r,
            None => return Vec::new(),
        };
        let mut out = Vec::with_capacity(reply.monitors.len());
        for (i, m) in reply.monitors.iter().enumerate() {
            let name = self
                .conn
                .get_atom_name(m.name)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Display {}", i + 1));
            out.push(ScreenDesc {
                id: i as u32,
                name,
                w: m.width as u32,
                h: m.height as u32,
            });
        }
        out
    }

    fn monitor_rect(&mut self, id: u32) -> Option<(i16, i16, u16, u16)> {
        let reply = self
            .conn
            .randr_get_monitors(self.root, true)
            .ok()?
            .reply()
            .ok()?;
        let m = reply.monitors.get(id as usize)?;
        Some((m.x, m.y, m.width, m.height))
    }

    fn geometry(&mut self) -> Result<(u32, u32), CaptureError> {
        // Refresh RandR state so re-configurations (hotplug, resolution
        // change) are picked up between captures.
        if let Some((_, _, w, h)) = self.monitor_rect(self.selected) {
            self.rect.2 = w;
            self.rect.3 = h;
        }
        let (w, h) = (self.rect.2 as u32, self.rect.3 as u32);
        if w == 0 || h == 0 {
            return Err(CaptureError::Resized);
        }
        Ok((w, h))
    }
}

impl ScreenCapture for X11Capture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        let (w, h) = self.geometry()?;
        let reply = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                self.rect.0,
                self.rect.1,
                self.rect.2,
                self.rect.3,
                !0,
            )
            .map_err(|e| CaptureError::X11(format!("get_image: {e}")))?
            .reply()
            .map_err(|e| CaptureError::X11(format!("get_image reply: {e}")))?;
        let mut rgba = reply.data;
        bgra_to_rgba(&mut rgba);
        Ok(Frame::new(w, h, rgba))
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        self.geometry()
    }

    fn screens(&mut self) -> Vec<ScreenDesc> {
        let mut list = self.query_screens();
        if list.is_empty() {
            let (w, h) = self.root_geometry();
            list.push(ScreenDesc {
                id: 0,
                name: "Screen".into(),
                w,
                h,
            });
        }
        list
    }

    fn select_screen(&mut self, id: u32) -> Result<bool, CaptureError> {
        let list = self.query_screens();
        let exists = list.iter().any(|s| s.id == id);
        if !exists {
            return Err(CaptureError::X11(format!("unknown screen id {id}")));
        }
        let changed = id != self.selected;
        if changed {
            if let Some(rect) = self.monitor_rect(id) {
                self.rect = rect;
                self.selected = id;
            }
        }
        Ok(changed)
    }

    fn backend(&self) -> &'static str {
        "x11"
    }
}
