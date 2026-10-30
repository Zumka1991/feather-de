//! The store window: a floating sidebar with search and sections, a grid of programs and a
//! page per program. Painting records the clickable zones it drew, so hit testing always
//! matches what is on screen.

use crate::catalog::{self, AppInfo, Catalog};
use crate::packagekit::{Event, Op};
use feather_ui::appearance::colors;
use feather_ui::draw::*;
use feather_ui::i18n::tr;
use feather_ui::input::{Edit, TextInput};
use feather_ui::text::FontSystem;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tiny_skia::{Color, Pixmap, PixmapMut};

pub const DEFAULT_W: f32 = 1060.0;
pub const DEFAULT_H: f32 = 720.0;
pub const MIN_W: f32 = 780.0;
pub const MIN_H: f32 = 520.0;
const SIDEBAR_W: f32 = 236.0;
const TOOLBAR_H: f32 = 52.0;
const NAV_TOP: f32 = 98.0;
const NAV_ROW: f32 = 32.0;
const PAD: f32 = 30.0;
const GAP: f32 = 14.0;
const CARD_MIN_W: f32 = 300.0;
const CARD_H: f32 = 88.0;
const CARD_ICON: f32 = 56.0;
const BANNER_H: f32 = 156.0;
const DETAIL_ICON: f32 = 104.0;
const RESIZE_BAND: f32 = 5.0;
const BTN_LEFT: u32 = 0x110;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Sections of the sidebar: label and the freedesktop categories they gather
pub const CATEGORIES: [(&str, &[&str]); 9] = [
    ("Интернет", &["Network", "WebBrowser", "Email", "Chat", "InstantMessaging", "P2P", "FileTransfer"]),
    ("Офис", &["Office", "WordProcessor", "Spreadsheet", "Presentation"]),
    ("Графика и фото", &["Graphics", "Photography"]),
    ("Музыка и видео", &["AudioVideo", "Audio", "Video"]),
    ("Игры", &["Game"]),
    ("Разработка", &["Development", "IDE"]),
    ("Образование и наука", &["Education", "Science"]),
    ("Утилиты", &["Utility"]),
    ("Система", &["System", "Settings"]),
];

/// The home page's picks (AppStream ids); those missing from the catalog are skipped
const EDITORS_CHOICE: [&str; 22] = [
    "org.gimp.GIMP",
    "org.inkscape.Inkscape",
    "org.kde.krita",
    "org.videolan.vlc",
    "com.obsproject.Studio",
    "org.kde.kdenlive.desktop",
    "blender.desktop",
    "org.audacityteam.Audacity",
    "org.telegram.desktop",
    "org.keepassxc.KeePassXC.desktop",
    "org.qbittorrent.qBittorrent",
    "com.calibre_ebook.calibre",
    "org.darktable.darktable",
    "fr.handbrake.ghb",
    "org.shotcut.Shotcut",
    "org.flameshot.Flameshot",
    "com.github.xournalpp.xournalpp",
    "org.filezilla-project.FileZilla",
    "org.remmina.Remmina.desktop",
    "org.musescore.MuseScore",
    "org.stellarium.Stellarium",
    "org.kde.kate.desktop",
];

const GAMES: [&str; 8] = [
    "net.supertuxkart.SuperTuxKart",
    "com.play0ad.zeroad",
    "net.minetest.minetest",
    "org.wesnoth.Wesnoth",
    "openttd.desktop",
    "net.wz2100.warzone2100",
    "org.supertuxproject.SuperTux",
    "frozen-bubble.desktop",
];

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Close,
    Minimize,
    ToggleMaximize,
    Move,
    Resize(u32),
    /// Start a PackageKit operation for a package; `confirmed`: what else it removes is agreed
    Run(Op, String, bool),
    Launch(String),
    OpenUrl(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Default,
    Text,
    Resize(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Page {
    Home,
    Category(usize),
    Installed,
    Search,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Home => "Главная",
            Page::Category(i) => CATEGORIES[i].0,
            Page::Installed => "Установленные",
            Page::Search => "Поиск",
        }
    }
}

fn nav_pages() -> impl Iterator<Item = Page> {
    std::iter::once(Page::Home).chain((0..CATEGORIES.len()).map(Page::Category)).chain(std::iter::once(Page::Installed))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Nothing,
    Close,
    Minimize,
    Zoom,
    Toolbar,
    Search,
    SearchClear,
    Nav(Page),
    Back,
    Card(usize),
    CardButton(usize),
    Primary,
    Remove,
    Confirm,
    CancelConfirm,
    Homepage,
    NoticeClose,
    Content,
}

/// An operation in progress
#[derive(Clone, Debug)]
struct Task {
    app: usize,
    op: Op,
    fraction: Option<f32>,
    message: String,
    /// Packages the user agreed to remove along with it
    also: Vec<String>,
}

pub struct App {
    fonts: FontSystem,
    pub w: f32,
    pub h: f32,
    pub dirty: bool,
    pub maximized: bool,
    pub focused: bool,
    catalog: Option<Catalog>,
    page: Page,
    /// The page before a search, to return to when it is cleared
    before_search: Page,
    detail: Option<usize>,
    /// Program to open as soon as the catalog arrives (command line)
    pending_detail: Option<String>,
    search: TextInput,
    search_focus: bool,
    scroll: f32,
    list_scroll: f32,
    content_h: f32,
    results: Vec<usize>,
    by_id: HashMap<String, usize>,
    icons: HashMap<usize, Option<Pixmap>>,
    requested: HashSet<usize>,
    icon_queue: Vec<(usize, PathBuf)>,
    task: Option<Task>,
    confirm: Option<(usize, Vec<String>)>,
    /// Text and whether it is an error
    notice: Option<(String, bool)>,
    zones: Vec<(Hit, (f32, f32, f32, f32))>,
    hover: Hit,
    pressed: Option<Hit>,
    last_click: Option<(Instant, f32, f32)>,
}

impl App {
    pub fn new(fonts: FontSystem) -> Self {
        Self {
            fonts,
            w: DEFAULT_W,
            h: DEFAULT_H,
            dirty: true,
            maximized: false,
            focused: true,
            catalog: None,
            page: Page::Home,
            before_search: Page::Home,
            detail: None,
            pending_detail: None,
            search: TextInput::default().with_limit(80),
            search_focus: false,
            scroll: 0.0,
            list_scroll: 0.0,
            content_h: 0.0,
            results: Vec::new(),
            by_id: HashMap::new(),
            icons: HashMap::new(),
            requested: HashSet::new(),
            icon_queue: Vec::new(),
            task: None,
            confirm: None,
            notice: None,
            zones: Vec::new(),
            hover: Hit::Nothing,
            pressed: None,
            last_click: None,
        }
    }

    /// Opens this program's page once the catalog is there ("org.gimp.GIMP", appstream://…)
    pub fn open_later(&mut self, id: &str) {
        let id = id.trim_start_matches("appstream://").trim_start_matches("appstream:");
        self.pending_detail = Some(id.to_string());
    }

    pub fn loaded(&mut self, catalog: Catalog) {
        self.by_id = catalog.apps.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
        self.catalog = Some(catalog);
        if let Some(id) = self.pending_detail.take() {
            let found = self.by_id.get(&id).or_else(|| self.by_id.get(&format!("{id}.desktop"))).copied();
            if let Some(i) = found {
                self.detail = Some(i);
            }
        }
        self.refresh_results();
        self.dirty = true;
    }

