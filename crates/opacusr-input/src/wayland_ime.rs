//! Native-Wayland Unicode text injection (zwp_virtual_keyboard_v1).
//!
//! uinput types ASCII only: the compositor resolves evdev keycodes through
//! the system keymap, which has no entries for arbitrary Unicode. This
//! module closes that gap — the same one the Android IME batch closed on
//! phones — with the approach `wtype` proved in the field: create a SECOND
//! virtual keyboard through the Wayland virtual-keyboard protocol and push
//! a self-generated XKB keymap in which a block of keycodes maps to the
//! Unicode keysyms of the string being typed, then tap those keycodes like
//! any keyboard. No grabs, no activation dance, no input-method contract:
//! the compositor sees one more keyboard whose keymap happens to speak
//! emoji.
//!
//! Ground truths this module encodes (all verified against the proven
//! implementations / shipped data, not invented):
//! - the `key` request takes the EVDEV keycode; the XKB keymap declares
//!   the same key at evdev + 8 (wtype's WAYLAND 142 / XKB 150 offsets);
//! - keysyms are written NUMERICALLY (classic XKB grammar; the shipped
//!   xkeyboard-config does the same — `symbols/keypad` uses `0x10000D7`
//!   and friends), so no keysym-name table is needed;
//! - keysym value = codepoint for latin-1 (0x20..=0xff) and
//!   `0x01000000 | codepoint` above (the documented 0x100..=0x10ffff
//!   range); `\u{1b}` maps to Escape (0xff1b) — `\n`/`\t` never get here,
//!   uinput taps them;
//! - XKB keycodes stop at 255, so one keymap carries ≤ 100 custom keys
//!   (slots 150..249); longer strings are typed in chunks, each with a
//!   fresh keymap — repeated `keymap` requests on one keyboard are legal
//!   (wtype re-uploads mid-stream the same way);
//! - the keymap text mirrors wtype's shape minus the standard-layout
//!   includes (this device never types standard keys — uinput does);
//!   `include "complete"` stays for types/compatibility, it exists on
//!   every desktop (xkeyboard-config);
//! - a trailing NUL byte and the byte length go with the keymap fd, and
//!   every request is followed by a blocking roundtrip so the compositor
//!   has compiled/processed it before the next event (wtype's rhythm).
//!
//! One short-lived connection per `text()` call — the same shape as the
//! capture crate's layout probe: no global state, no locks, paste-scale
//! latency. Compositor support: wlroots family + KWin expose the
//! virtual-keyboard manager to clients; where it is missing (no
//! `WAYLAND_DISPLAY`, no global, portal-restricted compositors) we return
//! an error BEFORE anything is typed, so the session keeps the honest
//! pre-IME behavior instead of a half-typed prefix.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicU32, Ordering};

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};

use crate::uinput::UinputInjector;
use crate::InputError;

use zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

/// First evdev keycode of the custom block (wtype's proven offset — above
/// the keys the standard layout actually uses).
const EVDEV_BASE: u32 = 142;
/// XKB keycode = evdev + 8 (the wl_keyboard offset the compositor applies).
const XKB_BASE: u32 = EVDEV_BASE + 8;
/// XKB keycodes stop at 255 → 150..=249 usable; chunk at 100 unique chars.
const CHUNK_CHARS: usize = 100;
/// `wl_keyboard.keymap_format.xkb` (the XML arg is a bare uint here).
const KEYMAP_FORMAT_XKB: u32 = 1;
/// `wl_keyboard.key_state` pressed/released.
const KEY_PRESSED: u32 = 1;
const KEY_RELEASED: u32 = 0;

