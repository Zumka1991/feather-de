//! What a key press sends to the program, as xterm (and Konsole) do for
//! `TERM=xterm-256color`; and mouse reports for programs that asked for them.

use smithay_client_toolkit::seat::keyboard::Keysym as K;

use crate::vt::term::Modes;

#[derive(Clone, Copy, Default, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Mods {
    /// xterm modifier parameter: 1 + Shift + 2·Alt + 4·Ctrl
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

/// Russian ЙЦУКЕН letters on the same keys as Latin QWERTY ones, so that Ctrl+С sends ^C
/// in the Russian layout too
fn latin_for(c: char) -> Option<char> {
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбю";
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.";
    let lower = c.to_lowercase().next()?;
    RU.chars().position(|r| r == lower).and_then(|i| EN.chars().nth(i))
}

fn ctrl_byte(c: char) -> Option<u8> {
    let c = if c.is_ascii() { c } else { latin_for(c)? };
    Some(match c.to_ascii_lowercase() {
        'a'..='z' => c.to_ascii_lowercase() as u8 - b'a' + 1,
        ' ' | '2' | '@' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '-' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Bytes for a key, or None when the key sends nothing (modifiers alone, unknown keys)
pub fn encode(sym: K, text: Option<&str>, m: Mods, modes: &Modes) -> Option<Vec<u8>> {
    let p = m.param();
    let csi_mod = |final_: char| -> Vec<u8> {
        if p > 1 { format!("\x1b[1;{p}{final_}") } else if modes.cursor_keys_app { format!("\x1bO{final_}") } else { format!("\x1b[{final_}") }.into_bytes()
    };
    let tilde = |n: u8| -> Vec<u8> { if p > 1 { format!("\x1b[{n};{p}~") } else { format!("\x1b[{n}~") }.into_bytes() };
    let ss3 = |final_: char| -> Vec<u8> { if p > 1 { format!("\x1b[1;{p}{final_}") } else { format!("\x1bO{final_}") }.into_bytes() };
    let alt = |bytes: &[u8]| -> Vec<u8> {
        let mut v = if m.alt { vec![0x1b] } else { Vec::new() };
        v.extend_from_slice(bytes);
        v
    };
    Some(match sym {
        K::Up => csi_mod('A'),
        K::Down => csi_mod('B'),
        K::Right => csi_mod('C'),
        K::Left => csi_mod('D'),
        K::Home => csi_mod('H'),
        K::End => csi_mod('F'),
        K::Insert => tilde(2),
        K::Delete => tilde(3),
        K::Page_Up => tilde(5),
        K::Page_Down => tilde(6),
        K::F1 => ss3('P'),
        K::F2 => ss3('Q'),
        K::F3 => ss3('R'),
        K::F4 => ss3('S'),
        K::F5 => tilde(15),
        K::F6 => tilde(17),
        K::F7 => tilde(18),
        K::F8 => tilde(19),
        K::F9 => tilde(20),
        K::F10 => tilde(21),
        K::F11 => tilde(23),
        K::F12 => tilde(24),
        K::Return => alt(b"\r"),
        K::KP_Enter => {
            if modes.keypad_app { b"\x1bOM".to_vec() } else { alt(b"\r") }
        }
        K::BackSpace => {
            if m.ctrl { alt(b"\x08") } else { alt(b"\x7f") }
        }
        K::Tab => {
            if m.shift { b"\x1b[Z".to_vec() } else { alt(b"\t") }
        }
        K::ISO_Left_Tab => b"\x1b[Z".to_vec(),
        K::Escape => alt(b"\x1b"),
        _ => {
            let text = text.filter(|t| !t.is_empty())?;
            let c = text.chars().next()?;
            if m.ctrl {
                // xkb already turns Ctrl+letter into a control code in Latin layouts
                let byte = if text.len() == 1 && (text.as_bytes()[0] < 0x20 || text.as_bytes()[0] == 0x7f) { Some(text.as_bytes()[0]) } else { ctrl_byte(c) };
                if let Some(b) = byte {
                    return Some(alt(&[b]));
                }
            }
            alt(text.as_bytes())
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseEvent {
    Press(u8),
    Release(u8),
    /// Motion with the given button held (3 = none)
    Motion(u8),
    WheelUp,
    WheelDown,
}

/// Mouse report for cell (col, row), both 0-based. SGR (1006) when enabled, else X10.
pub fn mouse_report(event: MouseEvent, col: usize, row: usize, m: Mods, sgr: bool) -> Vec<u8> {
    let mods = 4 * m.shift as u8 + 8 * m.alt as u8 + 16 * m.ctrl as u8;
    let (button, release) = match event {
        MouseEvent::Press(b) => (b, false),
        MouseEvent::Release(b) => (b, true),
        MouseEvent::Motion(b) => (b + 32, false),
        MouseEvent::WheelUp => (64, false),
        MouseEvent::WheelDown => (65, false),
    };
    if sgr {
        return format!("\x1b[<{};{};{}{}", button + mods, col + 1, row + 1, if release { 'm' } else { 'M' }).into_bytes();
    }
    // X10: release is button 3; coordinates above 222 cannot be encoded
    let b = if release { 3 } else { button } + mods;
    vec![0x1b, b'[', b'M', 32 + b, 32 + (col.min(222) as u8 + 1), 32 + (row.min(222) as u8 + 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(sym: K, text: Option<&str>, m: Mods) -> String {
        String::from_utf8(encode(sym, text, m, &Modes::default()).unwrap()).unwrap()
    }

    #[test]
    fn keys_like_xterm() {
        let none = Mods::default();
        let ctrl = Mods { ctrl: true, ..none };
        assert_eq!(enc(K::Up, None, none), "\x1b[A");
        assert_eq!(enc(K::Up, None, ctrl), "\x1b[1;5A");
        assert_eq!(enc(K::Delete, None, none), "\x1b[3~");
        assert_eq!(enc(K::Page_Up, None, Mods { shift: true, ..none }), "\x1b[5;2~");
        assert_eq!(enc(K::F5, None, none), "\x1b[15~");
        assert_eq!(enc(K::F1, None, none), "\x1bOP");
        assert_eq!(enc(K::BackSpace, None, none), "\x7f");
        assert_eq!(enc(K::Tab, None, Mods { shift: true, ..none }), "\x1b[Z");
        assert_eq!(enc(K::c, Some("\x03"), ctrl), "\x03");
        // Russian layout: Ctrl+С is ^C, Alt+ф is ESC ф
        assert_eq!(enc(K::Cyrillic_es, Some("с"), ctrl), "\x03");
        assert_eq!(enc(K::Cyrillic_ef, Some("ф"), Mods { alt: true, ..none }), "\x1bф");
        let app = Modes { cursor_keys_app: true, ..Modes::default() };
        assert_eq!(encode(K::Left, None, none, &app).unwrap(), b"\x1bOD");
        assert!(encode(K::Shift_L, None, none, &Modes::default()).is_none());
    }

    #[test]
    fn mouse_reports() {
        let m = Mods::default();
        assert_eq!(mouse_report(MouseEvent::Press(0), 4, 9, m, true), b"\x1b[<0;5;10M");
        assert_eq!(mouse_report(MouseEvent::Release(0), 4, 9, m, true), b"\x1b[<0;5;10m");
        assert_eq!(mouse_report(MouseEvent::WheelUp, 0, 0, Mods { ctrl: true, ..m }, true), b"\x1b[<80;1;1M");
        assert_eq!(mouse_report(MouseEvent::Press(2), 0, 0, m, false), vec![0x1b, b'[', b'M', 34, 33, 33]);
    }
}
