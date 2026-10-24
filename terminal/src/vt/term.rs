//! Terminal state: screen grid, scrollback, cursor and modes, driven by the parser.
//! Compatible with what `TERM=xterm-256color` promises to programs (vim, htop, mc, less,
//! tmux): SGR with 16/256/true colour, scroll regions, insert/delete, alternate screen,
//! application keys, mouse (SGR), bracketed paste, synchronized output, DA/DSR replies.

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use base64::Engine;

use super::parser::{Params, Perform};

/// Default, one of 256 palette entries, or 24-bit RGB, packed in four bytes
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Color(u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorKind {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    pub const DEFAULT: Color = Color(0);
    pub fn indexed(i: u8) -> Self {
        Color(0x0100_0000 | i as u32)
    }
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color(0x0200_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }
    pub fn kind(self) -> ColorKind {
        match self.0 >> 24 {
            0 => ColorKind::Default,
            1 => ColorKind::Indexed(self.0 as u8),
            _ => ColorKind::Rgb((self.0 >> 16) as u8, (self.0 >> 8) as u8, self.0 as u8),
        }
    }
}

pub mod flags {
    pub const BOLD: u16 = 1;
    pub const DIM: u16 = 2;
    pub const ITALIC: u16 = 4;
    pub const UNDERLINE: u16 = 8;
    pub const BLINK: u16 = 16;
    pub const INVERSE: u16 = 32;
    pub const HIDDEN: u16 = 64;
    pub const STRIKE: u16 = 128;
    pub const DOUBLE_UNDERLINE: u16 = 256;
    /// Left half of a double-width character
    pub const WIDE: u16 = 512;
    /// Right half of a double-width character (draws nothing itself)
    pub const SPACER: u16 = 1024;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: Color::DEFAULT, bg: Color::DEFAULT, flags: 0 }
    }
}

impl Cell {
    /// Erased cell: keeps the current background (xterm's "bce")
    fn blank(bg: Color) -> Self {
        Cell { bg, ..Cell::default() }
    }

    fn is_blank(&self) -> bool {
        self.ch == ' ' && self.bg == Color::DEFAULT && self.flags & !(flags::BOLD | flags::ITALIC | flags::DIM) == 0
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The line continues on the next row (autowrap), for copying and search
    pub wrapped: bool,
    /// Combining marks by column (rare, so kept out of every cell)
    pub combining: Vec<(u16, String)>,
}

impl Row {
    fn new(cols: usize, bg: Color) -> Self {
        Row { cells: vec![Cell::blank(bg); cols], wrapped: false, combining: Vec::new() }
    }

    fn reset(&mut self, cols: usize, bg: Color) {
        self.cells.clear();
        self.cells.resize(cols, Cell::blank(bg));
        self.wrapped = false;
        self.combining.clear();
    }

    fn clear_combining(&mut self, from: usize, to: usize) {
        if !self.combining.is_empty() {
            self.combining.retain(|(c, _)| (*c as usize) < from || (*c as usize) >= to);
        }
    }

    /// Scrollback keeps no trailing blanks: most lines are much shorter than the window
    fn trimmed(mut self) -> Self {
        let keep = self.cells.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1);
        self.cells.truncate(keep);
        self.cells.shrink_to_fit();
        self
    }

