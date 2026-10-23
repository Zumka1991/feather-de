//! Escape-sequence parser: the DEC/ANSI state machine (after Paul Williams' description of
//! the VT500 parser) with UTF-8 decoding in the ground and OSC states. It only splits the
//! byte stream into actions; meaning is given by a `Perform` implementation.

/// Up to 32 parameters, each with up to 8 colon-separated sub-parameters
/// (`38:2::255:0:0`). A missing value is 0; handlers decide what 0 means.
#[derive(Default, Debug)]
pub struct Params {
    values: Vec<u16>,
    /// Start index in `values` of every parameter
    starts: Vec<usize>,
}

impl Params {
    fn clear(&mut self) {
        self.values.clear();
        self.starts.clear();
    }

    fn begin_param(&mut self) {
        if self.starts.len() < 32 {
            self.starts.push(self.values.len());
            self.values.push(0);
        }
    }

    fn begin_sub(&mut self) {
        let len = self.starts.last().map_or(0, |&s| self.values.len() - s);
        if !self.starts.is_empty() && len < 8 {
            self.values.push(0);
        }
    }

    fn digit(&mut self, d: u8) {
        if self.starts.is_empty() {
            self.begin_param();
        }
        if let Some(v) = self.values.last_mut() {
            *v = v.saturating_mul(10).saturating_add((d - b'0') as u16);
        }
    }

    pub fn len(&self) -> usize {
        self.starts.len()
    }

    /// Parameter `i` with its sub-parameters
    pub fn get(&self, i: usize) -> &[u16] {
        match self.starts.get(i) {
            Some(&start) => {
                let end = self.starts.get(i + 1).copied().unwrap_or(self.values.len());
                &self.values[start..end]
            }
            None => &[],
        }
    }

    /// First value of parameter `i`, or `default` if it is missing or 0
    pub fn arg(&self, i: usize, default: u16) -> u16 {
        match self.get(i).first() {
            Some(&0) | None => default,
            Some(&v) => v,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u16]> {
        (0..self.len()).map(move |i| self.get(i))
    }
}

pub trait Perform {
    fn print(&mut self, c: char);
    fn execute(&mut self, byte: u8);
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8);
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8);
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    /// DCS, SOS, PM and APC payloads are skipped up to the string terminator
    StringIgnore,
    OscString,
}

const MAX_INTERMEDIATES: usize = 2;
const MAX_OSC: usize = 64 * 1024;

