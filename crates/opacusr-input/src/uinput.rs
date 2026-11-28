//! Native-Wayland input injection through the kernel `uinput` interface.
//!
//! enigo speaks X11 (XTest), so on a native Wayland session (no XWayland
//! access) remote input silently does nothing while the PipeWire capture
//! works fine. This backend closes that gap at the kernel level: two
//! virtual evdev devices (pointer + keyboard) are created via
//! `/dev/uinput`, and the compositor's libinput picks them up like real
//! hardware — no display-server protocol, no permission dialogs.
//!
//! Coordinate model: the pointer device exposes ABS_X/ABS_Y over a fixed
//! range (0..32767). libinput normalizes absolute axes to [0..1] and the
//! compositor maps that over the desktop, so the physical resolution and
//! the Wayland scale factor cancel out — [`InputInjector::set_viewport`]
//! (the real capture size in device px) is all we need to convert.
//!
//! Multi-monitor: the compositor maps [0..1] over the bounding box of
//! ALL outputs, not the streamed one — so the session hands the streamed
//! output's logical geometry ([`InputInjector::set_output_mapping`],
//! probed via wl_output + xdg-output by the capture backend) and every
//! pointer position is translated frame px → output-logical px → desktop
//! bbox before hitting the ABS range. Without a mapping (probe failed,
//! single monitor) the frame itself is the bbox — the v1 behavior.
//!
//! Permissions: opening `/dev/uinput` needs write access (the packaged
//! udev rule grants it to the `input` group, same group the physical
//! input lock already requires). Without it we fall back to the X11
//! backend with a warning.
//!
//! The physical input lock (`crate::lock`) stays compatible: `EVIOCGRAB`
//! only grabs the host's physical event nodes — events injected through
//! these virtual devices appear on their own nodes and pass.

use std::mem::size_of;
use std::os::unix::io::RawFd;
use std::time::{SystemTime, UNIX_EPOCH};

use opacusr_proto::message::{mods, MouseButtonId, OpKey, OutputMapping};

use crate::{InputError, InputInjector};

// ——— Linux uinput / evdev ABI (linux/uinput.h, linux/input-event-codes.h) ——

const UI_SET_EVBIT: u64 = iow(b'U', 100, size_of::<libc::c_int>());
const UI_SET_KEYBIT: u64 = iow(b'U', 101, size_of::<libc::c_int>());
const UI_SET_RELBIT: u64 = iow(b'U', 102, size_of::<libc::c_int>());
const UI_SET_ABSBIT: u64 = iow(b'U', 103, size_of::<libc::c_int>());
const UI_DEV_CREATE: u64 = io(b'U', 1);
const UI_DEV_DESTROY: u64 = io(b'U', 2);
const UI_DEV_SETUP: u64 = iow(b'U', 3, size_of::<UinputSetup>());
const UI_ABS_SETUP: u64 = iow(b'U', 4, size_of::<UinputAbsSetup>());

/// `_IOC(dir, type, nr, size)` — asm-generic ioctl encoding.
const fn ioc(dir: u64, ty: u8, nr: u8, size: usize) -> u64 {
    (dir << 30) | ((size as u64 & 0x3fff) << 16) | ((ty as u64) << 8) | nr as u64
}
const fn iow(ty: u8, nr: u8, size: usize) -> u64 {
    ioc(1, ty, nr, size) // _IOC_WRITE
}
const fn io(ty: u8, nr: u8) -> u64 {
    ioc(0, ty, nr, 0) // _IOC_NONE
}

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const REL_WHEEL: u16 = 0x08;
const REL_HWHEEL: u16 = 0x06;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;

// Keys used by this backend (see `key_code` / `char_code`).
const KEY_ESC: u16 = 1;
const KEY_1: u16 = 2;
const KEY_2: u16 = 3;
const KEY_3: u16 = 4;
const KEY_4: u16 = 5;
const KEY_5: u16 = 6;
const KEY_6: u16 = 7;
const KEY_7: u16 = 8;
const KEY_8: u16 = 9;
const KEY_9: u16 = 10;
const KEY_0: u16 = 11;
const KEY_MINUS: u16 = 12;
const KEY_EQUAL: u16 = 13;
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_Q: u16 = 16;
const KEY_W: u16 = 17;
const KEY_E: u16 = 18;
const KEY_R: u16 = 19;
const KEY_T: u16 = 20;
const KEY_Y: u16 = 21;
const KEY_U: u16 = 22;
const KEY_I: u16 = 23;
const KEY_O: u16 = 24;
const KEY_P: u16 = 25;
const KEY_LEFTBRACE: u16 = 26;
const KEY_RIGHTBRACE: u16 = 27;
const KEY_ENTER: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTALT: u16 = 56;
const KEY_A: u16 = 30;
const KEY_S: u16 = 31;
const KEY_D: u16 = 32;
const KEY_F: u16 = 33;
const KEY_G: u16 = 34;
const KEY_H: u16 = 35;
const KEY_J: u16 = 36;
const KEY_K: u16 = 37;
const KEY_L: u16 = 38;
const KEY_SEMICOLON: u16 = 39;
const KEY_APOSTROPHE: u16 = 40;
const KEY_GRAVE: u16 = 41;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_BACKSLASH: u16 = 43;
const KEY_Z: u16 = 44;
const KEY_X: u16 = 45;
const KEY_C: u16 = 46;
const KEY_V: u16 = 47;
const KEY_B: u16 = 48;
const KEY_N: u16 = 49;
const KEY_M: u16 = 50;
const KEY_COMMA: u16 = 51;
const KEY_DOT: u16 = 52;
const KEY_SLASH: u16 = 53;
const KEY_F1: u16 = 59;
const KEY_F2: u16 = 60;
const KEY_F3: u16 = 61;
const KEY_F4: u16 = 62;
const KEY_F5: u16 = 63;
const KEY_F6: u16 = 64;
const KEY_F7: u16 = 65;
const KEY_F8: u16 = 66;
const KEY_F9: u16 = 67;
const KEY_F10: u16 = 68;
const KEY_CAPSLOCK: u16 = 58;
const KEY_F11: u16 = 87;
const KEY_F12: u16 = 88;
const KEY_SYSRQ: u16 = 99;
const KEY_HOME: u16 = 102;
const KEY_UP: u16 = 103;
const KEY_PAGEUP: u16 = 104;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_END: u16 = 107;
const KEY_DOWN: u16 = 108;
const KEY_PAGEDOWN: u16 = 109;
const KEY_INSERT: u16 = 110;
const KEY_DELETE: u16 = 111;
const KEY_NUMLOCK: u16 = 69;
const KEY_SPACE: u16 = 57;
const KEY_LEFTMETA: u16 = 125;