    #[cfg(test)]
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, c) in self.cells.iter().enumerate() {
            if c.flags & flags::SPACER != 0 {
                continue;
            }
            out.push(c.ch);
            for (col, marks) in &self.combining {
                if *col as usize == i {
                    out.push_str(marks);
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseMode {
    Off,
    /// 1000: presses and releases
    Click,
    /// 1002: plus motion with a button held
    Drag,
    /// 1003: all motion
    Motion,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub origin: bool,
    pub autowrap: bool,
    pub insert: bool,
    pub linefeed_newline: bool,
    pub cursor_keys_app: bool,
    pub keypad_app: bool,
    pub bracketed_paste: bool,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    pub focus_events: bool,
    pub cursor_visible: bool,
    pub reverse_video: bool,
    /// 1007: the wheel scrolls as arrow keys on the alternate screen
    pub alternate_scroll: bool,
    /// 2026: the program asks to show its frame at once
    pub synchronized: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Modes {
            origin: false,
            autowrap: true,
            insert: false,
            linefeed_newline: false,
            cursor_keys_app: false,
            keypad_app: false,
            bracketed_paste: false,
            mouse: MouseMode::Off,
            mouse_sgr: false,
            focus_events: false,
            cursor_visible: true,
            reverse_video: false,
            alternate_scroll: true,
            synchronized: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Saved {
    x: usize,
    y: usize,
    pen: Cell,
    origin: bool,
    charsets: [bool; 2],
    active_g1: bool,
    pending_wrap: bool,
}

pub struct Term {
    cols: usize,
    rows: usize,
    screen: Vec<Row>,
    /// The other buffer: the main screen while the alternate one is shown, and vice versa
    other: Vec<Row>,
    pub alt_active: bool,
    scrollback: VecDeque<Row>,
    max_scrollback: usize,
    pub x: usize,
    pub y: usize,
    pending_wrap: bool,
    pen: Cell,
    saved: [Option<Saved>; 2],
    top: usize,
    bottom: usize,
    pub modes: Modes,
    tabs: Vec<bool>,
    /// G0/G1 designated as DEC special graphics (line drawing)
    charsets: [bool; 2],
    active_g1: bool,
    last_printed: Option<char>,
    pub cursor_shape: CursorShape,
    pub title: Option<String>,
    pub cwd: Option<String>,
    /// Bytes to send back to the program (DA, DSR, colour queries)
    pub responses: Vec<u8>,
    pub bell: bool,
    /// OSC 52 writes are delivered to the native Wayland clipboard by the UI.
    pub clipboard: Option<String>,
    /// Rows of the screen that changed since the renderer last looked
    dirty: Vec<bool>,
    /// Cursor row, column and visibility when the renderer last looked
    painted_cursor: (usize, usize, bool),
    /// Lines pushed into the scrollback since start (keeps a scrolled view in place)
    pub scrolled_out: u64,
    /// Colours reported to programs that ask (OSC 10/11): (foreground, background)
    pub report_colors: ([u8; 3], [u8; 3]),
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(2), rows.max(1));
        Term {
            cols,
            rows,
            screen: vec![Row::new(cols, Color::DEFAULT); rows],
            other: vec![Row::new(cols, Color::DEFAULT); rows],
            alt_active: false,
            scrollback: VecDeque::new(),
            max_scrollback: 10_000,
            x: 0,
            y: 0,
            pending_wrap: false,
            pen: Cell::default(),
            saved: [None, None],
            top: 0,
            bottom: rows - 1,
            modes: Modes::default(),
            tabs: (0..cols).map(|c| c % 8 == 0).collect(),
            charsets: [false, false],
            active_g1: false,
            last_printed: None,
            cursor_shape: CursorShape::Block,
            title: None,
            cwd: None,
            responses: Vec::new(),
            bell: false,
            clipboard: None,
            dirty: vec![true; rows],
            painted_cursor: (0, 0, false),
            scrolled_out: 0,
            report_colors: ([0, 0, 0], [255, 255, 255]),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Row `y` of the view scrolled back by `offset` lines (0 = the live screen)
    pub fn view_row(&self, offset: usize, y: usize) -> &Row {
        let offset = offset.min(self.scrollback.len());
        if y < offset {
            &self.scrollback[self.scrollback.len() - offset + y]
        } else {
            &self.screen[y - offset]
        }
    }

    /// Absolute number of the oldest line kept: lines keep their number while they scroll
    /// from the screen into the history (selection and search use these numbers)
    pub fn first_abs(&self) -> i64 {
        self.scrolled_out as i64 - self.scrollback.len() as i64
    }

    pub fn abs_row(&self, abs: i64) -> Option<&Row> {
        let i = abs - self.first_abs();
        if i < 0 {
            return None;
        }
        let i = i as usize;
        match i.checked_sub(self.scrollback.len()) {
            None => self.scrollback.get(i),
            Some(y) => self.screen.get(y),
        }
    }

    /// Text between two cells (inclusive), joining wrapped lines and trimming line ends
    pub fn text_between(&self, start: (i64, usize), end: (i64, usize)) -> String {
        let mut out = String::new();
        for abs in start.0..=end.0 {
            let Some(row) = self.abs_row(abs) else { continue };
            let from = if abs == start.0 { start.1 } else { 0 };
            let to = if abs == end.0 { end.1 + 1 } else { usize::MAX };
            let mut line = String::new();
            for (x, c) in row.cells.iter().enumerate().take(to).skip(from) {
                if c.flags & flags::SPACER != 0 {
                    continue;
                }
                line.push(c.ch);
                if let Some((_, marks)) = row.combining.iter().find(|(col, _)| *col as usize == x) {
                    line.push_str(marks);
                }
            }
            let continues = row.wrapped && abs != end.0;
            if continues {
                out.push_str(&line);
            } else {
                out.push_str(line.trim_end());
                if abs != end.0 {
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Takes the set of changed screen rows. A cursor that moved without changing cells
    /// (BS, CR, CSI C/D — readline's arrow keys) dirties the row it left and the row it is on.
    pub fn take_dirty(&mut self) -> Vec<bool> {
        let cursor = (self.y, self.x, self.modes.cursor_visible);
        if cursor != self.painted_cursor {
            let left = self.painted_cursor.0;
            self.touch(left);
            self.touch(cursor.0);
            self.painted_cursor = cursor;
        }
        std::mem::replace(&mut self.dirty, vec![false; self.rows])
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty.iter_mut().for_each(|d| *d = true);
    }

    fn touch(&mut self, y: usize) {
        if let Some(d) = self.dirty.get_mut(y) {
            *d = true;
        }
    }

    fn touch_range(&mut self, from: usize, to: usize) {
        for y in from..=to.min(self.rows - 1) {
            self.touch(y);
        }
    }

    /// Plain text of the live screen, for tests
    #[cfg(test)]
    pub fn screen_text(&self) -> Vec<String> {
        self.screen.iter().map(|r| r.text().trim_end().to_string()).collect()
    }

    #[cfg(test)]
    pub fn cell(&self, x: usize, y: usize) -> Cell {
        self.screen[y].cells[x]
    }

    // ---------- resizing ----------

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        for row in self.screen.iter_mut().chain(self.other.iter_mut()) {
            row.cells.resize(cols, Cell::default());
            if cols < self.cols {
                row.clear_combining(cols, usize::MAX);
                // A wide character cut in half becomes a blank
                if let Some(last) = row.cells.last_mut() {
                    if last.flags & flags::WIDE != 0 {
                        *last = Cell::default();
                    }
                }
            }
        }
        let (main, alt) = if self.alt_active { (&mut self.other, &mut self.screen) } else { (&mut self.screen, &mut self.other) };
        let mut cursor_y = self.y as isize;
        if rows < self.rows {
            let mut excess = self.rows - rows;
            // Empty lines below the cursor go first, then the top goes to the scrollback
            while excess > 0 && main.len() > (self.y + 1).max(1) && main.last().is_some_and(|r| r.cells.iter().all(Cell::is_blank)) {
                main.pop();
                excess -= 1;
            }
            for _ in 0..excess {
                let row = main.remove(0);
                if !self.alt_active {
                    push_scrollback(&mut self.scrollback, row, self.max_scrollback, &mut self.scrolled_out);
                }
                cursor_y -= 1;
            }
            main.truncate(rows);
            alt.truncate(rows);
        } else {
            // Growing pulls lines back from the scrollback, like xterm
            for _ in self.rows..rows {
                match (!self.alt_active).then(|| self.scrollback.pop_back()).flatten() {
                    Some(mut row) => {
                        row.cells.resize(cols, Cell::default());
                        main.insert(0, row);
                        cursor_y += 1;
                    }
                    None => main.push(Row::new(cols, Color::DEFAULT)),
                }
            }
            alt.resize(rows, Row::new(cols, Color::DEFAULT));
        }
        main.resize(rows, Row::new(cols, Color::DEFAULT));
        if !self.alt_active {
            self.y = cursor_y.clamp(0, rows as isize - 1) as usize;
        }
        self.cols = cols;
        self.rows = rows;
        self.y = self.y.min(rows - 1);
        self.x = self.x.min(cols - 1);
        self.pending_wrap = false;
        self.top = 0;
        self.bottom = rows - 1;
        self.tabs = (0..cols).map(|c| c % 8 == 0).collect();
        self.dirty = vec![true; rows];
    }

    // ---------- cursor and scrolling ----------

    fn region_top(&self) -> usize {
        if self.modes.origin { self.top } else { 0 }
    }

    fn goto(&mut self, x: usize, y: usize) {
        let (min_y, max_y) = if self.modes.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        self.x = x.min(self.cols - 1);
        self.y = y.clamp(min_y, max_y);
        self.pending_wrap = false;
    }

    fn scroll_up(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        for _ in 0..n {
            let row = self.screen.remove(top);
            if top == 0 && !self.alt_active {
                push_scrollback(&mut self.scrollback, row, self.max_scrollback, &mut self.scrolled_out);
            }
            self.screen.insert(bottom, Row::new(self.cols, self.pen.bg));
        }
        self.touch_range(top, bottom);
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        for _ in 0..n {
            self.screen.remove(bottom);
            self.screen.insert(top, Row::new(self.cols, self.pen.bg));
        }
        self.touch_range(top, bottom);
    }

    fn index(&mut self) {
        if self.y == self.bottom {
            self.scroll_up(1);
        } else if self.y < self.rows - 1 {
            self.y += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.y == self.top {
            self.scroll_down(1);
        } else if self.y > 0 {
            self.y -= 1;
        }
    }

    fn newline(&mut self) {
        self.index();
        if self.modes.linefeed_newline {
            self.x = 0;
        }
    }

    // ---------- editing ----------

    fn erase_cells(&mut self, y: usize, from: usize, to: usize) {
        let bg = self.pen.bg;
        let row = &mut self.screen[y];
        let to = to.min(row.cells.len());
        if from >= to {
            return;
        }
        // Half of a wide character must not survive alone
        if from > 0 && row.cells[from].flags & flags::SPACER != 0 {
            row.cells[from - 1] = Cell::blank(bg);
        }
        if to < row.cells.len() && row.cells[to - 1].flags & flags::WIDE != 0 {
            row.cells[to] = Cell::blank(bg);
        }
        row.cells[from..to].fill(Cell::blank(bg));
        row.clear_combining(from, to);
        self.touch(y);
    }

    fn insert_chars(&mut self, n: usize) {
        let (x, cols, bg) = (self.x, self.cols, self.pen.bg);
        let row = &mut self.screen[self.y];
        let n = n.min(cols - x);
        row.cells[x..].rotate_right(n);
        row.cells[x..x + n].fill(Cell::blank(bg));
        row.clear_combining(x, cols);
        self.touch(self.y);
    }

    fn delete_chars(&mut self, n: usize) {
        let (x, cols, bg) = (self.x, self.cols, self.pen.bg);
        let row = &mut self.screen[self.y];
        let n = n.min(cols - x);
        row.cells[x..].rotate_left(n);
        row.cells[cols - n..].fill(Cell::blank(bg));
        row.clear_combining(x, cols);
        self.touch(self.y);
    }

    fn insert_lines(&mut self, n: usize) {
        if self.y < self.top || self.y > self.bottom {
            return;
        }
        let saved_top = self.top;
        self.top = self.y;
        self.scroll_down(n);
        self.top = saved_top;
        self.x = 0;
    }

    fn delete_lines(&mut self, n: usize) {
        if self.y < self.top || self.y > self.bottom {
            return;
        }
        let (saved_top, y) = (self.top, self.y);
        self.top = y;
        let n = n.min(self.bottom - y + 1);
        for _ in 0..n {
            self.screen.remove(y);
            self.screen.insert(self.bottom, Row::new(self.cols, self.pen.bg));
        }
        self.touch_range(y, self.bottom);
        self.top = saved_top;
        self.x = 0;
    }

    fn erase_display(&mut self, mode: u16) {
        match mode {
            0 => {
                self.erase_cells(self.y, self.x, self.cols);
                for y in self.y + 1..self.rows {
                    self.erase_cells(y, 0, self.cols);
                    self.screen[y].wrapped = false;
                }
            }
            1 => {
                for y in 0..self.y {
                    self.erase_cells(y, 0, self.cols);
                }
                self.erase_cells(self.y, 0, self.x + 1);
            }
            2 => {
                for y in 0..self.rows {
                    self.erase_cells(y, 0, self.cols);
                    self.screen[y].wrapped = false;
                }
            }
            3 => {
                self.scrollback.clear();
                self.mark_all_dirty();
            }
            _ => {}
        }
    }

    fn put(&mut self, c: char, width: usize) {
        if self.pending_wrap && self.modes.autowrap {
            self.screen[self.y].wrapped = true;
            self.x = 0;
            self.index();
        }
        self.pending_wrap = false;
        if width == 2 && self.x == self.cols - 1 {
            if self.modes.autowrap {
                self.erase_cells(self.y, self.x, self.cols);
                self.screen[self.y].wrapped = true;
                self.x = 0;
                self.index();
            } else {
                return;
            }
        }
        if self.modes.insert {
            self.insert_chars(width);
        }
        let (x, y) = (self.x, self.y);
        self.erase_cells(y, x, x + width); // also splits wide neighbours
        let mut cell = self.pen;
        cell.ch = c;
        let row = &mut self.screen[y];
        if width == 2 {
            cell.flags |= flags::WIDE;
            row.cells[x] = cell;
            row.cells[x + 1] = Cell { ch: ' ', flags: (cell.flags & !flags::WIDE) | flags::SPACER, ..cell };
        } else {
            row.cells[x] = cell;
        }
        self.touch(y);
        if x + width >= self.cols {
            self.x = self.cols - 1;
            self.pending_wrap = true;
        } else {
            self.x = x + width;
        }
    }

    fn attach_combining(&mut self, c: char) {
        let (mut x, y) = (self.x, self.y);
        if !self.pending_wrap {
            if x == 0 {
                return;
            }
            x -= 1;
        }
        if self.screen[y].cells[x].flags & flags::SPACER != 0 && x > 0 {
            x -= 1;
        }
        let row = &mut self.screen[y];
        match row.combining.iter_mut().find(|(col, _)| *col as usize == x) {
            Some((_, marks)) => {
                if marks.chars().count() < 8 {
                    marks.push(c);
                }
            }
            None => row.combining.push((x as u16, c.to_string())),
        }
        self.touch(y);
    }

    fn save_cursor(&mut self) {
        self.saved[self.alt_active as usize] = Some(Saved {
            x: self.x,
            y: self.y,
            pen: self.pen,
            origin: self.modes.origin,
            charsets: self.charsets,
            active_g1: self.active_g1,
            pending_wrap: self.pending_wrap,
        });
    }

    fn restore_cursor(&mut self) {
        match self.saved[self.alt_active as usize] {
            Some(s) => {
                self.pen = s.pen;
                self.modes.origin = s.origin;
                self.charsets = s.charsets;
                self.active_g1 = s.active_g1;
                self.x = s.x.min(self.cols - 1);
                self.y = s.y.min(self.rows - 1);
                self.pending_wrap = s.pending_wrap;
            }
            None => {
                self.pen = Cell::default();
                self.modes.origin = false;
                self.goto(0, 0);
            }
        }
    }

    fn set_alt_screen(&mut self, on: bool, clear: bool) {
        if on == self.alt_active {
            return;
        }
        std::mem::swap(&mut self.screen, &mut self.other);
        self.alt_active = on;
        if on && clear {
            let cols = self.cols;
            self.screen.iter_mut().for_each(|r| r.reset(cols, Color::DEFAULT));
        }
        self.mark_all_dirty();
    }

    fn reset(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        let scrollback = std::mem::take(&mut self.scrollback);
        let scrolled = self.scrolled_out;
        let colors = self.report_colors;
        *self = Term::new(cols, rows);
        self.scrollback = scrollback;
        self.scrolled_out = scrolled;
        self.report_colors = colors;
    }

    fn set_mode(&mut self, params: &Params, private: bool, on: bool) {
        for p in params.iter() {
            let mode = p.first().copied().unwrap_or(0);
            if !private {
                match mode {
                    4 => self.modes.insert = on,
                    20 => self.modes.linefeed_newline = on,
                    _ => {}
                }
                continue;
            }
            match mode {
                1 => self.modes.cursor_keys_app = on,
                5 => {
                    self.modes.reverse_video = on;
                    self.mark_all_dirty();
                }
                6 => {
                    self.modes.origin = on;
                    self.goto(0, self.region_top());
                }
                7 => self.modes.autowrap = on,
                25 => {
                    self.modes.cursor_visible = on;
                    self.touch(self.y);
                }
                47 => self.set_alt_screen(on, false),
                1047 => {
                    if !on && self.alt_active {
                        let cols = self.cols;
                        self.screen.iter_mut().for_each(|r| r.reset(cols, Color::DEFAULT));
                    }
                    self.set_alt_screen(on, false);
                }
                1048 => {
                    if on { self.save_cursor() } else { self.restore_cursor() }
                }
                1049 => {
                    if on {
                        self.save_cursor();
                        self.set_alt_screen(true, true);
                    } else {
                        self.set_alt_screen(false, false);
                        self.restore_cursor();
                    }
                }
                1000 => self.modes.mouse = if on { MouseMode::Click } else { MouseMode::Off },
                1002 => self.modes.mouse = if on { MouseMode::Drag } else { MouseMode::Off },
                1003 => self.modes.mouse = if on { MouseMode::Motion } else { MouseMode::Off },
                1004 => self.modes.focus_events = on,
                1006 => self.modes.mouse_sgr = on,
                1007 => self.modes.alternate_scroll = on,
                2004 => self.modes.bracketed_paste = on,
                2026 => self.modes.synchronized = on,
                _ => {}
            }
        }
    }

    fn sgr(&mut self, params: &Params) {
        if params.len() == 0 {
            self.pen = Cell { ch: ' ', ..Cell::default() };
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = params.get(i);
            let code = p.first().copied().unwrap_or(0);
            let f = &mut self.pen.flags;
            match code {
                0 => {
                    self.pen.fg = Color::DEFAULT;
                    self.pen.bg = Color::DEFAULT;
                    self.pen.flags = 0;
                }
                1 => *f |= flags::BOLD,
                2 => *f |= flags::DIM,
                3 => *f |= flags::ITALIC,
                4 => {
                    *f &= !(flags::UNDERLINE | flags::DOUBLE_UNDERLINE);
                    match p.get(1).copied() {
                        Some(0) => {}
                        Some(2) => *f |= flags::DOUBLE_UNDERLINE,
                        _ => *f |= flags::UNDERLINE,
                    }
                }
                5 | 6 => *f |= flags::BLINK,
                7 => *f |= flags::INVERSE,
                8 => *f |= flags::HIDDEN,
                9 => *f |= flags::STRIKE,
                21 => *f |= flags::DOUBLE_UNDERLINE,
                22 => *f &= !(flags::BOLD | flags::DIM),
                23 => *f &= !flags::ITALIC,
                24 => *f &= !(flags::UNDERLINE | flags::DOUBLE_UNDERLINE),
                25 => *f &= !flags::BLINK,
                27 => *f &= !flags::INVERSE,
                28 => *f &= !flags::HIDDEN,
                29 => *f &= !flags::STRIKE,
                30..=37 => self.pen.fg = Color::indexed((code - 30) as u8),
                39 => self.pen.fg = Color::DEFAULT,
                40..=47 => self.pen.bg = Color::indexed((code - 40) as u8),
                49 => self.pen.bg = Color::DEFAULT,
                90..=97 => self.pen.fg = Color::indexed((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::indexed((code - 100 + 8) as u8),
                38 | 48 | 58 => {
                    let (color, used) = extended_color(params, i);
                    if let Some(color) = color {
                        match code {
                            38 => self.pen.fg = color,
                            48 => self.pen.bg = color,
                            _ => {} // underline colour: not drawn separately
                        }
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn report(&mut self, text: &str) {
        self.responses.extend_from_slice(text.as_bytes());
    }
}

/// `38;5;n`, `38;2;r;g;b` and the colon forms `38:5:n`, `38:2:[id]:r:g:b`.
/// Returns the colour and how many extra parameters the semicolon form consumed.
fn extended_color(params: &Params, i: usize) -> (Option<Color>, usize) {
    let p = params.get(i);
    if p.len() > 1 {
        let c = match p[1] {
            5 => p.get(2).map(|&n| Color::indexed(n as u8)),
            2 => {
                let rgb = if p.len() >= 6 { &p[3..6] } else if p.len() >= 5 { &p[2..5] } else { &[][..] };
                (rgb.len() == 3).then(|| Color::rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
            }
            _ => None,
        };
        return (c, 0);
    }
    let arg = |k: usize| params.get(i + k).first().copied();
    match arg(1) {
        Some(5) => (arg(2).map(|n| Color::indexed(n as u8)), 2),
        Some(2) => match (arg(2), arg(3), arg(4)) {
            (Some(r), Some(g), Some(b)) => (Some(Color::rgb(r as u8, g as u8, b as u8)), 4),
            _ => (None, 4),
        },
        _ => (None, 1),
    }
}

fn push_scrollback(scrollback: &mut VecDeque<Row>, row: Row, max: usize, counter: &mut u64) {
    if max == 0 {
        return;
    }
    if scrollback.len() >= max {
        scrollback.pop_front();
    }
    scrollback.push_back(row.trimmed());
    *counter += 1;
}

/// DEC special graphics: the line-drawing set selected by `ESC ( 0`
fn dec_graphics(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => c,
    }
}

impl Perform for Term {
    fn print(&mut self, c: char) {
        let c = if self.charsets[self.active_g1 as usize] { dec_graphics(c) } else { c };
        match c.width() {
            Some(0) => self.attach_combining(c),
            Some(w) => {
                self.put(c, w.min(2));
                self.last_printed = Some(c);
            }
            None => {}
        }
    }

    fn execute(&mut self, b: u8) {
        match b {
            0x07 => self.bell = true,
            0x08 => {
                if self.pending_wrap {
                    self.pending_wrap = false;
                } else {
                    self.x = self.x.saturating_sub(1);
                }
            }
            0x09 => {
                let next = (self.x + 1..self.cols).find(|&c| self.tabs[c]).unwrap_or(self.cols - 1);
                self.x = next;
                self.pending_wrap = false;
            }
            0x0A..=0x0C => self.newline(),
            0x0D => {
                self.x = 0;
                self.pending_wrap = false;
            }
            0x0E => self.active_g1 = true,
            0x0F => self.active_g1 = false,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], ignore: bool, action: u8) {
        if ignore {
            return;
        }
        let private = inter.first() == Some(&b'?');
        let n = |i: usize| params.arg(i, 1) as usize;
        match (action, inter) {
            (b'@', []) => self.insert_chars(n(0)),
            (b'A', []) => {
                let min = if self.y >= self.top { self.top } else { 0 };
                self.y = self.y.saturating_sub(n(0)).max(min);
                self.pending_wrap = false;
            }
            (b'B' | b'e', []) => {
                let max = if self.y <= self.bottom { self.bottom } else { self.rows - 1 };
                self.y = (self.y + n(0)).min(max);
                self.pending_wrap = false;
            }
            (b'C' | b'a', []) => {
                self.x = (self.x + n(0)).min(self.cols - 1);
                self.pending_wrap = false;
            }
            (b'D', []) => {
                self.x = self.x.saturating_sub(n(0));
                self.pending_wrap = false;
            }
            (b'E', []) => {
                self.goto(0, self.y + n(0));
            }
            (b'F', []) => {
                self.goto(0, self.y.saturating_sub(n(0)));
            }
            (b'G' | b'`', []) => {
                self.x = (n(0) - 1).min(self.cols - 1);
                self.pending_wrap = false;
            }
            (b'H' | b'f', []) => self.goto(n(1) - 1, self.region_top() + n(0) - 1),
            (b'd', []) => self.goto(self.x, self.region_top() + n(0) - 1),
            (b'I', []) => {
                for _ in 0..n(0) {
                    self.execute(0x09);
                }
            }
            (b'Z', []) => {
                for _ in 0..n(0) {
                    self.x = (0..self.x).rev().find(|&c| self.tabs[c]).unwrap_or(0);
                }
                self.pending_wrap = false;
            }
            (b'J', []) | (b'J', [b'?']) => self.erase_display(params.arg(0, 0)),
            (b'K', []) | (b'K', [b'?']) => match params.arg(0, 0) {
                0 => self.erase_cells(self.y, self.x, self.cols),
                1 => self.erase_cells(self.y, 0, self.x + 1),
                2 => self.erase_cells(self.y, 0, self.cols),
                _ => {}
            },
            (b'L', []) => self.insert_lines(n(0)),
            (b'M', []) => self.delete_lines(n(0)),
            (b'P', []) => self.delete_chars(n(0)),
            (b'X', []) => {
                let to = (self.x + n(0)).min(self.cols);
                self.erase_cells(self.y, self.x, to);
            }
            (b'S', []) => self.scroll_up(n(0)),
            (b'T', []) => self.scroll_down(n(0)),
            (b'b', []) => {
                if let Some(c) = self.last_printed {
                    for _ in 0..n(0).min(self.cols * self.rows) {
                        self.print(c);
                    }
                }
            }
            (b'c', []) => self.report("\x1b[?62;22c"),
            (b'c', [b'>']) => self.report("\x1b[>1;10;0c"),
            (b'g', []) => match params.arg(0, 0) {
                0 => self.tabs[self.x] = false,
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            (b'h', _) if private || inter.is_empty() => self.set_mode(params, private, true),
            (b'l', _) if private || inter.is_empty() => self.set_mode(params, private, false),
            (b'm', []) => self.sgr(params),
            (b'n', []) => match params.arg(0, 0) {
                5 => self.report("\x1b[0n"),
                6 => {
                    let y = self.y - self.region_top() + 1;
                    let text = format!("\x1b[{};{}R", y, self.x + 1);
                    self.report(&text);
                }
                _ => {}
            },
            (b'n', [b'?']) if params.arg(0, 0) == 6 => {
                let text = format!("\x1b[?{};{}R", self.y - self.region_top() + 1, self.x + 1);
                self.report(&text);
            }
            (b'q', [b' ']) => {
                self.cursor_shape = match params.arg(0, 1) {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
                self.touch(self.y);
            }
            (b'r', []) => {
                let top = params.arg(0, 1) as usize - 1;
                let bottom = (params.arg(1, self.rows as u16) as usize).min(self.rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, self.region_top());
                }
            }
            (b's', []) => self.save_cursor(),
            (b'u', []) => self.restore_cursor(),
            (b't', []) if params.arg(0, 0) == 18 => {
                let text = format!("\x1b[8;{};{}t", self.rows, self.cols);
                self.report(&text);
            }
            (b'p', [b'!']) => {
                // DECSTR soft reset: modes and pen, not the screen
                self.modes = Modes::default();
                self.pen = Cell::default();
                self.top = 0;
                self.bottom = self.rows - 1;
                self.charsets = [false, false];
                self.active_g1 = false;
                self.saved = [None, None];
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, b: u8) {
        match (inter, b) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([b'#'], b'8') => {
                // DECALN: fill the screen with E (alignment test)
                for y in 0..self.rows {
                    for c in self.screen[y].cells.iter_mut() {
                        *c = Cell { ch: 'E', ..Cell::default() };
                    }
                }
                self.mark_all_dirty();
            }
            ([], b'D') => self.index(),
            ([], b'E') => {
                self.x = 0;
                self.index();
            }
            ([], b'M') => self.reverse_index(),
            ([], b'H') => self.tabs[self.x] = true,
            ([], b'c') => self.reset(),
            ([], b'=') => self.modes.keypad_app = true,
            ([], b'>') => self.modes.keypad_app = false,
            ([b'('], c) => self.charsets[0] = c == b'0',
            ([b')'], c) => self.charsets[1] = c == b'0',
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
        let kind = params.first().map(|p| String::from_utf8_lossy(p).into_owned()).unwrap_or_default();
        let rest = || params[1..].join(&b';').to_vec();
        match kind.as_str() {
            "52" if params.len() == 3 && (params[1].is_empty() || params[1].contains(&b'c')) && params[2] != b"?" => {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(params[2]) {
                    if let Ok(text) = String::from_utf8(bytes) { self.clipboard = Some(text); }
                }
            }
            "0" | "2" => self.title = Some(String::from_utf8_lossy(&rest()).into_owned()),
            "7" => {
                // file://host/path (percent-encoded)
                let url = String::from_utf8_lossy(&rest()).into_owned();
                if let Some(path) = url.strip_prefix("file://").and_then(|r| r.find('/').map(|i| r[i..].to_string())) {
                    self.cwd = feather_ui::fileops::parse_uri_list(&format!("file://{path}")).first().map(|p| p.to_string_lossy().into_owned());
                }
            }
            "10" | "11" if params.get(1) == Some(&&b"?"[..]) => {
                let [r, g, b] = if kind == "10" { self.report_colors.0 } else { self.report_colors.1 };
                let end = if bell { "\x07" } else { "\x1b\\" };
                let text = format!("\x1b]{kind};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{end}");
                self.report(&text);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parser::Parser;
    use super::*;

    fn term(cols: usize, rows: usize, input: &str) -> Term {
        let mut t = Term::new(cols, rows);
        Parser::new().advance(&mut t, input.as_bytes());
        t
    }

    #[test]
    fn cursor_moves_alone_repaint_the_rows_they_touch() {
        let mut t = Term::new(10, 4);
        let mut p = Parser::new();
        p.advance(&mut t, b"abc");
        t.take_dirty();
        assert!(t.take_dirty().iter().all(|d| !d), "nothing moved");
        // Left arrow in readline: a bare backspace
        p.advance(&mut t, b"\x08");
        assert_eq!(t.take_dirty(), [true, false, false, false]);
        p.advance(&mut t, b"\x1b[C");
        assert_eq!(t.take_dirty(), [true, false, false, false]);
        // To another row: both the old and the new one
        p.advance(&mut t, b"\x1b[3;1H");
        assert_eq!(t.take_dirty(), [true, false, true, false]);
        p.advance(&mut t, b"\x1b[?25l");
        assert_eq!(t.take_dirty(), [false, false, true, false], "hiding the cursor");
    }

    #[test]
    fn printing_wrapping_and_scrolling_into_history() {
        let t = term(5, 2, "abcdefgh\r\nij");
        assert_eq!(t.screen_text(), ["fgh", "ij"]);
        assert_eq!(t.scrollback_len(), 1);
        assert_eq!(t.view_row(1, 0).text(), "abcde");
        assert!(t.view_row(1, 0).wrapped);
        // Pending wrap: writing the last column does not move to the next line yet
        let t = term(3, 2, "abc");
        assert_eq!((t.x, t.y, t.pending_wrap), (2, 0, true));
        let t = term(3, 2, "abc\r\n");
        assert_eq!((t.x, t.y), (0, 1));
    }

    #[test]
    fn osc52_copies_utf8_but_never_reads_the_clipboard_or_accepts_invalid_data() {
        let text="Copied from Codex — Привет 界";
        let encoded=base64::engine::general_purpose::STANDARD.encode(text);
        let mut t=Term::new(80,24);let mut p=Parser::new();
        let seq=format!("\x1b]52;c;{encoded}\x1b\\");
        for chunk in seq.as_bytes().chunks(3){p.advance(&mut t,chunk);}
        assert_eq!(t.clipboard.take().as_deref(),Some(text));
        p.advance(&mut t,format!("\x1b]52;;{encoded}\x07").as_bytes());
        assert_eq!(t.clipboard.take().as_deref(),Some(text));
        p.advance(&mut t,b"\x1b]52;c;?\x07\x1b]52;c;bad!\x07\x1b]52;p;YWJj\x07");
        assert!(t.clipboard.is_none());assert!(t.responses.is_empty());
        let too_long=format!("\x1b]52;c;{}\x07","YWFh".repeat(20000));
        p.advance(&mut t,too_long.as_bytes());assert!(t.clipboard.is_none());
    }

    #[test]
    fn wide_and_combining_characters() {
        let t = term(6, 1, "a界b");
        assert_eq!(t.screen_text(), ["a界b"]);
        assert_eq!(t.cell(1, 0).flags & flags::WIDE, flags::WIDE);
        assert_eq!(t.cell(2, 0).flags & flags::SPACER, flags::SPACER);
        assert_eq!(t.x, 4);
        // A wide char that does not fit at the end wraps whole
        let t = term(3, 2, "ab界");
        assert_eq!(t.screen_text(), ["ab", "界"]);
        // Overwriting half of a wide char clears the other half
        let t = term(4, 1, "界\x1b[2Gx");
        assert_eq!(t.screen_text(), [" x"]);
        let t = term(4, 1, "e\u{301}x");
        assert_eq!(t.screen_text(), ["e\u{301}x"]);
    }

    #[test]
    fn cursor_movement_erase_and_insert_delete() {
        let t = term(10, 3, "hello\x1b[1;3HX\x1b[2;2HY\x1b[3G\x1b[KZ");
        assert_eq!(t.screen_text(), ["heXlo", " YZ", ""]);
        let t = term(6, 1, "abcdef\x1b[3G\x1b[2P");
        assert_eq!(t.screen_text(), ["abef"]);
        let t = term(6, 1, "abcdef\x1b[3G\x1b[2@");
        assert_eq!(t.screen_text(), ["ab  cd"]);
        let t = term(6, 1, "abcdef\x1b[2G\x1b[3X");
        assert_eq!(t.screen_text(), ["a   ef"]);
        let t = term(4, 3, "1\r\n2\r\n3\x1b[1;1H\x1b[J");
        assert_eq!(t.screen_text(), ["", "", ""]);
        let t = term(4, 3, "1\r\n2\r\n3\x1b[2;1H\x1b[L");
        assert_eq!(t.screen_text(), ["1", "", "2"]);
        let t = term(4, 3, "1\r\n2\r\n3\x1b[1;1H\x1b[M");
        assert_eq!(t.screen_text(), ["2", "3", ""]);
    }

    #[test]
    fn scroll_region_origin_mode_and_reverse_index() {
        // Lines scroll only inside rows 2..3; the top line stays
        let t = term(4, 4, "top\x1b[2;3r\x1b[2;1Ha\r\nb\r\nc\r\nd");
        assert_eq!(t.screen_text(), ["top", "c", "d", ""]);
        assert_eq!(t.scrollback_len(), 0, "region scrolling keeps history clean");
        let t = term(4, 4, "\x1b[2;3r\x1b[?6h\x1b[1;1HX");
        assert_eq!(t.screen_text(), ["", "X", "", ""]);
        let t = term(4, 2, "a\r\nb\x1b[1;1H\x1bM");
        assert_eq!(t.screen_text(), ["", "a"]);
    }

    #[test]
    fn sgr_colors_and_attributes() {
        let t = term(10, 1, "\x1b[1;4;31;42ma\x1b[38;5;200;48;2;1;2;3mb\x1b[38:2::9:8:7mc\x1b[0md");
        let [a, b, c, d] = [0, 1, 2, 3].map(|x| t.cell(x, 0));
        assert_eq!((a.fg, a.bg, a.flags), (Color::indexed(1), Color::indexed(2), flags::BOLD | flags::UNDERLINE));
        assert_eq!((b.fg, b.bg), (Color::indexed(200), Color::rgb(1, 2, 3)));
        assert_eq!(c.fg.kind(), ColorKind::Rgb(9, 8, 7));
        assert_eq!((d.fg, d.bg, d.flags), (Color::DEFAULT, Color::DEFAULT, 0));
        let t = term(4, 1, "\x1b[91;7mx\x1b[27;39mx");
        assert_eq!((t.cell(0, 0).fg, t.cell(0, 0).flags), (Color::indexed(9), flags::INVERSE));
        assert_eq!((t.cell(1, 0).fg, t.cell(1, 0).flags), (Color::DEFAULT, 0));
        // Erase uses the current background (bce)
        let t = term(3, 1, "\x1b[44m\x1b[2K");
        assert_eq!(t.cell(2, 0).bg, Color::indexed(4));
    }

    #[test]
    fn alternate_screen_and_saved_cursor() {
        let t = term(5, 2, "main\x1b[?1049h\x1b[Halt\x1b[?1049l");
        assert_eq!(t.screen_text(), ["main", ""]);
        assert_eq!((t.x, t.y), (4, 0));
        let t = term(5, 2, "main\x1b[?1049hx");
        assert!(t.alt_active);
        assert_eq!(t.screen_text(), ["    x", ""]);
        let t = term(5, 2, "ab\x1b7\x1b[2;4Hz\x1b8c");
        assert_eq!(t.screen_text(), ["abc", "   z"]);
    }

    #[test]
    fn modes_tabs_charsets_and_replies() {
        let mut t = term(20, 2, "\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?25l");
        assert!(t.modes.cursor_keys_app && t.modes.bracketed_paste && t.modes.mouse_sgr && !t.modes.cursor_visible);
        assert_eq!(t.modes.mouse, MouseMode::Drag);
        Parser::new().advance(&mut t, b"\x1b[5;9H\x1b[6n\x1b[c\x1b]11;?\x07");
        assert_eq!(String::from_utf8_lossy(&t.responses), "\x1b[2;9R\x1b[?62;22c\x1b]11;rgb:ffff/ffff/ffff\x07");
        let t = term(20, 1, "a\tb\x1b(0lqk\x1b(Bq");
        assert_eq!(t.screen_text(), ["a       b┌─┐q"]);
        let t = term(10, 1, "\x1b]2;vim — файл\x07x\x1b[3b");
        assert_eq!(t.title.as_deref(), Some("vim — файл"));
        assert_eq!(t.screen_text(), ["xxxx"]);
        let t = term(10, 1, "\x1b]7;file://host/home/u/%D0%BF\x07");
        assert_eq!(t.cwd.as_deref(), Some("/home/u/п"));
    }

    #[test]
    fn absolute_lines_and_copied_text() {
        let t = term(5, 2, "one\r\ntwo\r\nthree45678");
        // "one" scrolled out: still line 0, the screen starts at line 1
        assert_eq!(t.first_abs(), 0);
        assert_eq!(t.abs_row(0).unwrap().text().trim_end(), "one");
        assert_eq!(t.text_between((0, 0), (1, 4)), "one\ntwo");
        // A wrapped line is copied as one line
        assert_eq!(t.text_between((2, 0), (3, 4)), "three45678");
        assert_eq!(t.text_between((0, 1), (0, 1)), "n");
    }

    #[test]
    fn many_resizes_never_panic_and_keep_the_grid_consistent() {
        let mut seed = 12345u64;
        let mut rnd = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let mut t = Term::new(80, 24);
        let mut p = Parser::new();
        for i in 0..400 {
            let text = match i % 5 {
                0 => "line of text with 界 wide\r\n".repeat(rnd(30) as usize),
                1 => "\x1b[?1049h\x1b[2J\x1b[5;5Halt".to_string(),
                2 => "\x1b[?1049l".to_string(),
                3 => format!("\x1b[{};{}H\x1b[2;{}r", rnd(60), rnd(200), rnd(50)),
                _ => "x".repeat(rnd(500) as usize),
            };
            p.advance(&mut t, text.as_bytes());
            t.resize(2 + rnd(200) as usize, 1 + rnd(70) as usize);
            assert_eq!(t.screen.len(), t.rows);
            assert_eq!(t.other.len(), t.rows);
            assert!(t.screen.iter().chain(t.other.iter()).all(|r| r.cells.len() == t.cols));
            assert!(t.x < t.cols && t.y < t.rows);
            for o in 0..=t.scrollback_len().min(3) {
                for y in 0..t.rows {
                    let _ = t.view_row(o, y);
                }
            }
        }
    }

    #[test]
    fn resize_keeps_cursor_line_and_uses_history() {
        let mut t = term(6, 4, "1\r\n2\r\n3\r\n4");
        t.resize(6, 2);
        assert_eq!(t.screen_text(), ["3", "4"]);
        assert_eq!((t.y, t.scrollback_len()), (1, 2));
        t.resize(6, 4);
        assert_eq!(t.screen_text(), ["1", "2", "3", "4"]);
        assert_eq!((t.y, t.scrollback_len()), (3, 0));
        // Shrinking drops empty rows below the cursor before the top
        let mut t = term(6, 4, "a");
        t.resize(3, 2);
        assert_eq!(t.screen_text(), ["a", ""]);
        assert_eq!(t.scrollback_len(), 0);
    }
}