/// The XKB keysym for a char the uinput tap path can't type, written
/// numerically into the keymap text. Control chars other than Escape have
/// no keysym here — callers pre-validate with this fn before typing
/// anything.
pub(crate) fn keysym(c: char) -> Option<u32> {
    match c {
        // Escape has no uinput keycode in `char_code`; the keymap carries
        // the standard Escape keysym.
        '\u{1b}' => Some(0xff1b),
        _ => {
            let cp = c as u32;
            match cp {
                // ASCII printables ride latin-1 keysyms (keysym = cp).
                0x20..=0x7e => Some(cp),
                // latin-1 printables: keysymdef has NO entries between
                // 0x7f and 0xa0 (DEL + C1 controls are untypable here).
                0xa0..=0xff => Some(cp),
                // The Unicode keysym range: 0x100..=0x10ffff → 0x01000000|cp.
                0x100..=0x10ffff => Some(0x0100_0000 | cp),
                // Everything else (other control chars, DEL, C1): untypable.
                _ => None,
            }
        }
    }
}

/// Typing plan for one `text()` call: chunks of ≤ 100 IME chars in
/// first-use order, plus the char → (chunk index, evdev keycode) lookup.
/// Chars the uinput tap path owns (ASCII, Enter, Tab) are skipped — they
/// ride the proven direct-keycode path and never touch this module.
struct TypingPlan {
    chunks: Vec<Vec<char>>,
    code_of: HashMap<char, (usize, u32)>,
}

fn plan(text: &str) -> TypingPlan {
    let mut chunks: Vec<Vec<char>> = vec![Vec::new()];
    let mut code_of: HashMap<char, (usize, u32)> = HashMap::new();
    for c in text.chars() {
        if UinputInjector::char_code(c).is_some() || keysym(c).is_none() || code_of.contains_key(&c)
        {
            continue;
        }
        let last = chunks.len() - 1;
        let idx = if chunks[last].len() >= CHUNK_CHARS {
            chunks.push(Vec::new());
            chunks.len() - 1
        } else {
            last
        };
        let slot = chunks[idx].len() as u32;
        chunks[idx].push(c);
        code_of.insert(c, (idx, EVDEV_BASE + slot));
    }
    TypingPlan { chunks, code_of }
}

/// The XKB keymap text for one chunk: keycode block 150.. + numeric
/// Unicode keysyms, types/compatibility from the standard `complete`
/// include (wtype's shape, minus the standard-layout includes this
/// device never needs).
fn keymap_text(chunk: &[char]) -> String {
    let mut s = String::with_capacity(chunk.len() * 48 + 256);
    s.push_str("xkb_keymap {\n");
    s.push_str("    xkb_keycodes \"opacusr-ime\" {\n");
    s.push_str("        minimum = 8;\n");
    s.push_str("        maximum = 255;\n");
    for (i, _) in chunk.iter().enumerate() {
        s.push_str(&format!("        <OI{i}> = {};\n", XKB_BASE + i as u32));
    }
    s.push_str("    };\n");
    s.push_str("    xkb_types \"opacusr-ime\" { include \"complete\" };\n");
    s.push_str("    xkb_compatibility \"opacusr-ime\" { include \"complete\" };\n");
    s.push_str("    xkb_symbols \"opacusr-ime\" {\n");
    for (i, c) in chunk.iter().enumerate() {
        // Every planned char has a keysym — plan() filtered on it.
        let ks = keysym(*c).unwrap_or(0xff1b);
        s.push_str(&format!("        key <OI{i}> {{ [ 0x{ks:x} ] }};\n"));
    }
    s.push_str("    };\n");
    s.push_str("};\n");
    s
}

static KEYMAP_SEQ: AtomicU32 = AtomicU32::new(0);

