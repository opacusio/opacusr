//! Desktop input injection via `enigo` (X11 / Windows / macOS).

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};

use opacusr_proto::message::{mods, MouseButtonId, OpKey};

use crate::{InputError, InputInjector};

pub struct DesktopInjector {
    enigo: Enigo,
}

impl DesktopInjector {
    pub fn new() -> Result<Self, InputError> {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| InputError::Backend(format!("enigo init: {e}")))?;
        Ok(Self { enigo })
    }

    fn map_key(key: OpKey) -> Result<Key, InputError> {
        let k = match key {
            OpKey::Char(c) => Key::Unicode(c),
            OpKey::Enter => Key::Return,
            OpKey::Escape => Key::Escape,
            OpKey::Tab => Key::Tab,
            OpKey::Backspace => Key::Backspace,
            OpKey::Delete => Key::Delete,
            OpKey::Insert => Key::Insert,
            OpKey::Home => Key::Home,
            OpKey::End => Key::End,
            OpKey::PageUp => Key::PageUp,
            OpKey::PageDown => Key::PageDown,
            OpKey::ArrowUp => Key::UpArrow,
            OpKey::ArrowDown => Key::DownArrow,
            OpKey::ArrowLeft => Key::LeftArrow,
            OpKey::ArrowRight => Key::RightArrow,
            OpKey::Space => Key::Space,
            OpKey::F1 => Key::F1,
            OpKey::F2 => Key::F2,
            OpKey::F3 => Key::F3,
            OpKey::F4 => Key::F4,
            OpKey::F5 => Key::F5,
            OpKey::F6 => Key::F6,
            OpKey::F7 => Key::F7,
            OpKey::F8 => Key::F8,
            OpKey::F9 => Key::F9,
            OpKey::F10 => Key::F10,
            OpKey::F11 => Key::F11,
            OpKey::F12 => Key::F12,
            OpKey::CapsLock => Key::CapsLock,
            OpKey::NumLock => Key::Numlock,
            OpKey::PrintScreen => Key::PrintScr,
            OpKey::ContextMenu => {
                return Err(InputError::UnsupportedKey(OpKey::ContextMenu));
            }
        };
        Ok(k)
    }

    fn map_button(b: MouseButtonId) -> Button {
        match b {
            MouseButtonId::Left => Button::Left,
            MouseButtonId::Right => Button::Right,
            MouseButtonId::Middle => Button::Middle,
        }
    }

    /// Press (or release) modifier keys, returning the list we touched so
    /// they can be balanced afterwards.
    fn apply_mods(&mut self, m: u8, down: bool) -> Result<(), InputError> {
        let dir = if down {
            Direction::Press
        } else {
            Direction::Release
        };
        let pairs: &[(u8, Key)] = &[
            (mods::SHIFT, Key::Shift),
            (mods::CTRL, Key::Control),
            (mods::ALT, Key::Alt),
            (mods::META, Key::Meta),
        ];
        for (bit, key) in pairs {
            if m & bit != 0 {
                self.enigo
                    .key(*key, dir)
                    .map_err(|e| InputError::Backend(format!("modifier {key:?}: {e}")))?;
            }
        }
        Ok(())
    }
}

impl InputInjector for DesktopInjector {
    fn mouse_move(&mut self, x: u32, y: u32) -> Result<(), InputError> {
        self.enigo
            .move_mouse(x as i32, y as i32, Coordinate::Abs)
            .map_err(|e| InputError::Backend(format!("move: {e}")))
    }

    fn mouse_button(
        &mut self,
        x: u32,
        y: u32,
        button: MouseButtonId,
        down: bool,
    ) -> Result<(), InputError> {
        self.enigo
            .move_mouse(x as i32, y as i32, Coordinate::Abs)
            .map_err(|e| InputError::Backend(format!("move: {e}")))?;
        let dir = if down {
            Direction::Press
        } else {
            Direction::Release
        };
        self.enigo
            .button(Self::map_button(button), dir)
            .map_err(|e| InputError::Backend(format!("button: {e}")))
    }

    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        if dy != 0 {
            self.enigo
                .scroll(dy, Axis::Vertical)
                .map_err(|e| InputError::Backend(format!("scroll y: {e}")))?;
        }
        if dx != 0 {
            self.enigo
                .scroll(dx, Axis::Horizontal)
                .map_err(|e| InputError::Backend(format!("scroll x: {e}")))?;
        }
        Ok(())
    }

    fn key(&mut self, key: OpKey, m: u8, down: bool) -> Result<(), InputError> {
        if down && m != 0 {
            self.apply_mods(m, true)?;
        }
        let res = self
            .enigo
            .key(
                Self::map_key(key)?,
                if down {
                    Direction::Press
                } else {
                    Direction::Release
                },
            )
            .map_err(|e| InputError::Backend(format!("key: {e}")));
        if !down && m != 0 {
            self.apply_mods(m, false)?;
        }
        res
    }

    fn text(&mut self, text: &str) -> Result<(), InputError> {
        self.enigo
            .text(text)
            .map_err(|e| InputError::Backend(format!("text: {e}")))
    }
}
