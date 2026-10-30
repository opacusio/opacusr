//! Linux/X11 backend: full-screen `GetImage` on the root window.
//!
//! Simple and dependency-free. Known limits (see roadmap): XShm + Damage
//! for performance, and Wayland sessions need PipeWire instead. Under
//! Wayland the connection attempt fails and callers get
//! [`crate::CaptureError::X11`].

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};

use crate::source::ScreenCapture;
use crate::{bgra_to_rgba, CaptureError, Frame};

pub struct X11Capture {
    conn: x11rb::rust_connection::RustConnection,
    screen: usize,
    root: u32,
}

impl X11Capture {
    /// Connect to the default display (`$DISPLAY`).
    pub fn connect() -> Result<Self, CaptureError> {
        let (conn, screen) =
            x11rb::connect(None).map_err(|e| CaptureError::X11(format!("connect: {e}")))?;
        let root = conn.setup().roots[screen].root;
        Ok(Self { conn, screen, root })
    }

    fn geometry(&mut self) -> Result<(u32, u32), CaptureError> {
        let roots = &self.conn.setup().roots[self.screen];
        Ok((roots.width_in_pixels as u32, roots.height_in_pixels as u32))
    }
}

impl ScreenCapture for X11Capture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        let (w, h) = self.geometry()?;
        if w == 0 || h == 0 {
            return Err(CaptureError::Resized);
        }
        let reply = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                0,
                0,
                w as u16,
                h as u16,
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

    fn backend(&self) -> &'static str {
        "x11"
    }
}