/// Registry state for the IME connection: binds the seat (≤ v7, wtype's
/// cap) and the virtual-keyboard manager (v1).
#[derive(Default)]
struct ImeState {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwpVirtualKeyboardManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ImeState {
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
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind::<wl_seat::WlSeat, (), Self>(
                        name,
                        version.min(7),
                        qh,
                        (),
                    ));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.manager = Some(registry.bind::<
                        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
                        (),
                        Self,
                    >(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ImeState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for ImeState {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: zwp_virtual_keyboard_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for ImeState {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: zwp_virtual_keyboard_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// One `text()` call's live IME: connection + virtual keyboard + the
/// typing plan. Drop it and the compositor side goes away with it.
pub(crate) struct ImeSession {
    eq: EventQueue<ImeState>,
    state: ImeState,
    keyboard: ZwpVirtualKeyboardV1,
    plan: TypingPlan,
    chunk_live: Option<usize>,
}

impl ImeSession {
    /// Connect, create the virtual keyboard, precompute the plan for
    /// `text`. Every failure here happens BEFORE the caller typed
    /// anything — the v1 «no half-typed prefix» contract rides on that.
    pub(crate) fn start(text: &str) -> Result<Self, InputError> {
        let conn = Connection::connect_to_env().map_err(|e| {
            InputError::Backend(format!("wayland ime: no compositor connection: {e}"))
        })?;
        let mut state = ImeState::default();
        let mut eq: EventQueue<ImeState> = conn.new_event_queue();
        let qh = eq.handle();
        conn.display().get_registry(&qh, ());
        // Roundtrip 1: globals → bind seat + manager.
        eq.roundtrip(&mut state)
            .map_err(|e| InputError::Backend(format!("wayland ime: registry dispatch: {e}")))?;
        let manager = state.manager.clone().ok_or_else(|| {
            InputError::Backend(
                "wayland ime: compositor does not expose the virtual-keyboard manager \
                 (Unicode text needs wtype-style virtual keyboard support)"
                    .to_string(),
            )
        })?;
        let seat = state
            .seat
            .clone()
            .ok_or_else(|| InputError::Backend("wayland ime: no seat found".to_string()))?;
        let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        let plan = plan(text);
        Ok(Self {
            eq,
            state,
            keyboard,
            plan,
            chunk_live: None,
        })
    }

    /// Type one IME char: switch keymaps at chunk boundaries, then
    /// press + release the char's keycode (roundtrip per event, wtype's
    /// rhythm).
    pub(crate) fn type_char(&mut self, c: char) -> Result<(), InputError> {
        let (chunk_idx, code) = self.plan.code_of.get(&c).copied().ok_or_else(|| {
            InputError::Backend(format!(
                "wayland ime: char U+{:04X} is not in the typing plan",
                c as u32
            ))
        })?;
        if self.chunk_live != Some(chunk_idx) {
            let chunk = self.plan.chunks[chunk_idx].clone();
            self.upload_keymap(&chunk)?;
            self.chunk_live = Some(chunk_idx);
        }
        self.keyboard.key(0, code, KEY_PRESSED);
        self.eq
            .roundtrip(&mut self.state)
            .map_err(|e| InputError::Backend(format!("wayland ime: key press: {e}")))?;
        self.keyboard.key(0, code, KEY_RELEASED);
        self.eq
            .roundtrip(&mut self.state)
            .map_err(|e| InputError::Backend(format!("wayland ime: key release: {e}")))?;
        Ok(())
    }

    /// Compile-proof the chunk into a temp file, push it as the keyboard's
    /// keymap, and wait for the compositor to swallow it (a keymap that
    /// fails to compile kills the connection here — the error surfaces
    /// instead of silent dead keys).
    fn upload_keymap(&mut self, chunk: &[char]) -> Result<(), InputError> {
        let text = keymap_text(chunk);
        let seq = KEYMAP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("opacusr-ime-{}-{seq}", std::process::id()));
        let mut f = File::create(&path)
            .map_err(|e| InputError::Backend(format!("wayland ime: keymap temp file: {e}")))?;
        f.write_all(text.as_bytes())
            .map_err(|e| InputError::Backend(format!("wayland ime: keymap write: {e}")))?;
        // wtype appends a trailing NUL and reports len+1 — mirror it.
        f.write_all(&[0])
            .map_err(|e| InputError::Backend(format!("wayland ime: keymap write: {e}")))?;
        let size = text.len() as u32 + 1;
        // BorrowedFd: the request dups the fd on the wire, but the source
        // file stays open through the roundtrip (the compositor may mmap
        // it at dispatch time) — wtype closes after the roundtrip too.
        self.keyboard.keymap(KEYMAP_FORMAT_XKB, f.as_fd(), size);
        self.eq
            .roundtrip(&mut self.state)
            .map_err(|e| InputError::Backend(format!("wayland ime: keymap upload: {e}")))?;
        drop(f);
        // Unlink only now: the name was a uniqueness device, the content
        // already reached the compositor.
        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysym_matches_the_documented_ranges() {
        // latin-1: keysym = codepoint.
        assert_eq!(keysym('é'), Some(0xe9));
        assert_eq!(keysym(' '), Some(0x20));
        // Unicode scheme above 0xff.
        assert_eq!(keysym('中'), Some(0x0100_4e2d));
        assert_eq!(keysym('\u{1F600}'), Some(0x0101_f600));
        assert_eq!(keysym('\u{10FFFF}'), Some(0x0110_ffff));
        // Escape rides the IME path (char_code has no keycode for it).
        assert_eq!(keysym('\u{1b}'), Some(0xff1b));
        // Tap-path chars and non-typable controls never get a keysym.
        assert_eq!(keysym('\n'), None);
        assert_eq!(keysym('\t'), None);
        assert_eq!(keysym('\u{7f}'), None);
        assert_eq!(keysym('\u{1}'), None);
    }

    #[test]
    fn plan_assigns_first_use_order_and_chunks() {
        let mut s = String::new();
        for i in 0..150u32 {
            s.push(char::from_u32(0x4e00 + i).unwrap());
        }
        let p = plan(&s);
        // 150 unique IME chars → 2 chunks (100 + 50).
        assert_eq!(p.chunks.len(), 2);
        assert_eq!(p.chunks[0].len(), CHUNK_CHARS);
        assert_eq!(p.chunks[1].len(), 50);
        // Slots restart per chunk: 142..241, then 142..191.
        let first = char::from_u32(0x4e00).unwrap();
        let hundredth = char::from_u32(0x4e00 + 99).unwrap();
        let hundred_first = char::from_u32(0x4e00 + 100).unwrap();
        assert_eq!(p.code_of[&first], (0, 142));
        assert_eq!(p.code_of[&hundredth], (0, 241));
        assert_eq!(p.code_of[&hundred_first], (1, 142));
        // Repeated chars share the first-use slot.
        let p = plan("中中中世世");
        assert_eq!(p.chunks.len(), 1);
        assert_eq!(p.chunks[0], vec!['中', '世']);
        assert_eq!(p.code_of.len(), 2);
        assert_eq!(p.code_of[&'中'], (0, 142));
    }

    #[test]
    fn plan_skips_tap_path_chars() {
        // ASCII, \n, \t belong to uinput — the plan carries only IME chars.
        let p = plan("hello 你好\nworld\tмир");
        assert_eq!(p.chunks[0], vec!['你', '好', 'м', 'и', 'р']);
        assert_eq!(p.code_of.len(), 5);
    }

    #[test]
    fn keymap_text_declares_keycodes_and_numeric_keysyms() {
        let s = keymap_text(&['中', 'é']);
        assert!(s.contains("minimum = 8;"));
        assert!(s.contains("maximum = 255;"));
        assert!(s.contains("<OI0> = 150;"));
        assert!(s.contains("<OI1> = 151;"));
        assert!(s.contains("key <OI0> { [ 0x1004e2d ] };"));
        assert!(s.contains("key <OI1> { [ 0xe9 ] };"));
        // types + compatibility both ride the standard include.
        assert_eq!(s.matches("include \"complete\"").count(), 2);
        // No standard-layout includes: this device never types plain keys.
        assert!(!s.contains("include \"pc+us\""));
    }

    #[test]
    fn keymap_chunk_stays_inside_the_xkb_code_range() {
        let mut chunk = Vec::new();
        for i in 0..CHUNK_CHARS {
            chunk.push(char::from_u32(0x4e00 + i as u32).unwrap());
        }
        let s = keymap_text(&chunk);
        let max_xkb = XKB_BASE + CHUNK_CHARS as u32 - 1;
        assert!(
            max_xkb <= 255,
            "XKB keycodes stop at 255, plan needs {max_xkb}"
        );
        assert!(s.contains(&format!("<OI{}> = {max_xkb};", CHUNK_CHARS - 1)));
        assert!(!s.contains("<OI100>"));
    }
}
