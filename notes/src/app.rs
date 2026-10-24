//! Notes' model and UI: a list of notes in a floating sidebar and a plain-text editor whose
//! first line is the title, as in macOS Notes. Everything is drawn with tiny-skia; one
//! layout of the text serves painting, the caret and hit testing.
use crate::store::{self, Note};
use crate::theme::{self, colors, rgba};
use feather_ui::text::FontSystem;
use std::time::{Duration, Instant};
use tiny_skia::PixmapMut;

pub const SIDEBAR_W: f32 = 290.0;
const TOOLBAR_H: f32 = 52.0;
const LIST_TOP: f32 = 96.0;
const ROW_H: f32 = 62.0;
const PAD: f32 = 40.0;
const TEXT_TOP: f32 = TOOLBAR_H + 40.0;
const TITLE: (f32, f32) = (22.0, 32.0);
const BODY: (f32, f32) = (15.0, 23.0);
const RESIZE_BAND: f32 = 5.0;
const BTN_LEFT: u32 = 0x110;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Typing within this pause is one undo step.
const UNDO_PAUSE: Duration = Duration::from_millis(900);
const MAX_UNDO: usize = 200;
/// Notes' highlight colour.
const YELLOW: (u8, u8, u8) = (255, 204, 0);

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Close,
    Minimize,
    ToggleMaximize,
    Move,
    Resize(u32),
    Save(u64, String),
    Trash(u64),
    Remove(u64),
    Copy(String),
    Paste,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Default,
    Text,
    Resize(u32),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Close,
    Minimize,
    Zoom,
    New,
    Delete,
    Search,
    Row(usize),
    List,
    Editor,
    Toolbar,
    Nothing,
}

/// One visual line of the open note (byte offsets into its text).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Line {
    start: usize,
    end: usize,
    y: f32,
    size: f32,
    height: f32,
    bold: bool,
}

pub struct App {
    fonts: FontSystem,
    pub w: f32,
    pub h: f32,
    pub dirty: bool,
    pub maximized: bool,
    pub focused: bool,
    notes: Vec<Note>,
    selected: Option<u64>,
    loaded: bool,
    search: feather_ui::input::TextInput,
    search_focus: bool,
    caret: usize,
    anchor: Option<usize>,
    /// Remembered x for Up/Down
    goal_x: Option<f32>,
    scroll: f32,
    list_scroll: f32,
    layout: Vec<Line>,
    layout_valid: bool,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    last_edit: Option<Instant>,
    /// Note with changes not yet written
    unsaved: Option<u64>,
    hover: Hit,
    selecting: bool,
    last_click: Option<(Instant, f32, f32)>,
}

fn russian_date(secs: i64) -> String {
    use chrono::{Datelike, TimeZone, Timelike};
    let Some(t) = chrono::Local.timestamp_opt(secs, 0).single() else { return String::new() };
    const MONTHS: [&str; 12] = [
        "января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря",
    ];
    format!("{} {} {} г. в {}:{:02}", t.day(), MONTHS[t.month0() as usize], t.year(), t.hour(), t.minute())
}

fn short_date(secs: i64) -> String {
    use chrono::TimeZone;
    let Some(t) = chrono::Local.timestamp_opt(secs, 0).single() else { return String::new() };
    let today = chrono::Local::now().date_naive();
    match (today - t.date_naive()).num_days() {
        0 => t.format("%H:%M").to_string(),
        1 => "Вчера".into(),
        _ => t.format("%d.%m.%Y").to_string(),
    }
}

/// Title and preview lines of a note for the list.
fn summary(text: &str) -> (String, String) {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let title = lines.next().unwrap_or("Новая заметка").chars().take(120).collect();
    let preview = lines.next().unwrap_or("Нет дополнительного текста").chars().take(160).collect();
    (title, preview)
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(j, _)| j)
}

