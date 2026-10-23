//! One-line text field shared by every Feather input (Launchpad, Spotlight, Settings, Files,
//! Notes): caret anywhere, selection with Shift or Ctrl+A, word jumps, and painting of the
//! selection and caret with horizontal scrolling. Painting is stateless: the visible part is
//! chosen so the caret stays in view.
use crate::{draw, text::FontSystem};
use tiny_skia::{Color, PixmapMut};

const LEFT: u32 = 0xff51;
const RIGHT: u32 = 0xff53;
const HOME: u32 = 0xff50;
const END: u32 = 0xff57;
const BACKSPACE: u32 = 0xff08;
const DELETE: u32 = 0xffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// The key is not a field key (Enter, Escape, Up/Down…): the caller decides
    Ignored,
    /// Caret or selection moved
    Moved,
    /// Text changed
    Changed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    caret: usize,
    anchor: Option<usize>,
    max_chars: Option<usize>,
}

fn prev(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(j, _)| j)
}

fn next(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

fn word_left(s: &str, mut i: usize) -> usize {
    while i > 0 && s[..i].chars().next_back().is_some_and(char::is_whitespace) {
        i = prev(s, i);
    }
    while i > 0 && s[..i].chars().next_back().is_some_and(|c| !c.is_whitespace()) {
        i = prev(s, i);
    }
    i
}

fn word_right(s: &str, mut i: usize) -> usize {
    while i < s.len() && s[i..].chars().next().is_some_and(char::is_whitespace) {
        i = next(s, i);
    }
    while i < s.len() && s[i..].chars().next().is_some_and(|c| !c.is_whitespace()) {
        i = next(s, i);
    }
    i
}

impl TextInput {
    pub fn new(text: &str) -> Self {
        let mut input = Self::default();
        input.set(text);
        input
    }

    /// Accepts at most `max` characters (typing and pasting beyond it are cut).
    pub fn with_limit(mut self, max: usize) -> Self {
        self.max_chars = Some(max);
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the text; the caret goes to the end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.caret = self.text.len();
        self.anchor = None;
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.len();
    }

    /// Inserts at the caret, replacing the selection; control characters are dropped.
    pub fn insert(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        let (a, b) = self.selection().unwrap_or((self.caret, self.caret));
        let room = self
            .max_chars
            .map_or(usize::MAX, |m| m.saturating_sub(self.text.chars().count() - self.text[a..b].chars().count()));
        let clean: String = clean.chars().take(room).collect();
        self.text.replace_range(a..b, &clean);
        self.caret = a + clean.len();
        self.anchor = None;
    }

    fn move_to(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to;
    }

    fn delete(&mut self, from: usize, to: usize) -> Edit {
        if from == to {
            return Edit::Moved;
        }
        self.text.replace_range(from..to, "");
        self.caret = from;
        self.anchor = None;
        Edit::Changed
    }

    /// An XKB key press: `sym` keysym, `utf8` its text, Ctrl/Shift held.
    pub fn key(&mut self, sym: u32, utf8: Option<&str>, ctrl: bool, shift: bool) -> Edit {
        let t = &self.text;
        match sym {
            0x61 | 0x41 if ctrl => {
                self.select_all();
                Edit::Moved
            }
            LEFT => {
                let to = match self.selection() {
                    Some((a, _)) if !shift => a,
                    _ if ctrl => word_left(t, self.caret),
                    _ => prev(t, self.caret),
                };
                self.move_to(to, shift);
                Edit::Moved
            }
            RIGHT => {
                let to = match self.selection() {
                    Some((_, b)) if !shift => b,
                    _ if ctrl => word_right(t, self.caret),
                    _ => next(t, self.caret),
                };
                self.move_to(to, shift);
                Edit::Moved
            }
            HOME => {
                self.move_to(0, shift);
                Edit::Moved
            }
            END => {
                let end = self.text.len();
                self.move_to(end, shift);
                Edit::Moved
            }
            BACKSPACE => match self.selection() {
                Some((a, b)) => self.delete(a, b),
                None => {
                    let from = if ctrl { word_left(t, self.caret) } else { prev(t, self.caret) };
                    self.delete(from, self.caret)
                }
            },
            DELETE => match self.selection() {
                Some((a, b)) => self.delete(a, b),
                None => {
                    let to = if ctrl { word_right(t, self.caret) } else { next(t, self.caret) };
                    self.delete(self.caret, to)
                }
            },
            _ => match utf8.filter(|s| !ctrl && !s.is_empty() && !s.chars().any(char::is_control)) {
                Some(s) => {
                    self.insert(s);
                    Edit::Changed
                }
                None => Edit::Ignored,
            },
        }
    }

    /// First visible byte so the caret fits into `width` (stateless scrolling).
    fn visible_start(&self, fonts: &mut FontSystem, width: f32, size: f32, bold: bool) -> usize {
        let mut start = 0;
        while start < self.caret && fonts.measure_text(&self.text[start..self.caret], size, bold) > width - 2.0 {
            start = next(&self.text, start);
        }
        start
    }

    /// Places the caret at a click `x` (relative to the text start); Shift extends.
    pub fn click(&mut self, fonts: &mut FontSystem, x: f32, width: f32, size: f32, bold: bool, extend: bool) {
        let start = self.visible_start(fonts, width, size, bold);
        let mut best = (start, f32::MAX);
        let mut i = start;
        loop {
            let d = (fonts.measure_text(&self.text[start..i], size, bold) - x).abs();
            if d < best.1 {
                best = (i, d);
            }
            if i >= self.text.len() {
                break;
            }
            i = next(&self.text, i);
        }
        self.move_to(best.0, extend);
    }

    /// Text, selection and (when `focused`) the caret inside `[x, x + width)`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        x: f32,
        y: f32,
        width: f32,
        size: f32,
        bold: bool,
        color: Color,
        focused: bool,
        accent: Color,
    ) {
        let start = self.visible_start(fonts, width, size, bold);
        let mut end = start;
        while end < self.text.len() {
            let n = next(&self.text, end);
            if fonts.measure_text(&self.text[start..n], size, bold) > width {
                break;
            }
            end = n;
        }
        let at = |fonts: &mut FontSystem, i: usize| fonts.measure_text(&self.text[start..i.clamp(start, end)], size, bold);
        if let Some((a, b)) = self.selection() {
            let (sx, ex) = (at(fonts, a), at(fonts, b));
            if ex > sx {
                let alpha = if focused { 0.35 } else { 0.18 };
                let sel = Color::from_rgba(accent.red(), accent.green(), accent.blue(), alpha).unwrap_or(accent);
                draw::fill_rect(pm, x + sx, y - 1.0, ex - sx, size + 4.0, sel);
            }
        }
        fonts.draw_text(pm, &self.text[start..end], x, y, size, color, bold);
        if focused && self.selection().is_none() {
            let cx = x + at(fonts, self.caret);
            draw::fill_rect(pm, cx, y - 1.0, 1.5, size + 4.0, accent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_selection_words_and_limits() {
        let mut f = TextInput::new("привет мир");
        assert_eq!(f.key(LEFT, None, true, false), Edit::Moved);
        assert_eq!(f.caret(), "привет ".len());
        f.key(0, Some("!"), false, false);
        assert_eq!(f.text(), "привет !мир");
        f.key(0x61, Some("a"), true, false);
        assert_eq!(f.selected_text(), Some("привет !мир"));
        assert_eq!(f.key(0, Some("x"), false, false), Edit::Changed);
        assert_eq!(f.text(), "x");
        f.set("один два");
        f.key(LEFT, None, false, true);
        f.key(LEFT, None, false, true);
        assert_eq!(f.selected_text(), Some("ва"));
        assert_eq!(f.key(BACKSPACE, None, false, false), Edit::Changed);
        assert_eq!(f.text(), "один д");
        f.key(HOME, None, false, false);
        f.key(DELETE, None, true, false);
        assert_eq!(f.text(), " д");
        assert_eq!(f.key(0xff0d, None, false, false), Edit::Ignored);
        let mut limited = TextInput::default().with_limit(3);
        limited.insert("abcdef");
        assert_eq!(limited.text(), "abc");
        limited.insert("\nz");
        assert_eq!(limited.text(), "abc");
    }
}