const ABS_RANGE_MAX: u32 = 32767;

/// KEY_A..KEY_Z in kernel scan order — NOT a contiguous range (QWERTY
/// layout: A=30, B=48, C=46…). Never generate letter codes arithmetically.
const LETTER_CODES: [u16; 26] = [
    30, // A
    48, // B
    46, // C
    32, // D
    18, // E
    33, // F
    34, // G
    35, // H
    23, // I
    36, // J
    37, // K
    38, // L
    50, // M
    49, // N
    24, // O
    25, // P
    16, // Q
    19, // R
    31, // S
    20, // T
    22, // U
    47, // V
    17, // W
    45, // X
    21, // Y
    44, // Z
];

/// `struct input_id` (linux/input.h) — modern `UI_DEV_SETUP` payload part.
#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

/// `struct uinput_setup` (linux/uinput.h) — the `UI_DEV_SETUP` payload.
/// Modern kernels (4.4+) register the device via this ioctl; the name is
/// NUL-terminated within 80 bytes (UINPUT_MAX_NAME_SIZE).
#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [libc::c_char; 80],
    ff_effects_max: libc::c_int,
}
const _: () = assert!(size_of::<UinputSetup>() == 92);

/// `struct input_absinfo` (linux/input.h).
#[repr(C)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// `struct uinput_abs_setup` — per-axis config for `UI_ABS_SETUP`
/// (u16 code + 2 bytes padding before the 4-aligned absinfo).
#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsInfo,
}
const _: () = assert!(size_of::<UinputAbsSetup>() == 28);

/// True when the agent runs inside a native Wayland session (mirrors
/// `opacusr_capture::is_wayland_session` without pulling the capture deps).
pub fn is_wayland_session() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("WAYLAND_SOCKET").is_some()
    {
        return true;
    }
    matches!(std::env::var("XDG_SESSION_TYPE").as_deref(), Ok("wayland"))
}

/// One virtual evdev device (its own `/dev/uinput` fd).
struct UinputDevice {
    fd: RawFd,
}