fn next_boundary(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

fn word_left(s: &str, i: usize) -> usize {
    let mut j = i;
    while j > 0 && s[..j].chars().next_back().is_some_and(char::is_whitespace) {
        j = prev_boundary(s, j);
    }
    while j > 0 && s[..j].chars().next_back().is_some_and(|c| !c.is_whitespace()) {
        j = prev_boundary(s, j);
    }
    j
}

fn word_right(s: &str, i: usize) -> usize {
    let mut j = i;
    while j < s.len() && s[j..].chars().next().is_some_and(char::is_whitespace) {
        j = next_boundary(s, j);
    }
    while j < s.len() && s[j..].chars().next().is_some_and(|c| !c.is_whitespace()) {
        j = next_boundary(s, j);
    }
    j
}

impl App {
    pub fn new(fonts: FontSystem) -> Self {
        Self {
            fonts,
            w: 920.0,
            h: 620.0,
            dirty: true,
            maximized: false,
            focused: true,
            notes: Vec::new(),
            selected: None,
            loaded: false,
            search: Default::default(),
            search_focus: false,
            caret: 0,
            anchor: None,
            goal_x: None,
            scroll: 0.0,
            list_scroll: 0.0,
            layout: Vec::new(),
            layout_valid: false,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            unsaved: None,
            hover: Hit::Nothing,
            selecting: false,
            last_click: None,
        }
    }

    pub fn loaded(&mut self, notes: Vec<Note>) {
        self.notes = notes;
        self.loaded = true;
        if let Some(first) = self.visible().first().map(|n| n.id) {
            self.open(first);
        }
        self.dirty = true;
    }

    // ---------- model ----------

    fn visible(&self) -> Vec<&Note> {
        let q = self.search.to_lowercase();
        self.notes.iter().filter(|n| q.is_empty() || n.text.to_lowercase().contains(&q)).collect()
    }

    fn current(&self) -> Option<&Note> {
        self.notes.iter().find(|n| Some(n.id) == self.selected)
    }

    fn text(&self) -> &str {
        self.current().map_or("", |n| n.text.as_str())
    }

    /// Writes for the debounce timer: the note with unsaved changes, once.
    pub fn take_save(&mut self) -> Option<Effect> {
        let id = self.unsaved.take()?;
        self.notes.iter().find(|n| n.id == id).map(|n| Effect::Save(id, n.text.clone()))
    }

    pub fn has_unsaved(&self) -> bool {
        self.unsaved.is_some()
    }

    /// On exit: pending text is written, an empty open note removed.
    pub fn on_exit(&mut self) -> Vec<Effect> {
        self.leave_note()
    }

    /// Leaving a note: an empty one disappears (as in macOS Notes).
    fn leave_note(&mut self) -> Vec<Effect> {
        let mut effects: Vec<Effect> = self.take_save().into_iter().collect();
        if let Some(id) = self.selected {
            if self.current().is_some_and(|n| n.text.trim().is_empty()) {
                self.notes.retain(|n| n.id != id);
                effects.retain(|e| !matches!(e, Effect::Save(i, _) if *i == id));
                effects.push(Effect::Remove(id));
            }
        }
        effects
    }

    fn open(&mut self, id: u64) {
        self.selected = Some(id);
        self.caret = self.text().len();
        self.anchor = None;
        self.goal_x = None;
        self.scroll = 0.0;
        self.undo.clear();
        self.redo.clear();
        self.last_edit = None;
        self.layout_valid = false;
        self.dirty = true;
    }

    fn select_note(&mut self, id: u64) -> Vec<Effect> {
        if self.selected == Some(id) {
            return Vec::new();
        }
        let effects = self.leave_note();
        self.open(id);
        effects
    }

    pub fn new_note(&mut self) -> Vec<Effect> {
        let mut effects = self.leave_note();
        self.search.clear();
        self.search_focus = false;
        let id = store::new_id().max(self.notes.iter().map(|n| n.id + 1).max().unwrap_or(0));
        self.notes.insert(0, Note { id, text: String::new(), modified: store::now() });
        self.open(id);
        self.list_scroll = 0.0;
        effects.extend(self.take_save());
        effects
    }

    fn delete_note(&mut self) -> Vec<Effect> {
        let Some(id) = self.selected else { return Vec::new() };
        let visible: Vec<u64> = self.visible().iter().map(|n| n.id).collect();
        let pos = visible.iter().position(|&v| v == id).unwrap_or(0);
        let empty = self.current().is_some_and(|n| n.text.trim().is_empty());
        self.unsaved = None;
        self.notes.retain(|n| n.id != id);
        self.selected = None;
        let next = visible.iter().copied().filter(|&v| v != id).nth(pos.min(visible.len().saturating_sub(2)));
        if let Some(next) = next {
            self.open(next);
        }
        self.dirty = true;
        vec![if empty { Effect::Remove(id) } else { Effect::Trash(id) }]
    }

    /// Replaces `[from, to)` of the open note with `with`; one undo step per typing burst.
    fn edit(&mut self, from: usize, to: usize, with: &str, coalesce: bool) {
        let Some(id) = self.selected else { return };
        let now = Instant::now();
        let merge = coalesce && self.last_edit.is_some_and(|t| now - t < UNDO_PAUSE);
        let before = (self.text().to_owned(), self.caret);
        if !merge {
            self.undo.push(before);
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = coalesce.then_some(now);
        let note = self.notes.iter_mut().find(|n| n.id == id).unwrap();
        note.text.replace_range(from..to, with);
        note.modified = store::now();
        self.caret = from + with.len();
        self.anchor = None;
        self.goal_x = None;
        self.unsaved = Some(id);
        // Newest change on top, as macOS Notes sorts.
        if let Some(i) = self.notes.iter().position(|n| n.id == id) {
            let note = self.notes.remove(i);
            self.notes.insert(0, note);
        }
        self.layout_valid = false;
        self.dirty = true;
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }

    fn insert(&mut self, s: &str) {
        let (from, to) = self.selection().unwrap_or((self.caret, self.caret));
        let coalesce = s.chars().count() == 1 && from == to;
        self.edit(from, to, s, coalesce);
    }

    fn restore(&mut self, redo: bool) {
        let Some(id) = self.selected else { return };
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some((text, caret)) = from.pop() else { return };
        let note = self.notes.iter_mut().find(|n| n.id == id).unwrap();
        to.push((std::mem::replace(&mut note.text, text), self.caret));
        note.modified = store::now();
        self.caret = caret.min(note.text.len());
        while !note.text.is_char_boundary(self.caret) {
            self.caret -= 1;
        }
        self.anchor = None;
        self.last_edit = None;
        self.unsaved = Some(id);
        self.layout_valid = false;
        self.dirty = true;
    }

    // ---------- layout ----------

    fn text_x(&self) -> f32 {
        SIDEBAR_W + PAD
    }

    fn text_w(&self) -> f32 {
        (self.w - SIDEBAR_W - PAD * 2.0).max(120.0)
    }

    fn relayout(&mut self) {
        if self.layout_valid {
            return;
        }
        self.layout_valid = true;
        let width = self.text_w();
        let text = self.text().to_owned();
        let mut lines = Vec::new();
        let mut y = 0.0;
        let mut start = 0;
        for (index, para) in text.split('\n').enumerate() {
            let ((size, height), bold) = if index == 0 { (TITLE, true) } else { (BODY, false) };
            let mut ls = start;
            let end = start + para.len();
            loop {
                // Longest prefix of [ls, end) that fits, preferring a break after a space.
                let mut fit = end;
                if self.fonts.measure_text(&text[ls..end], size, bold) > width {
                    let mut last_space = None;
                    let mut i = ls;
                    fit = ls;
                    while i < end {
                        let next = next_boundary(&text, i);
                        if self.fonts.measure_text(&text[ls..next], size, bold) > width {
                            break;
                        }
                        if text[i..next].chars().all(char::is_whitespace) {
                            last_space = Some(next);
                        }
                        fit = next;
                        i = next;
                    }
                    if let Some(s) = last_space.filter(|&s| s > ls && fit < end) {
                        fit = s;
                    }
                    if fit == ls {
                        fit = next_boundary(&text, ls);
                    }
                }
                lines.push(Line { start: ls, end: fit, y, size, height, bold });
                y += height;
                if fit >= end {
                    break;
                }
                ls = fit;
            }
            start = end + 1;
        }
        self.layout = lines;
    }

    /// Visual line of the caret; at a soft wrap the caret belongs to the next line.
    fn line_of(&self, pos: usize) -> usize {
        self.layout.iter().rposition(|l| l.start <= pos).unwrap_or(0)
    }

    fn x_of(&mut self, line: Line, pos: usize) -> f32 {
        let text = self.text().to_owned();
        self.fonts.measure_text(&text[line.start..pos.clamp(line.start, line.end)], line.size, line.bold)
    }

    fn pos_at_x(&mut self, line: Line, x: f32) -> usize {
        let text = self.text().to_owned();
        // A soft-wrapped line's end is the next line's start: stop before it.
        let soft = line.end < text.len() && text.as_bytes()[line.end] != b'\n';
        let last = if soft && line.end > line.start { prev_boundary(&text, line.end) } else { line.end };
        let mut best = line.start;
        let mut best_d = f32::MAX;
        let mut i = line.start;
        loop {
            let d = (self.fonts.measure_text(&text[line.start..i], line.size, line.bold) - x).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
            if i >= last {
                break;
            }
            i = next_boundary(&text, i);
        }
        best
    }

    fn pos_at(&mut self, x: f32, y: f32) -> usize {
        self.relayout();
        let ly = y - TEXT_TOP + self.scroll;
        let Some(&line) = self.layout.iter().find(|l| ly < l.y + l.height).or(self.layout.last()) else { return 0 };
        self.pos_at_x(line, x - self.text_x())
    }

    fn content_h(&mut self) -> f32 {
        self.relayout();
        self.layout.last().map_or(0.0, |l| l.y + l.height)
    }

    fn reveal_caret(&mut self) {
        self.relayout();
        let Some(&line) = self.layout.get(self.line_of(self.caret)) else { return };
        let view = self.h - TEXT_TOP - 24.0;
        if line.y < self.scroll {
            self.scroll = line.y;
        } else if line.y + line.height > self.scroll + view {
            self.scroll = line.y + line.height - view;
        }
    }

    // ---------- geometry ----------

    fn traffic(&self) -> [(Hit, f32); 3] {
        [(Hit::Close, 26.0), (Hit::Minimize, 46.0), (Hit::Zoom, 66.0)]
    }

    fn search_rect(&self) -> (f32, f32, f32, f32) {
        (20.0, 50.0, SIDEBAR_W - 40.0, 30.0)
    }

    fn button_rects(&self) -> [(Hit, (f32, f32, f32, f32)); 2] {
        [(Hit::Delete, (self.w - 96.0, 12.0, 36.0, 28.0)), (Hit::New, (self.w - 52.0, 12.0, 36.0, 28.0))]
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| x >= rx && x < rx + rw && y >= ry && y < ry + rh;
        if x < SIDEBAR_W {
            if y < 44.0 {
                for (hit, cx) in self.traffic() {
                    if (x - cx).abs() <= 8.0 && (y - 26.0).abs() <= 8.0 {
                        return hit;
                    }
                }
                return Hit::Toolbar;
            }
            if inside(self.search_rect()) {
                return Hit::Search;
            }
            if y >= LIST_TOP && y < self.h - 12.0 {
                let i = ((y - LIST_TOP + self.list_scroll) / ROW_H).floor();
                if i >= 0.0 && (i as usize) < self.visible().len() {
                    return Hit::Row(i as usize);
                }
                return Hit::List;
            }
            return Hit::Toolbar;
        }
        for (hit, r) in self.button_rects() {
            if inside(r) {
                return hit;
            }
        }
        if y < TOOLBAR_H {
            return Hit::Toolbar;
        }
        if self.selected.is_some() {
            Hit::Editor
        } else {
            Hit::Nothing
        }
    }

    pub fn resize_edges(&self, x: f32, y: f32) -> u32 {
        if self.maximized {
            return 0;
        }
        let mut edges = 0;
        if y < RESIZE_BAND { edges |= 1 }
        if y >= self.h - RESIZE_BAND { edges |= 2 }
        if x < RESIZE_BAND { edges |= 4 }
        if x >= self.w - RESIZE_BAND { edges |= 8 }
        edges
    }

    pub fn cursor_at(&self, x: f32, y: f32) -> Cursor {
        match (self.resize_edges(x, y), self.hit(x, y)) {
            (e, _) if e != 0 => Cursor::Resize(e),
            (_, Hit::Editor | Hit::Search) => Cursor::Text,
            _ => Cursor::Default,
        }
    }

    // ---------- input ----------

    pub fn resize(&mut self, w: f32, h: f32) {
        if (w, h) != (self.w, self.h) {
            self.w = w.max(560.0);
            self.h = h.max(360.0);
            self.layout_valid = false;
            self.dirty = true;
        }
    }

    pub fn motion(&mut self, x: f32, y: f32) {
        if self.selecting {
            let pos = self.pos_at(x, y);
            if pos != self.caret {
                self.caret = pos;
                self.goal_x = None;
                self.dirty = true;
            }
            return;
        }
        let hit = self.hit(x, y);
        let visual = |h: Hit| matches!(h, Hit::Close | Hit::Minimize | Hit::Zoom | Hit::New | Hit::Delete | Hit::Row(_));
        if hit != self.hover && (visual(hit) || visual(self.hover)) {
            self.dirty = true;
        }
        self.hover = hit;
    }

    pub fn leave(&mut self) {
        if self.hover != Hit::Nothing {
            self.hover = Hit::Nothing;
            self.dirty = true;
        }
    }

    pub fn press(&mut self, button: u32, x: f32, y: f32, mods: Mods) -> Vec<Effect> {
        if button != BTN_LEFT {
            return Vec::new();
        }
        let edges = self.resize_edges(x, y);
        if edges != 0 {
            return vec![Effect::Resize(edges)];
        }
        self.dirty = true;
        let now = Instant::now();
        let double = self.last_click.is_some_and(|(t, px, py)| now - t < DOUBLE_CLICK && (px - x).hypot(py - y) < 4.0);
        self.last_click = Some((now, x, y));
        let hit = self.hit(x, y);
        self.search_focus = hit == Hit::Search;
        match hit {
            Hit::Close => vec![Effect::Close],
            Hit::Minimize => vec![Effect::Minimize],
            Hit::Zoom => vec![Effect::ToggleMaximize],
            Hit::Toolbar if double => vec![Effect::ToggleMaximize],
            Hit::Toolbar => vec![Effect::Move],
            Hit::Search => {
                let (sx, _, sw, _) = self.search_rect();
                self.search.click(&mut self.fonts, x - sx - 30.0, sw - 40.0, 13.0, false, mods.shift);
                Vec::new()
            }
            Hit::New => self.new_note(),
            Hit::Delete => self.delete_note(),
            Hit::Row(i) => {
                let id = self.visible()[i].id;
                self.select_note(id)
            }
            Hit::Editor => {
                let pos = self.pos_at(x, y);
                if double {
                    let text = self.text().to_owned();
                    self.anchor = Some(word_left(&text, next_boundary(&text, pos).min(text.len())));
                    self.caret = word_right(&text, pos);
                } else {
                    if mods.shift {
                        self.anchor.get_or_insert(self.caret);
                    } else {
                        self.anchor = Some(pos);
                    }
                    self.caret = pos;
                    self.selecting = true;
                }
                self.goal_x = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn release(&mut self, button: u32) {
        if button == BTN_LEFT && self.selecting {
            self.selecting = false;
            if self.anchor == Some(self.caret) {
                self.anchor = None;
            }
        }
    }

    pub fn scroll(&mut self, dy: f32, x: f32) {
        if x < SIDEBAR_W {
            let max = (self.visible().len() as f32 * ROW_H - (self.h - LIST_TOP - 12.0)).max(0.0);
            self.list_scroll = (self.list_scroll + dy).clamp(0.0, max);
        } else {
            let max = (self.content_h() - (self.h - TEXT_TOP - 40.0)).max(0.0);
            self.scroll = (self.scroll + dy).clamp(0.0, max);
        }
        self.dirty = true;
    }

    fn move_caret(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to;
        self.reveal_caret();
    }

    pub fn paste_text(&mut self, text: &str) {
        if self.selected.is_none() || self.search_focus {
            if self.search_focus {
                self.search.insert(&text.replace(['\n', '\r'], " "));
                self.dirty = true;
            }
            return;
        }
        let clean: String = text.replace("\r\n", "\n").chars().filter(|c| *c == '\n' || *c == '\t' || !c.is_control()).collect();
        self.insert(&clean.replace('\t', "    "));
        self.reveal_caret();
    }

    pub fn key(&mut self, sym: u32, utf8: Option<&str>, mods: Mods) -> Vec<Effect> {
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        let sym = K::new(sym);
        self.dirty = true;
        let latin = |c: char| sym == K::new(c as u32) || sym == K::new(c.to_ascii_uppercase() as u32);
        if mods.ctrl {
            if latin('n') {
                return self.new_note();
            }
            if latin('f') {
                self.search_focus = true;
                return Vec::new();
            }
            if latin('w') {
                return vec![Effect::Close];
            }
        }
        if self.search_focus {
            match sym {
                K::Escape => {
                    self.search.clear();
                    self.search_focus = false;
                }
                K::Return | K::KP_Enter | K::Down => self.search_focus = false,
                _ if mods.ctrl && latin('v') => return vec![Effect::Paste],
                _ => {
                    if self.search.key(sym.raw(), utf8, mods.ctrl, mods.shift) != feather_ui::input::Edit::Changed {
                        return Vec::new();
                    }
                }
            }
            self.list_scroll = 0.0;
            if let Some(first) = self.visible().first().map(|n| n.id) {
                if !self.visible().iter().any(|n| Some(n.id) == self.selected) {
                    return self.select_note(first);
                }
            }
            return Vec::new();
        }
        if self.selected.is_none() {
            // Typing with no note open starts one, like clicking ✎.
            if let Some(t) = utf8.filter(|t| !mods.ctrl && !t.chars().any(char::is_control)) {
                let effects = self.new_note();
                self.insert(t);
                return effects;
            }
            return Vec::new();
        }
        self.relayout();
        let text = self.text().to_owned();
        let extend = mods.shift;
        match sym {
            _ if mods.ctrl && latin('a') => {
                self.anchor = Some(0);
                self.caret = text.len();
            }
            _ if mods.ctrl && (latin('c') || latin('x')) => {
                if let Some((a, b)) = self.selection() {
                    let copied = text[a..b].to_owned();
                    if latin('x') {
                        self.edit(a, b, "", false);
                    }
                    return vec![Effect::Copy(copied)];
                }
            }
            _ if mods.ctrl && latin('v') => return vec![Effect::Paste],
            _ if mods.ctrl && latin('z') => self.restore(mods.shift),
            _ if mods.ctrl && latin('y') => self.restore(true),
            K::Left => {
                let to = match self.selection() {
                    Some((a, _)) if !extend => a,
                    _ if mods.ctrl => word_left(&text, self.caret),
                    _ => prev_boundary(&text, self.caret),
                };
                self.move_caret(to, extend);
                self.goal_x = None;
            }
            K::Right => {
                let to = match self.selection() {
                    Some((_, b)) if !extend => b,
                    _ if mods.ctrl => word_right(&text, self.caret),
                    _ => next_boundary(&text, self.caret),
                };
                self.move_caret(to, extend);
                self.goal_x = None;
            }
            K::Up | K::Down => {
                let li = self.line_of(self.caret);
                let line = self.layout[li];
                let x = match self.goal_x {
                    Some(x) => x,
                    None => self.x_of(line, self.caret),
                };
                let target = if sym == K::Up { li.checked_sub(1) } else { (li + 1 < self.layout.len()).then_some(li + 1) };
                let to = match target {
                    Some(t) => {
                        let tl = self.layout[t];
                        self.pos_at_x(tl, x)
                    }
                    None if sym == K::Up => 0,
                    None => text.len(),
                };
                self.move_caret(to, extend);
                self.goal_x = Some(x);
            }
            K::Home | K::End => {
                let line = self.layout[self.line_of(self.caret)];
                let to = match (sym == K::Home, mods.ctrl) {
                    (true, true) => 0,
                    (false, true) => text.len(),
                    (true, false) => line.start,
                    (false, false) => line.end,
                };
                self.move_caret(to, extend);
                self.goal_x = None;
            }
            K::BackSpace => {
                match self.selection() {
                    Some((a, b)) => self.edit(a, b, "", false),
                    None if self.caret > 0 => {
                        let from = if mods.ctrl { word_left(&text, self.caret) } else { prev_boundary(&text, self.caret) };
                        self.edit(from, self.caret, "", !mods.ctrl);
                    }
                    None => {}
                }
                self.reveal_caret();
            }
            K::Delete => {
                match self.selection() {
                    Some((a, b)) => self.edit(a, b, "", false),
                    None if self.caret < text.len() => {
                        let to = if mods.ctrl { word_right(&text, self.caret) } else { next_boundary(&text, self.caret) };
                        self.edit(self.caret, to, "", false);
                    }
                    None => {}
                }
                self.reveal_caret();
            }
            K::Return | K::KP_Enter => {
                self.insert("\n");
                self.reveal_caret();
            }
            K::Tab => {
                self.insert("    ");
                self.reveal_caret();
            }
            K::Escape => self.anchor = None,
            _ => {
                if let Some(t) = utf8.filter(|t| !mods.ctrl && !t.is_empty() && !t.chars().any(char::is_control)) {
                    self.insert(t);
                    self.reveal_caret();
                }
            }
        }
        Vec::new()
    }

    // ---------- painting ----------

    pub fn paint(&mut self, pm: &mut PixmapMut) {
        self.dirty = false;
        self.relayout();
        pm.fill(colors::window());
        self.paint_editor(pm);
        self.paint_toolbar(pm);
        self.paint_sidebar(pm);
    }

    fn paint_sidebar(&mut self, pm: &mut PixmapMut) {
        theme::fill_rrect(pm, 8.0, 8.0, SIDEBAR_W - 16.0, self.h - 16.0, 14.0, colors::sidebar());
        theme::stroke_rrect(pm, 8.5, 8.5, SIDEBAR_W - 17.0, self.h - 17.0, 13.5, 1.0, colors::sidebar_edge());
        let hovered_traffic = matches!(self.hover, Hit::Close | Hit::Minimize | Hit::Zoom);
        for (hit, cx) in self.traffic() {
            let color = if !self.focused && !hovered_traffic {
                colors::traffic_inactive()
            } else {
                match hit {
                    Hit::Close => rgba(255, 95, 87, 255),
                    Hit::Minimize => rgba(254, 188, 46, 255),
                    _ => rgba(40, 200, 64, 255),
                }
            };
            theme::fill_circle(pm, cx, 26.0, 6.0, color);
            if hovered_traffic {
                let ink = rgba(0, 0, 0, 140);
                match hit {
                    Hit::Close => {
                        theme::line(pm, cx - 2.5, 23.5, cx + 2.5, 28.5, 1.2, ink);
                        theme::line(pm, cx + 2.5, 23.5, cx - 2.5, 28.5, 1.2, ink);
                    }
                    Hit::Minimize => theme::line(pm, cx - 3.0, 26.0, cx + 3.0, 26.0, 1.2, ink),
                    _ => {
                        theme::line(pm, cx - 3.0, 26.0, cx + 3.0, 26.0, 1.2, ink);
                        theme::line(pm, cx, 23.0, cx, 29.0, 1.2, ink);
                    }
                }
            }
        }
        let count = self.visible().len();
        let label = match (count % 10, count % 100) {
            (1, r) if r != 11 => "заметка",
            (2..=4, r) if !(12..=14).contains(&r) => "заметки",
            _ => "заметок",
        };
        self.fonts.draw_text_right(pm, &format!("{count} {label}"), SIDEBAR_W - 22.0, 19.0, 11.0, colors::tertiary(), false);
        // Search
        let (sx, sy, sw, sh) = self.search_rect();
        theme::fill_rrect(pm, sx, sy, sw, sh, 15.0, colors::control());
        if self.search_focus {
            theme::stroke_rrect(pm, sx - 1.0, sy - 1.0, sw + 2.0, sh + 2.0, 16.0, 2.5, rgba(YELLOW.0, YELLOW.1, YELLOW.2, 150));
        }
        let ink = colors::secondary();
        theme::stroke_circle(pm, sx + 16.0, sy + 14.0, 4.5, 1.5, ink);
        theme::line(pm, sx + 19.5, sy + 17.5, sx + 22.5, sy + 20.5, 1.6, ink);
        if self.search.is_empty() && !self.search_focus {
            self.fonts.draw_text(pm, "Поиск", sx + 30.0, sy + 7.5, 13.0, colors::tertiary(), false);
        } else {
            self.search.draw(pm, &mut self.fonts, sx + 30.0, sy + 7.5, sw - 40.0, 13.0, false, colors::text(), self.search_focus, rgba(YELLOW.0, YELLOW.1, YELLOW.2, 255));
        }
        // List
        let rows: Vec<(u64, String, String, i64)> = self
            .visible()
            .iter()
            .map(|n| {
                let (t, p) = summary(&n.text);
                (n.id, t, p, n.modified)
            })
            .collect();
        if rows.is_empty() {
            let msg = if !self.loaded {
                ""
            } else if self.search.is_empty() {
                "Нет заметок"
            } else {
                "Ничего не найдено"
            };
            self.fonts.draw_text_centered(pm, msg, SIDEBAR_W / 2.0, LIST_TOP + 40.0, 13.0, colors::tertiary(), false);
        }
        let bottom = self.h - 14.0;
        for (i, (id, title, preview, modified)) in rows.into_iter().enumerate() {
            let y = LIST_TOP + i as f32 * ROW_H - self.list_scroll;
            if y + ROW_H < LIST_TOP - 4.0 || y > bottom {
                continue;
            }
            let selected = Some(id) == self.selected;
            if selected {
                theme::fill_rrect(pm, 16.0, y + 2.0, SIDEBAR_W - 32.0, ROW_H - 4.0, 9.0, colors::note_selection(self.focused));
            } else if self.hover == Hit::Row(i) {
                theme::fill_rrect(pm, 16.0, y + 2.0, SIDEBAR_W - 32.0, ROW_H - 4.0, 9.0, colors::hover());
            } else if i > 0 && !matches!(self.hover, Hit::Row(h) if h + 1 == i) {
                theme::fill_rect(pm, 28.0, y, SIDEBAR_W - 56.0, 1.0, colors::separator());
            }
            let w = SIDEBAR_W - 56.0;
            let title = self.fonts.ellipsize(&title, w, 13.0, true);
            self.fonts.draw_text(pm, &title, 28.0, y + 11.0, 13.0, colors::text(), true);
            let date = short_date(modified);
            let dw = self.fonts.draw_text(pm, &date, 28.0, y + 33.0, 12.0, colors::text(), false);
            let preview = self.fonts.ellipsize(&preview, (w - dw - 8.0).max(10.0), 12.0, false);
            self.fonts.draw_text(pm, &preview, 28.0 + dw + 8.0, y + 33.0, 12.0, colors::secondary(), false);
        }
        // Rows scroll under the search field: repaint the band above the list.
        theme::fill_rect(pm, 9.0, LIST_TOP - 10.0, SIDEBAR_W - 18.0, 8.0, colors::sidebar());
    }

    fn paint_toolbar(&mut self, pm: &mut PixmapMut) {
        theme::fill_rect(pm, SIDEBAR_W, 0.0, self.w - SIDEBAR_W, TOOLBAR_H, colors::window());
        if self.scroll > 0.0 {
            theme::fill_rect(pm, SIDEBAR_W, TOOLBAR_H - 1.0, self.w - SIDEBAR_W, 1.0, colors::separator());
        }
        for (hit, (x, y, w, h)) in self.button_rects() {
            let enabled = hit == Hit::New || self.selected.is_some();
            if enabled && self.hover == hit {
                theme::fill_rrect(pm, x, y, w, h, 8.0, colors::hover());
            }
            let ink = if enabled { colors::secondary() } else { colors::tertiary() };
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            if hit == Hit::New {
                // Square with a pencil (compose)
                theme::polyline(pm, &[(cx + 1.0, cy - 7.0), (cx - 7.0, cy - 7.0), (cx - 7.0, cy + 7.0), (cx + 7.0, cy + 7.0), (cx + 7.0, cy - 1.0)], 1.4, ink);
                theme::line(pm, cx - 2.0, cy + 2.0, cx + 6.5, cy - 6.5, 1.6, ink);
                theme::line(pm, cx + 5.0, cy - 8.0, cx + 8.0, cy - 5.0, 1.6, ink);
            } else {
                // Trash outline
                theme::line(pm, cx - 7.0, cy - 5.0, cx + 7.0, cy - 5.0, 1.4, ink);
                theme::polyline(pm, &[(cx - 2.5, cy - 5.0), (cx - 2.0, cy - 8.0), (cx + 2.0, cy - 8.0), (cx + 2.5, cy - 5.0)], 1.3, ink);
                theme::polyline(pm, &[(cx - 5.5, cy - 5.0), (cx - 4.5, cy + 8.0), (cx + 4.5, cy + 8.0), (cx + 5.5, cy - 5.0)], 1.4, ink);
                theme::line(pm, cx - 1.8, cy - 2.0, cx - 1.5, cy + 5.0, 1.1, ink);
                theme::line(pm, cx + 1.8, cy - 2.0, cx + 1.5, cy + 5.0, 1.1, ink);
            }
        }
    }

    fn paint_editor(&mut self, pm: &mut PixmapMut) {
        let Some(note) = self.current() else {
            if self.loaded {
                let cx = SIDEBAR_W + (self.w - SIDEBAR_W) / 2.0;
                self.fonts.draw_text_centered(pm, "Выберите заметку или создайте новую (Ctrl+N)", cx, self.h / 2.0 - 8.0, 13.0, colors::tertiary(), false);
            }
            return;
        };
        let modified = note.modified;
        let text = note.text.clone();
        let x0 = self.text_x();
        let date = russian_date(modified);
        self.fonts.draw_text_centered(pm, &date, SIDEBAR_W + (self.w - SIDEBAR_W) / 2.0, TOOLBAR_H + 10.0 - self.scroll, 12.0, colors::tertiary(), false);
        let selection = self.selection();
        let lines = self.layout.clone();
        for line in &lines {
            let y = TEXT_TOP + line.y - self.scroll;
            if y + line.height < TOOLBAR_H || y > self.h {
                continue;
            }
            if let Some((a, b)) = selection {
                if a <= line.end && b >= line.start && !(a == line.end && b == line.end && line.end < b) {
                    let sx = self.fonts.measure_text(&text[line.start..a.clamp(line.start, line.end)], line.size, line.bold);
                    let mut ex = self.fonts.measure_text(&text[line.start..b.clamp(line.start, line.end)], line.size, line.bold);
                    if b > line.end {
                        ex += 6.0;
                    }
                    if ex > sx {
                        let tint = if self.focused { rgba(YELLOW.0, YELLOW.1, YELLOW.2, 110) } else { colors::selected_gray() };
                        theme::fill_rect(pm, x0 + sx, y + 1.0, ex - sx, line.height - 2.0, tint);
                    }
                }
            }
            let baseline_pad = (line.height - line.size) / 2.0 - 1.0;
            self.fonts.draw_text(pm, &text[line.start..line.end], x0, y + baseline_pad, line.size, colors::text(), line.bold);
        }
        if text.is_empty() {
            self.fonts.draw_text(pm, "Заголовок", x0, TEXT_TOP + (TITLE.1 - TITLE.0) / 2.0 - 1.0 - self.scroll, TITLE.0, colors::tertiary(), true);
        }
        if selection.is_none() && !self.search_focus && self.focused {
            let li = self.line_of(self.caret);
            if let Some(&line) = lines.get(li) {
                let cx = x0 + self.x_of(line, self.caret);
                let y = TEXT_TOP + line.y - self.scroll;
                theme::fill_rect(pm, cx, y + 3.0, 2.0, line.height - 6.0, rgba(YELLOW.0 - 20, YELLOW.1 - 40, 0, 255));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(text: &str) -> App {
        let mut app = App::new(FontSystem::new());
        app.loaded(vec![Note { id: 1, text: text.into(), modified: 0 }]);
        app
    }

    fn typing(app: &mut App, s: &str) {
        for c in s.chars() {
            let t = c.to_string();
            app.key(0, Some(&t), Mods::default());
        }
    }

    #[test]
    fn search_selection_mouse_and_paste_do_not_edit_the_note() {
        feather_ui::scale::with_render_settings(1.0, feather_ui::scale::Appearance::Light, || {
        let mut app = app_with("Список покупок");
        let ctrl = Mods { ctrl: true, ..Default::default() };
        app.key(0x66, Some("f"), ctrl);
        typing(&mut app, "покупок");
        app.key(0x61, Some("a"), ctrl);
        assert_eq!(app.search.selected_text(), Some("покупок"));
        assert_eq!(app.search.text(), "покупок");
        app.paste_text("Список");
        assert_eq!(app.search.text(), "Список");
        let (x, y, _, _) = app.search_rect();
        app.press(BTN_LEFT, x + 30.0, y + 12.0, Mods::default());
        assert_eq!(app.search.caret(), 0);
        app.key(0xff53, None, Mods { shift: true, ..Default::default() });
        assert_eq!(app.search.selected_text(), Some("С"));
        typing(&mut app, "с");
        assert_eq!(app.search.text(), "список");
        assert_eq!(app.text(), "Список покупок");
        let mut image = tiny_skia::Pixmap::new(app.w as u32, app.h as u32).unwrap();
        app.paint(&mut image.as_mut());
        std::fs::create_dir_all("/tmp/feather-inputs").unwrap();
        image.save_png("/tmp/feather-inputs/notes.png").unwrap();
        });
    }

    #[test]
    fn dark_theme_keeps_note_and_search_readable_at_fractional_scales() {
        std::fs::create_dir_all("/tmp/feather-inputs").unwrap();
        for scale in [1.0, 1.25, 2.0] {
            feather_ui::scale::with_render_settings(scale, feather_ui::scale::Appearance::Dark, || {
                let mut app = app_with("Планы на день\nПроверить тёмную тему и выделение текста.");
                app.search_focus = true;
                app.search.set("Планы");
                app.search.select_all();
                let mut image = tiny_skia::Pixmap::new((app.w * scale) as u32, (app.h * scale) as u32).unwrap();
                app.paint(&mut image.as_mut());
                let bg = image.pixel((800.0 * scale) as u32, (500.0 * scale) as u32).unwrap();
                assert!(bg.red() < 60 && bg.green() < 60 && bg.blue() < 60, "white editor in dark mode");
                image.save_png(format!("/tmp/feather-inputs/notes-dark-{scale}.png")).unwrap();
            });
        }
    }

    #[test]
    fn typing_editing_undo_and_save() {
        let mut app = app_with("Список");
        assert_eq!(app.caret, "Список".len());
        app.key(0xff0d, None, Mods::default());
        typing(&mut app, "молоко");
        assert_eq!(app.text(), "Список\nмолоко");
        assert_eq!(app.take_save(), Some(Effect::Save(1, "Список\nмолоко".into())));
        assert_eq!(app.take_save(), None);
        // Shift+Left ×6 selects the word, Ctrl+X cuts it.
        let shift = Mods { shift: true, ..Default::default() };
        for _ in 0..6 {
            app.key(0xff51, None, shift);
        }
        let ctrl = Mods { ctrl: true, ..Default::default() };
        assert_eq!(app.key(0x78, Some("x"), ctrl), vec![Effect::Copy("молоко".into())]);
        assert_eq!(app.text(), "Список\n");
        app.paste_text("хлеб\r\nсыр");
        assert_eq!(app.text(), "Список\nхлеб\nсыр");
        app.key(0x7a, Some("z"), ctrl);
        assert_eq!(app.text(), "Список\n");
        app.key(0x7a, Some("z"), ctrl);
        assert_eq!(app.text(), "Список\nмолоко");
        app.key(0x7a, Some("z"), Mods { ctrl: true, shift: true });
        assert_eq!(app.text(), "Список\n");
        // Backspace through a multi-byte character keeps the text valid.
        app.key(0xff08, None, Mods::default());
        app.key(0xff08, None, Mods::default());
        assert_eq!(app.text(), "Списо");
    }

    #[test]
    fn wrapping_caret_motion_and_empty_note_cleanup() {
        let mut app = app_with(&format!("Заголовок\n{}", "слово ".repeat(60)));
        app.resize(700.0, 500.0);
        app.relayout();
        assert!(app.layout.len() > 3, "long paragraph wraps");
        assert!(app.layout.iter().all(|l| app.text().is_char_boundary(l.start) && app.text().is_char_boundary(l.end)));
        let first = app.layout[0];
        assert!(first.bold && first.size == TITLE.0);
        // Up from the end walks through every visual line to the start.
        let lines = app.layout.len();
        for _ in 0..lines {
            app.key(0xff52, None, Mods::default());
        }
        assert_eq!(app.caret, 0);
        // A new note left empty disappears; a written one stays and is listed first.
        app.new_note();
        let id = app.selected.unwrap();
        assert_eq!(app.select_note(1), vec![Effect::Remove(id)]);
        assert_eq!(app.notes.len(), 1);
        app.new_note();
        typing(&mut app, "Идея");
        let idea = app.selected.unwrap();
        assert_eq!(app.visible()[0].text, "Идея");
        // Deleting goes to the Trash and opens the neighbour.
        assert_eq!(app.delete_note(), vec![Effect::Trash(idea)]);
        assert_eq!(app.selected, Some(1));
    }
}