pub struct Parser {
    state: State,
    params: Params,
    intermediates: [u8; MAX_INTERMEDIATES],
    n_intermediates: usize,
    ignoring: bool,
    osc: Vec<u8>,
    osc_overflow: bool,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Params::default(),
            intermediates: [0; MAX_INTERMEDIATES],
            n_intermediates: 0,
            ignoring: false,
            osc: Vec::new(),
            osc_overflow: false,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    pub fn advance<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) {
        for &b in bytes {
            self.byte(performer, b);
        }
    }

    fn clear(&mut self) {
        self.params.clear();
        self.n_intermediates = 0;
        self.ignoring = false;
    }

    fn collect(&mut self, b: u8) {
        if self.n_intermediates < MAX_INTERMEDIATES {
            self.intermediates[self.n_intermediates] = b;
            self.n_intermediates += 1;
        } else {
            self.ignoring = true;
        }
    }

    fn intermediates(&self) -> &[u8] {
        &self.intermediates[..self.n_intermediates]
    }

    /// UTF-8 in the ground state; malformed input becomes U+FFFD, like xterm
    fn utf8_byte<P: Perform>(&mut self, p: &mut P, b: u8) {
        if self.utf8_need == 0 {
            let need = match b {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => {
                    p.print('\u{FFFD}');
                    return;
                }
            };
            self.utf8[0] = b;
            self.utf8_len = 1;
            self.utf8_need = need;
            return;
        }
        if b & 0xC0 != 0x80 {
            // Truncated sequence: report it and handle this byte afresh
            self.utf8_need = 0;
            p.print('\u{FFFD}');
            self.byte(p, b);
            return;
        }
        self.utf8[self.utf8_len] = b;
        self.utf8_len += 1;
        if self.utf8_len == self.utf8_need {
            self.utf8_need = 0;
            let c = std::str::from_utf8(&self.utf8[..self.utf8_len]).ok().and_then(|s| s.chars().next()).unwrap_or('\u{FFFD}');
            p.print(c);
        }
    }

    fn osc_end<P: Perform>(&mut self, p: &mut P, bell: bool) {
        let parts: Vec<&[u8]> = self.osc.split(|&c| c == b';').collect();
        if !self.osc_overflow { p.osc_dispatch(&parts, bell); }
        self.osc.clear();
        self.osc_overflow = false;
    }

    fn byte<P: Perform>(&mut self, p: &mut P, b: u8) {
        if self.utf8_need > 0 && self.state == State::Ground {
            self.utf8_byte(p, b);
            return;
        }
        // Transitions from any state
        match b {
            0x18 | 0x1A => {
                if self.state == State::OscString {
                    self.osc.clear();
                    self.osc_overflow = false;
                }
                p.execute(b);
                self.state = State::Ground;
                return;
            }
            0x1B => {
                if self.state == State::OscString {
                    // ESC \ (string terminator) ends the OSC; the backslash is ignored below
                    self.osc_end(p, false);
                }
                self.clear();
                self.state = State::Escape;
                return;
            }
            _ => {}
        }
        match self.state {
            State::Ground => match b {
                0x00..=0x1F => p.execute(b),
                0x20..=0x7E => p.print(b as char),
                0x7F => {}
                _ => self.utf8_byte(p, b),
            },
            State::Escape => match b {
                0x00..=0x1F => p.execute(b),
                0x20..=0x2F => {
                    self.collect(b);
                    self.state = State::EscapeIntermediate;
                }
                b'[' => {
                    self.clear();
                    self.state = State::CsiEntry;
                }
                b']' => {
                    self.osc.clear();
                    self.osc_overflow = false;
                    self.state = State::OscString;
                }
                b'P' | b'X' | b'^' | b'_' => self.state = State::StringIgnore,
                0x30..=0x7E => {
                    p.esc_dispatch(self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::EscapeIntermediate => match b {
                0x00..=0x1F => p.execute(b),
                0x20..=0x2F => self.collect(b),
                0x30..=0x7E => {
                    p.esc_dispatch(self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiEntry | State::CsiParam => match b {
                0x00..=0x1F => p.execute(b),
                b'0'..=b'9' => {
                    self.params.digit(b);
                    self.state = State::CsiParam;
                }
                b';' => {
                    if self.params.len() == 0 {
                        self.params.begin_param();
                    }
                    self.params.begin_param();
                    self.state = State::CsiParam;
                }
                b':' => {
                    if self.params.len() == 0 {
                        self.params.begin_param();
                    }
                    self.params.begin_sub();
                    self.state = State::CsiParam;
                }
                // Private markers are only valid before the first parameter
                0x3C..=0x3F if self.state == State::CsiEntry => {
                    self.collect(b);
                    self.state = State::CsiParam;
                }
                0x3C..=0x3F => self.state = State::CsiIgnore,
                0x20..=0x2F => {
                    self.collect(b);
                    self.state = State::CsiIntermediate;
                }
                0x40..=0x7E => {
                    p.csi_dispatch(&self.params, self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiIntermediate => match b {
                0x00..=0x1F => p.execute(b),
                0x20..=0x2F => self.collect(b),
                0x30..=0x3F => self.state = State::CsiIgnore,
                0x40..=0x7E => {
                    p.csi_dispatch(&self.params, self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiIgnore => match b {
                0x00..=0x1F => p.execute(b),
                0x40..=0x7E => self.state = State::Ground,
                _ => {}
            },
            State::StringIgnore => {}
            State::OscString => match b {
                0x07 => {
                    self.osc_end(p, true);
                    self.state = State::Ground;
                }
                0x00..=0x1F => {}
                _ => {
                    if self.osc.len() < MAX_OSC {
                        self.osc.push(b);
                    } else {
                        self.osc_overflow = true;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<String>);
    impl Perform for Log {
        fn print(&mut self, c: char) {
            self.0.push(format!("print {c}"));
        }
        fn execute(&mut self, b: u8) {
            self.0.push(format!("exec {b:02x}"));
        }
        fn csi_dispatch(&mut self, params: &Params, inter: &[u8], ignore: bool, action: u8) {
            let ps: Vec<String> = params.iter().map(|p| p.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(":")).collect();
            self.0.push(format!("csi {}{} {}{}", String::from_utf8_lossy(inter), ps.join(";"), action as char, if ignore { " ignored" } else { "" }));
        }
        fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, b: u8) {
            self.0.push(format!("esc {}{}", String::from_utf8_lossy(inter), b as char));
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
            let ps: Vec<String> = params.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect();
            self.0.push(format!("osc {} {}", ps.join("|"), if bell { "bel" } else { "st" }));
        }
    }

    fn parse(bytes: &[u8]) -> Vec<String> {
        let mut log = Log::default();
        Parser::new().advance(&mut log, bytes);
        log.0
    }

    #[test]
    fn text_controls_and_utf8() {
        assert_eq!(parse("aЖ€😀\r\n".as_bytes()), ["print a", "print Ж", "print €", "print 😀", "exec 0d", "exec 0a"]);
        // Truncated and invalid sequences
        assert_eq!(parse(b"\xD0a\xFF"), ["print \u{FFFD}", "print a", "print \u{FFFD}"]);
    }

    #[test]
    fn csi_params_private_markers_and_subparams() {
        assert_eq!(parse(b"\x1b[1;31m\x1b[m\x1b[?1049h"), ["csi 1;31 m", "csi  m", "csi ?1049 h"]);
        assert_eq!(parse(b"\x1b[38:2::255:0:10m\x1b[;5H"), ["csi 38:2:0:255:0:10 m", "csi 0;5 H"]);
        assert_eq!(parse(b"\x1b[2 q\x1b[>c"), ["csi  2 q", "csi > c"]);
        // A control inside a sequence executes without breaking it
        assert_eq!(parse(b"\x1b[1\n2A"), ["exec 0a", "csi 12 A"]);
    }

    #[test]
    fn osc_esc_and_ignored_strings() {
        assert_eq!(parse(b"\x1b]0;\xd0\x9f\xd1\x80\xd0\xb8\x07x"), ["osc 0|При bel", "print x"]);
        assert_eq!(parse(b"\x1b]2;t\x1b\\y"), ["osc 2|t st", "esc \\", "print y"]);
        assert_eq!(parse(b"\x1bP1$r0m\x1b\\z\x1b(0\x1b7"), ["esc \\", "print z", "esc (0", "esc 7"]);
        assert_eq!(parse(b"\x1b[1;\x18q"), ["exec 18", "print q"]);
    }
}