impl UinputDevice {
    /// Create a device with the given name and event-bit sets, then
    /// register it (`UI_DEV_CREATE`); libinput sees it immediately.
    fn new(
        name: &str,
        keys: &[u16],
        rels: &[u16],
        abs: &[(u16, u32, u32)], // (axis, min, max)
    ) -> Result<Self, InputError> {
        let fd = unsafe { libc::open(c"/dev/uinput".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(InputError::Backend(format!(
                "open /dev/uinput: {} (needs write access — 'input' group or the packaged udev rule)",
                std::io::Error::last_os_error()
            )));
        }
        let err = |what: &str| -> InputError {
            InputError::Backend(format!(
                "uinput {what}: {}",
                std::io::Error::last_os_error()
            ))
        };
        let mut ok =
            unsafe { libc::ioctl(fd, UI_SET_EVBIT as libc::c_ulong, EV_KEY as libc::c_int) == 0 };
        for &k in keys {
            ok &= unsafe { libc::ioctl(fd, UI_SET_KEYBIT as libc::c_ulong, k as libc::c_int) == 0 };
        }
        for &r in rels {
            ok &= unsafe { libc::ioctl(fd, UI_SET_RELBIT as libc::c_ulong, r as libc::c_int) == 0 };
        }
        if !abs.is_empty() {
            ok &= unsafe {
                libc::ioctl(fd, UI_SET_EVBIT as libc::c_ulong, EV_ABS as libc::c_int) == 0
            };
        }
        for &(axis, ..) in abs {
            ok &= unsafe {
                libc::ioctl(fd, UI_SET_ABSBIT as libc::c_ulong, axis as libc::c_int) == 0
            };
        }
        // Modern registration path (kernel 4.4+): per-axis absinfo, then
        // the device identity, then UI_DEV_CREATE — no legacy write.
        for &(axis, min, max) in abs {
            let abs_setup = UinputAbsSetup {
                code: axis,
                absinfo: InputAbsInfo {
                    value: 0,
                    minimum: min as i32,
                    maximum: max as i32,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            };
            ok &= unsafe {
                libc::ioctl(
                    fd,
                    UI_ABS_SETUP as libc::c_ulong,
                    &abs_setup as *const UinputAbsSetup as *const libc::c_void,
                ) == 0
            };
        }
        let mut setup = UinputSetup {
            // BUS_VIRTUAL: a synthetic device, exactly what this is.
            id: InputId {
                bustype: 0x06, // BUS_VIRTUAL
                vendor: 0x0001,
                product: if abs.is_empty() { 2 } else { 1 }, // 1 pointer / 2 keyboard
                version: 1,
            },
            name: [0; 80],
            ff_effects_max: 0,
        };
        let bytes = name.as_bytes();
        let n = bytes.len().min(79);
        for (dst, src) in setup.name[..n].iter_mut().zip(bytes.iter()) {
            *dst = *src as libc::c_char;
        }
        ok &= unsafe {
            libc::ioctl(
                fd,
                UI_DEV_SETUP as libc::c_ulong,
                &setup as *const UinputSetup as *const libc::c_void,
            ) == 0
        };
        ok &= unsafe { libc::ioctl(fd, UI_DEV_CREATE as libc::c_ulong) == 0 };
        if !ok {
            unsafe { libc::close(fd) };
            return Err(err("setup"));
        }
        Ok(Self { fd })
    }

    /// Emit `type/code/value` events followed by a SYN report.
    fn emit(&self, events: &[(u16, u16, i32)]) -> Result<(), InputError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let stamp = libc::timeval {
            tv_sec: now.as_secs() as libc::time_t,
            tv_usec: now.subsec_micros() as libc::suseconds_t,
        };
        let mut ev = libc::input_event {
            time: stamp,
            type_: 0,
            code: 0,
            value: 0,
        };
        for &(t, code, value) in events {
            ev.type_ = t;
            ev.code = code;
            ev.value = value;
            let n = unsafe {
                libc::write(
                    self.fd,
                    &ev as *const libc::input_event as *const libc::c_void,
                    size_of::<libc::input_event>(),
                )
            };
            if n != size_of::<libc::input_event>() as isize {
                return Err(InputError::Backend(format!(
                    "uinput write: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
        ev.type_ = EV_SYN;
        ev.code = 0; // SYN_REPORT
        ev.value = 0;
        let n = unsafe {
            libc::write(
                self.fd,
                &ev as *const libc::input_event as *const libc::c_void,
                size_of::<libc::input_event>(),
            )
        };
        if n != size_of::<libc::input_event>() as isize {
            return Err(InputError::Backend(format!(
                "uinput syn: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

impl Drop for UinputDevice {
    fn drop(&mut self) {
        // Best effort: the kernel also destroys devices when the fd closes.
        unsafe {
            libc::ioctl(self.fd, UI_DEV_DESTROY as libc::c_ulong);
            libc::close(self.fd);
        }
    }
}

/// Wayland-native injector: two virtual devices (absolute pointer + keyboard).
pub struct UinputInjector {
    pointer: UinputDevice,
    keyboard: UinputDevice,
    viewport: (u32, u32),
    /// Streamed output's position inside the desktop (multi-monitor);
    /// `None` = frame-is-the-bbox single-monitor mapping.
    mapping: Option<OutputMapping>,
}

impl UinputInjector {
    pub fn new() -> Result<Self, InputError> {
        let pointer = UinputDevice::new(
            "OpacusR virtual pointer",
            &[BTN_LEFT, BTN_RIGHT, BTN_MIDDLE],
            &[REL_WHEEL, REL_HWHEEL],
            &[(ABS_X, 0, ABS_RANGE_MAX), (ABS_Y, 0, ABS_RANGE_MAX)],
        )?;
        let keyboard = UinputDevice::new("OpacusR virtual keyboard", KEYBOARD_KEYS, &[], &[])?;
        Ok(Self {
            pointer,
            keyboard,
            viewport: (1920, 1080),
            mapping: None,
        })
    }

    /// Device px (0..viewport) → normalized ABS range (0..32767).
    fn to_abs(v: u32, span: u32) -> i32 {
        if span <= 1 {
            return 0;
        }
        (((v.min(span - 1)) as u64 * ABS_RANGE_MAX as u64) / (span - 1) as u64) as i32
    }

    /// ABS values for a frame-relative pointer position: through the
    /// per-output mapping when we have one, else the v1 frame-is-the-bbox
    /// normalization.
    fn abs_at(&self, x: u32, y: u32) -> (i32, i32) {
        let (w, h) = self.viewport;
        match self.mapping {
            Some(m) => frame_to_desktop_abs(&m, x, y, w, h, ABS_RANGE_MAX),
            None => (Self::to_abs(x, w), Self::to_abs(y, h)),
        }
    }

    fn btn_code(b: MouseButtonId) -> u16 {
        match b {
            MouseButtonId::Left => BTN_LEFT,
            MouseButtonId::Right => BTN_RIGHT,
            MouseButtonId::Middle => BTN_MIDDLE,
        }
    }

    fn key_code(key: OpKey) -> Result<u16, InputError> {
        let k = match key {
            OpKey::Enter => KEY_ENTER,
            OpKey::Escape => KEY_ESC,
            OpKey::Tab => KEY_TAB,
            OpKey::Backspace => KEY_BACKSPACE,
            OpKey::Delete => KEY_DELETE,
            OpKey::Insert => KEY_INSERT,
            OpKey::Home => KEY_HOME,
            OpKey::End => KEY_END,
            OpKey::PageUp => KEY_PAGEUP,
            OpKey::PageDown => KEY_PAGEDOWN,
            OpKey::ArrowUp => KEY_UP,
            OpKey::ArrowDown => KEY_DOWN,
            OpKey::ArrowLeft => KEY_LEFT,
            OpKey::ArrowRight => KEY_RIGHT,
            OpKey::Space => KEY_SPACE,
            OpKey::F1 => KEY_F1,
            OpKey::F2 => KEY_F2,
            OpKey::F3 => KEY_F3,
            OpKey::F4 => KEY_F4,
            OpKey::F5 => KEY_F5,
            OpKey::F6 => KEY_F6,
            OpKey::F7 => KEY_F7,
            OpKey::F8 => KEY_F8,
            OpKey::F9 => KEY_F9,
            OpKey::F10 => KEY_F10,
            OpKey::F11 => KEY_F11,
            OpKey::F12 => KEY_F12,
            OpKey::CapsLock => KEY_CAPSLOCK,
            // Characters go through the US-layout char map (see `tap_char`).
            OpKey::Char(_) => return Err(InputError::UnsupportedKey(key)),
            OpKey::NumLock => KEY_NUMLOCK,
            OpKey::PrintScreen => KEY_SYSRQ,
            OpKey::ContextMenu => return Err(InputError::UnsupportedKey(OpKey::ContextMenu)),
        };
        Ok(k)
    }

    /// US-layout mapping for a character → (key code, needs Shift).
    fn char_code(c: char) -> Option<(u16, bool)> {
        let shifted = |base: u16| Some((base, true));
        let plain = |base: u16| Some((base, false));
        match c {
            'a'..='z' => plain(LETTER_CODES[(c as u16 - 'a' as u16) as usize]),
            'A'..='Z' => shifted(LETTER_CODES[(c as u16 - 'A' as u16) as usize]),
            '1' => plain(KEY_1),
            '2' => plain(KEY_2),
            '3' => plain(KEY_3),
            '4' => plain(KEY_4),
            '5' => plain(KEY_5),
            '6' => plain(KEY_6),
            '7' => plain(KEY_7),
            '8' => plain(KEY_8),
            '9' => plain(KEY_9),
            '0' => plain(KEY_0),
            '!' => shifted(KEY_1),
            '@' => shifted(KEY_2),
            '#' => shifted(KEY_3),
            '$' => shifted(KEY_4),
            '%' => shifted(KEY_5),
            '^' => shifted(KEY_6),
            '&' => shifted(KEY_7),
            '*' => shifted(KEY_8),
            '(' => shifted(KEY_9),
            ')' => shifted(KEY_0),
            '`' => plain(KEY_GRAVE),
            '~' => shifted(KEY_GRAVE),
            '-' => plain(KEY_MINUS),
            '_' => shifted(KEY_MINUS),
            '=' => plain(KEY_EQUAL),
            '+' => shifted(KEY_EQUAL),
            '[' => plain(KEY_LEFTBRACE),
            '{' => shifted(KEY_LEFTBRACE),
            ']' => plain(KEY_RIGHTBRACE),
            '}' => shifted(KEY_RIGHTBRACE),
            '\\' => plain(KEY_BACKSLASH),
            '|' => shifted(KEY_BACKSLASH),
            ';' => plain(KEY_SEMICOLON),
            ':' => shifted(KEY_SEMICOLON),
            '\'' => plain(KEY_APOSTROPHE),
            '"' => shifted(KEY_APOSTROPHE),
            ',' => plain(KEY_COMMA),
            '<' => shifted(KEY_COMMA),
            '.' => plain(KEY_DOT),
            '>' => shifted(KEY_DOT),
            '/' => plain(KEY_SLASH),
            '?' => shifted(KEY_SLASH),
            ' ' => plain(KEY_SPACE),
            '\n' | '\r' => plain(KEY_ENTER),
            '\t' => plain(KEY_TAB),
            _ => None,
        }
    }

    /// Press/release one modifier bit (mirrors the X11 backend's contract).
    fn apply_mods(&mut self, m: u8, down: bool) -> Result<(), InputError> {
        let pairs: &[(u8, u16)] = &[
            (mods::SHIFT, KEY_LEFTSHIFT),
            (mods::CTRL, KEY_LEFTCTRL),
            (mods::ALT, KEY_LEFTALT),
            (mods::META, KEY_LEFTMETA),
        ];
        for (bit, code) in pairs {
            if m & bit != 0 {
                self.keyboard
                    .emit(&[(EV_KEY, *code, if down { 1 } else { 0 })])?;
            }
        }
        Ok(())
    }

    fn tap_char(&mut self, c: char, m: u8) -> Result<(), InputError> {
        let (code, needs_shift) =
            UinputInjector::char_code(c).ok_or(InputError::UnsupportedKey(OpKey::Char(c)))?;
        // Shift is already down when the viewer reports it in `m` — pressing
        // it again would desynchronize the press/release balance.
        let shift_here = needs_shift && m & mods::SHIFT == 0;
        if shift_here {
            self.keyboard.emit(&[(EV_KEY, KEY_LEFTSHIFT, 1)])?;
        }
        self.keyboard.emit(&[(EV_KEY, code, 1)])?;
        self.keyboard.emit(&[(EV_KEY, code, 0)])?;
        if shift_here {
            self.keyboard.emit(&[(EV_KEY, KEY_LEFTSHIFT, 0)])?;
        }
        Ok(())
    }
}

/// Every key code the keyboard device must accept up front.
const KEYBOARD_KEYS: &[u16] = &[
    KEY_ESC,
    KEY_1,
    KEY_2,
    KEY_3,
    KEY_4,
    KEY_5,
    KEY_6,
    KEY_7,
    KEY_8,
    KEY_9,
    KEY_0,
    KEY_MINUS,
    KEY_EQUAL,
    KEY_BACKSPACE,
    KEY_TAB,
    KEY_Q,
    KEY_W,
    KEY_E,
    KEY_R,
    KEY_T,
    KEY_Y,
    KEY_U,
    KEY_I,
    KEY_O,
    KEY_P,
    KEY_LEFTBRACE,
    KEY_RIGHTBRACE,
    KEY_ENTER,
    KEY_LEFTCTRL,
    KEY_LEFTALT,
    KEY_A,
    KEY_S,
    KEY_D,
    KEY_F,
    KEY_G,
    KEY_H,
    KEY_J,
    KEY_K,
    KEY_L,
    KEY_SEMICOLON,
    KEY_APOSTROPHE,
    KEY_GRAVE,
    KEY_LEFTSHIFT,
    KEY_BACKSLASH,
    KEY_Z,
    KEY_X,
    KEY_C,
    KEY_V,
    KEY_B,
    KEY_N,
    KEY_M,
    KEY_COMMA,
    KEY_DOT,
    KEY_SLASH,
    KEY_SPACE,
    KEY_F1,
    KEY_F2,
    KEY_F3,
    KEY_F4,
    KEY_F5,
    KEY_F6,
    KEY_F7,
    KEY_F8,
    KEY_F9,
    KEY_F10,
    KEY_F11,
    KEY_F12,
    KEY_CAPSLOCK,
    KEY_NUMLOCK,
    KEY_SYSRQ,
    KEY_HOME,
    KEY_UP,
    KEY_PAGEUP,
    KEY_LEFT,
    KEY_RIGHT,
    KEY_END,
    KEY_DOWN,
    KEY_PAGEDOWN,
    KEY_INSERT,
    KEY_DELETE,
    KEY_LEFTMETA,
];

impl InputInjector for UinputInjector {
    fn set_viewport(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.viewport = (w, h);
        }
    }

    fn set_output_mapping(&mut self, mapping: Option<OutputMapping>) {
        self.mapping = mapping;
    }

    fn mouse_move(&mut self, x: u32, y: u32) -> Result<(), InputError> {
        let (ax, ay) = self.abs_at(x, y);
        self.pointer
            .emit(&[(EV_ABS, ABS_X, ax), (EV_ABS, ABS_Y, ay)])
    }

    fn mouse_button(
        &mut self,
        x: u32,
        y: u32,
        button: MouseButtonId,
        down: bool,
    ) -> Result<(), InputError> {
        let (ax, ay) = self.abs_at(x, y);
        self.pointer.emit(&[
            (EV_ABS, ABS_X, ax),
            (EV_ABS, ABS_Y, ay),
            (EV_KEY, Self::btn_code(button), if down { 1 } else { 0 }),
        ])
    }

    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        // Protocol: positive = up / right. REL_WHEEL/REL_HWHEEL: positive =
        // up / right. The viewer batches whole 8-px notches, one notch maps
        // to one wheel count.
        let mut events = Vec::with_capacity(2);
        if dy != 0 {
            events.push((EV_REL, REL_WHEEL, dy));
        }
        if dx != 0 {
            events.push((EV_REL, REL_HWHEEL, dx));
        }
        if events.is_empty() {
            return Ok(());
        }
        self.pointer.emit(&events)
    }

    fn key(&mut self, key: OpKey, m: u8, down: bool) -> Result<(), InputError> {
        if down && m != 0 {
            self.apply_mods(m, true)?;
        }
        let res = match key {
            OpKey::Char(c) => {
                if down {
                    self.tap_char(c, m)
                } else {
                    // Character releases carry no codes to release (the tap
                    // already pressed+released); balancing the modifiers the
                    // viewer reports is still ours to do below.
                    Ok(())
                }
            }
            _ => {
                let code = Self::key_code(key)?;
                self.keyboard
                    .emit(&[(EV_KEY, code, if down { 1 } else { 0 })])
            }
        };
        if !down && m != 0 {
            self.apply_mods(m, false)?;
        }
        res
    }

    fn text(&mut self, text: &str) -> Result<(), InputError> {
        // Pre-validate: type nothing when anything is untypeable, so a
        // Unicode string never leaves a half-typed ASCII prefix behind.
        if let Some(c) = text
            .chars()
            .find(|c| UinputInjector::char_code(*c).is_none())
        {
            return Err(InputError::Backend(format!(
                "text: '{c}' needs a Wayland IME path (uinput types ASCII only)"
            )));
        }
        for c in text.chars() {
            self.tap_char(c, 0)?;
        }
        Ok(())
    }
}

/// Translate a frame-relative device pixel of the streamed output into
/// the desktop ABS range. Path: frame px → output-logical px (the frame
/// covers exactly the output, so the scale is `logical / frame`) →
/// offset inside the desktop bounding box → normalized [0, 1] → ABS.
/// The compositor maps ABS a/32767 linearly over the bbox, so the
/// normalized position is the CONTINUOUS fraction `(logical − bbox) /
/// bbox` — no `span − 1` pixel-index convention here (that one is kept
/// only in the legacy `to_abs` fallback path, ≤ 1 device px off).
/// Out-of-frame positions clamp, mirroring `to_abs`.
fn frame_to_desktop_abs(
    m: &OutputMapping,
    x: u32,
    y: u32,
    vw: u32,
    vh: u32,
    abs_max: u32,
) -> (i32, i32) {
    // frame px → logical px on the streamed output
    let fx = if vw > 0 {
        (x.min(vw - 1)) as f64 * m.logical_w as f64 / vw as f64
    } else {
        0.0
    };
    let fy = if vh > 0 {
        (y.min(vh - 1)) as f64 * m.logical_h as f64 / vh as f64
    } else {
        0.0
    };
    let lx = m.out_x as f64 + fx;
    let ly = m.out_y as f64 + fy;
    // logical position inside the desktop bbox → continuous [0, 1] → ABS
    let norm = |v: f64, origin: i32, span: u32| -> f64 {
        if span > 0 {
            ((v - origin as f64) / span as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let nx = norm(lx, m.bbox_x, m.bbox_w);
    let ny = norm(ly, m.bbox_y, m.bbox_h);
    (
        (nx * abs_max as f64).floor() as i32,
        (ny * abs_max as f64).floor() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected(c: char) -> (u16, bool) {
        UinputInjector::char_code(c).unwrap_or_else(|| panic!("char {c:?} must map"))
    }

    #[test]
    fn ioctl_encoding_matches_kernel_constants() {
        // Values straight from linux/uinput.h (UINPUT_IOCTL_BASE = 'U').
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
        assert_eq!(UI_SET_RELBIT, 0x4004_5566);
        assert_eq!(UI_SET_ABSBIT, 0x4004_5567);
        assert_eq!(UI_DEV_CREATE, 0x0000_5501);
        assert_eq!(UI_DEV_DESTROY, 0x0000_5502);
        assert_eq!(UI_DEV_SETUP, 0x405C_5503); // _IOW('U', 3, uinput_setup[92])
        assert_eq!(UI_ABS_SETUP, 0x401C_5504); // _IOW('U', 4, uinput_abs_setup[28])
    }

    #[test]
    fn setup_struct_layout_matches_kernel() {
        // struct uinput_setup { input_id; name[80]; ff_effects_max; }
        assert_eq!(size_of::<InputId>(), 8);
        assert_eq!(size_of::<UinputSetup>(), 92);
        // struct uinput_abs_setup { __u16 code; struct input_absinfo; }
        assert_eq!(size_of::<InputAbsInfo>(), 24);
        assert_eq!(size_of::<UinputAbsSetup>(), 28);
    }

    #[test]
    fn us_layout_letters_and_digits() {
        assert_eq!(expected('a'), (KEY_A, false));
        assert_eq!(expected('z'), (KEY_Z, false));
        assert_eq!(expected('A'), (KEY_A, true));
        assert_eq!(expected('Q'), (KEY_Q, true));
        assert_eq!(expected('1'), (KEY_1, false));
        assert_eq!(expected('0'), (KEY_0, false));
        assert_eq!(expected(' '), (KEY_SPACE, false));
        assert_eq!(expected('\n'), (KEY_ENTER, false));
        assert_eq!(expected('\r'), (KEY_ENTER, false));
        assert_eq!(expected('\t'), (KEY_TAB, false));
    }

    #[test]
    fn us_layout_punctuation() {
        assert_eq!(expected('!'), (KEY_1, true));
        assert_eq!(expected('@'), (KEY_2, true));
        assert_eq!(expected('('), (KEY_9, true));
        assert_eq!(expected(')'), (KEY_0, true));
        assert_eq!(expected('-'), (KEY_MINUS, false));
        assert_eq!(expected('_'), (KEY_MINUS, true));
        assert_eq!(expected('='), (KEY_EQUAL, false));
        assert_eq!(expected('+'), (KEY_EQUAL, true));
        assert_eq!(expected('['), (KEY_LEFTBRACE, false));
        assert_eq!(expected('{'), (KEY_LEFTBRACE, true));
        assert_eq!(expected(']'), (KEY_RIGHTBRACE, false));
        assert_eq!(expected('}'), (KEY_RIGHTBRACE, true));
        assert_eq!(expected('\\'), (KEY_BACKSLASH, false));
        assert_eq!(expected('|'), (KEY_BACKSLASH, true));
        assert_eq!(expected(';'), (KEY_SEMICOLON, false));
        assert_eq!(expected(':'), (KEY_SEMICOLON, true));
        assert_eq!(expected('\''), (KEY_APOSTROPHE, false));
        assert_eq!(expected('"'), (KEY_APOSTROPHE, true));
        assert_eq!(expected('`'), (KEY_GRAVE, false));
        assert_eq!(expected('~'), (KEY_GRAVE, true));
        assert_eq!(expected(','), (KEY_COMMA, false));
        assert_eq!(expected('<'), (KEY_COMMA, true));
        assert_eq!(expected('.'), (KEY_DOT, false));
        assert_eq!(expected('>'), (KEY_DOT, true));
        assert_eq!(expected('/'), (KEY_SLASH, false));
        assert_eq!(expected('?'), (KEY_SLASH, true));
    }

    #[test]
    fn every_printable_ascii_char_maps() {
        for b in 0x20u8..0x7f {
            let c = b as char;
            assert!(UinputInjector::char_code(c).is_some(), "missing {c:?}");
        }
        assert!(
            UinputInjector::char_code('\u{0436}').is_none(),
            "кириллица unsupported"
        );
        assert!(
            UinputInjector::char_code('\u{7f}').is_none(),
            "DEL unsupported"
        );
    }

    #[test]
    fn letter_row_is_not_a_plain_range() {
        // KEY_A..KEY_Z is NOT contiguous (QWERTY scan layout) — the char
        // table must never be generated by a naive range.
        assert_ne!(KEY_A + 1, KEY_B);
        assert_ne!(KEY_Z - 1, KEY_Y);
        for (i, code) in LETTER_CODES.iter().enumerate() {
            let c = (b'a' + i as u8) as char;
            let (got, sh) = UinputInjector::char_code(c).unwrap();
            assert_eq!(got, *code, "letter {c}");
            assert!(!sh);
            let (got, sh) = UinputInjector::char_code(c.to_ascii_uppercase()).unwrap();
            assert_eq!(got, *code, "letter {}", c.to_ascii_uppercase());
            assert!(sh);
        }
        assert_eq!(UinputInjector::char_code('b').unwrap().0, KEY_B);
        assert_eq!(UinputInjector::char_code('q').unwrap().0, KEY_Q);
        assert_eq!(UinputInjector::char_code('z').unwrap().0, KEY_Z);
    }

    #[test]
    fn opkey_table_covers_all_variants() {
        use OpKey::*;
        let ok: &[OpKey] = &[
            Enter,
            Escape,
            Tab,
            Backspace,
            Delete,
            Insert,
            Home,
            End,
            PageUp,
            PageDown,
            ArrowUp,
            ArrowDown,
            ArrowLeft,
            ArrowRight,
            Space,
            F1,
            F2,
            F3,
            F4,
            F5,
            F6,
            F7,
            F8,
            F9,
            F10,
            F11,
            F12,
            CapsLock,
            NumLock,
            PrintScreen,
        ];
        for k in ok {
            assert!(UinputInjector::key_code(*k).is_ok(), "no code for {k:?}");
        }
        assert!(UinputInjector::key_code(ContextMenu).is_err());
    }

    #[test]
    fn abs_scaling_normalizes_viewport() {
        assert_eq!(UinputInjector::to_abs(0, 2560), 0);
        assert_eq!(UinputInjector::to_abs(2559, 2560), ABS_RANGE_MAX as i32);
        // Half of the screen: pixel 1279.5 is fractional — the linear
        // span-1 mapping lands a few counts off the exact midpoint.
        let half = UinputInjector::to_abs(1279, 2560);
        assert!(
            (half - ABS_RANGE_MAX as i32 / 2).abs() <= 8,
            "midpoint drift {half}"
        );
        // Degenerate viewports clamp instead of dividing by zero.
        assert_eq!(UinputInjector::to_abs(5, 1), 0);
        assert_eq!(UinputInjector::to_abs(5, 0), 0);
        // Values beyond the viewport clamp into range.
        assert_eq!(UinputInjector::to_abs(9999, 2560), ABS_RANGE_MAX as i32);
    }

    fn mapping(out_x: i32, out_y: i32, lw: u32, lh: u32, bw: u32, bh: u32) -> OutputMapping {
        OutputMapping {
            out_x,
            out_y,
            logical_w: lw,
            logical_h: lh,
            bbox_x: 0,
            bbox_y: 0,
            bbox_w: bw,
            bbox_h: bh,
        }
    }

    #[test]
    fn desktop_mapping_matches_single_monitor_behavior() {
        // One monitor: bbox == output, frame == device px at scale 1.
        // The continuous desktop mapping and the legacy span-1 fallback
        // differ by ≤ 1 device px (= ABS_RANGE_MAX/span counts) — assert
        // that, not bit equality; both land within the same pixel.
        let m = mapping(0, 0, 2560, 1440, 2560, 1440);
        // Legacy drift ≤ 1 device px = ABS_MAX/span counts per axis.
        let tol_x = (ABS_RANGE_MAX as i64 / 2560) as i64 + 1;
        let tol_y = (ABS_RANGE_MAX as i64 / 1440) as i64 + 1;
        for (x, y) in [(0u32, 0u32), (1280, 720), (2559, 1439), (640, 100)] {
            let (ax, ay) = frame_to_desktop_abs(&m, x, y, 2560, 1440, ABS_RANGE_MAX);
            let (lx, ly) = (
                UinputInjector::to_abs(x, 2560) as i64,
                UinputInjector::to_abs(y, 1440) as i64,
            );
            assert!((ax as i64 - lx).abs() <= tol_x, "x for {x}: {ax} vs {lx}");
            assert!((ay as i64 - ly).abs() <= tol_y, "y for {y}: {ay} vs {ly}");
        }
    }

    #[test]
    fn desktop_mapping_lands_on_the_right_monitor() {
        // Two side-by-side 1920x1080 monitors; streaming the RIGHT one.
        // Frame center must hit the desktop's ¾ mark, not the middle.
        let m = mapping(1920, 0, 1920, 1080, 3840, 1080);
        let (ax, ay) = frame_to_desktop_abs(&m, 960, 540, 1920, 1080, ABS_RANGE_MAX);
        let three_quarter = (ABS_RANGE_MAX as f64 * 0.75).round() as i32;
        // The continuous mapping lands on the geometric fraction exactly;
        // ±2 counts absorbs the floor.
        assert!(
            (ax - three_quarter).abs() <= 2,
            "x {ax} vs ¾ {three_quarter}"
        );
        assert!((ay - ABS_RANGE_MAX as i32 / 2).abs() <= 2, "y {ay}");

        // Left edge of the streamed right monitor = desktop middle.
        let (ax, _) = frame_to_desktop_abs(&m, 0, 0, 1920, 1080, ABS_RANGE_MAX);
        assert!((ax - ABS_RANGE_MAX as i32 / 2).abs() <= 8, "left edge {ax}");
        // Right edge = within one device px of ABS max (the continuous
        // fraction maps the last pixel INDEX slightly inside the edge —
        // the position itself is continuous, this is sub-pixel).
        let (ax, _) = frame_to_desktop_abs(&m, 1919, 1079, 1920, 1080, ABS_RANGE_MAX);
        assert!((ABS_RANGE_MAX as i32 - ax) <= 10, "right edge {ax}");
    }

    #[test]
    fn desktop_mapping_scales_retina_frames() {
        // The right monitor streams at 2x device px (frame 3840x2160 for
        // a 1920x1080 logical output): frame center still lands at ¾.
        let m = mapping(1920, 0, 1920, 1080, 3840, 1080);
        let (ax, ay) = frame_to_desktop_abs(&m, 1920, 1080, 3840, 2160, ABS_RANGE_MAX);
        let three_quarter = (ABS_RANGE_MAX as f64 * 0.75).round() as i32;
        assert!((ax - three_quarter).abs() <= 2, "x {ax}");
        assert!((ay - ABS_RANGE_MAX as i32 / 2).abs() <= 2, "y {ay}");
    }

    #[test]
    fn desktop_mapping_clamps_stray_coordinates() {
        // Coordinates beyond the streamed frame (viewer rescale races)
        // clamp into the frame, so the ABS value stays inside the bbox —
        // within one device px of the edge, never outside.
        let m = mapping(1920, 0, 1920, 1080, 3840, 1080);
        let (ax, ay) = frame_to_desktop_abs(&m, 9999, 9999, 1920, 1080, ABS_RANGE_MAX);
        // One device px per axis = ABS_MAX/span counts.
        assert!(
            (ABS_RANGE_MAX as i64 - ax as i64) <= ABS_RANGE_MAX as i64 / 3840 + 1,
            "x {ax}"
        );
        assert!(
            (ABS_RANGE_MAX as i64 - ay as i64) <= ABS_RANGE_MAX as i64 / 1080 + 1,
            "y {ay}"
        );
    }

    #[test]
    fn desktop_mapping_handles_negative_and_gapped_layouts() {
        // Left monitor at (-1920, -600), gap, right monitor at (0, 480);
        // streaming the LEFT one: its center is the desktop's ¼ mark on
        // both axes.
        let m = OutputMapping {
            out_x: -1920,
            out_y: -600,
            logical_w: 1920,
            logical_h: 1080,
            bbox_x: -1920,
            bbox_y: -600,
            bbox_w: 3840,
            bbox_h: 2160,
        };
        let (ax, ay) = frame_to_desktop_abs(&m, 960, 540, 1920, 1080, ABS_RANGE_MAX);
        let quarter = (ABS_RANGE_MAX as f64 * 0.25).round() as i32;
        assert!((ax - quarter).abs() <= 2, "x {ax} vs ¼ {quarter}");
        assert!((ay - quarter).abs() <= 2, "y {ay} vs ¼ {quarter}");
        // Top-left of the layout maps to ABS 0 on both axes.
        let (ax, ay) = frame_to_desktop_abs(&m, 0, 0, 1920, 1080, ABS_RANGE_MAX);
        assert_eq!((ax, ay), (0, 0));
    }

    #[test]
    fn keyboard_keys_list_covers_every_emitted_code() {
        // Anything emitted must have been registered via UI_SET_KEYBIT,
        // otherwise the kernel drops the event silently.
        let emitted: &[u16] = &[
            KEY_ESC,
            KEY_1,
            KEY_2,
            KEY_3,
            KEY_4,
            KEY_5,
            KEY_6,
            KEY_7,
            KEY_8,
            KEY_9,
            KEY_0,
            KEY_MINUS,
            KEY_EQUAL,
            KEY_BACKSPACE,
            KEY_TAB,
            KEY_Q,
            KEY_W,
            KEY_E,
            KEY_R,
            KEY_T,
            KEY_Y,
            KEY_U,
            KEY_I,
            KEY_O,
            KEY_P,
            KEY_LEFTBRACE,
            KEY_RIGHTBRACE,
            KEY_ENTER,
            KEY_LEFTCTRL,
            KEY_LEFTALT,
            KEY_A,
            KEY_S,
            KEY_D,
            KEY_F,
            KEY_G,
            KEY_H,
            KEY_J,
            KEY_K,
            KEY_L,
            KEY_SEMICOLON,
            KEY_APOSTROPHE,
            KEY_GRAVE,
            KEY_LEFTSHIFT,
            KEY_BACKSLASH,
            KEY_Z,
            KEY_X,
            KEY_C,
            KEY_V,
            KEY_B,
            KEY_N,
            KEY_M,
            KEY_COMMA,
            KEY_DOT,
            KEY_SLASH,
            KEY_SPACE,
            KEY_F1,
            KEY_F2,
            KEY_F3,
            KEY_F4,
            KEY_F5,
            KEY_F6,
            KEY_F7,
            KEY_F8,
            KEY_F9,
            KEY_F10,
            KEY_F11,
            KEY_F12,
            KEY_CAPSLOCK,
            KEY_NUMLOCK,
            KEY_SYSRQ,
            KEY_HOME,
            KEY_UP,
            KEY_PAGEUP,
            KEY_LEFT,
            KEY_RIGHT,
            KEY_END,
            KEY_DOWN,
            KEY_PAGEDOWN,
            KEY_INSERT,
            KEY_DELETE,
            KEY_LEFTMETA,
        ];
        for code in emitted {
            assert!(KEYBOARD_KEYS.contains(code), "unregistered key {code}");
        }
    }
}