    /// dpkg's list after the window was away: something may have been installed elsewhere
    pub fn installed_changed(&mut self, installed: HashSet<String>) {
        if let Some(c) = &mut self.catalog {
            if c.installed != installed {
                c.installed = installed;
                if self.page == Page::Installed {
                    self.refresh_results();
                }
                self.dirty = true;
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.task.is_some()
    }

    pub fn icons_loaded(&mut self, icons: Vec<(usize, Option<Pixmap>)>) {
        for (i, icon) in icons {
            self.icons.insert(i, icon);
        }
        self.dirty = true;
    }

    /// Icons painting found missing; the caller loads them off the event loop
    pub fn take_icon_queue(&mut self) -> Vec<(usize, PathBuf)> {
        std::mem::take(&mut self.icon_queue)
    }

    fn app(&self, i: usize) -> Option<&AppInfo> {
        self.catalog.as_ref().and_then(|c| c.apps.get(i))
    }

    fn installed(&self, i: usize) -> bool {
        match (&self.catalog, self.app(i)) {
            (Some(c), Some(a)) => c.installed.contains(&a.package),
            _ => false,
        }
    }

    fn picks(&self, ids: &[&str]) -> Vec<usize> {
        ids.iter().filter_map(|id| self.by_id.get(*id).copied()).collect()
    }

    fn refresh_results(&mut self) {
        let Some(catalog) = &self.catalog else { return };
        let picked: HashSet<usize> = self.picks(&EDITORS_CHOICE).into_iter().chain(self.picks(&GAMES)).collect();
        self.results = match self.page {
            Page::Home => Vec::new(),
            Page::Search => search(&catalog.apps, self.search.text()),
            Page::Installed => (0..catalog.apps.len()).filter(|i| catalog.installed.contains(&catalog.apps[*i].package)).collect(),
            Page::Category(c) => {
                let wanted = CATEGORIES[c].1;
                let mut list: Vec<usize> = (0..catalog.apps.len()).filter(|i| catalog.apps[*i].categories.iter().any(|k| wanted.contains(&k.as_str()))).collect();
                // Picks first, the rest stay alphabetical
                list.sort_by_key(|i| !picked.contains(i));
                list
            }
        };
    }

    fn go(&mut self, page: Page) {
        if page != Page::Search && !self.search.is_empty() {
            self.search.clear();
        }
        self.page = page;
        self.detail = None;
        self.confirm = None;
        self.scroll = 0.0;
        self.refresh_results();
        self.dirty = true;
    }

    fn open_detail(&mut self, i: usize) {
        self.list_scroll = self.scroll;
        self.detail = Some(i);
        self.confirm = None;
        self.scroll = 0.0;
        self.dirty = true;
    }

    fn back(&mut self) {
        if self.detail.take().is_some() {
            self.confirm = None;
            self.scroll = self.list_scroll;
            self.dirty = true;
        }
    }

    fn search_changed(&mut self) {
        if self.search.text().trim().is_empty() {
            if self.page == Page::Search {
                let back = self.before_search;
                self.go(back);
            }
            return;
        }
        if self.page != Page::Search {
            self.before_search = self.page;
            self.page = Page::Search;
        }
        self.detail = None;
        self.confirm = None;
        self.scroll = 0.0;
        self.refresh_results();
    }

    // ---------- operations ----------

    fn start(&mut self, op: Op, app: usize, confirmed: Vec<String>) -> Vec<Effect> {
        if self.task.is_some() {
            return Vec::new();
        }
        let Some(package) = self.app(app).map(|a| a.package.clone()) else { return Vec::new() };
        let confirmed_all = !confirmed.is_empty();
        self.notice = None;
        self.confirm = None;
        self.task = Some(Task { app, op, fraction: None, message: tr("Подготовка…").into_owned(), also: confirmed });
        self.dirty = true;
        vec![Effect::Run(op, package, confirmed_all)]
    }

    /// "Открыть": the program's .desktop command; without one its page
    fn open_program(&mut self, i: usize) -> Vec<Effect> {
        match self.app(i).and_then(catalog::launcher) {
            Some(command) => vec![Effect::Launch(command)],
            None => {
                if self.detail != Some(i) {
                    self.open_detail(i);
                }
                self.notice = Some((tr("У этой программы нет ярлыка для запуска").into_owned(), false));
                Vec::new()
            }
        }
    }

    pub fn task_event(&mut self, event: Event) {
        let Some(task) = self.task.as_mut() else { return };
        self.dirty = true;
        match event {
            Event::Progress(fraction, message) => {
                task.fraction = fraction;
                task.message = tr(&message).into_owned();
            }
            Event::Confirm(others) => {
                let app = task.app;
                self.task = None;
                if self.detail != Some(app) {
                    self.open_detail(app);
                }
                self.confirm = Some((app, others));
            }
            Event::Done => {
                let task = self.task.take().unwrap_or_else(|| unreachable!());
                if let Some(c) = &mut self.catalog {
                    let package = c.apps[task.app].package.clone();
                    match task.op {
                        Op::Install => {
                            c.installed.insert(package);
                        }
                        Op::Remove => {
                            c.installed.remove(&package);
                            for p in &task.also {
                                c.installed.remove(p);
                            }
                        }
                    }
                }
                if self.page == Page::Installed {
                    self.refresh_results();
                }
            }
            Event::Cancelled => self.task = None,
            Event::Failed(text) => {
                let (app, op) = (task.app, task.op);
                self.task = None;
                let name = self.app(app).map(|a| a.name.clone()).unwrap_or_default();
                let head = match op {
                    Op::Install => tr("Не удалось установить"),
                    Op::Remove => tr("Не удалось удалить"),
                };
                self.notice = Some((format!("{head} «{name}»: {}", tr(&text)), true));
            }
        }
    }

    // ---------- input ----------

    fn hit(&self, x: f32, y: f32) -> Hit {
        self.zones
            .iter()
            .rev()
            .find(|(_, (zx, zy, zw, zh))| x >= *zx && x < zx + zw && y >= *zy && y < zy + zh)
            .map_or(Hit::Nothing, |(hit, _)| *hit)
    }

    fn resize_edges(&self, x: f32, y: f32) -> u32 {
        if self.maximized {
            return 0;
        }
        let mut edges = 0;
        if y < RESIZE_BAND {
            edges |= 1
        }
        if y >= self.h - RESIZE_BAND {
            edges |= 2
        }
        if x < RESIZE_BAND {
            edges |= 4
        }
        if x >= self.w - RESIZE_BAND {
            edges |= 8
        }
        edges
    }

    pub fn cursor_at(&self, x: f32, y: f32) -> Cursor {
        match (self.resize_edges(x, y), self.hit(x, y)) {
            (e, _) if e != 0 => Cursor::Resize(e),
            (_, Hit::Search) => Cursor::Text,
            _ => Cursor::Default,
        }
    }

    pub fn resize(&mut self, w: f32, h: f32) {
        let (w, h) = (w.max(MIN_W), h.max(MIN_H));
        if (w, h) != (self.w, self.h) {
            self.w = w;
            self.h = h;
            self.dirty = true;
        }
    }

    pub fn motion(&mut self, x: f32, y: f32) {
        let hit = self.hit(x, y);
        if hit != self.hover {
            let quiet = |h: Hit| matches!(h, Hit::Nothing | Hit::Toolbar | Hit::Content | Hit::Search);
            if !(quiet(hit) && quiet(self.hover)) {
                self.dirty = true;
            }
            self.hover = hit;
        }
    }

    pub fn leave(&mut self) {
        if self.hover != Hit::Nothing {
            self.hover = Hit::Nothing;
            self.dirty = true;
        }
    }

    pub fn press(&mut self, button: u32, x: f32, y: f32, shift: bool) -> Vec<Effect> {
        if button != BTN_LEFT {
            // The mouse's back button
            if button == 0x113 {
                self.back();
            }
            return Vec::new();
        }
        let edges = self.resize_edges(x, y);
        if edges != 0 {
            return vec![Effect::Resize(edges)];
        }
        let now = Instant::now();
        let double = self.last_click.is_some_and(|(t, px, py)| now - t < DOUBLE_CLICK && (px - x).hypot(py - y) < 4.0);
        self.last_click = Some((now, x, y));
        let hit = self.hit(x, y);
        if self.search_focus != (hit == Hit::Search) {
            self.search_focus = hit == Hit::Search;
            self.dirty = true;
        }
        match hit {
            Hit::Close => vec![Effect::Close],
            Hit::Minimize => vec![Effect::Minimize],
            Hit::Zoom => vec![Effect::ToggleMaximize],
            Hit::Toolbar if double => vec![Effect::ToggleMaximize],
            Hit::Toolbar => vec![Effect::Move],
            Hit::Search => {
                let (sx, _, sw, _) = search_rect();
                self.search.click(&mut self.fonts, x - sx - 30.0, sw - 56.0, 13.0, false, shift);
                self.dirty = true;
                Vec::new()
            }
            Hit::Nothing | Hit::Content => Vec::new(),
            other => {
                self.pressed = Some(other);
                self.dirty = true;
                Vec::new()
            }
        }
    }

    pub fn release(&mut self, button: u32, x: f32, y: f32) -> Vec<Effect> {
        if button != BTN_LEFT {
            return Vec::new();
        }
        let Some(pressed) = self.pressed.take() else { return Vec::new() };
        self.dirty = true;
        if self.hit(x, y) != pressed {
            return Vec::new();
        }
        self.activate(pressed)
    }

    fn activate(&mut self, hit: Hit) -> Vec<Effect> {
        match hit {
            Hit::SearchClear => {
                self.search.clear();
                self.search_changed();
                Vec::new()
            }
            Hit::Nav(page) => {
                self.go(page);
                Vec::new()
            }
            Hit::Back => {
                self.back();
                Vec::new()
            }
            Hit::Card(i) => {
                self.open_detail(i);
                Vec::new()
            }
            Hit::CardButton(_) | Hit::Primary => {
                let i = if let Hit::CardButton(i) = hit { i } else { self.detail.unwrap_or(usize::MAX) };
                if self.installed(i) {
                    self.open_program(i)
                } else {
                    self.start(Op::Install, i, Vec::new())
                }
            }
            Hit::Remove => match self.detail {
                Some(i) => self.start(Op::Remove, i, Vec::new()),
                None => Vec::new(),
            },
            Hit::Confirm => match self.confirm.take() {
                Some((i, others)) => self.start(Op::Remove, i, others),
                None => Vec::new(),
            },
            Hit::CancelConfirm => {
                self.confirm = None;
                Vec::new()
            }
            Hit::Homepage => match self.detail.and_then(|i| self.app(i)).map(|a| a.homepage.clone()) {
                Some(url) if !url.is_empty() => vec![Effect::OpenUrl(url)],
                _ => Vec::new(),
            },
            Hit::NoticeClose => {
                self.notice = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn scroll(&mut self, dy: f32, x: f32) {
        if x < SIDEBAR_W {
            return;
        }
        self.scroll_by(dy);
    }

    fn scroll_by(&mut self, dy: f32) {
        let max = (self.content_h - self.h).max(0.0);
        let to = (self.scroll + dy).clamp(0.0, max);
        if to != self.scroll {
            self.scroll = to;
            self.dirty = true;
        }
    }

    /// `code` is the evdev key code: shortcuts work in any layout (Ctrl+Ф is Ctrl+F)
    pub fn key(&mut self, sym: u32, code: u32, utf8: Option<&str>, ctrl: bool, shift: bool, alt: bool) -> Vec<Effect> {
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        let letter = feather_ui::input::shortcut_letter(sym, code);
        let key = K::new(sym);
        if ctrl && (letter == Some('w') || letter == Some('q')) {
            return vec![Effect::Close];
        }
        if ctrl && letter == Some('f') {
            self.search_focus = true;
            self.search.select_all();
            self.dirty = true;
            return Vec::new();
        }
        if alt && key == K::Left {
            self.back();
            return Vec::new();
        }
        if self.search_focus {
            match key {
                K::Escape => {
                    self.search_focus = false;
                    if !self.search.is_empty() {
                        self.search.clear();
                        self.search_changed();
                    }
                }
                K::Return | K::KP_Enter | K::Down | K::Tab => self.search_focus = false,
                _ => {
                    if self.search.key(sym, utf8, ctrl, shift) == Edit::Changed {
                        self.search_changed();
                    }
                }
            }
            self.dirty = true;
            return Vec::new();
        }
        let page = self.h - TOOLBAR_H - 60.0;
        match key {
            K::Escape if self.confirm.is_some() => {
                self.confirm = None;
                self.dirty = true;
            }
            K::Escape | K::BackSpace => self.back(),
            K::Up => self.scroll_by(-48.0),
            K::Down => self.scroll_by(48.0),
            K::Page_Up => self.scroll_by(-page),
            K::Page_Down | K::space => self.scroll_by(page),
            K::Home => self.scroll_by(-self.scroll),
            K::End => self.scroll_by(self.content_h),
            _ => {
                // Typing anywhere searches
                if let Some(t) = utf8.filter(|t| !ctrl && !alt && !t.is_empty() && !t.chars().any(char::is_control)) {
                    self.search_focus = true;
                    self.search.clear();
                    self.search.insert(t);
                    self.search_changed();
                    self.dirty = true;
                }
            }
        }
        Vec::new()
    }

    // ---------- painting ----------

    fn zone(&mut self, hit: Hit, x: f32, y: f32, w: f32, h: f32) {
        self.zones.push((hit, (x, y, w, h)));
    }

    /// A zone in the scrolling content: only its part below the toolbar is clickable
    fn content_zone(&mut self, hit: Hit, x: f32, y: f32, w: f32, h: f32) {
        let top = y.max(TOOLBAR_H);
        let bottom = (y + h).min(self.h);
        if bottom > top {
            self.zone(hit, x, top, w, bottom - top);
        }
    }

    pub fn paint(&mut self, pm: &mut PixmapMut) {
        self.dirty = false;
        self.zones.clear();
        pm.fill(colors::window());
        self.zone(Hit::Content, SIDEBAR_W, TOOLBAR_H, self.w - SIDEBAR_W, self.h - TOOLBAR_H);
        if let Some(i) = self.detail {
            self.paint_detail(pm, i);
        } else {
            self.paint_list(pm);
        }
        self.paint_notice(pm);
        self.paint_toolbar(pm);
        self.paint_sidebar(pm);
    }

    fn content_x(&self) -> (f32, f32) {
        (SIDEBAR_W + PAD - 8.0, self.w - SIDEBAR_W - 2.0 * PAD + 8.0)
    }

    fn paint_list(&mut self, pm: &mut PixmapMut) {
        let (x0, inner_w) = self.content_x();
        let mut y = TOOLBAR_H + 2.0 - self.scroll;
        let title = match self.page {
            Page::Search => format!("{} «{}»", tr("Поиск:"), self.search.text().trim()),
            page => tr(page.title()).into_owned(),
        };
        let title = self.fonts.ellipsize_raw(&title, inner_w, 26.0, true);
        self.fonts.draw_raw(pm, &title, x0, y, 26.0, colors::text(), true);
        y += 50.0;
        let Some(catalog) = &self.catalog else {
            self.content_h = 0.0;
            self.centered_message(pm, "Загрузка каталога…", None);
            return;
        };
        if catalog.apps.is_empty() {
            self.content_h = 0.0;
            self.centered_message(pm, "Каталог программ пуст", Some("Он появится после обновления списков программ: sudo apt update (нужен пакет appstream)"));
            return;
        }
        let count = catalog.apps.len();
        if self.page == Page::Home {
            self.paint_banner(pm, x0, y, inner_w, count);
            y += BANNER_H + 30.0;
            for (heading, ids) in [("Выбор редакции", &EDITORS_CHOICE[..]), ("Игры", &GAMES[..])] {
                let list = self.picks(ids);
                if list.is_empty() {
                    continue;
                }
                self.fonts.draw_text(pm, heading, x0, y, 19.0, colors::text(), true);
                y += 36.0;
                y = self.paint_grid(pm, &list, x0, y, inner_w) + 22.0;
            }
        } else if self.results.is_empty() {
            let text = match self.page {
                Page::Search => "Ничего не найдено",
                Page::Installed => "Нет установленных программ из каталога",
                _ => "В этом разделе пока нет программ",
            };
            self.fonts.draw_text(pm, text, x0, y + 8.0, 14.0, colors::secondary(), false);
            y += 40.0;
        } else {
            let list = self.results.clone();
            y = self.paint_grid(pm, &list, x0, y, inner_w);
        }
        self.content_h = y + self.scroll + PAD;
    }

    fn centered_message(&mut self, pm: &mut PixmapMut, title: &str, details: Option<&str>) {
        let cx = SIDEBAR_W + (self.w - SIDEBAR_W) / 2.0;
        let cy = self.h / 2.0 - 20.0;
        self.fonts.draw_text_centered(pm, title, cx, cy, 15.0, colors::secondary(), true);
        if let Some(details) = details {
            let width = (self.w - SIDEBAR_W - 2.0 * PAD).min(520.0);
            let mut y = cy + 28.0;
            for line in wrap(&mut self.fonts, &tr(details), width, 13.0, 3) {
                self.fonts.draw_text_centered(pm, &line, cx, y, 13.0, colors::tertiary(), false);
                y += 19.0;
            }
        }
    }

    fn paint_banner(&mut self, pm: &mut PixmapMut, x: f32, y: f32, w: f32, count: usize) {
        fill_rrect_gradient(pm, x, y, w, BANNER_H, 18.0, rgba(64, 140, 255, 255), rgba(120, 72, 230, 255));
        // Soft light circles, as on the app icon
        fill_circle(pm, x + w - 90.0, y + 30.0, 70.0, rgba(255, 255, 255, 26));
        fill_circle(pm, x + w - 200.0, y + BANNER_H - 10.0, 46.0, rgba(255, 255, 255, 18));
        draw_bag(pm, x + w - 150.0, y + 30.0, 96.0);
        let text_w = w - 220.0;
        self.fonts.draw_text(pm, "Подборка Пёрышка", x + 28.0, y + 30.0, 28.0, Color::WHITE, true);
        let mut ty = y + 74.0;
        for line in wrap(&mut self.fonts, &tr("Проверенные программы для работы, творчества и отдыха. Установка в один щелчок, обновления вместе с системой."), text_w, 14.0, 2) {
            self.fonts.draw_raw(pm, &line, x + 28.0, ty, 14.0, rgba(255, 255, 255, 230), false);
            ty += 20.0;
        }
        let label = format!("{} {count}", tr("Программ в каталоге:"));
        self.fonts.draw_raw(pm, &label, x + 28.0, y + BANNER_H - 30.0, 12.0, rgba(255, 255, 255, 190), false);
    }

    /// Cards in as many columns as fit; returns the y below them
    fn paint_grid(&mut self, pm: &mut PixmapMut, list: &[usize], x0: f32, y0: f32, inner_w: f32) -> f32 {
        let cols = (((inner_w + GAP) / (CARD_MIN_W + GAP)).floor() as usize).max(1);
        let card_w = (inner_w - GAP * (cols - 1) as f32) / cols as f32;
        let rows = list.len().div_ceil(cols);
        for (k, &i) in list.iter().enumerate() {
            let (col, row) = (k % cols, k / cols);
            let (x, y) = (x0 + col as f32 * (card_w + GAP), y0 + row as f32 * (CARD_H + GAP));
            if y + CARD_H < TOOLBAR_H || y > self.h {
                continue;
            }
            self.paint_card(pm, i, x, y, card_w);
        }
        y0 + rows as f32 * (CARD_H + GAP) - GAP
    }

    fn paint_card(&mut self, pm: &mut PixmapMut, i: usize, x: f32, y: f32, w: f32) {
        let Some(app) = self.app(i).cloned() else { return };
        let dark = feather_ui::scale::appearance() == feather_ui::scale::Appearance::Dark;
        let hovered = matches!(self.hover, Hit::Card(h) | Hit::CardButton(h) if h == i);
        let bg = match (dark, hovered) {
            (true, false) => rgba(255, 255, 255, 12),
            (true, true) => rgba(255, 255, 255, 22),
            (false, false) => rgba(0, 0, 0, 9),
            (false, true) => rgba(0, 0, 0, 17),
        };
        fill_rrect(pm, x, y, w, CARD_H, 14.0, bg);
        self.content_zone(Hit::Card(i), x, y, w, CARD_H);
        self.paint_app_icon(pm, i, &app, x + 14.0, y + (CARD_H - CARD_ICON) / 2.0, CARD_ICON);

        let label = self.card_label(i);
        let bw = (self.fonts.measure_raw(&label, 12.5, true) + 28.0).max(92.0);
        let (bx, by, bh) = (x + w - 14.0 - bw, y + (CARD_H - 28.0) / 2.0, 28.0);
        self.paint_pill(pm, i, &label, bx, by, bw, bh);
        self.content_zone(Hit::CardButton(i), bx, by, bw, bh);

        let tx = x + 14.0 + CARD_ICON + 14.0;
        let tw = (bx - 12.0 - tx).max(40.0);
        let name = self.fonts.ellipsize_raw(&app.name, tw, 14.0, true);
        self.fonts.draw_raw(pm, &name, tx, y + 17.0, 14.0, colors::text(), true);
        let mut ly = y + 39.0;
        for line in wrap(&mut self.fonts, &app.summary, tw, 12.5, 2) {
            self.fonts.draw_raw(pm, &line, tx, ly, 12.5, colors::secondary(), false);
            ly += 17.0;
        }
    }

    fn card_label(&self, i: usize) -> String {
        match &self.task {
            Some(t) if t.app == i => match t.fraction {
                Some(f) => format!("{} %", (f * 100.0).round() as u32),
                None => "…".into(),
            },
            _ if self.installed(i) => tr("Открыть").into_owned(),
            _ => tr("Установить").into_owned(),
        }
    }

    /// The App Store style button of a card: tinted, with the progress inside while it runs
    fn paint_pill(&mut self, pm: &mut PixmapMut, i: usize, label: &str, x: f32, y: f32, w: f32, h: f32) {
        let accent = colors::accent();
        let running = self.task.as_ref().filter(|t| t.app == i);
        let blocked = self.task.is_some() && running.is_none() && !self.installed(i);
        let hovered = self.hover == Hit::CardButton(i);
        let pressed = hovered && self.pressed == Some(Hit::CardButton(i));
        let tint = |a: f32| Color::from_rgba(accent.red(), accent.green(), accent.blue(), a).unwrap_or(accent);
        let alpha = if blocked { 0.07 } else if pressed { 0.28 } else if hovered { 0.22 } else { 0.15 };
        fill_rrect(pm, x, y, w, h, h / 2.0, tint(alpha));
        if let Some(t) = running {
            let f = t.fraction.unwrap_or(0.0);
            if f > 0.0 {
                fill_rrect(pm, x, y, (w * f).max(h), h, h / 2.0, tint(0.32));
            }
        }
        let color = if blocked { tint(0.45) } else { accent };
        self.fonts.draw_text_centered(pm, label, x + w / 2.0, y + (h - 13.0) / 2.0 - 0.5, 12.5, color, true);
    }

    fn paint_app_icon(&mut self, pm: &mut PixmapMut, i: usize, app: &AppInfo, x: f32, y: f32, size: f32) {
        match self.icons.get(&i) {
            Some(Some(icon)) => {
                let s = feather_ui::scale::ui_scale();
                let k = size * s / icon.width().max(1) as f32;
                let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
                pm.draw_pixmap(0, 0, icon.as_ref(), &paint, tiny_skia::Transform::from_row(k, 0.0, 0.0, k, x * s, y * s), None);
            }
            loaded => {
                if loaded.is_none() && !self.requested.contains(&i) {
                    if let Some(path) = &app.icon {
                        self.requested.insert(i);
                        self.icon_queue.push((i, path.clone()));
                    }
                }
                self.paint_letter_icon(pm, &app.name, x, y, size);
            }
        }
    }

    /// A program without an icon: its first letter on a tile coloured by its name
    fn paint_letter_icon(&mut self, pm: &mut PixmapMut, name: &str, x: f32, y: f32, size: f32) {
        const TILES: [((u8, u8, u8), (u8, u8, u8)); 5] = [
            ((110, 196, 255), (20, 110, 236)),
            ((255, 170, 90), (238, 96, 40)),
            ((140, 220, 120), (40, 160, 70)),
            ((200, 150, 255), (120, 70, 220)),
            ((255, 130, 160), (220, 50, 90)),
        ];
        let hash = name.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
        let ((r0, g0, b0), (r1, g1, b1)) = TILES[hash % TILES.len()];
        let (inset, s) = (size * 0.06, size * 0.88);
        fill_rrect_gradient(pm, x + inset, y + inset, s, s, s * 0.225, rgba(r0, g0, b0, 255), rgba(r1, g1, b1, 255));
        let letter: String = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_default();
        let fs = size * 0.42;
        self.fonts.draw_text_centered(pm, &letter, x + size / 2.0, y + (size - fs) / 2.0 - fs * 0.08, fs, Color::WHITE, true);
    }

    fn paint_detail(&mut self, pm: &mut PixmapMut, i: usize) {
        let Some(app) = self.app(i).cloned() else { return };
        let installed = self.installed(i);
        let (x0, inner_w) = self.content_x();
        let right = x0 + inner_w;
        let top = TOOLBAR_H + 14.0 - self.scroll;
        self.paint_app_icon(pm, i, &app, x0, top, DETAIL_ICON);

        // Buttons (or progress) on the right of the header
        let mut buttons_left;
        let running = self.task.as_ref().filter(|t| t.app == i).cloned();
        if let Some(task) = running {
            let bw = 240.0f32.min(inner_w * 0.4);
            let bx = right - bw;
            let message = self.fonts.ellipsize_raw(&task.message, bw, 12.5, false);
            self.fonts.draw_raw(pm, &message, bx, top + 10.0, 12.5, colors::secondary(), false);
            let dark = feather_ui::scale::appearance() == feather_ui::scale::Appearance::Dark;
            let track = if dark { rgba(255, 255, 255, 30) } else { rgba(0, 0, 0, 22) };
            fill_rrect(pm, bx, top + 34.0, bw, 6.0, 3.0, track);
            if let Some(f) = task.fraction.filter(|f| *f > 0.0) {
                fill_rrect(pm, bx, top + 34.0, (bw * f).max(6.0), 6.0, 3.0, colors::accent());
            }
            buttons_left = bx;
        } else {
            let primary = if installed { tr("Открыть") } else { tr("Установить") }.into_owned();
            let blocked = self.task.is_some() && !installed;
            let pw = (self.fonts.measure_raw(&primary, 13.0, true) + 36.0).max(116.0);
            let px = right - pw;
            self.paint_button(pm, Hit::Primary, &primary, px, top + 8.0, pw, true, blocked);
            buttons_left = px;
            if installed {
                let label = tr("Удалить").into_owned();
                let rw = (self.fonts.measure_raw(&label, 13.0, false) + 32.0).max(96.0);
                let rx = px - 10.0 - rw;
                self.paint_button(pm, Hit::Remove, &label, rx, top + 8.0, rw, false, self.task.is_some());
                buttons_left = rx;
            }
        }

        let tx = x0 + DETAIL_ICON + 24.0;
        let tw = (buttons_left - 20.0 - tx).max(80.0);
        let name = self.fonts.ellipsize_raw(&app.name, tw, 26.0, true);
        self.fonts.draw_raw(pm, &name, tx, top + 6.0, 26.0, colors::text(), true);
        let mut y = top + 46.0;
        for line in wrap(&mut self.fonts, &app.summary, tw, 14.0, 2) {
            self.fonts.draw_raw(pm, &line, tx, y, 14.0, colors::secondary(), false);
            y += 20.0;
        }
        let by = if app.developer.is_empty() { app.package.clone() } else { app.developer.clone() };
        let by = self.fonts.ellipsize_raw(&by, tw, 12.5, false);
        self.fonts.draw_raw(pm, &by, tx, y + 4.0, 12.5, colors::tertiary(), false);

        let mut y = top + DETAIL_ICON + 26.0;
        if let Some((_, others)) = self.confirm.clone().filter(|(a, _)| *a == i) {
            y = self.paint_confirm(pm, &app, &others, x0, y, inner_w) + 20.0;
        }
        fill_rect(pm, x0, y, inner_w, 1.0, colors::separator());
        y += 22.0;

        let text_w = inner_w.min(760.0);
        self.fonts.draw_text(pm, "Описание", x0, y, 18.0, colors::text(), true);
        y += 34.0;
        let description = if app.description.is_empty() { vec![app.summary.clone()] } else { app.description.clone() };
        for paragraph in &description {
            let (indent, size) = if paragraph.starts_with("• ") { (14.0, 14.0) } else { (0.0, 14.0) };
            for line in wrap(&mut self.fonts, paragraph, text_w - indent, size, usize::MAX) {
                if y > TOOLBAR_H - 30.0 && y < self.h {
                    self.fonts.draw_raw(pm, &line, x0 + indent, y, size, colors::text(), false);
                }
                y += 21.0;
            }
            y += 8.0;
        }
        y += 18.0;
        self.fonts.draw_text(pm, "Сведения", x0, y, 18.0, colors::text(), true);
        y += 36.0;
        let categories: Vec<String> = CATEGORIES.iter().filter(|(_, keys)| app.categories.iter().any(|c| keys.contains(&c.as_str()))).map(|(label, _)| tr(label).into_owned()).collect();
        let facts = [
            ("Разработчик", app.developer.clone()),
            ("Лицензия", app.license.clone()),
            ("Пакет", app.package.clone()),
            ("Разделы", categories.join(", ")),
            ("Сайт", app.homepage.clone()),
        ];
        let label_w = 150.0;
        for (label, value) in facts {
            if value.is_empty() {
                continue;
            }
            self.fonts.draw_text(pm, label, x0, y, 13.0, colors::secondary(), false);
            let value = self.fonts.ellipsize_raw(&value, text_w - label_w, 13.0, false);
            if label == "Сайт" {
                let w = self.fonts.draw_raw(pm, &value, x0 + label_w, y, 13.0, colors::accent(), false);
                if self.hover == Hit::Homepage {
                    fill_rect(pm, x0 + label_w, y + 17.0, w, 1.0, colors::accent());
                }
                self.content_zone(Hit::Homepage, x0 + label_w, y - 2.0, w, 20.0);
            } else {
                self.fonts.draw_raw(pm, &value, x0 + label_w, y, 13.0, colors::text(), false);
            }
            y += 26.0;
        }
        self.content_h = y + self.scroll + PAD;
    }

    /// The warning before a removal that takes other packages along; returns the y below it
    fn paint_confirm(&mut self, pm: &mut PixmapMut, app: &AppInfo, others: &[String], x: f32, y: f32, w: f32) -> f32 {
        let text_w = w - 48.0;
        let shown: Vec<&str> = others.iter().take(12).map(String::as_str).collect();
        let rest = if others.len() > shown.len() { format!(" {} {}", tr("и ещё"), others.len() - shown.len()) } else { String::new() };
        let text = format!("{} «{}» {} {}{rest}", tr("Вместе с"), app.name, tr("будут удалены:"), shown.join(", "));
        let lines = wrap(&mut self.fonts, &text, text_w, 13.0, 4);
        let h = 24.0 + lines.len() as f32 * 19.0 + 14.0 + 30.0 + 18.0;
        fill_rrect(pm, x, y, w, h, 12.0, rgba(236, 140, 20, 34));
        let mut ty = y + 18.0;
        for line in lines {
            self.fonts.draw_raw(pm, &line, x + 24.0, ty, 13.0, colors::text(), false);
            ty += 19.0;
        }
        let by = ty + 14.0;
        let yes = tr("Удалить всё").into_owned();
        let no = tr("Отмена").into_owned();
        let yw = (self.fonts.measure_raw(&yes, 13.0, true) + 32.0).max(110.0);
        let nw = (self.fonts.measure_raw(&no, 13.0, false) + 32.0).max(96.0);
        self.paint_button(pm, Hit::Confirm, &yes, x + 24.0, by, yw, true, false);
        self.paint_button(pm, Hit::CancelConfirm, &no, x + 24.0 + yw + 10.0, by, nw, false, false);
        y + h
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_button(&mut self, pm: &mut PixmapMut, hit: Hit, label: &str, x: f32, y: f32, w: f32, primary: bool, disabled: bool) {
        let h = 32.0;
        let dark = feather_ui::scale::appearance() == feather_ui::scale::Appearance::Dark;
        let hovered = self.hover == hit && !disabled;
        let pressed = hovered && self.pressed == Some(hit);
        let destructive = hit == Hit::Confirm;
        if primary {
            let base = if destructive { rgba(226, 64, 56, 255) } else { colors::accent() };
            let k = if pressed { 0.82 } else if hovered { 0.92 } else { 1.0 };
            let mut fill = shade(base, k);
            if disabled {
                fill.set_alpha(0.4);
            }
            fill_rrect(pm, x, y, w, h, h / 2.0, fill);
            self.fonts.draw_text_centered(pm, label, x + w / 2.0, y + 8.0, 13.0, Color::WHITE, true);
        } else {
            let a = if pressed { 40 } else if hovered { 30 } else { 18 };
            let fill = if dark { rgba(255, 255, 255, a + 8) } else { rgba(0, 0, 0, a) };
            fill_rrect(pm, x, y, w, h, h / 2.0, fill);
            let ink = if disabled { colors::tertiary() } else if hit == Hit::Remove { rgba(226, 64, 56, 255) } else { colors::text() };
            self.fonts.draw_text_centered(pm, label, x + w / 2.0, y + 8.0, 13.0, ink, false);
        }
        if !disabled {
            self.content_zone(hit, x, y, w, h);
        }
    }

    fn paint_notice(&mut self, pm: &mut PixmapMut) {
        let Some((text, error)) = self.notice.clone() else { return };
        let (x0, inner_w) = self.content_x();
        let w = inner_w.min(640.0);
        let x = x0 + (inner_w - w) / 2.0;
        let lines = wrap(&mut self.fonts, &text, w - 64.0, 13.0, 3);
        let h = 24.0 + lines.len() as f32 * 19.0;
        let y = self.h - 20.0 - h;
        let dark = feather_ui::scale::appearance() == feather_ui::scale::Appearance::Dark;
        let bg = if dark { rgba(58, 58, 64, 250) } else { rgba(255, 255, 255, 252) };
        fill_rrect(pm, x, y + 2.0, w, h, 12.0, rgba(0, 0, 0, 30));
        fill_rrect(pm, x, y, w, h, 12.0, bg);
        stroke_rrect(pm, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 12.0, 1.0, colors::separator());
        let ink = if error { rgba(220, 60, 52, 255) } else { colors::text() };
        let mut ty = y + 12.0;
        for line in lines {
            self.fonts.draw_raw(pm, &line, x + 18.0, ty, 13.0, ink, false);
            ty += 19.0;
        }
        // ×
        let (cx, cy) = (x + w - 22.0, y + h / 2.0);
        if self.hover == Hit::NoticeClose {
            fill_circle(pm, cx, cy, 11.0, colors::hover());
        }
        let c = colors::secondary();
        line(pm, cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, 1.4, c);
        line(pm, cx + 4.0, cy - 4.0, cx - 4.0, cy + 4.0, 1.4, c);
        self.zone(Hit::Content, x, y, w, h);
        self.zone(Hit::NoticeClose, cx - 14.0, cy - 14.0, 28.0, 28.0);
    }

    fn paint_toolbar(&mut self, pm: &mut PixmapMut) {
        fill_rect(pm, SIDEBAR_W, 0.0, self.w - SIDEBAR_W, TOOLBAR_H, colors::window());
        self.zone(Hit::Toolbar, SIDEBAR_W, 0.0, self.w - SIDEBAR_W, TOOLBAR_H);
        if self.scroll > 0.0 {
            fill_rect(pm, SIDEBAR_W, TOOLBAR_H - 1.0, self.w - SIDEBAR_W, 1.0, colors::separator());
        }
        let (x0, inner_w) = self.content_x();
        let mut tx = x0;
        if self.detail.is_some() {
            let (bx, by, bw, bh) = (x0 - 6.0, 12.0, 32.0, 28.0);
            if self.hover == Hit::Back {
                fill_rrect(pm, bx, by, bw, bh, 8.0, colors::hover());
            }
            let (cx, cy) = (bx + bw / 2.0, by + bh / 2.0);
            polyline(pm, &[(cx + 3.0, cy - 7.0), (cx - 4.0, cy), (cx + 3.0, cy + 7.0)], 1.8, colors::secondary());
            self.zone(Hit::Back, bx, by, bw, bh);
            tx = bx + bw + 8.0;
        }
        // The title joins the toolbar once the big one scrolled away (always on a program's page)
        let title = match self.detail.and_then(|i| self.app(i)) {
            Some(app) => Some(app.name.clone()),
            None if self.scroll > 40.0 => Some(match self.page {
                Page::Search => tr("Поиск").into_owned(),
                page => tr(page.title()).into_owned(),
            }),
            None => None,
        };
        if let Some(title) = title {
            let title = self.fonts.ellipsize_raw(&title, inner_w - (tx - x0), 14.0, true);
            self.fonts.draw_raw(pm, &title, tx, 17.0, 14.0, colors::text(), true);
        }
    }

    fn paint_sidebar(&mut self, pm: &mut PixmapMut) {
        fill_rrect(pm, 8.0, 8.0, SIDEBAR_W - 16.0, self.h - 16.0, 14.0, colors::sidebar());
        stroke_rrect(pm, 8.5, 8.5, SIDEBAR_W - 17.0, self.h - 17.0, 13.5, 1.0, colors::sidebar_edge());
        self.zone(Hit::Toolbar, 0.0, 0.0, SIDEBAR_W, self.h);
        self.paint_traffic(pm);

        let (sx, sy, sw, sh) = search_rect();
        fill_rrect(pm, sx, sy, sw, sh, 15.0, colors::control());
        if self.search_focus {
            let a = colors::accent();
            let ring = Color::from_rgba(a.red(), a.green(), a.blue(), 0.55).unwrap_or(a);
            stroke_rrect(pm, sx - 1.0, sy - 1.0, sw + 2.0, sh + 2.0, 16.0, 2.5, ring);
        }
        let ink = colors::secondary();
        stroke_circle(pm, sx + 16.0, sy + 14.0, 4.5, 1.5, ink);
        line(pm, sx + 19.5, sy + 17.5, sx + 22.5, sy + 20.5, 1.6, ink);
        if self.search.is_empty() && !self.search_focus {
            self.fonts.draw_text(pm, "Поиск программ", sx + 30.0, sy + 7.5, 13.0, colors::tertiary(), false);
        } else {
            self.search.draw(pm, &mut self.fonts, sx + 30.0, sy + 7.5, sw - 56.0, 13.0, false, colors::text(), self.search_focus, colors::accent());
        }
        self.zone(Hit::Search, sx, sy, sw, sh);
        if !self.search.is_empty() {
            let (cx, cy) = (sx + sw - 15.0, sy + sh / 2.0);
            fill_circle(pm, cx, cy, 7.0, colors::tertiary());
            line(pm, cx - 2.5, cy - 2.5, cx + 2.5, cy + 2.5, 1.4, colors::sidebar());
            line(pm, cx + 2.5, cy - 2.5, cx - 2.5, cy + 2.5, 1.4, colors::sidebar());
            self.zone(Hit::SearchClear, cx - 11.0, cy - 11.0, 22.0, 22.0);
        }

        let mut y = NAV_TOP;
        for page in nav_pages() {
            if page == Page::Installed {
                fill_rect(pm, 28.0, y + 5.0, SIDEBAR_W - 56.0, 1.0, colors::separator());
                y += 12.0;
            }
            let selected = self.page == page && self.page != Page::Search;
            let (rx, rw) = (16.0, SIDEBAR_W - 32.0);
            if selected {
                fill_rrect(pm, rx, y + 1.0, rw, NAV_ROW - 2.0, 8.0, colors::selected_gray());
            } else if self.hover == Hit::Nav(page) {
                fill_rrect(pm, rx, y + 1.0, rw, NAV_ROW - 2.0, 8.0, colors::hover());
            }
            let color = if selected { colors::accent() } else { colors::secondary() };
            draw_nav_glyph(pm, page, rx + 20.0, y + NAV_ROW / 2.0, color);
            self.fonts.draw_text(pm, page.title(), rx + 40.0, y + 8.0, 13.0, colors::text(), selected);
            self.zone(Hit::Nav(page), rx, y, rw, NAV_ROW);
            y += NAV_ROW;
        }
    }

    fn paint_traffic(&mut self, pm: &mut PixmapMut) {
        let hovered = matches!(self.hover, Hit::Close | Hit::Minimize | Hit::Zoom);
        for (hit, cx) in [(Hit::Close, 26.0), (Hit::Minimize, 46.0), (Hit::Zoom, 66.0)] {
            let color = if !self.focused && !hovered {
                colors::traffic_inactive()
            } else {
                match hit {
                    Hit::Close => rgba(255, 95, 87, 255),
                    Hit::Minimize => rgba(254, 188, 46, 255),
                    _ => rgba(40, 200, 64, 255),
                }
            };
            fill_circle(pm, cx, 26.0, 6.0, color);
            if hovered {
                let ink = rgba(0, 0, 0, 140);
                match hit {
                    Hit::Close => {
                        line(pm, cx - 2.5, 23.5, cx + 2.5, 28.5, 1.2, ink);
                        line(pm, cx + 2.5, 23.5, cx - 2.5, 28.5, 1.2, ink);
                    }
                    Hit::Minimize => line(pm, cx - 3.0, 26.0, cx + 3.0, 26.0, 1.2, ink),
                    _ => {
                        line(pm, cx - 3.0, 26.0, cx + 3.0, 26.0, 1.2, ink);
                        line(pm, cx, 23.0, cx, 29.0, 1.2, ink);
                    }
                }
            }
            self.zone(hit, cx - 8.0, 18.0, 16.0, 16.0);
        }
    }

    #[cfg(test)]
    fn click(&mut self, hit: Hit) -> Vec<Effect> {
        let (_, (x, y, w, h)) = *self.zones.iter().rev().find(|(z, _)| *z == hit).unwrap_or_else(|| panic!("no zone {hit:?}"));
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        self.motion(cx, cy);
        let mut effects = self.press(BTN_LEFT, cx, cy, false);
        effects.extend(self.release(BTN_LEFT, cx, cy));
        effects
    }
}

fn search_rect() -> (f32, f32, f32, f32) {
    (20.0, 50.0, SIDEBAR_W - 40.0, 30.0)
}

/// Programs matching every word of the query, best first: the name before keywords and summary
pub fn search(apps: &[AppInfo], query: &str) -> Vec<usize> {
    let terms: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(u32, usize)> = apps
        .iter()
        .enumerate()
        .filter_map(|(i, app)| {
            let mut score = 0;
            for term in &terms {
                score += if app.search_name.starts_with(term.as_str()) {
                    40
                } else if app.search_name.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(term.as_str())) {
                    25
                } else if app.search_name.contains(term.as_str()) {
                    15
                } else if app.search_keywords.contains(term.as_str()) {
                    5
                } else {
                    return None;
                };
            }
            Some((score, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

fn shade(c: Color, k: f32) -> Color {
    Color::from_rgba(c.red() * k, c.green() * k, c.blue() * k, c.alpha()).unwrap_or(c)
}

/// Word wrap without UI translation (names and descriptions of programs); at most
/// `max_lines`, the last one ending in an ellipsis when text remains
pub fn wrap(fonts: &mut FontSystem, text: &str, width: f32, size: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut truncated = false;
    'paragraphs: for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
            if current.is_empty() || fonts.measure_raw(&candidate, size, false) <= width {
                current = candidate;
                continue;
            }
            if lines.len() + 1 >= max_lines {
                lines.push(current);
                truncated = true;
                break 'paragraphs;
            }
            lines.push(std::mem::replace(&mut current, word.to_string()));
        }
        if !current.is_empty() {
            lines.push(current);
        }
        if lines.len() >= max_lines {
            break;
        }
    }
    if truncated {
        if let Some(last) = lines.last_mut() {
            *last = fonts.ellipsize_raw(&format!("{last} …"), width, size, false);
            if !last.ends_with('…') {
                last.push('…');
            }
        }
    }
    for line in &mut lines {
        if fonts.measure_raw(line, size, false) > width {
            *line = fonts.ellipsize_raw(line, width, size, false);
        }
    }
    lines
}

/// The store's mark: a shopping bag with a feather-white handle
pub fn draw_bag(pm: &mut PixmapMut, x: f32, y: f32, s: f32) {
    stroke_circle(pm, x + s * 0.5, y + s * 0.3, s * 0.17, s * 0.06, rgba(255, 255, 255, 235));
    fill_rrect(pm, x + s * 0.16, y + s * 0.34, s * 0.68, s * 0.56, s * 0.1, rgba(255, 255, 255, 245));
    fill_rrect_gradient(pm, x + s * 0.22, y + s * 0.42, s * 0.56, s * 0.12, s * 0.04, rgba(110, 170, 255, 120), rgba(140, 110, 240, 60));
}

/// Small outline glyphs of the sidebar sections
fn draw_nav_glyph(pm: &mut PixmapMut, page: Page, cx: f32, cy: f32, c: Color) {
    let w = 1.4;
    match page {
        Page::Home | Page::Search => {
            let pts: Vec<(f32, f32)> = (0..=10)
                .map(|k| {
                    let r = if k % 2 == 0 { 7.0 } else { 3.2 };
                    let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
                    (cx + r * a.cos(), cy + r * a.sin())
                })
                .collect();
            polyline(pm, &pts, w, c);
        }
        Page::Category(0) => {
            stroke_circle(pm, cx, cy, 6.5, w, c);
            line(pm, cx - 6.5, cy, cx + 6.5, cy, w, c);
            polyline(pm, &[(cx, cy - 6.5), (cx - 3.0, cy - 2.0), (cx - 3.0, cy + 2.0), (cx, cy + 6.5)], w, c);
            polyline(pm, &[(cx, cy - 6.5), (cx + 3.0, cy - 2.0), (cx + 3.0, cy + 2.0), (cx, cy + 6.5)], w, c);
        }
        Page::Category(1) => {
            polyline(pm, &[(cx + 1.5, cy - 7.0), (cx - 5.5, cy - 7.0), (cx - 5.5, cy + 7.0), (cx + 5.5, cy + 7.0), (cx + 5.5, cy - 3.0), (cx + 1.5, cy - 7.0)], w, c);
            line(pm, cx - 3.0, cy, cx + 3.0, cy, w, c);
            line(pm, cx - 3.0, cy + 3.5, cx + 3.0, cy + 3.5, w, c);
        }
        Page::Category(2) => {
            stroke_rrect(pm, cx - 7.0, cy - 5.5, 14.0, 11.0, 2.0, w, c);
            polyline(pm, &[(cx - 7.0, cy + 4.0), (cx - 2.0, cy - 1.0), (cx + 1.5, cy + 2.5), (cx + 3.5, cy + 0.5), (cx + 7.0, cy + 4.0)], w, c);
            fill_circle(pm, cx + 3.0, cy - 2.5, 1.3, c);
        }
        Page::Category(3) => {
            stroke_rrect(pm, cx - 7.0, cy - 5.5, 14.0, 11.0, 2.5, w, c);
            polyline(pm, &[(cx - 2.0, cy - 3.0), (cx + 3.0, cy), (cx - 2.0, cy + 3.0), (cx - 2.0, cy - 3.0)], w, c);
        }
        Page::Category(4) => {
            stroke_rrect(pm, cx - 7.5, cy - 4.5, 15.0, 9.0, 4.5, w, c);
            line(pm, cx - 5.0, cy, cx - 2.0, cy, w, c);
            line(pm, cx - 3.5, cy - 1.5, cx - 3.5, cy + 1.5, w, c);
            fill_circle(pm, cx + 3.0, cy - 1.0, 1.0, c);
            fill_circle(pm, cx + 5.0, cy + 1.0, 1.0, c);
        }
        Page::Category(5) => {
            polyline(pm, &[(cx - 3.0, cy - 5.0), (cx - 7.0, cy), (cx - 3.0, cy + 5.0)], w, c);
            polyline(pm, &[(cx + 3.0, cy - 5.0), (cx + 7.0, cy), (cx + 3.0, cy + 5.0)], w, c);
            line(pm, cx + 1.5, cy - 6.0, cx - 1.5, cy + 6.0, w, c);
        }
        Page::Category(6) => {
            polyline(pm, &[(cx - 7.0, cy - 2.0), (cx, cy - 5.5), (cx + 7.0, cy - 2.0), (cx, cy + 1.5), (cx - 7.0, cy - 2.0)], w, c);
            polyline(pm, &[(cx - 4.0, cy), (cx - 4.0, cy + 4.0), (cx, cy + 6.0), (cx + 4.0, cy + 4.0), (cx + 4.0, cy)], w, c);
        }
        Page::Category(7) => {
            stroke_circle(pm, cx + 2.5, cy - 2.5, 3.5, w, c);
            line(pm, cx, cy, cx - 6.0, cy + 6.0, w * 1.4, c);
        }
        Page::Category(_) => {
            stroke_circle(pm, cx, cy, 3.0, w, c);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                line(pm, cx + 5.0 * a.cos(), cy + 5.0 * a.sin(), cx + 7.0 * a.cos(), cy + 7.0 * a.sin(), w * 1.3, c);
            }
        }
        Page::Installed => {
            stroke_circle(pm, cx, cy, 7.0, w, c);
            polyline(pm, &[(cx - 3.0, cy), (cx - 0.5, cy + 2.5), (cx + 3.5, cy - 2.5)], w, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, package: &str, name: &str, summary: &str, categories: &[&str]) -> AppInfo {
        AppInfo {
            id: id.into(),
            package: package.into(),
            name: name.into(),
            summary: summary.into(),
            description: vec!["A long description of the program that wraps over several lines in the page of the program.".into(), "• First feature".into()],
            categories: categories.iter().map(|c| c.to_string()).collect(),
            homepage: "https://example.org".into(),
            developer: "Example team".into(),
            license: "GPL-3.0+".into(),
            search_name: format!("{} {}", name.to_lowercase(), package),
            search_keywords: summary.to_lowercase(),
            ..Default::default()
        }
    }

    fn sample() -> App {
        let apps = vec![
            info("org.gimp.GIMP", "gimp", "GIMP", "Редактор изображений", &["Graphics"]),
            info("org.inkscape.Inkscape", "inkscape", "Inkscape", "Редактор векторной графики", &["Graphics"]),
            info("org.videolan.vlc", "vlc", "VLC", "Медиапроигрыватель", &["AudioVideo"]),
            info("net.supertuxkart.SuperTuxKart", "supertuxkart", "SuperTuxKart", "Гонки на картах", &["Game"]),
        ];
        let mut app = App::new(FontSystem::new());
        app.loaded(Catalog { apps, installed: ["vlc".to_string()].into_iter().collect() });
        app
    }

    fn painted(app: &mut App) {
        let mut pixmap = Pixmap::new(app.w as u32, app.h as u32).unwrap();
        app.paint(&mut pixmap.as_mut());
    }

    #[test]
    fn search_ranks_names_first_and_needs_every_word() {
        let app = sample();
        let apps = &app.catalog.as_ref().unwrap().apps;
        assert_eq!(search(apps, "редактор"), [0, 1]);
        assert_eq!(search(apps, "ink"), [1]);
        assert_eq!(search(apps, "редактор вектор"), [1]);
        assert!(search(apps, "  ").is_empty());
    }

    #[test]
    fn navigation_search_and_details() {
        let mut app = sample();
        painted(&mut app);
        app.click(Hit::Nav(Page::Category(2)));
        assert_eq!(app.results, [0, 1]);
        painted(&mut app);
        app.click(Hit::Card(1));
        assert_eq!(app.detail, Some(1));
        painted(&mut app);
        app.click(Hit::Back);
        assert_eq!((app.detail, app.page), (None, Page::Category(2)));
        // Typing anywhere searches; Escape returns to the section
        app.key(0x76, 0, Some("v"), false, false, false);
        assert_eq!((app.page, app.search_focus), (Page::Search, true));
        app.key(0x6c, 0, Some("l"), false, false, false);
        assert_eq!(app.results, [2]);
        app.key(0xff1b, 0, None, false, false, false);
        assert_eq!((app.page, app.search.text()), (Page::Category(2), ""));
        // Installed section
        painted(&mut app);
        app.click(Hit::Nav(Page::Installed));
        assert_eq!(app.results, [2]);
    }

    #[test]
    fn install_progress_failure_and_confirmed_removal() {
        let mut app = sample();
        painted(&mut app);
        let effects = app.click(Hit::CardButton(0));
        assert_eq!(effects, [Effect::Run(Op::Install, "gimp".into(), false)]);
        // One operation at a time
        painted(&mut app);
        assert!(app.click(Hit::CardButton(1)).is_empty());
        app.task_event(Event::Progress(Some(0.4), "Загрузка…".into()));
        assert_eq!(app.card_label(0), "40 %");
        app.task_event(Event::Done);
        assert!(app.installed(0) && !app.busy());
        assert_eq!(app.card_label(0), "Открыть");

        app.open_detail(0);
        painted(&mut app);
        assert_eq!(app.click(Hit::Remove), [Effect::Run(Op::Remove, "gimp".into(), false)]);
        app.task_event(Event::Confirm(vec!["gimp-plugin".into()]));
        assert_eq!(app.confirm, Some((0, vec!["gimp-plugin".into()])));
        painted(&mut app);
        assert_eq!(app.click(Hit::Confirm), [Effect::Run(Op::Remove, "gimp".into(), true)]);
        app.task_event(Event::Done);
        assert!(!app.installed(0));

        painted(&mut app);
        app.click(Hit::Primary);
        app.task_event(Event::Failed("Недостаточно места на диске".into()));
        assert!(matches!(&app.notice, Some((t, true)) if t.contains("GIMP") && t.contains("места")));
        painted(&mut app);
        app.click(Hit::NoticeClose);
        assert!(app.notice.is_none());
    }

    #[test]
    fn wrap_keeps_to_the_width() {
        let mut fonts = FontSystem::new();
        let text = "Проверенные программы для работы, творчества и отдыха. Установка в один щелчок.";
        let lines = wrap(&mut fonts, text, 180.0, 13.0, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
        assert!(lines.iter().all(|l| fonts.measure_raw(l, 13.0, false) <= 180.5));
        assert_eq!(wrap(&mut fonts, "один", 180.0, 13.0, 2), ["один"]);
    }

    /// Screens with the real catalog: FEATHER_STORE_PREVIEW=/dir cargo test -p feather-store -- --ignored preview
    #[test]
    #[ignore]
    fn preview() {
        let dir = std::env::var("FEATHER_STORE_PREVIEW").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
        let catalog = catalog::load();
        for dark in [false, true] {
            feather_ui::scale::set_appearance(if dark { feather_ui::scale::Appearance::Dark } else { feather_ui::scale::Appearance::Light });
            let theme = if dark { "dark" } else { "light" };
            let mut app = App::new(FontSystem::new());
            app.loaded(Catalog { apps: catalog.apps.clone(), installed: catalog.installed.clone() });
            let shot = |app: &mut App, name: &str| {
                // Twice: the first paint asks for icons, loaded here synchronously
                painted(app);
                let icons = app.take_icon_queue().into_iter().map(|(i, p)| (i, Pixmap::load_png(p).ok())).collect();
                app.icons_loaded(icons);
                let mut pixmap = Pixmap::new(app.w as u32, app.h as u32).unwrap();
                app.paint(&mut pixmap.as_mut());
                pixmap.save_png(format!("{dir}/store-{name}-{theme}.png")).unwrap();
            };
            shot(&mut app, "home");
            app.go(Page::Category(2));
            shot(&mut app, "graphics");
            if let Some(&gimp) = app.by_id.get("org.gimp.GIMP") {
                app.open_detail(gimp);
                app.task = Some(Task { app: gimp, op: Op::Install, fraction: Some(0.45), message: "Загрузка…".into(), also: Vec::new() });
                shot(&mut app, "detail-progress");
                app.task = None;
                app.confirm = Some((gimp, vec!["gimp-data-extras".into(), "gimp-plugin-registry".into()]));
                app.notice = Some(("Не удалось установить «GIMP»: Не удалось скачать пакеты. Проверьте подключение к интернету".into(), true));
                shot(&mut app, "detail-confirm");
            }
            app.go(Page::Home);
            app.search.set("видео");
            app.search_changed();
            shot(&mut app, "search");
        }
    }
}
