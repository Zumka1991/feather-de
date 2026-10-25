//! TextEdit («Текстовый редактор») model and UI in macOS Tahoe style.
//! Supports multiple document tabs, line numbers, soft word wrap, font size scaling,
//! inline Find & Replace, syntax-aware line styling, Undo/Redo, and non-blocking Open/Save.

use crate::theme::{self, colors, rgba};
use feather_ui::i18n::tr;
use feather_ui::input::{Edit, TextInput};
use feather_ui::text::FontSystem;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tiny_skia::PixmapMut;

pub const TOOLBAR_H: f32 = 48.0;
pub const TAB_BAR_H: f32 = 30.0;
pub const FIND_BAR_H: f32 = 38.0;
pub const STATUS_BAR_H: f32 = 26.0;
pub const GUTTER_W: f32 = 50.0;
const PAD_X: f32 = 24.0;
const PAD_TOP: f32 = 16.0;
const RESIZE_BAND: f32 = 5.0;
const BTN_LEFT: u32 = 0x110;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const UNDO_PAUSE: Duration = Duration::from_millis(800);
const MAX_UNDO: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Close,
    Minimize,
    ToggleMaximize,
    Move,
    Resize(u32),
    OpenDialog,
    SaveDialog { doc_id: u64, suggested_name: String },
    WriteFile { doc_id: u64, path: PathBuf, content: String },
    Copy(String),
    Paste,
    SetTitle(String),
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Default,
    Text,
    Pointer,
    Resize(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyntaxKind {
    Plain,
    Markdown,
    Rust,
    TomlIni,
    JsonYaml,
    ShellPy,
    CLike,
    HtmlXml,
}

impl SyntaxKind {
    pub fn from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "md" | "markdown" => Self::Markdown,
            "rs" => Self::Rust,
            "toml" | "ini" | "conf" | "cfg" | "desktop" => Self::TomlIni,
            "json" | "yaml" | "yml" => Self::JsonYaml,
            "sh" | "bash" | "zsh" | "py" | "nix" => Self::ShellPy,
            "c" | "h" | "cpp" | "hpp" | "cc" | "js" | "ts" | "tsx" | "jsx" | "go" | "java"
            | "kt" | "zig" | "css" | "sql" | "lua" => Self::CLike,
            "html" | "htm" | "xml" | "svg" => Self::HtmlXml,
            _ => Self::Plain,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "Текст",
            Self::Markdown => "Markdown",
            Self::Rust => "Rust",
            Self::TomlIni => "TOML / Конфиг",
            Self::JsonYaml => "JSON / YAML",
            Self::ShellPy => "Скрипт",
            Self::CLike => "Исходный код",
            Self::HtmlXml => "HTML / XML",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LineStyle {
    Normal,
    Heading,
    Subheading,
    Comment,
    Section,
    CodeOrQuote,
    Bullet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct VisualLine {
    /// Byte range `[start, end)` within `doc.text`
    start: usize,
    end: usize,
    /// 1-based logical line index
    logical_line: usize,
    /// True if this is the first visual row of the logical line
    first_of_logical: bool,
    y: f32,
    size: f32,
    height: f32,
    bold: bool,
    style: LineStyle,
}

#[derive(Clone, Debug)]
pub struct Doc {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub title: String,
    pub text: String,
    pub saved_text: String,
    pub syntax: SyntaxKind,
    pub caret: usize,
    pub anchor: Option<usize>,
    pub goal_x: Option<f32>,
    pub scroll: f32,
    pub undo: Vec<(String, usize)>,
    pub redo: Vec<(String, usize)>,
    pub last_edit: Option<Instant>,
}

impl Doc {
    pub fn new_untitled(id: u64, index: usize) -> Self {
        let title = if index <= 1 {
            format!("{}.txt", tr("Без названия"))
        } else {
            format!("{} {}.txt", tr("Без названия"), index)
        };
        Self {
            id,
            path: None,
            title,
            text: String::new(),
            saved_text: String::new(),
            syntax: SyntaxKind::Plain,
            caret: 0,
            anchor: None,
            goal_x: None,
            scroll: 0.0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
        }
    }

    pub fn from_file(id: u64, path: PathBuf, text: String) -> Self {
        let title = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("document.txt")
            .to_string();
        let syntax = SyntaxKind::from_path(&path);
        let clean = text.replace("\r\n", "\n");
        Self {
            id,
            path: Some(path),
            title,
            saved_text: clean.clone(),
            text: clean,
            syntax,
            caret: 0,
            anchor: None,
            goal_x: None,
            scroll: 0.0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
        }
    }

    pub fn is_modified(&self) -> bool {
        self.text != self.saved_text
    }

    pub fn line_count(&self) -> usize {
        self.text.bytes().filter(|&b| b == b'\n').count() + 1
    }

    pub fn word_count(&self) -> usize {
        self.text.split_whitespace().count()
    }

    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// Returns 1-based `(line, column)` of `self.caret`.
    pub fn caret_line_col(&self) -> (usize, usize) {
        let clamped = self.caret.min(self.text.len());
        let prefix = &self.text[..clamped];
        let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
        let col = prefix[line_start..].chars().count() + 1;
        (line, col)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Close,
    Minimize,
    Zoom,
    NewTab,
    OpenFile,
    SaveFile,
    ToggleLineNumbers,
    ToggleWrap,
    FontOut,
    FontReset,
    FontIn,
    ToggleFind,
    Tab(usize),
    CloseTab(usize),
    FindInput,
    ReplaceInput,
    FindPrev,
    FindNext,
    ReplaceOne,
    ReplaceAll,
    CloseFind,
    Editor,
    Toolbar,
    Nothing,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FindFocus {
    None,
    Find,
    Replace,
}

pub struct App {
    pub fonts: FontSystem,
    pub w: f32,
    pub h: f32,
    pub dirty: bool,
    pub maximized: bool,
    pub focused: bool,
    docs: Vec<Doc>,
    active: usize,
    next_id: u64,
    untitled_counter: usize,
    pub show_line_numbers: bool,
    pub word_wrap: bool,
    pub font_size: f32,
    pub find_open: bool,
    find_focus: FindFocus,
    find_input: TextInput,
    replace_input: TextInput,
    matches: Vec<(usize, usize)>,
    active_match: usize,
    layout: Vec<VisualLine>,
    layout_valid: bool,
    hover: Hit,
    selecting: bool,
    last_click: Option<(Instant, f32, f32)>,
    status_message: Option<String>,
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

fn classify_line(syntax: SyntaxKind, line: &str) -> (LineStyle, bool) {
    let trimmed = line.trim_start();
    match syntax {
        SyntaxKind::Markdown => {
            if trimmed.starts_with("# ") {
                (LineStyle::Heading, true)
            } else if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
                (LineStyle::Subheading, true)
            } else if trimmed.starts_with("> ") || trimmed.starts_with("```") || trimmed.starts_with("    ") {
                (LineStyle::CodeOrQuote, false)
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                (LineStyle::Bullet, false)
            } else {
                (LineStyle::Normal, false)
            }
        }
        SyntaxKind::Rust | SyntaxKind::CLike => {
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                (LineStyle::Comment, false)
            } else if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
                (LineStyle::Section, false)
            } else if trimmed.starts_with("pub fn ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("impl")
            {
                (LineStyle::Subheading, true)
            } else {
                (LineStyle::Normal, false)
            }
        }
        SyntaxKind::TomlIni => {
            if trimmed.starts_with('#') || trimmed.starts_with(';') {
                (LineStyle::Comment, false)
            } else if trimmed.starts_with('[') && trimmed.ends_with(']') {
                (LineStyle::Section, true)
            } else {
                (LineStyle::Normal, false)
            }
        }
        SyntaxKind::ShellPy | SyntaxKind::JsonYaml => {
            if trimmed.starts_with('#') || trimmed.starts_with("//") {
                (LineStyle::Comment, false)
            } else if trimmed.starts_with("def ") || trimmed.starts_with("class ") {
                (LineStyle::Subheading, true)
            } else {
                (LineStyle::Normal, false)
            }
        }
        SyntaxKind::HtmlXml => {
            if trimmed.starts_with("<!--") {
                (LineStyle::Comment, false)
            } else {
                (LineStyle::Normal, false)
            }
        }
        SyntaxKind::Plain => {
            if trimmed.starts_with("# ") {
                (LineStyle::Heading, true)
            } else if trimmed.starts_with("//") {
                (LineStyle::Comment, false)
            } else {
                (LineStyle::Normal, false)
            }
        }
    }
}

pub fn sample_markdown_content() -> String {
    concat!(
        "# Пёрышко — Текстовый редактор (macOS Tahoe)\n",
        "\n",
        "Быстрый и лёгкий нативный редактор текста и исходного кода для **Feather DE**.\n",
        "\n",
        "## Ключевые возможности\n",
        "- Поддержка вкладок документов (`Ctrl+N`, `Ctrl+W`, `Ctrl+Tab`)\n",
        "- Нумерация строк (`Ctrl+L`), перенос строк (`Ctrl+Alt+W`) и масштаб текста (`Ctrl++` / `Ctrl+-`)\n",
        "- Встроенный поиск и замена по документу (`Ctrl+F` / `Ctrl+H`)\n",
        "- Открытие и сохранение через нативный диалог «Файлы» (`Ctrl+O`, `Ctrl+S`, `Ctrl+Shift+S`)\n",
        "- 0 % нагрузки на CPU в простое, мгновенный отклик и общая память `wl_shm`\n",
        "\n",
        "## Пример конфигурации и кода\n",
        "// Обработка сохранения документа в фоновом потоке без блокировки UI\n",
        "pub fn save_atomic(path: &Path, content: &str) -> std::io::Result<()> {\n",
        "    std::fs::write(path, content.as_bytes())\n",
        "}\n",
        "\n",
        "> Совет: перетащите любой текстовый файл или исходный код из «Файлов» прямо в окно редактора.\n"
    )
    .to_string()
}

impl App {
    pub fn new(fonts: FontSystem) -> Self {
        let doc = Doc::new_untitled(1, 1);
        Self {
            fonts,
            w: 960.0,
            h: 640.0,
            dirty: true,
            maximized: false,
            focused: true,
            docs: vec![doc],
            active: 0,
            next_id: 2,
            untitled_counter: 1,
            show_line_numbers: true,
            word_wrap: true,
            font_size: 14.0,
            find_open: false,
            find_focus: FindFocus::None,
            find_input: TextInput::default(),
            replace_input: TextInput::default(),
            matches: Vec::new(),
            active_match: 0,
            layout: Vec::new(),
            layout_valid: false,
            hover: Hit::Nothing,
            selecting: false,
            last_click: None,
            status_message: None,
        }
    }

    pub fn doc(&self) -> &Doc {
        &self.docs[self.active]
    }

    pub fn doc_mut(&mut self) -> &mut Doc {
        &mut self.docs[self.active]
    }

    pub fn window_title(&self) -> String {
        let d = self.doc();
        let dot = if d.is_modified() { " •" } else { "" };
        format!("{}{} — {}", d.title, dot, tr("Текстовый редактор"))
    }

    pub fn open_loaded_file(&mut self, path: PathBuf, text: String) -> Vec<Effect> {
        // If already open in a tab, switch to it and refresh content if unmodified
        if let Some(idx) = self.docs.iter().position(|d| d.path.as_ref() == Some(&path)) {
            self.active = idx;
            self.layout_valid = false;
            self.recompute_matches();
            self.dirty = true;
            return vec![Effect::SetTitle(self.window_title())];
        }
        let id = self.next_id;
        self.next_id += 1;
        let new_doc = Doc::from_file(id, path, text);
        // Replace the initial empty untitled tab if it hasn't been touched
        if self.docs.len() == 1
            && self.docs[0].path.is_none()
            && self.docs[0].text.is_empty()
            && !self.docs[0].is_modified()
        {
            self.docs[0] = new_doc;
            self.active = 0;
        } else {
            self.docs.push(new_doc);
            self.active = self.docs.len() - 1;
        }
        self.layout_valid = false;
        self.recompute_matches();
        self.status_message = None;
        self.dirty = true;
        vec![Effect::SetTitle(self.window_title())]
    }

    pub fn mark_saved(&mut self, doc_id: u64, path: PathBuf, content: String) -> Vec<Effect> {
        if let Some(doc) = self.docs.iter_mut().find(|d| d.id == doc_id) {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                doc.title = name.to_string();
            }
            doc.syntax = SyntaxKind::from_path(&path);
            doc.path = Some(path);
            doc.saved_text = content;
        }
        self.status_message = Some(tr("Сохранено").into_owned());
        self.layout_valid = false;
        self.dirty = true;
        vec![Effect::SetTitle(self.window_title())]
    }

    pub fn set_error(&mut self, msg: String) {
        self.status_message = Some(msg);
        self.dirty = true;
    }

    pub fn new_tab(&mut self) -> Vec<Effect> {
        let id = self.next_id;
        self.next_id += 1;
        self.untitled_counter += 1;
        self.docs.push(Doc::new_untitled(id, self.untitled_counter));
        self.active = self.docs.len() - 1;
        self.find_focus = FindFocus::None;
        self.layout_valid = false;
        self.recompute_matches();
        self.status_message = None;
        self.dirty = true;
        vec![Effect::SetTitle(self.window_title())]
    }

    pub fn close_tab(&mut self, index: usize) -> Vec<Effect> {
        if self.docs.len() <= 1 {
            return vec![Effect::Close];
        }
        self.docs.remove(index);
        if self.active >= self.docs.len() {
            self.active = self.docs.len() - 1;
        } else if index < self.active {
            self.active -= 1;
        }
        self.layout_valid = false;
        self.recompute_matches();
        self.dirty = true;
        vec![Effect::SetTitle(self.window_title())]
    }

    pub fn request_save(&mut self, save_as: bool) -> Vec<Effect> {
        let d = self.doc();
        if !save_as {
            if let Some(path) = d.path.clone() {
                return vec![Effect::WriteFile {
                    doc_id: d.id,
                    path,
                    content: d.text.clone(),
                }];
            }
        }
        vec![Effect::SaveDialog {
            doc_id: d.id,
            suggested_name: d.title.clone(),
        }]
    }

    pub fn invalidate_layout(&mut self) {
        self.layout_valid = false;
        self.dirty = true;
    }

    // ---------- Geometry ----------

    fn tab_bar_h(&self) -> f32 {
        if self.docs.len() > 1 {
            TAB_BAR_H
        } else {
            0.0
        }
    }

    fn find_bar_h(&self) -> f32 {
        if self.find_open {
            FIND_BAR_H
        } else {
            0.0
        }
    }

    fn editor_top(&self) -> f32 {
        TOOLBAR_H + self.tab_bar_h() + self.find_bar_h()
    }

    fn editor_bottom(&self) -> f32 {
        (self.h - STATUS_BAR_H).max(self.editor_top() + 60.0)
    }

    fn gutter_w(&self) -> f32 {
        if self.show_line_numbers {
            GUTTER_W
        } else {
            0.0
        }
    }

    fn text_x(&self) -> f32 {
        self.gutter_w() + PAD_X
    }

    fn text_w(&self) -> f32 {
        if self.word_wrap {
            (self.w - self.text_x() - PAD_X - 10.0).max(120.0)
        } else {
            100_000.0
        }
    }

    fn line_height(&self, size: f32) -> f32 {
        (size * 1.55).round()
    }

    // ---------- Layout & Caret ----------

    fn relayout(&mut self) {
        if self.layout_valid {
            return;
        }
        self.layout_valid = true;
        let width = self.text_w();
        let base_size = self.font_size;
        let text = self.docs[self.active].text.clone();
        let syntax = self.docs[self.active].syntax;
        let mut lines = Vec::new();
        let mut y = 0.0;
        let mut start = 0;

        for (logical_idx, para) in text.split('\n').enumerate() {
            let logical_line = logical_idx + 1;
            let (style, bold) = classify_line(syntax, para);
            let size = match style {
                LineStyle::Heading => (base_size + 4.0).min(30.0),
                LineStyle::Subheading => (base_size + 1.0).min(28.0),
                _ => base_size,
            };
            let height = self.line_height(size);
            let mut ls = start;
            let end = start + para.len();
            let mut first_of_logical = true;
            loop {
                let mut fit = end;
                if self.word_wrap && self.fonts.measure_text(&text[ls..end], size, bold) > width {
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
                lines.push(VisualLine {
                    start: ls,
                    end: fit,
                    logical_line,
                    first_of_logical,
                    y,
                    size,
                    height,
                    bold,
                    style,
                });
                y += height;
                first_of_logical = false;
                if fit >= end {
                    break;
                }
                ls = fit;
            }
            start = end + 1;
        }
        self.layout = lines;
    }

    fn line_of(&self, pos: usize) -> usize {
        self.layout.iter().rposition(|l| l.start <= pos).unwrap_or(0)
    }

    fn x_of(&mut self, line: VisualLine, pos: usize) -> f32 {
        let text = self.docs[self.active].text.clone();
        self.fonts.measure_text(
            &text[line.start..pos.clamp(line.start, line.end)],
            line.size,
            line.bold,
        )
    }

    fn pos_at_x(&mut self, line: VisualLine, x: f32) -> usize {
        let text = self.docs[self.active].text.clone();
        let soft = line.end < text.len() && text.as_bytes()[line.end] != b'\n';
        let last = if soft && line.end > line.start {
            prev_boundary(&text, line.end)
        } else {
            line.end
        };
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
        let top = self.editor_top() + PAD_TOP;
        let scroll = self.docs[self.active].scroll;
        let ly = y - top + scroll;
        let Some(&line) = self
            .layout
            .iter()
            .find(|l| ly < l.y + l.height)
            .or(self.layout.last())
        else {
            return 0;
        };
        self.pos_at_x(line, x - self.text_x())
    }

    fn content_h(&mut self) -> f32 {
        self.relayout();
        self.layout.last().map_or(0.0, |l| l.y + l.height)
    }

    fn reveal_caret(&mut self) {
        self.relayout();
        let caret = self.docs[self.active].caret;
        let Some(&line) = self.layout.get(self.line_of(caret)) else {
            return;
        };
        let view = (self.editor_bottom() - self.editor_top() - PAD_TOP * 2.0).max(40.0);
        let doc = &mut self.docs[self.active];
        if line.y < doc.scroll {
            doc.scroll = line.y;
        } else if line.y + line.height > doc.scroll + view {
            doc.scroll = line.y + line.height - view;
        }
    }

    // ---------- Editing & Undo/Redo ----------

    fn selection(&self) -> Option<(usize, usize)> {
        let d = self.doc();
        let a = d.anchor?;
        (a != d.caret).then(|| (a.min(d.caret), a.max(d.caret)))
    }

    fn edit(&mut self, from: usize, to: usize, with: &str, coalesce: bool) -> Vec<Effect> {
        let now = Instant::now();
        let d = &mut self.docs[self.active];
        let merge = coalesce && d.last_edit.is_some_and(|t| now - t < UNDO_PAUSE);
        let was_modified = d.is_modified();
        if !merge {
            d.undo.push((d.text.clone(), d.caret));
            if d.undo.len() > MAX_UNDO {
                d.undo.remove(0);
            }
        }
        d.redo.clear();
        d.last_edit = coalesce.then_some(now);
        d.text.replace_range(from..to, with);
        d.caret = from + with.len();
        d.anchor = None;
        d.goal_x = None;
        let now_modified = d.is_modified();
        self.status_message = None;
        self.layout_valid = false;
        self.recompute_matches();
        self.dirty = true;
        if was_modified != now_modified {
            vec![Effect::SetTitle(self.window_title())]
        } else {
            Vec::new()
        }
    }

    fn insert(&mut self, s: &str) -> Vec<Effect> {
        let (from, to) = self
            .selection()
            .unwrap_or((self.doc().caret, self.doc().caret));
        let coalesce = s.chars().count() == 1 && from == to && s != "\n";
        self.edit(from, to, s, coalesce)
    }

    fn restore(&mut self, redo: bool) -> Vec<Effect> {
        let d = &mut self.docs[self.active];
        let was_modified = d.is_modified();
        let (from, to) = if redo {
            (&mut d.redo, &mut d.undo)
        } else {
            (&mut d.undo, &mut d.redo)
        };
        let Some((text, caret)) = from.pop() else {
            return Vec::new();
        };
        to.push((std::mem::replace(&mut d.text, text), d.caret));
        d.caret = caret.min(d.text.len());
        while !d.text.is_char_boundary(d.caret) {
            d.caret -= 1;
        }
        d.anchor = None;
        d.last_edit = None;
        let now_modified = d.is_modified();
        self.layout_valid = false;
        self.recompute_matches();
        self.reveal_caret();
        self.dirty = true;
        if was_modified != now_modified {
            vec![Effect::SetTitle(self.window_title())]
        } else {
            Vec::new()
        }
    }

    // ---------- Find & Replace ----------

    fn recompute_matches(&mut self) {
        self.matches.clear();
        if !self.find_open || self.find_input.is_empty() {
            self.active_match = 0;
            return;
        }
        let needle = self.find_input.text().to_lowercase();
        if needle.is_empty() {
            self.active_match = 0;
            return;
        }
        let hay = self.docs[self.active].text.to_lowercase();
        let mut pos = 0;
        while let Some(rel) = hay[pos..].find(&needle) {
            let start = pos + rel;
            let end = start + needle.len();
            if self.docs[self.active].text.is_char_boundary(start)
                && self.docs[self.active].text.is_char_boundary(end)
            {
                self.matches.push((start, end));
            }
            pos = end.max(pos + 1);
            if pos >= hay.len() {
                break;
            }
        }
        if self.active_match >= self.matches.len() {
            self.active_match = 0;
        }
    }

    fn jump_to_match(&mut self, forward: bool) {
        if self.matches.is_empty() {
            return;
        }
        if forward {
            self.active_match = (self.active_match + 1) % self.matches.len();
        } else if self.active_match == 0 {
            self.active_match = self.matches.len() - 1;
        } else {
            self.active_match -= 1;
        }
        let (start, end) = self.matches[self.active_match];
        let d = &mut self.docs[self.active];
        d.anchor = Some(start);
        d.caret = end;
        self.reveal_caret();
        self.dirty = true;
    }

    fn replace_current_match(&mut self) -> Vec<Effect> {
        if self.matches.is_empty() {
            return Vec::new();
        }
        let idx = self.active_match.min(self.matches.len() - 1);
        let (start, end) = self.matches[idx];
        let replacement = self.replace_input.text().to_owned();
        let effects = self.edit(start, end, &replacement, false);
        if !self.matches.is_empty() {
            let next = idx.min(self.matches.len() - 1);
            self.active_match = next;
            let (ns, ne) = self.matches[next];
            self.docs[self.active].anchor = Some(ns);
            self.docs[self.active].caret = ne;
            self.reveal_caret();
        }
        effects
    }

    fn replace_all_matches(&mut self) -> Vec<Effect> {
        if self.matches.is_empty() {
            return Vec::new();
        }
        let replacement = self.replace_input.text().to_owned();
        let matches = self.matches.clone();
        let mut new_text = String::with_capacity(self.doc().text.len());
        let old = &self.doc().text;
        let mut last = 0;
        for &(s, e) in &matches {
            new_text.push_str(&old[last..s]);
            new_text.push_str(&replacement);
            last = e;
        }
        new_text.push_str(&old[last..]);
        let len = old.len();
        let effects = self.edit(0, len, &new_text, false);
        self.status_message = Some(format!("{}: {}", tr("Заменено"), matches.len()));
        effects
    }

    // ---------- Toolbar & Bar Rects ----------

    fn traffic(&self) -> [(Hit, f32); 3] {
        [(Hit::Close, 22.0), (Hit::Minimize, 42.0), (Hit::Zoom, 62.0)]
    }

    fn toolbar_buttons(&self) -> Vec<(Hit, (f32, f32, f32, f32))> {
        let y = 10.0;
        let h = 28.0;
        let mut out = vec![
            (Hit::NewTab, (88.0, y, 32.0, h)),
            (Hit::OpenFile, (126.0, y, 82.0, h)),
            (Hit::SaveFile, (214.0, y, 88.0, h)),
        ];
        let mut rx = self.w - 14.0;
        let find_w = 32.0;
        rx -= find_w;
        out.push((Hit::ToggleFind, (rx, y, find_w, h)));
        rx -= 8.0;

        let font_in_w = 30.0;
        let font_rst_w = 48.0;
        let font_out_w = 30.0;
        rx -= font_in_w;
        out.push((Hit::FontIn, (rx, y, font_in_w, h)));
        rx -= font_rst_w;
        out.push((Hit::FontReset, (rx, y, font_rst_w, h)));
        rx -= font_out_w;
        out.push((Hit::FontOut, (rx, y, font_out_w, h)));
        rx -= 8.0;

        let wrap_w = 32.0;
        rx -= wrap_w;
        out.push((Hit::ToggleWrap, (rx, y, wrap_w, h)));
        rx -= 6.0;

        let ln_w = 32.0;
        rx -= ln_w;
        out.push((Hit::ToggleLineNumbers, (rx, y, ln_w, h)));
        out
    }

    fn find_bar_rects(&self) -> Vec<(Hit, (f32, f32, f32, f32))> {
        if !self.find_open {
            return Vec::new();
        }
        let top = TOOLBAR_H + self.tab_bar_h();
        let y = top + 5.0;
        let h = 28.0;
        let find_w = ((self.w - 380.0) * 0.5).clamp(140.0, 260.0);
        let rep_w = ((self.w - 380.0) * 0.5).clamp(120.0, 240.0);
        let mut x = 14.0;
        let find_rect = (x, y, find_w, h);
        x += find_w + 6.0;
        let prev_rect = (x, y, 28.0, h);
        x += 30.0;
        let next_rect = (x, y, 28.0, h);
        x += 36.0;
        let rep_rect = (x, y, rep_w, h);
        x += rep_w + 6.0;
        let rep_one_rect = (x, y, 80.0, h);
        x += 84.0;
        let rep_all_rect = (x, y, 54.0, h);
        let close_rect = (self.w - 38.0, y, 24.0, h);
        vec![
            (Hit::FindInput, find_rect),
            (Hit::FindPrev, prev_rect),
            (Hit::FindNext, next_rect),
            (Hit::ReplaceInput, rep_rect),
            (Hit::ReplaceOne, rep_one_rect),
            (Hit::ReplaceAll, rep_all_rect),
            (Hit::CloseFind, close_rect),
        ]
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
            x >= rx && x < rx + rw && y >= ry && y < ry + rh
        };
        if y < TOOLBAR_H {
            for (hit, cx) in self.traffic() {
                if (x - cx).abs() <= 8.0 && (y - 24.0).abs() <= 8.0 {
                    return hit;
                }
            }
            for (hit, r) in self.toolbar_buttons() {
                if inside(r) {
                    return hit;
                }
            }
            return Hit::Toolbar;
        }
        let tab_h = self.tab_bar_h();
        if tab_h > 0.0 && y < TOOLBAR_H + tab_h {
            let n = self.docs.len();
            let avail = (self.w - 44.0).max(120.0);
            let tw = (avail / n as f32).clamp(90.0, 210.0);
            for i in 0..n {
                let tx = 8.0 + i as f32 * tw;
                if x >= tx && x < tx + tw {
                    if x >= tx + tw - 24.0 && x < tx + tw - 4.0 {
                        return Hit::CloseTab(i);
                    }
                    return Hit::Tab(i);
                }
            }
            let plus_x = 8.0 + n as f32 * tw + 4.0;
            if x >= plus_x && x < plus_x + 28.0 {
                return Hit::NewTab;
            }
            return Hit::Nothing;
        }
        if self.find_open && y < self.editor_top() {
            for (hit, r) in self.find_bar_rects() {
                if inside(r) {
                    return hit;
                }
            }
            return Hit::Nothing;
        }
        if y >= self.editor_top() && y < self.editor_bottom() {
            return Hit::Editor;
        }
        Hit::Nothing
    }

    pub fn resize_edges(&self, x: f32, y: f32) -> u32 {
        if self.maximized {
            return 0;
        }
        let mut edges = 0;
        if y < RESIZE_BAND {
            edges |= 1;
        }
        if y >= self.h - RESIZE_BAND {
            edges |= 2;
        }
        if x < RESIZE_BAND {
            edges |= 4;
        }
        if x >= self.w - RESIZE_BAND {
            edges |= 8;
        }
        edges
    }

    pub fn cursor_at(&self, x: f32, y: f32) -> Cursor {
        let edges = self.resize_edges(x, y);
        if edges != 0 {
            return Cursor::Resize(edges);
        }
        match self.hit(x, y) {
            Hit::Editor | Hit::FindInput | Hit::ReplaceInput => Cursor::Text,
            Hit::Close
            | Hit::Minimize
            | Hit::Zoom
            | Hit::NewTab
            | Hit::OpenFile
            | Hit::SaveFile
            | Hit::ToggleLineNumbers
            | Hit::ToggleWrap
            | Hit::FontOut
            | Hit::FontReset
            | Hit::FontIn
            | Hit::ToggleFind
            | Hit::Tab(_)
            | Hit::CloseTab(_)
            | Hit::FindPrev
            | Hit::FindNext
            | Hit::ReplaceOne
            | Hit::ReplaceAll
            | Hit::CloseFind => Cursor::Pointer,
            _ => Cursor::Default,
        }
    }

    // ---------- Input Handling ----------

    pub fn resize(&mut self, w: f32, h: f32) {
        let nw = w.max(580.0);
        let nh = h.max(380.0);
        if (nw, nh) != (self.w, self.h) {
            self.w = nw;
            self.h = nh;
            self.layout_valid = false;
            self.dirty = true;
        }
    }

    pub fn motion(&mut self, x: f32, y: f32) {
        if self.selecting {
            let pos = self.pos_at(x, y);
            if pos != self.doc().caret {
                self.doc_mut().caret = pos;
                self.doc_mut().goal_x = None;
                self.dirty = true;
            }
            return;
        }
        let hit = self.hit(x, y);
        if hit != self.hover {
            self.hover = hit;
            self.dirty = true;
        }
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
        let double = self
            .last_click
            .is_some_and(|(t, px, py)| now - t < DOUBLE_CLICK && (px - x).hypot(py - y) < 4.0);
        self.last_click = Some((now, x, y));

        let hit = self.hit(x, y);
        self.find_focus = match hit {
            Hit::FindInput => FindFocus::Find,
            Hit::ReplaceInput => FindFocus::Replace,
            Hit::FindPrev | Hit::FindNext | Hit::ReplaceOne | Hit::ReplaceAll => self.find_focus,
            _ => FindFocus::None,
        };

        match hit {
            Hit::Close => vec![Effect::Close],
            Hit::Minimize => vec![Effect::Minimize],
            Hit::Zoom => vec![Effect::ToggleMaximize],
            Hit::Toolbar if double => vec![Effect::ToggleMaximize],
            Hit::Toolbar => vec![Effect::Move],
            Hit::NewTab => self.new_tab(),
            Hit::OpenFile => vec![Effect::OpenDialog],
            Hit::SaveFile => self.request_save(mods.shift),
            Hit::ToggleLineNumbers => {
                self.show_line_numbers = !self.show_line_numbers;
                self.layout_valid = false;
                Vec::new()
            }
            Hit::ToggleWrap => {
                self.word_wrap = !self.word_wrap;
                self.layout_valid = false;
                Vec::new()
            }
            Hit::FontOut => {
                self.font_size = (self.font_size - 1.0).max(10.0);
                self.layout_valid = false;
                Vec::new()
            }
            Hit::FontReset => {
                self.font_size = 14.0;
                self.layout_valid = false;
                Vec::new()
            }
            Hit::FontIn => {
                self.font_size = (self.font_size + 1.0).min(28.0);
                self.layout_valid = false;
                Vec::new()
            }
            Hit::ToggleFind => {
                self.find_open = !self.find_open;
                self.find_focus = if self.find_open {
                    FindFocus::Find
                } else {
                    FindFocus::None
                };
                self.recompute_matches();
                Vec::new()
            }
            Hit::Tab(i) => {
                if i < self.docs.len() && i != self.active {
                    self.active = i;
                    self.layout_valid = false;
                    self.recompute_matches();
                    return vec![Effect::SetTitle(self.window_title())];
                }
                Vec::new()
            }
            Hit::CloseTab(i) => self.close_tab(i),
            Hit::FindInput => {
                if let Some((_, (rx, _, rw, _))) =
                    self.find_bar_rects().into_iter().find(|(h, _)| *h == Hit::FindInput)
                {
                    self.find_input
                        .click(&mut self.fonts, x - rx - 10.0, rw - 54.0, 12.0, false, mods.shift);
                }
                Vec::new()
            }
            Hit::ReplaceInput => {
                if let Some((_, (rx, _, rw, _))) = self
                    .find_bar_rects()
                    .into_iter()
                    .find(|(h, _)| *h == Hit::ReplaceInput)
                {
                    self.replace_input
                        .click(&mut self.fonts, x - rx - 10.0, rw - 20.0, 12.0, false, mods.shift);
                }
                Vec::new()
            }
            Hit::FindPrev => {
                self.jump_to_match(false);
                Vec::new()
            }
            Hit::FindNext => {
                self.jump_to_match(true);
                Vec::new()
            }
            Hit::ReplaceOne => self.replace_current_match(),
            Hit::ReplaceAll => self.replace_all_matches(),
            Hit::CloseFind => {
                self.find_open = false;
                self.find_focus = FindFocus::None;
                self.matches.clear();
                Vec::new()
            }
            Hit::Editor => {
                let pos = self.pos_at(x, y);
                let text = self.doc().text.clone();
                let d = self.doc_mut();
                if double {
                    d.anchor = Some(word_left(&text, next_boundary(&text, pos).min(text.len())));
                    d.caret = word_right(&text, pos);
                } else {
                    if mods.shift {
                        d.anchor.get_or_insert(d.caret);
                    } else {
                        d.anchor = Some(pos);
                    }
                    d.caret = pos;
                    self.selecting = true;
                }
                self.doc_mut().goal_x = None;
                Vec::new()
            }
            Hit::Nothing => Vec::new(),
        }
    }

    pub fn release(&mut self, button: u32) {
        if button == BTN_LEFT && self.selecting {
            self.selecting = false;
            let d = self.doc_mut();
            if d.anchor == Some(d.caret) {
                d.anchor = None;
            }
        }
    }

    pub fn scroll(&mut self, dy: f32, mods: Mods) {
        if mods.ctrl {
            if dy < 0.0 {
                self.font_size = (self.font_size + 1.0).min(28.0);
            } else if dy > 0.0 {
                self.font_size = (self.font_size - 1.0).max(10.0);
            }
            self.layout_valid = false;
            self.dirty = true;
            return;
        }
        let content_h = self.content_h();
        let view_h = (self.editor_bottom() - self.editor_top() - PAD_TOP * 2.0).max(40.0);
        let max_scroll = (content_h - view_h).max(0.0);
        let d = self.doc_mut();
        d.scroll = (d.scroll + dy).clamp(0.0, max_scroll);
        self.dirty = true;
    }

    fn move_caret(&mut self, to: usize, extend: bool) {
        let d = self.doc_mut();
        if extend {
            d.anchor.get_or_insert(d.caret);
        } else {
            d.anchor = None;
        }
        d.caret = to;
        self.reveal_caret();
    }

    pub fn paste_text(&mut self, text: &str) -> Vec<Effect> {
        match self.find_focus {
            FindFocus::Find => {
                self.find_input.insert(&text.replace(['\n', '\r'], " "));
                self.recompute_matches();
                self.dirty = true;
                return Vec::new();
            }
            FindFocus::Replace => {
                self.replace_input.insert(&text.replace(['\n', '\r'], " "));
                self.dirty = true;
                return Vec::new();
            }
            FindFocus::None => {}
        }
        let clean: String = text
            .replace("\r\n", "\n")
            .chars()
            .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
            .collect();
        let effects = self.insert(&clean.replace('\t', "    "));
        self.reveal_caret();
        effects
    }

    pub fn key(&mut self, sym: u32, utf8: Option<&str>, mods: Mods) -> Vec<Effect> {
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        let sym = K::new(sym);
        self.dirty = true;
        let latin = |c: char| {
            sym == K::new(c as u32)
                || sym == K::new(c.to_ascii_uppercase() as u32)
                || utf8.is_some_and(|u| u.eq_ignore_ascii_case(&c.to_string()))
        };

        // Global shortcuts
        if mods.ctrl {
            if latin('n') || latin('t') || utf8 == Some("т") || utf8 == Some("е") {
                return self.new_tab();
            }
            if latin('o') || utf8 == Some("щ") {
                return vec![Effect::OpenDialog];
            }
            if latin('s') || utf8 == Some("ы") {
                return self.request_save(mods.shift);
            }
            if latin('w') || utf8 == Some("ц") {
                return self.close_tab(self.active);
            }
            if latin('q') || utf8 == Some("й") {
                return vec![Effect::Close];
            }
            if latin('f') || utf8 == Some("а") {
                self.find_open = true;
                self.find_focus = FindFocus::Find;
                if let Some((a, b)) = self.selection() {
                    let sel = self.doc().text[a..b].to_owned();
                    if !sel.contains('\n') && sel.len() <= 120 {
                        self.find_input.set(&sel);
                    }
                }
                self.recompute_matches();
                return Vec::new();
            }
            if latin('h') || utf8 == Some("р") {
                self.find_open = true;
                self.find_focus = FindFocus::Replace;
                self.recompute_matches();
                return Vec::new();
            }
            if latin('l') || utf8 == Some("д") {
                self.show_line_numbers = !self.show_line_numbers;
                self.layout_valid = false;
                return Vec::new();
            }
            if mods.alt && (latin('w') || utf8 == Some("ц")) {
                self.word_wrap = !self.word_wrap;
                self.layout_valid = false;
                return Vec::new();
            }
            if matches!(sym, K::plus | K::equal | K::KP_Add) {
                self.font_size = (self.font_size + 1.0).min(28.0);
                self.layout_valid = false;
                return Vec::new();
            }
            if matches!(sym, K::minus | K::KP_Subtract) {
                self.font_size = (self.font_size - 1.0).max(10.0);
                self.layout_valid = false;
                return Vec::new();
            }
            if matches!(sym, K:: _0 | K::KP_0) {
                self.font_size = 14.0;
                self.layout_valid = false;
                return Vec::new();
            }
            if sym == K::Tab && self.docs.len() > 1 {
                if mods.shift {
                    self.active = if self.active == 0 {
                        self.docs.len() - 1
                    } else {
                        self.active - 1
                    };
                } else {
                    self.active = (self.active + 1) % self.docs.len();
                }
                self.layout_valid = false;
                self.recompute_matches();
                return vec![Effect::SetTitle(self.window_title())];
            }
        }

        if sym == K::F3 {
            if !self.find_open {
                self.find_open = true;
                self.find_focus = FindFocus::Find;
                self.recompute_matches();
            } else {
                self.jump_to_match(!mods.shift);
            }
            return Vec::new();
        }

        // Find / Replace bar input handling
        if self.find_focus != FindFocus::None {
            match sym {
                K::Escape => {
                    self.find_open = false;
                    self.find_focus = FindFocus::None;
                    self.matches.clear();
                    return Vec::new();
                }
                K::Tab => {
                    self.find_focus = if self.find_focus == FindFocus::Find {
                        FindFocus::Replace
                    } else {
                        FindFocus::Find
                    };
                    return Vec::new();
                }
                K::Return | K::KP_Enter => {
                    if self.find_focus == FindFocus::Find {
                        self.jump_to_match(!mods.shift);
                        return Vec::new();
                    } else {
                        return self.replace_current_match();
                    }
                }
                _ if mods.ctrl && (latin('v') || utf8 == Some("м")) => {
                    return vec![Effect::Paste];
                }
                _ => {
                    if self.find_focus == FindFocus::Find {
                        if self.find_input.key(sym.raw(), utf8, mods.ctrl, mods.shift)
                            == Edit::Changed
                        {
                            self.recompute_matches();
                            if let Some(&(s, e)) = self.matches.first() {
                                self.docs[self.active].anchor = Some(s);
                                self.docs[self.active].caret = e;
                                self.reveal_caret();
                            }
                        }
                    } else {
                        self.replace_input
                            .key(sym.raw(), utf8, mods.ctrl, mods.shift);
                    }
                    return Vec::new();
                }
            }
        }

        self.relayout();
        let text = self.doc().text.clone();
        let extend = mods.shift;

        match sym {
            _ if mods.ctrl && (latin('a') || utf8 == Some("ф")) => {
                let d = self.doc_mut();
                d.anchor = Some(0);
                d.caret = text.len();
            }
            _ if mods.ctrl && (latin('c') || latin('x') || utf8 == Some("с") || utf8 == Some("ч")) => {
                if let Some((a, b)) = self.selection() {
                    let copied = text[a..b].to_owned();
                    let mut effects = Vec::new();
                    if latin('x') || utf8 == Some("ч") {
                        effects.extend(self.edit(a, b, "", false));
                    }
                    effects.push(Effect::Copy(copied));
                    return effects;
                }
            }
            _ if mods.ctrl && (latin('v') || utf8 == Some("м")) => {
                return vec![Effect::Paste];
            }
            _ if mods.ctrl && (latin('z') || utf8 == Some("я")) => {
                return self.restore(mods.shift);
            }
            _ if mods.ctrl && (latin('y') || utf8 == Some("н")) => {
                return self.restore(true);
            }
            K::Left => {
                let caret = self.doc().caret;
                let to = match self.selection() {
                    Some((a, _)) if !extend => a,
                    _ if mods.ctrl => word_left(&text, caret),
                    _ => prev_boundary(&text, caret),
                };
                self.move_caret(to, extend);
                self.doc_mut().goal_x = None;
            }
            K::Right => {
                let caret = self.doc().caret;
                let to = match self.selection() {
                    Some((_, b)) if !extend => b,
                    _ if mods.ctrl => word_right(&text, caret),
                    _ => next_boundary(&text, caret),
                };
                self.move_caret(to, extend);
                self.doc_mut().goal_x = None;
            }
            K::Up | K::Down => {
                let caret = self.doc().caret;
                let li = self.line_of(caret);
                let line = self.layout[li];
                let x = match self.doc().goal_x {
                    Some(x) => x,
                    None => self.x_of(line, caret),
                };
                let target = if sym == K::Up {
                    li.checked_sub(1)
                } else {
                    (li + 1 < self.layout.len()).then_some(li + 1)
                };
                let to = match target {
                    Some(t) => {
                        let tl = self.layout[t];
                        self.pos_at_x(tl, x)
                    }
                    None if sym == K::Up => 0,
                    None => text.len(),
                };
                self.move_caret(to, extend);
                self.doc_mut().goal_x = Some(x);
            }
            K::Page_Up | K::Page_Down => {
                let caret = self.doc().caret;
                let li = self.line_of(caret);
                let step = ((self.editor_bottom() - self.editor_top()) / self.line_height(self.font_size))
                    .floor()
                    .max(1.0) as usize;
                let target = if sym == K::Page_Up {
                    li.saturating_sub(step)
                } else {
                    (li + step).min(self.layout.len().saturating_sub(1))
                };
                let tl = self.layout[target];
                let x = self.doc().goal_x.unwrap_or_else(|| self.x_of(self.layout[li], caret));
                let to = self.pos_at_x(tl, x);
                self.move_caret(to, extend);
                self.doc_mut().goal_x = Some(x);
            }
            K::Home | K::End => {
                let caret = self.doc().caret;
                let line = self.layout[self.line_of(caret)];
                let to = match (sym == K::Home, mods.ctrl) {
                    (true, true) => 0,
                    (false, true) => text.len(),
                    (true, false) => line.start,
                    (false, false) => line.end,
                };
                self.move_caret(to, extend);
                self.doc_mut().goal_x = None;
            }
            K::BackSpace => {
                let caret = self.doc().caret;
                let effects = match self.selection() {
                    Some((a, b)) => self.edit(a, b, "", false),
                    None if caret > 0 => {
                        let from = if mods.ctrl {
                            word_left(&text, caret)
                        } else {
                            prev_boundary(&text, caret)
                        };
                        self.edit(from, caret, "", !mods.ctrl)
                    }
                    None => Vec::new(),
                };
                self.reveal_caret();
                return effects;
            }
            K::Delete => {
                let caret = self.doc().caret;
                let effects = match self.selection() {
                    Some((a, b)) => self.edit(a, b, "", false),
                    None if caret < text.len() => {
                        let to = if mods.ctrl {
                            word_right(&text, caret)
                        } else {
                            next_boundary(&text, caret)
                        };
                        self.edit(caret, to, "", false)
                    }
                    None => Vec::new(),
                };
                self.reveal_caret();
                return effects;
            }
            K::Return | K::KP_Enter => {
                // Auto-indent: preserve leading spaces of current logical line
                let caret = self.doc().caret;
                let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
                let indent: String = text[line_start..caret]
                    .chars()
                    .take_while(|&c| c == ' ')
                    .take(32)
                    .collect();
                let ins = format!("\n{indent}");
                let effects = self.insert(&ins);
                self.reveal_caret();
                return effects;
            }
            K::Tab => {
                let effects = self.insert("    ");
                self.reveal_caret();
                return effects;
            }
            K::Escape => {
                if self.find_open {
                    self.find_open = false;
                    self.find_focus = FindFocus::None;
                    self.matches.clear();
                } else {
                    self.doc_mut().anchor = None;
                }
            }
            _ => {
                if let Some(t) =
                    utf8.filter(|t| !mods.ctrl && !t.is_empty() && !t.chars().any(char::is_control))
                {
                    let effects = self.insert(t);
                    self.reveal_caret();
                    return effects;
                }
            }
        }
        Vec::new()
    }

    // ---------- Painting ----------

    pub fn paint(&mut self, pm: &mut PixmapMut) {
        self.dirty = false;
        self.relayout();
        pm.fill(colors::window());
        self.paint_editor(pm);
        if self.find_open {
            self.paint_find_bar(pm);
        }
        if self.docs.len() > 1 {
            self.paint_tab_bar(pm);
        }
        self.paint_toolbar(pm);
        self.paint_status_bar(pm);
        theme::stroke_rrect(
            pm,
            0.5,
            0.5,
            self.w - 1.0,
            self.h - 1.0,
            10.0,
            1.0,
            colors::separator(),
        );
    }

    fn paint_toolbar(&mut self, pm: &mut PixmapMut) {
        theme::fill_rect(pm, 0.0, 0.0, self.w, TOOLBAR_H, colors::glass());
        theme::fill_rect(pm, 0.0, TOOLBAR_H - 1.0, self.w, 1.0, colors::separator());

        // Traffic lights
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
            theme::fill_circle(pm, cx, 24.0, 6.0, color);
            if hovered_traffic {
                let ink = rgba(0, 0, 0, 140);
                match hit {
                    Hit::Close => {
                        theme::line(pm, cx - 2.5, 21.5, cx + 2.5, 26.5, 1.2, ink);
                        theme::line(pm, cx + 2.5, 21.5, cx - 2.5, 26.5, 1.2, ink);
                    }
                    Hit::Minimize => theme::line(pm, cx - 3.0, 24.0, cx + 3.0, 24.0, 1.2, ink),
                    _ => {
                        theme::line(pm, cx - 3.0, 24.0, cx + 3.0, 24.0, 1.2, ink);
                        theme::line(pm, cx, 21.0, cx, 27.0, 1.2, ink);
                    }
                }
            }
        }

        // Toolbar buttons
        let modified = self.doc().is_modified();
        for (hit, (bx, by, bw, bh)) in self.toolbar_buttons() {
            let active = matches!(
                (hit, self.show_line_numbers, self.word_wrap, self.find_open),
                (Hit::ToggleLineNumbers, true, _, _)
                    | (Hit::ToggleWrap, _, true, _)
                    | (Hit::ToggleFind, _, _, true)
            );
            let is_save_dirty = hit == Hit::SaveFile && modified;
            let bg = if is_save_dirty {
                colors::accent()
            } else if active {
                accent_alpha(42)
            } else if self.hover == hit {
                colors::hover()
            } else {
                colors::control()
            };
            theme::fill_rrect(pm, bx, by, bw, bh, 7.0, bg);
            theme::stroke_rrect(
                pm,
                bx + 0.5,
                by + 0.5,
                bw - 1.0,
                bh - 1.0,
                6.5,
                1.0,
                if active || is_save_dirty {
                    colors::accent()
                } else {
                    colors::separator()
                },
            );
            let fg = if is_save_dirty {
                rgba(255, 255, 255, 255)
            } else if active {
                colors::accent()
            } else {
                colors::text()
            };

            match hit {
                Hit::NewTab => {
                    let cx = bx + bw * 0.5;
                    let cy = by + bh * 0.5;
                    theme::line(pm, cx - 5.0, cy, cx + 5.0, cy, 1.6, fg);
                    theme::line(pm, cx, cy - 5.0, cx, cy + 5.0, 1.6, fg);
                }
                Hit::OpenFile => {
                    self.fonts.draw_text_centered(
                        pm,
                        &tr("Открыть…"),
                        bx + bw * 0.5,
                        by + 6.5,
                        12.0,
                        fg,
                        false,
                    );
                }
                Hit::SaveFile => {
                    self.fonts.draw_text_centered(
                        pm,
                        &tr("Сохранить"),
                        bx + bw * 0.5,
                        by + 6.5,
                        12.0,
                        fg,
                        is_save_dirty,
                    );
                }
                Hit::ToggleLineNumbers => {
                    self.fonts.draw_text_centered(
                        pm,
                        "#",
                        bx + bw * 0.5,
                        by + 5.5,
                        13.0,
                        fg,
                        true,
                    );
                }
                Hit::ToggleWrap => {
                    self.fonts.draw_text_centered(
                        pm,
                        "↩",
                        bx + bw * 0.5,
                        by + 5.5,
                        13.0,
                        fg,
                        true,
                    );
                }
                Hit::FontOut => {
                    self.fonts.draw_text_centered(
                        pm,
                        "A−",
                        bx + bw * 0.5,
                        by + 6.5,
                        11.5,
                        fg,
                        true,
                    );
                }
                Hit::FontReset => {
                    let lbl = format!("{:.0} {}", self.font_size, tr("пт"));
                    self.fonts.draw_text_centered(
                        pm,
                        &lbl,
                        bx + bw * 0.5,
                        by + 7.0,
                        11.0,
                        fg,
                        false,
                    );
                }
                Hit::FontIn => {
                    self.fonts.draw_text_centered(
                        pm,
                        "A+",
                        bx + bw * 0.5,
                        by + 6.5,
                        11.5,
                        fg,
                        true,
                    );
                }
                Hit::ToggleFind => {
                    let cx = bx + bw * 0.5 - 1.5;
                    let cy = by + bh * 0.5 - 1.5;
                    theme::stroke_circle(pm, cx, cy, 4.5, 1.5, fg);
                    theme::line(pm, cx + 3.2, cy + 3.2, cx + 6.5, cy + 6.5, 1.6, fg);
                }
                _ => {}
            }
        }

        // Centered Title & Subtitle
        let d = self.doc();
        let title = if d.is_modified() {
            format!("{} •", d.title)
        } else {
            d.title.clone()
        };
        let subtitle = format!(
            "{} · {}: {} · {}: {} · {}: {}",
            tr(d.syntax.label()),
            tr("Строк"),
            d.line_count(),
            tr("Слов"),
            d.word_count(),
            tr("Симв."),
            d.char_count()
        );
        let center_x = self.w * 0.5;
        self.fonts
            .draw_text_centered(pm, &title, center_x, 7.0, 13.0, colors::text(), true);
        self.fonts
            .draw_text_centered(pm, &subtitle, center_x, 25.0, 10.5, colors::secondary(), false);
    }

    fn paint_tab_bar(&mut self, pm: &mut PixmapMut) {
        let top = TOOLBAR_H;
        theme::fill_rect(pm, 0.0, top, self.w, TAB_BAR_H, colors::sidebar());
        theme::fill_rect(pm, 0.0, top + TAB_BAR_H - 1.0, self.w, 1.0, colors::separator());

        let n = self.docs.len();
        let avail = (self.w - 44.0).max(120.0);
        let tw = (avail / n as f32).clamp(90.0, 210.0);
        for i in 0..n {
            let tx = 8.0 + i as f32 * tw;
            let is_active = i == self.active;
            let bg = if is_active {
                colors::window()
            } else if self.hover == Hit::Tab(i) || self.hover == Hit::CloseTab(i) {
                colors::hover()
            } else {
                rgba(0, 0, 0, 0)
            };
            theme::fill_rrect(pm, tx + 2.0, top + 4.0, tw - 4.0, TAB_BAR_H - 5.0, 6.0, bg);
            if is_active {
                theme::stroke_rrect(
                    pm,
                    tx + 2.5,
                    top + 4.5,
                    tw - 5.0,
                    TAB_BAR_H - 6.0,
                    5.5,
                    1.0,
                    colors::separator(),
                );
            }
            let title = if self.docs[i].is_modified() {
                format!("{} •", self.docs[i].title)
            } else {
                self.docs[i].title.clone()
            };
            self.fonts.draw_text_clipped(
                pm,
                &title,
                tx + 10.0,
                top + 8.0,
                tw - 34.0,
                11.5,
                if is_active {
                    colors::text()
                } else {
                    colors::secondary()
                },
                is_active,
            );
            // Close tab button `×`
            let cx = tx + tw - 14.0;
            let cy = top + TAB_BAR_H * 0.5;
            if self.hover == Hit::CloseTab(i) {
                theme::fill_circle(pm, cx, cy, 7.0, colors::hover());
            }
            let ink = colors::secondary();
            theme::line(pm, cx - 2.5, cy - 2.5, cx + 2.5, cy + 2.5, 1.2, ink);
            theme::line(pm, cx + 2.5, cy - 2.5, cx - 2.5, cy + 2.5, 1.2, ink);
        }
    }

    fn paint_find_bar(&mut self, pm: &mut PixmapMut) {
        let top = TOOLBAR_H + self.tab_bar_h();
        theme::fill_rect(pm, 0.0, top, self.w, FIND_BAR_H, colors::glass());
        theme::fill_rect(
            pm,
            0.0,
            top + FIND_BAR_H - 1.0,
            self.w,
            1.0,
            colors::separator(),
        );

        for (hit, (rx, ry, rw, rh)) in self.find_bar_rects() {
            match hit {
                Hit::FindInput => {
                    theme::fill_rrect(pm, rx, ry, rw, rh, 6.0, colors::window());
                    theme::stroke_rrect(
                        pm,
                        rx + 0.5,
                        ry + 0.5,
                        rw - 1.0,
                        rh - 1.0,
                        5.5,
                        if self.find_focus == FindFocus::Find {
                            1.5
                        } else {
                            1.0
                        },
                        if self.find_focus == FindFocus::Find {
                            colors::accent()
                        } else {
                            colors::separator()
                        },
                    );
                    if self.find_input.is_empty() && self.find_focus != FindFocus::Find {
                        self.fonts.draw_text(
                            pm,
                            &tr("Найти…"),
                            rx + 10.0,
                            ry + 6.5,
                            12.0,
                            colors::tertiary(),
                            false,
                        );
                    } else {
                        self.find_input.draw(
                            pm,
                            &mut self.fonts,
                            rx + 10.0,
                            ry + 6.5,
                            rw - 56.0,
                            12.0,
                            false,
                            colors::text(),
                            self.find_focus == FindFocus::Find,
                            colors::accent(),
                        );
                    }
                    if !self.find_input.is_empty() {
                        let count_lbl = if self.matches.is_empty() {
                            "0".to_string()
                        } else {
                            format!("{}/{}", self.active_match + 1, self.matches.len())
                        };
                        self.fonts.draw_text_right(
                            pm,
                            &count_lbl,
                            rx + rw - 8.0,
                            ry + 7.5,
                            10.5,
                            colors::secondary(),
                            false,
                        );
                    }
                }
                Hit::ReplaceInput => {
                    theme::fill_rrect(pm, rx, ry, rw, rh, 6.0, colors::window());
                    theme::stroke_rrect(
                        pm,
                        rx + 0.5,
                        ry + 0.5,
                        rw - 1.0,
                        rh - 1.0,
                        5.5,
                        if self.find_focus == FindFocus::Replace {
                            1.5
                        } else {
                            1.0
                        },
                        if self.find_focus == FindFocus::Replace {
                            colors::accent()
                        } else {
                            colors::separator()
                        },
                    );
                    if self.replace_input.is_empty() && self.find_focus != FindFocus::Replace {
                        self.fonts.draw_text(
                            pm,
                            &tr("Заменить на…"),
                            rx + 10.0,
                            ry + 6.5,
                            12.0,
                            colors::tertiary(),
                            false,
                        );
                    } else {
                        self.replace_input.draw(
                            pm,
                            &mut self.fonts,
                            rx + 10.0,
                            ry + 6.5,
                            rw - 20.0,
                            12.0,
                            false,
                            colors::text(),
                            self.find_focus == FindFocus::Replace,
                            colors::accent(),
                        );
                    }
                }
                Hit::FindPrev | Hit::FindNext | Hit::ReplaceOne | Hit::ReplaceAll | Hit::CloseFind => {
                    let bg = if self.hover == hit {
                        colors::hover()
                    } else {
                        colors::control()
                    };
                    theme::fill_rrect(pm, rx, ry, rw, rh, 6.0, bg);
                    theme::stroke_rrect(
                        pm,
                        rx + 0.5,
                        ry + 0.5,
                        rw - 1.0,
                        rh - 1.0,
                        5.5,
                        1.0,
                        colors::separator(),
                    );
                    let lbl = match hit {
                        Hit::FindPrev => "‹".into(),
                        Hit::FindNext => "›".into(),
                        Hit::ReplaceOne => tr("Заменить"),
                        Hit::ReplaceAll => tr("Все"),
                        _ => "×".into(),
                    };
                    self.fonts.draw_text_centered(
                        pm,
                        &lbl,
                        rx + rw * 0.5,
                        ry + 6.5,
                        12.0,
                        colors::text(),
                        false,
                    );
                }
                _ => {}
            }
        }
    }

    fn paint_editor(&mut self, pm: &mut PixmapMut) {
        let top = self.editor_top();
        let bottom = self.editor_bottom();
        let view_h = (bottom - top).max(0.0);
        let gutter = self.gutter_w();
        let text_x = self.text_x();
        let scroll = self.doc().scroll;
        let caret = self.doc().caret;
        let sel = self.selection();
        let (caret_logical_line, _) = self.doc().caret_line_col();

        // Gutter background
        if self.show_line_numbers {
            theme::fill_rect(pm, 0.0, top, gutter, view_h, colors::sidebar());
            theme::fill_rect(pm, gutter - 1.0, top, 1.0, view_h, colors::separator());
        }

        let layout = self.layout.clone();
        let text = self.doc().text.clone();
        let matches = self.matches.clone();
        let active_match = self.active_match;

        for (li, line) in layout.iter().enumerate() {
            let y = top + PAD_TOP + line.y - scroll;
            if y + line.height < top || y > bottom {
                continue;
            }

            // Highlight active logical line
            if line.logical_line == caret_logical_line {
                theme::fill_rect(
                    pm,
                    gutter,
                    y,
                    self.w - gutter,
                    line.height,
                    rgba(128, 128, 140, 15),
                );
            }

            // Line number in gutter
            if self.show_line_numbers && line.first_of_logical {
                let num_str = line.logical_line.to_string();
                let is_curr = line.logical_line == caret_logical_line;
                self.fonts.draw_text_right(
                    pm,
                    &num_str,
                    gutter - 10.0,
                    y + (line.height - line.size) * 0.35,
                    (self.font_size - 2.0).clamp(9.5, 14.0),
                    if is_curr {
                        colors::text()
                    } else {
                        colors::tertiary()
                    },
                    is_curr,
                );
            }

            // Search match highlights on this line
            for (mi, &(ms, me)) in matches.iter().enumerate() {
                let a = ms.max(line.start);
                let b = me.min(line.end);
                if a < b {
                    let x0 = text_x
                        + self
                            .fonts
                            .measure_text(&text[line.start..a], line.size, line.bold);
                    let x1 = text_x
                        + self
                            .fonts
                            .measure_text(&text[line.start..b], line.size, line.bold);
                    let is_curr = mi == active_match;
                    theme::fill_rrect(
                        pm,
                        x0,
                        y + 1.0,
                        (x1 - x0).max(4.0),
                        line.height - 2.0,
                        4.0,
                        if is_curr {
                            rgba(255, 159, 10, 120)
                        } else {
                            rgba(255, 214, 10, 85)
                        },
                    );
                }
            }

            // Selection highlight on this line
            if let Some((sa, sb)) = sel {
                let a = sa.max(line.start);
                let b = sb.min(line.end);
                if a < b || (sa <= line.start && sb > line.end) {
                    let x0 = text_x
                        + self
                            .fonts
                            .measure_text(&text[line.start..a], line.size, line.bold);
                    let x1 = text_x
                        + self
                            .fonts
                            .measure_text(&text[line.start..b], line.size, line.bold)
                        + if sb > line.end { 6.0 } else { 0.0 };
                    theme::fill_rrect(
                        pm,
                        x0,
                        y + 1.0,
                        (x1 - x0).max(4.0),
                        line.height - 2.0,
                        4.0,
                        selection_color(),
                    );
                }
            }

            // Syntax/Structure-aware text color
            let fg = match line.style {
                LineStyle::Heading => colors::accent(),
                LineStyle::Subheading => colors::text(),
                LineStyle::Comment => colors::secondary(),
                LineStyle::Section => rgba(175, 82, 222, 255),
                LineStyle::CodeOrQuote => rgba(48, 176, 199, 255),
                LineStyle::Bullet => colors::text(),
                LineStyle::Normal => colors::text(),
            };

            let ty = y + (line.height - line.size) * 0.35;
            self.fonts.draw_text(
                pm,
                &text[line.start..line.end],
                text_x,
                ty,
                line.size,
                fg,
                line.bold,
            );

            // Caret
            if self.focused
                && self.find_focus == FindFocus::None
                && sel.is_none()
                && li == self.line_of(caret)
            {
                let cx = text_x
                    + self.fonts.measure_text(
                        &text[line.start..caret.clamp(line.start, line.end)],
                        line.size,
                        line.bold,
                    );
                theme::fill_rect(pm, cx, y + 2.0, 2.0, line.height - 4.0, colors::accent());
            }
        }

        // Scrollbar thumb
        let content_h = self.content_h();
        let inner_h = (view_h - PAD_TOP * 2.0).max(40.0);
        if content_h > inner_h {
            let ratio = (inner_h / content_h).clamp(0.08, 1.0);
            let thumb_h = (view_h * ratio).max(28.0);
            let max_scroll = (content_h - inner_h).max(1.0);
            let thumb_y = top + (scroll / max_scroll) * (view_h - thumb_h - 8.0) + 4.0;
            theme::fill_rrect(
                pm,
                self.w - 8.0,
                thumb_y,
                4.0,
                thumb_h,
                2.0,
                colors::tertiary(),
            );
        }
    }

    fn paint_status_bar(&mut self, pm: &mut PixmapMut) {
        let top = self.h - STATUS_BAR_H;
        theme::fill_rect(pm, 0.0, top, self.w, STATUS_BAR_H, colors::glass());
        theme::fill_rect(pm, 0.0, top, self.w, 1.0, colors::separator());

        let (line, col) = self.doc().caret_line_col();
        let mut left = format!("{} {}, {} {}", tr("Стр."), line, tr("стб."), col);
        if let Some((a, b)) = self.selection() {
            let chars = self.doc().text[a..b].chars().count();
            left.push_str(&format!(" · {}: {}", tr("Выделено"), chars));
        }
        if let Some(msg) = &self.status_message {
            left.push_str(&format!(" · {msg}"));
        }
        self.fonts
            .draw_text(pm, &left, 14.0, top + 6.0, 11.0, colors::secondary(), false);

        let d = self.doc();
        let state_lbl = if d.is_modified() {
            tr("Изменено")
        } else {
            tr("Сохранено")
        };
        let right = format!("{} · UTF-8 · {}", tr(d.syntax.label()), state_lbl);
        self.fonts.draw_text_right(
            pm,
            &right,
            self.w - 14.0,
            top + 6.0,
            11.0,
            colors::secondary(),
            false,
        );
    }
}

fn accent_alpha(alpha: u8) -> tiny_skia::Color {
    let c = colors::accent();
    rgba(
        (c.red() * 255.0) as u8,
        (c.green() * 255.0) as u8,
        (c.blue() * 255.0) as u8,
        alpha,
    )
}

fn selection_color() -> tiny_skia::Color {
    let c = colors::highlight();
    rgba(
        (c.red() * 255.0) as u8,
        (c.green() * 255.0) as u8,
        (c.blue() * 255.0) as u8,
        75,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    #[test]
    fn textedit_tabs_editing_undo_find_replace_and_paint() {
        let mut app = App::new(FontSystem::new());
        let mut pm = Pixmap::new(960, 640).unwrap();

        // Initial untitled document
        app.paint(&mut pm.as_mut());
        assert_eq!(app.doc().line_count(), 1);
        assert!(!app.doc().is_modified());

        // Type text and verify modified state + line/word count
        app.paste_text("# Заголовок\nПервая строка\nВторая строка");
        assert!(app.doc().is_modified());
        assert_eq!(app.doc().line_count(), 3);
        assert_eq!(app.doc().word_count(), 6);

        // Undo & Redo
        app.key(b'z' as u32, Some("z"), Mods { ctrl: true, shift: false, alt: false });
        assert_eq!(app.doc().text, "");
        app.key(b'y' as u32, Some("y"), Mods { ctrl: true, shift: false, alt: false });
        assert!(app.doc().text.contains("Первая строка"));

        // Find & Replace
        app.key(b'f' as u32, Some("f"), Mods { ctrl: true, shift: false, alt: false });
        assert!(app.find_open);
        app.paste_text("строка");
        assert_eq!(app.matches.len(), 2);

        // Switch to Replace and replace all
        app.key(0xff09, None, Mods::default()); // Tab -> Replace input
        app.paste_text("линия");
        app.replace_all_matches();
        assert_eq!(app.doc().text.matches("линия").count(), 2);

        // Multi-tab creation and closing
        app.new_tab();
        assert_eq!(app.docs.len(), 2);
        app.paint(&mut pm.as_mut());
        app.close_tab(1);
        assert_eq!(app.docs.len(), 1);
    }
}
