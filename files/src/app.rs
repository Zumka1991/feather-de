//! Window contents: Finder-like sidebar, unified toolbar, icon/list views, path bar.
//! No Wayland types here: input comes in as plain events, side effects go out as `Effect`s,
//! so the whole UI also runs headless in tests.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use tiny_skia::PixmapMut;

use crate::fsops::{self, Entry, Kind};
use crate::icons::{draw_glyph, Glyph, Icon, IconCache};
use crate::text::FontSystem;
use crate::theme::{self, colors, rgba};
#[path="picker.rs"] mod picker;
use picker::PickerState;

pub const SIDEBAR_W: f32 = 220.0;
pub const TOOLBAR_H: f32 = 52.0;
pub const PATHBAR_H: f32 = 28.0;
const RESIZE_BAND: f32 = 5.0;
const CELL_W: f32 = 104.0;
const CELL_H: f32 = 104.0;
const GRID_ICON: f32 = 64.0;
const ROW_H: f32 = 26.0;
const HEADER_H: f32 = 28.0;
const PLACE_H: f32 = 28.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_BACK: u32 = 0x113;
pub const BTN_FORWARD: u32 = 0x114;

/// Requests to the Wayland/OS side
#[derive(Debug, PartialEq)]
pub enum Effect {
    List(PathBuf, u64),
    Preview(PathBuf, u64),
    Close,
    Minimize,
    ToggleMaximize,
    Move,
    /// xdg_toplevel resize edge bits: top 1, bottom 2, left 4, right 8
    Resize(u32),
    /// Copies/moves/trashes on a worker thread; the folder is re-read afterwards
    Paste(Vec<PathBuf>, PathBuf, bool),
    Trash(Vec<PathBuf>),
    /// Trash folder: put back (from .trashinfo), delete for good, empty it all
    Restore(Vec<PathBuf>),
    DeleteForever(Vec<PathBuf>),
    EmptyTrash,
    /// Gather «Свойства» of a path on a worker thread
    Inspect(PathBuf),
    /// Offer these files to other programs (text/uri-list)
    Clipboard(Vec<PathBuf>),
    /// Drag these files out (Wayland DnD: other programs, the desktop, our own folders)
    StartDrag(Vec<PathBuf>),
    Picked(feather_ui::picker::Selection),
    CheckSave(PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Icons,
    List,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Default,
    Text,
    Resize(u32),
}

#[derive(Clone, PartialEq, Debug)]
enum Hit {
    Close,
    Minimize,
    Zoom,
    Back,
    Forward,
    ViewIcons,
    ViewList,
    Search,
    Place(usize),
    Item(usize),
    Crumb(PathBuf),
    Content,
    Toolbar,
    Menu(usize),
    EmptyTrash,
    Title,
    PropsClose,
    ConfirmOk,
    ConfirmCancel,
    Nothing,
    PickerAccept,PickerCancel,PickerName,PickerFilter,PickerChoice(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MenuAction {
    Open,
    Terminal,
    Rename,
    Copy,
    Cut,
    Paste,
    Trash,
    Restore,
    DeleteForever,
    EmptyTrash,
    Properties,
    NewFolder,
    ToggleHidden,
    ShowIcons,
    ShowList,
}

/// «Свойства»: filled on a worker; the size of a folder arrives later.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Props {
    pub path: PathBuf,
    pub is_dir: bool,
    pub kind: String,
    pub size: Option<u64>,
    pub items: Option<u64>,
    /// The walk stopped early (very large folder)
    pub partial: bool,
    pub modified: Option<std::time::SystemTime>,
    pub created: Option<std::time::SystemTime>,
    pub accessed: Option<std::time::SystemTime>,
    pub mode: u32,
    pub owner: String,
}

/// Finder-style sheet before deleting anything for good.
#[derive(Clone, Debug, PartialEq)]
enum Confirm {
    Delete(Vec<PathBuf>),
    Empty,
}

struct Menu {
    x: f32,
    y: f32,
    items: Vec<(String, Option<MenuAction>)>,
}

struct Place {
    name: String,
    path: PathBuf,
    glyph: Glyph,
    section: &'static str,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

struct QuickPreview {
    path: PathBuf,
    generation: u64,
    result: Option<Result<crate::preview::Content, String>>,
    scroll: usize,
}

pub struct App {
    preview: Option<QuickPreview>,
    preview_generation: u64,
    picker:Option<PickerState>,
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
    pub focused: bool,
    dir: PathBuf,
    history: Vec<PathBuf>,
    hist_pos: usize,
    entries: Vec<Entry>,
    /// Indices into `entries` after the hidden-file and search filters
    visible: Vec<usize>,
    selected: HashSet<PathBuf>,
    /// Keyboard cursor/shift-range anchor (index into `visible`)
    cursor: Option<usize>,
    anchor: Option<usize>,
    view: View,
    scroll: f32,
    show_hidden: bool,
    search: feather_ui::input::TextInput,
    search_focus: bool,
    rename: Option<(PathBuf, feather_ui::input::TextInput)>,
    hover: Hit,
    pointer: (f32, f32),
    pressed: Option<(Hit, (f32, f32))>,
    rubber: Option<((f32, f32), (f32, f32))>,
    rubber_base: HashSet<PathBuf>,
    menu: Option<Menu>,
    confirm: Option<Confirm>,
    /// «Перейти к папке»: the path field over the title (Ctrl+L, ⇧⌘G, click on the title)
    path_edit: Option<feather_ui::input::TextInput>,
    props: Option<Props>,
    menu_hover: Option<usize>,
    clipboard: Option<(Vec<PathBuf>, bool)>,
    places: Vec<Place>,
    generation: u64,
    loading: bool,
    error: Option<String>,
    status: Option<(String, Instant)>,
    type_ahead: (String, Instant),
    last_click: Option<(Instant, Hit)>,
    /// Focus this name once the next listing arrives (new folder, paste)
    reveal: Option<PathBuf>,
    /// Start renaming the revealed item (`--rename`)
    rename_on_reveal: bool,
    /// Item pressed with the left button: moving far enough starts a drag
    drag_origin: Option<(f32, f32)>,
    /// Where a drag over the window would drop (folder, sidebar place, path segment or here)
    drop_target: Option<PathBuf>,
    /// Path bar segments (path, label, x, width) as last painted, for hit testing
    crumb_layout: Vec<(PathBuf, String, f32, f32)>,
    pub fonts: FontSystem,
    icons: IconCache,
    pub dirty: bool,
}

impl App {
    /// `reveal` selects that item once the folder is read; `rename` then starts renaming it
    pub fn new(fonts: FontSystem, start: PathBuf, reveal: Option<PathBuf>, rename: bool) -> (Self, Vec<Effect>) {
        let mut app = Self {
            preview: None,
            preview_generation: 0,
            picker:None,
            w: 1000.0,
            h: 640.0,
            maximized: false,
            focused: true,
            dir: PathBuf::new(),
            history: Vec::new(),
            hist_pos: 0,
            entries: Vec::new(),
            visible: Vec::new(),
            selected: HashSet::new(),
            cursor: None,
            anchor: None,
            view: View::Icons,
            scroll: 0.0,
            show_hidden: false,
            search: Default::default(),
            search_focus: false,
            rename: None,
            hover: Hit::Nothing,
            pointer: (0.0, 0.0),
            pressed: None,
            rubber: None,
            rubber_base: HashSet::new(),
            menu: None,
            confirm: None,
            path_edit: None,
            props: None,
            menu_hover: None,
            clipboard: None,
            places: Vec::new(),
            generation: 0,
            loading: false,
            error: None,
            status: None,
            type_ahead: (String::new(), Instant::now()),
            last_click: None,
            reveal: None,
            rename_on_reveal: false,
            drag_origin: None,
            drop_target: None,
            crumb_layout: Vec::new(),
            fonts,
            icons: IconCache::default(),
            dirty: true,
        };
        app.refresh_places();
        let effects = app.navigate(start, true);
        if reveal.is_some() {
            app.reveal = reveal;
            app.rename_on_reveal = rename;
        }
        (app, effects)
    }

    pub fn title(&self) -> String {
        self.dir_name(&self.dir)
    }

    fn dir_name(&self, path: &Path) -> String {
        if path == fsops::home() {
            return std::env::var("USER").unwrap_or_else(|_| "Домой".into());
        }
        if self.picker.is_none() && path == fsops::trash_files_dir() {
            return "Корзина".into();
        }
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Компьютер".into())
    }

    pub fn refresh_places(&mut self) {
        let mut places = vec![
            ("Домой", fsops::home(), Glyph::Home),
            ("Рабочий стол", fsops::user_dir("DESKTOP", "Desktop"), Glyph::Desktop),
            ("Документы", fsops::user_dir("DOCUMENTS", "Documents"), Glyph::Documents),
            ("Загрузки", fsops::user_dir("DOWNLOAD", "Downloads"), Glyph::Downloads),
            ("Изображения", fsops::user_dir("PICTURES", "Pictures"), Glyph::Pictures),
            ("Музыка", fsops::user_dir("MUSIC", "Music"), Glyph::Music),
            ("Видео", fsops::user_dir("VIDEOS", "Videos"), Glyph::Videos),
        ]
        .into_iter()
        .filter(|(_, p, g)| *g == Glyph::Home || p.is_dir())
        .map(|(n, p, g)| Place { name: n.into(), path: p, glyph: g, section: "Избранное" })
        .collect::<Vec<_>>();
        places.push(Place { name: "Корзина".into(), path: fsops::trash_files_dir(), glyph: Glyph::Trash, section: "Избранное" });
        places.push(Place { name: "Компьютер".into(), path: PathBuf::from("/"), glyph: Glyph::Computer, section: "Места" });
        for (name, path) in fsops::volumes() {
            places.push(Place { name, path, glyph: Glyph::Drive, section: "Места" });
        }
        self.places = places;
    }

    /// Glyph engraved on a folder icon if it is one of the user folders
    fn special(&self, path: &Path) -> Option<Glyph> {
        self.places.iter().find(|p| p.path == path && p.glyph != Glyph::Computer && p.glyph != Glyph::Drive && p.glyph != Glyph::Home).map(|p| p.glyph)
    }

    // ---------- navigation and model ----------

    pub fn navigate(&mut self, dir: PathBuf, record: bool) -> Vec<Effect> {
        if record {
            self.history.truncate(self.hist_pos + usize::from(!self.history.is_empty()));
            self.history.push(dir.clone());
            self.hist_pos = self.history.len() - 1;
        }
        self.preview = None;
        let previous = std::mem::replace(&mut self.dir, dir.clone());
        self.selected.clear();
        // Going up selects the folder we came from, as Finder does
        if previous.parent() == Some(dir.as_path()) {
            self.reveal = Some(previous);
        }
        self.cursor = None;
        self.anchor = None;
        self.scroll = 0.0;
        self.search.clear();
        self.search_focus = false;
        self.rename = None;
        self.menu = None;
        self.error = None;
        self.reload()
    }

    pub fn reload(&mut self) -> Vec<Effect> {
        self.generation += 1;
        self.loading = true;
        self.dirty = true;
        vec![Effect::List(self.dir.clone(), self.generation)]
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Result of a background listing; stale generations are dropped
    pub fn listed(&mut self, generation: u64, result: Result<Vec<Entry>, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(err) => {
                self.entries.clear();
                self.error = Some(err);
            }
        }
        let existing: HashSet<&PathBuf> = self.entries.iter().map(|e| &e.path).collect();
        self.selected.retain(|p| existing.contains(p));
        self.refilter();
        if let Some(path) = self.reveal.take() {
            if let Some(i) = self.visible.iter().position(|&i| self.entries[i].path == path) {
                self.selected = HashSet::from([path]);
                self.cursor = Some(i);
                self.anchor = Some(i);
                self.scroll_to(i);
                if std::mem::take(&mut self.rename_on_reveal) {
                    self.start_rename();
                }
            }
        }
        self.dirty = true;
    }

    fn refilter(&mut self) {
        let query = self.search.to_lowercase();
        self.visible = (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                (self.show_hidden || !e.hidden) && (query.is_empty() || e.name.to_lowercase().contains(&query))&&self.picker_matches(e)
            })
            .collect();
        self.cursor = self.cursor.filter(|&c| c < self.visible.len());
        self.clamp_scroll();
    }

    fn entry(&self, visible: usize) -> &Entry {
        &self.entries[self.visible[visible]]
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.visible.iter().map(|&i| &self.entries[i].path).filter(|p| self.selected.contains(*p)).cloned().collect()
    }

    fn open(&mut self, visible: usize) -> Vec<Effect> {
        let entry = self.entry(visible).clone();
        if entry.is_dir {
            self.navigate(entry.path, true)
        } else if self.picker.is_some(){
            self.select_only(visible);
            if let Some(picker)=self.picker.as_mut(){if picker.config.mode==feather_ui::picker::Mode::Save{picker.name=entry.name;}}
            self.picker_accept()
        } else {
            fsops::open_external(&entry.path);
            self.flash(format!("Открывается «{}»", entry.name));
            Vec::new()
        }
    }

    fn open_selection(&mut self) -> Vec<Effect> {
        if self.picker.is_some(){
            let chosen=self.selected_paths();
            if chosen.len()==1&&self.entries.iter().any(|e|e.path==chosen[0]&&e.is_dir){return self.navigate(chosen[0].clone(),true);}
            return self.picker_accept();
        }
        let chosen: Vec<usize> = (0..self.visible.len()).filter(|&i| self.selected.contains(&self.entry(i).path)).collect();
        match chosen.as_slice() {
            [] => Vec::new(),
            [one] => self.open(*one),
            many => {
                // Several files open externally; a folder among them is entered last
                let mut effects = Vec::new();
                for &i in many {
                    if !self.entry(i).is_dir {
                        fsops::open_external(&self.entry(i).path);
                    }
                }
                if let Some(&dir) = many.iter().find(|&&i| self.entry(i).is_dir) {
                    effects = self.open(dir);
                }
                effects
            }
        }
    }

    fn go_back(&mut self) -> Vec<Effect> {
        if self.hist_pos == 0 || self.history.is_empty() {
            return Vec::new();
        }
        self.hist_pos -= 1;
        self.navigate(self.history[self.hist_pos].clone(), false)
    }

    fn go_forward(&mut self) -> Vec<Effect> {
        if self.hist_pos + 1 >= self.history.len() {
            return Vec::new();
        }
        self.hist_pos += 1;
        self.navigate(self.history[self.hist_pos].clone(), false)
    }

    fn go_up(&mut self) -> Vec<Effect> {
        match self.dir.parent() {
            Some(parent) => self.navigate(parent.to_path_buf(), true),
            None => Vec::new(),
        }
    }

    fn flash(&mut self, text: String) {
        self.status = Some((text, Instant::now()));
        self.dirty = true;
    }

    pub fn operation_done(&mut self, failed: usize, what: &str) -> Vec<Effect> {
        if failed > 0 {
            self.flash(format!("{what}: не удалось для {failed}"));
        }
        self.reload()
    }

    fn set_view(&mut self, view: View) {
        self.view = view;
        self.scroll = 0.0;
        if let Some(c) = self.cursor {
            self.scroll_to(c);
        }
        self.dirty = true;
    }

    fn open_preview(&mut self) -> Vec<Effect> {
        let index = self.cursor.filter(|&i| i < self.visible.len() && self.selected.contains(&self.entry(i).path))
            .or_else(|| (0..self.visible.len()).find(|&i| self.selected.contains(&self.entry(i).path)));
        let Some(index) = index else { return Vec::new(); };
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let path = self.entry(index).path.clone();
        self.preview = Some(QuickPreview { path: path.clone(), generation: self.preview_generation, result: None, scroll: 0 });
        self.menu = None;
        self.search_focus = false;
        self.pressed = None;
        self.drag_origin = None;
        vec![Effect::Preview(path, self.preview_generation)]
    }

    pub fn preview_ready(&mut self, generation: u64, result: Result<crate::preview::Content, String>) {
        if let Some(preview) = self.preview.as_mut().filter(|p| p.generation == generation) {
            preview.result = Some(result);
            self.dirty = true;
        }
    }

    /// Opt-in checks in an owned client; never inject input into the host.
    pub fn debug_preview(&mut self, path: &Path) -> Vec<Effect> {
        let Some(index) = (0..self.visible.len()).find(|&i| self.entry(i).path == path) else { return Vec::new(); };
        self.select_only(index);
        self.key(0x20, Some(" "), Mods::default())
    }

    fn preview_rect(&self) -> (f32, f32, f32, f32) {
        let (w, h) = ((self.w - 48.0).min(760.0), (self.h - 48.0).min(560.0));
        ((self.w - w) * 0.5, (self.h - h) * 0.5, w, h)
    }

    fn start_rename(&mut self) {
        if let Some(c) = self.cursor.filter(|_| self.selected.len() == 1) {
            let e = self.entry(c);
            let mut input = feather_ui::input::TextInput::new(&e.name);
            input.select_all();
            self.rename = Some((e.path.clone(), input));
            self.dirty = true;
        }
    }

    fn commit_rename(&mut self) -> Vec<Effect> {
        let Some((path, name)) = self.rename.take() else { return Vec::new() };
        self.dirty = true;
        let name = name.trim().to_string();
        if name.is_empty() || name.contains('/') || path.file_name().is_some_and(|n| n.to_string_lossy() == name) {
            return Vec::new();
        }
        let target = path.with_file_name(&name);
        if target.exists() {
            self.flash(format!("«{name}» уже существует"));
            return Vec::new();
        }
        match std::fs::rename(&path, &target) {
            Ok(()) => {
                self.reveal = Some(target);
                self.reload()
            }
            Err(err) => {
                self.flash(format!("Не удалось переименовать: {err}"));
                Vec::new()
            }
        }
    }

    fn new_folder(&mut self) -> Vec<Effect> {
        let path = fsops::unique_name(&self.dir, "Новая папка");
        match std::fs::create_dir(&path) {
            Ok(()) => {
                // Insert now so rename starts immediately; the watcher re-read keeps it in place
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                self.entries.push(Entry { name: name.clone(), path: path.clone(), kind: Kind::Folder, is_dir: true, hidden: false, size: 0, modified: Some(std::time::SystemTime::now()), ext: String::new() });
                fsops::sort_entries(&mut self.entries);
                self.search.clear();
                self.refilter();
                let i = self.visible.iter().position(|&i| self.entries[i].path == path);
                self.selected = HashSet::from([path.clone()]);
                self.cursor = i;
                self.anchor = i;
                if let Some(i) = i {
                    self.scroll_to(i);
                }
                let mut input = feather_ui::input::TextInput::new(&name);
                input.select_all();
                self.rename = Some((path, input));
                self.dirty = true;
                Vec::new()
            }
            Err(err) => {
                self.flash(format!("Не удалось создать папку: {err}"));
                Vec::new()
            }
        }
    }

    fn copy_selection(&mut self, cut: bool) -> Vec<Effect> {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return Vec::new();
        }
        let n = paths.len();
        self.clipboard = Some((paths.clone(), cut));
        self.flash(format!("{} {}", if cut { "Вырезано:" } else { "Скопировано:" }, objects(n)));
        vec![Effect::Clipboard(paths)]
    }

    /// Files from another program's clipboard (text/uri-list) replace our own
    pub fn external_clipboard(&mut self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.clipboard = Some((paths, false));
        }
    }

    fn paste(&mut self) -> Vec<Effect> {
        let Some((paths, cut)) = self.clipboard.clone() else { return Vec::new() };
        if cut {
            self.clipboard = None;
        }
        self.flash(format!("{} {}…", if cut { "Перемещение" } else { "Копирование" }, objects(paths.len())));
        vec![Effect::Paste(paths, self.dir.clone(), cut)]
    }

    /// The home trash folder itself (restore/delete act on its direct items).
    fn in_trash(&self) -> bool {
        self.picker.is_none() && self.dir == fsops::trash_files_dir()
    }

    fn confirm_ok(&mut self) -> Vec<Effect> {
        match self.confirm.take() {
            Some(Confirm::Delete(paths)) => {
                self.flash(format!("Удалено: {}", objects(paths.len())));
                self.selected.clear();
                vec![Effect::DeleteForever(paths)]
            }
            Some(Confirm::Empty) => {
                self.flash("Корзина очищается…".into());
                self.selected.clear();
                vec![Effect::EmptyTrash]
            }
            None => Vec::new(),
        }
    }

    fn empty_button(&self) -> Option<(f32, f32, f32, f32)> {
        (self.in_trash() && !self.entries.is_empty() && self.confirm.is_none()).then(|| {
            let y = self.h - PATHBAR_H - self.picker_height();
            (self.w - 14.0 - 84.0, y + 4.0, 84.0, PATHBAR_H - 8.0)
        })
    }

    fn title_rect(&self) -> (f32, f32, f32, f32) {
        let x = SIDEBAR_W + 86.0;
        (x, 12.0, (self.w - 196.0 - 84.0 - x - 10.0).max(80.0), 28.0)
    }

    fn open_path_edit(&mut self) {
        let mut input = feather_ui::input::TextInput::new(&self.dir.to_string_lossy());
        input.select_all();
        self.path_edit = Some(input);
        self.search_focus = false;
        self.dirty = true;
    }

    /// Enter in the path field: `~`, relative paths; a file opens its folder.
    fn commit_path(&mut self) -> Vec<Effect> {
        let Some(input) = self.path_edit.take() else { return Vec::new() };
        let text = input.text().trim();
        let path = match text.strip_prefix('~') {
            Some(rest) => fsops::home().join(rest.trim_start_matches('/')),
            None if text.starts_with('/') => PathBuf::from(text),
            None => self.dir.join(text),
        };
        if path.is_dir() {
            self.navigate(path, true)
        } else if let Some(parent) = path.parent().filter(|_| path.exists()) {
            self.navigate(parent.to_path_buf(), true)
        } else {
            self.flash(format!("Папка «{text}» не найдена"));
            Vec::new()
        }
    }

    /// Finished stat of «Свойства» (and later the measured size of a folder).
    pub fn props_ready(&mut self, props: Props) {
        if self.props.as_ref().is_none_or(|p| p.path == props.path) {
            self.props = Some(props);
            self.dirty = true;
        }
    }

    fn props_rect(&self) -> (f32, f32, f32, f32) {
        let (w, h) = (380.0, 330.0);
        (SIDEBAR_W + ((self.w - SIDEBAR_W - w) / 2.0).max(8.0), TOOLBAR_H + 12.0, w, h)
    }

    fn confirm_rects(&self) -> ((f32, f32, f32, f32), (f32, f32, f32, f32), (f32, f32, f32, f32)) {
        let (w, h) = (360.0, 150.0);
        let x = SIDEBAR_W + ((self.w - SIDEBAR_W - w) / 2.0).max(8.0);
        let y = TOOLBAR_H + 12.0;
        let bw = 140.0;
        ((x, y, w, h), (x + w - 20.0 - bw * 2.0 - 10.0, y + h - 46.0, bw, 30.0), (x + w - 20.0 - bw, y + h - 46.0, bw, 30.0))
    }

    fn trash_selection(&mut self) -> Vec<Effect> {
        let paths = self.selected_paths();
        if paths.is_empty() || self.dir.starts_with(fsops::trash_files_dir()) {
            return Vec::new();
        }
        self.flash(format!("В Корзину: {}", objects(paths.len())));
        vec![Effect::Trash(paths)]
    }

    // ---------- geometry ----------

    fn content(&self) -> (f32, f32, f32, f32) {
        (SIDEBAR_W, TOOLBAR_H, (self.w - SIDEBAR_W).max(0.0), (self.h - TOOLBAR_H - PATHBAR_H-self.picker_height()).max(0.0))
    }

    fn columns(&self) -> usize {
        (((self.content().2 - 16.0) / CELL_W).floor() as usize).max(1)
    }

    /// Item rectangle in window coordinates (already scrolled)
    fn item_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let (cx, cy, cw, _) = self.content();
        match self.view {
            View::Icons => {
                let cols = self.columns();
                let spacing = (cw - 16.0) / cols as f32;
                let (col, row) = (i % cols, i / cols);
                (cx + 8.0 + col as f32 * spacing + (spacing - CELL_W) * 0.5, cy + 10.0 + row as f32 * CELL_H - self.scroll, CELL_W, CELL_H)
            }
            View::List => (cx, cy + HEADER_H + i as f32 * ROW_H - self.scroll, cw, ROW_H),
        }
    }

    fn content_height(&self) -> f32 {
        match self.view {
            View::Icons => 20.0 + self.visible.len().div_ceil(self.columns()) as f32 * CELL_H,
            View::List => HEADER_H + self.visible.len() as f32 * ROW_H + 8.0,
        }
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_height() - self.content().3).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn scroll_to(&mut self, i: usize) {
        let (_, cy, _, ch) = self.content();
        let (_, y, _, h) = self.item_rect(i);
        let top = cy + if self.view == View::List { HEADER_H } else { 0.0 };
        if y < top {
            self.scroll -= top - y;
        } else if y + h > cy + ch {
            self.scroll += y + h - (cy + ch);
        }
        self.clamp_scroll();
    }

    fn traffic(&self) -> [(Hit, f32); 3] {
        [(Hit::Close, 26.0), (Hit::Minimize, 46.0), (Hit::Zoom, 66.0)]
    }

    fn search_rect(&self) -> (f32, f32, f32, f32) {
        (self.w - 196.0, 12.0, 180.0, 28.0)
    }

    fn view_rects(&self) -> [(Hit, (f32, f32, f32, f32)); 2] {
        let x = self.w - 196.0 - 84.0;
        [(Hit::ViewIcons, (x, 12.0, 36.0, 28.0)), (Hit::ViewList, (x + 36.0, 12.0, 36.0, 28.0))]
    }

    fn place_rects(&self) -> Vec<(usize, f32)> {
        let mut y = 56.0;
        let mut section = "";
        let mut out = Vec::new();
        for (i, p) in self.places.iter().enumerate() {
            if p.section != section {
                section = p.section;
                y += if i == 0 { 0.0 } else { 10.0 } + 22.0;
            }
            out.push((i, y));
            y += PLACE_H;
        }
        out
    }

    fn layout_crumbs(&mut self) {
        let mut parts: Vec<PathBuf> = self.dir.ancestors().map(Path::to_path_buf).collect();
        parts.reverse();
        let mut x = SIDEBAR_W + 12.0;
        let last = parts.len().saturating_sub(1);
        let mut out = Vec::new();
        for (k, p) in parts.into_iter().enumerate() {
            let name = self.dir_name(&p);
            let w = 16.0 + self.fonts.measure_text(&name, 11.5, k == last);
            out.push((p, name, x, w));
            x += w + 14.0;
        }
        // Long paths keep their tail visible
        let overflow = x - (self.w - 290.0);
        if overflow > 0.0 {
            for c in &mut out {
                c.2 -= overflow;
            }
            out.retain(|c| c.2 >= SIDEBAR_W + 4.0);
        }
        self.crumb_layout = out;
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| x >= rx && x < rx + rw && y >= ry && y < ry + rh;
        if self.props.is_some() {
            let (px, py, pw, ph) = self.props_rect();
            return if inside((px + pw - 116.0, py + ph - 46.0, 96.0, 30.0)) { Hit::PropsClose } else { Hit::Nothing };
        }
        if self.picker.is_none() && inside(self.title_rect()) {
            return Hit::Title;
        }
        if self.confirm.is_some() {
            let (_, cancel, ok) = self.confirm_rects();
            return if inside(ok) { Hit::ConfirmOk } else if inside(cancel) { Hit::ConfirmCancel } else { Hit::Nothing };
        }
        if self.empty_button().is_some_and(inside) {
            return Hit::EmptyTrash;
        }
        if let Some(menu) = &self.menu {
            let (mx, my, mw, _) = self.menu_rect(menu);
            if x >= mx && x < mx + mw {
                let mut iy = my + 6.0;
                for (i, (_, action)) in menu.items.iter().enumerate() {
                    let ih = if action.is_some() { 24.0 } else { 9.0 };
                    if y >= iy && y < iy + ih && action.is_some() {
                        return Hit::Menu(i);
                    }
                    iy += ih;
                }
            }
            return Hit::Nothing;
        }
        if let Some(hit)=self.picker_hit(x,y){return hit;}
        if y < TOOLBAR_H && x < 80.0 {
            for (hit, cx) in self.traffic() {
                if (x - cx).abs() <= 8.0 && (y - 26.0).abs() <= 8.0 {
                    return hit;
                }
            }
        }
        if x < SIDEBAR_W {
            for (i, py) in self.place_rects() {
                if y >= py && y < py + PLACE_H && x > 16.0 && x < SIDEBAR_W - 16.0 {
                    return Hit::Place(i);
                }
            }
            return Hit::Toolbar;
        }
        if y < TOOLBAR_H {
            let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| x >= rx && x < rx + rw && y >= ry && y < ry + rh;
            if inside((SIDEBAR_W + 10.0, 12.0, 32.0, 28.0)) {
                return Hit::Back;
            }
            if inside((SIDEBAR_W + 42.0, 12.0, 32.0, 28.0)) {
                return Hit::Forward;
            }
            for (hit, r) in self.view_rects() {
                if inside(r) {
                    return hit;
                }
            }
            if inside(self.search_rect()) {
                return Hit::Search;
            }
            return Hit::Toolbar;
        }
        if y >= self.h - PATHBAR_H-self.picker_height() {
            for (path, _, cx, cw) in &self.crumb_layout {
                if x >= *cx && x < cx + cw {
                    return Hit::Crumb(path.clone());
                }
            }
            return Hit::Nothing;
        }
        let (_, cy, _, _) = self.content();
        if self.view == View::List && y < cy + HEADER_H {
            return Hit::Nothing;
        }
        if let Some(i) = self.item_at(x, y) {
            return Hit::Item(i);
        }
        Hit::Content
    }

    fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        let (_, cy, _, _) = self.content();
        let first = match self.view {
            View::Icons => (((self.scroll + y - cy - 10.0) / CELL_H).floor().max(0.0) as usize) * self.columns(),
            View::List => ((self.scroll + y - cy - HEADER_H) / ROW_H).floor().max(0.0) as usize,
        };
        let last = (first + self.columns() + 1).min(self.visible.len());
        (first.min(self.visible.len())..last).find(|&i| {
            let (rx, ry, rw, rh) = self.item_rect(i);
            match self.view {
                // Finder: the icon and the name are hot, not the whole cell
                View::Icons => {
                    let editing_name = self.rename.as_ref().is_some_and(|(p, _)| *p == self.entry(i).path)
                        && x >= rx + 5.0 && x < rx + rw - 5.0
                        && y >= ry + GRID_ICON + 8.0 && y < ry + GRID_ICON + 27.0;
                    editing_name || (x >= rx + 12.0 && x < rx + rw - 12.0 && y >= ry + 2.0 && y < ry + rh - 4.0)
                }
                View::List => x >= rx && x < rx + rw && y >= ry && y < ry + rh,
            }
        })
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
        let edges = self.resize_edges(x, y);
        if edges != 0 {
            return Cursor::Resize(edges);
        }
        match self.hit(x, y) {
            Hit::Search => Cursor::Text,
            Hit::Title if self.path_edit.is_some() => Cursor::Text,
            Hit::Item(i) if self.rename.as_ref().is_some_and(|(p, _)| *p == self.entry(i).path) => Cursor::Text,
            _ => Cursor::Default,
        }
    }

    // ---------- input ----------

    pub fn resize(&mut self, w: f32, h: f32) {
        if (w, h) != (self.w, self.h) {
            self.w = w.max(480.0);
            self.h = h.max(320.0);
            self.clamp_scroll();
            self.menu = None;
            self.dirty = true;
        }
    }

    pub fn motion(&mut self, x: f32, y: f32) -> Vec<Effect> {
        if self.picker.is_some(){self.drag_origin=None;}
        self.pointer = (x, y);
        if let Some(origin) = self.drag_origin {
            if (x - origin.0).hypot(y - origin.1) > 4.0 {
                self.drag_origin = None;
                self.pressed = None;
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    return vec![Effect::StartDrag(paths)];
                }
            }
            return Vec::new();
        }
        if let Some((start, _)) = self.rubber {
            self.rubber = Some((start, (x, y)));
            self.update_rubber();
            self.dirty = true;
            return Vec::new();
        }
        if let Some((Hit::Content, origin)) = &self.pressed {
            if (x - origin.0).hypot(y - origin.1) > 3.0 {
                let origin = *origin;
                self.rubber = Some(((origin.0, origin.1 + self.scroll), (x, y)));
                self.update_rubber();
                self.dirty = true;
            }
            return Vec::new();
        }
        let hover = self.hit(x, y);
        let menu_hover = match hover {
            Hit::Menu(i) => Some(i),
            _ => None,
        };
        // Files and empty space have no hover look: moving over them repaints nothing
        let visual = |h: &Hit| !matches!(h, Hit::Item(_) | Hit::Content | Hit::Toolbar | Hit::Nothing | Hit::Search);
        if menu_hover != self.menu_hover || (hover != self.hover && (visual(&hover) || visual(&self.hover))) {
            self.dirty = true;
        }
        self.hover = hover;
        self.menu_hover = menu_hover;
        Vec::new()
    }

    pub fn leave(&mut self) {
        if !matches!(self.hover, Hit::Nothing | Hit::Item(_) | Hit::Content | Hit::Toolbar | Hit::Search) {
            self.hover = Hit::Nothing;
            self.dirty = true;
        }
    }

    fn update_rubber(&mut self) {
        let Some((start, end)) = self.rubber else { return };
        // The anchor is stored in content coordinates so it scrolls with the items
        let (x0, x1) = (start.0.min(end.0), start.0.max(end.0));
        let (y0, y1) = ((start.1 - self.scroll).min(end.1), (start.1 - self.scroll).max(end.1));
        let mut chosen = self.rubber_base.clone();
        for i in 0..self.visible.len() {
            let (rx, ry, rw, rh) = self.item_rect(i);
            let (rx, rw) = if self.view == View::Icons { (rx + 12.0, rw - 24.0) } else { (rx, rw) };
            if rx < x1 && rx + rw > x0 && ry < y1 && ry + rh > y0 {
                chosen.insert(self.entry(i).path.clone());
            }
        }
        self.selected = chosen;
    }

    pub fn press(&mut self, button: u32, x: f32, y: f32, mods: Mods) -> Vec<Effect> {
        if self.picker.is_some()&&button==BTN_RIGHT{return Vec::new();}
        let selection_mods=if self.picker.as_ref().is_some_and(|p|!p.config.multiple){Mods{ctrl:false,shift:false,..mods}}else{mods};
        self.pointer = (x, y);
        self.dirty = true;
        if self.preview.is_some() {
            if button == BTN_LEFT {
                let (px, py, pw, ph) = self.preview_rect();
                if x >= px + pw - 190.0 && x < px + pw - 16.0 && y >= py + ph - 39.0 && y < py + ph - 10.0 {
                    self.preview = None;
                    return self.open_selection();
                }
                if x < px || x >= px + pw || y < py || y >= py + ph || (x < px + 42.0 && y < py + 42.0) {
                    self.preview = None;
                }
            }
            return Vec::new();
        }
        let edges = self.resize_edges(x, y);
        if button == BTN_LEFT && edges != 0 && self.menu.is_none() {
            return vec![Effect::Resize(edges)];
        }
        match button {
            BTN_BACK => return self.go_back(),
            BTN_FORWARD => return self.go_forward(),
            _ => {}
        }
        let hit = self.hit(x, y);
        if self.confirm.is_some() && button != BTN_LEFT {
            return Vec::new();
        }
        if self.props.is_some() {
            if hit == Hit::PropsClose && button == BTN_LEFT {
                self.props = None;
            }
            return Vec::new();
        }
        if hit == Hit::Title && button == BTN_LEFT {
            if self.path_edit.is_none() {
                self.open_path_edit();
            } else {
                let (tx, _, tw, _) = self.title_rect();
                if let Some(input) = self.path_edit.as_mut() {
                    input.click(&mut self.fonts, x - tx - 10.0, tw - 20.0, 13.0, false, mods.shift);
                }
            }
            return Vec::new();
        }
        self.path_edit = None;
        if button==BTN_LEFT && !matches!(hit,Hit::PickerName|Hit::PickerAccept|Hit::PickerCancel) {
            if let Some(p)=self.picker.as_mut(){p.name_focus=false;}
        }
        if self.menu.is_some() {
            if let (Hit::Menu(i), BTN_LEFT) = (&hit, button) {
                let action = self.menu.as_ref().and_then(|m| m.items[*i].1);
                self.menu = None;
                return action.map(|a| self.menu_action(a)).unwrap_or_default();
            }
            self.menu = None;
            return Vec::new();
        }
        let mut effects = Vec::new();
        if button == BTN_LEFT {
            if let Hit::Item(i) = &hit {
                if self.rename.as_ref().is_some_and(|(p, _)| *p == self.entry(*i).path) {
                    let (rx, ry, rw, _) = self.item_rect(*i);
                    let (tx, ty, width, size) = match self.view {
                        View::Icons => (rx + 9.0, ry + 10.0 + GRID_ICON, rw - 18.0, 12.0),
                        View::List => {
                            let (cx, _, cw, _) = self.content();
                            let nw = cw - 160.0 - 90.0 - if cw > 640.0 { 130.0 } else { 0.0 } - 40.0;
                            (cx + 45.0, ry + 5.5, nw - 40.0, 13.0)
                        }
                    };
                    if (ty - 3.0..ty + 18.0).contains(&y) {
                        self.rename.as_mut().unwrap().1.click(&mut self.fonts, x - tx, width, size, false, mods.shift);
                        return Vec::new();
                    }
                }
            }
        }
        if self.rename.is_some() && !matches!(&hit, Hit::Item(i) if self.rename.as_ref().is_some_and(|(p, _)| *p == self.entry(*i).path)) {
            effects = self.commit_rename();
        }
        if !matches!(hit, Hit::Search) && self.search_focus {
            self.search_focus = false;
        }
        let now = Instant::now();
        let double = button == BTN_LEFT
            && self.last_click.as_ref().is_some_and(|(t, h)| *h == hit && now.duration_since(*t) < DOUBLE_CLICK);
        self.last_click = (button == BTN_LEFT).then(|| (now, hit.clone()));
        if button == BTN_RIGHT {
            self.open_menu(hit, x, y);
            return effects;
        }
        if button != BTN_LEFT {
            return effects;
        }
        if self.confirm.is_some() {
            return match hit {
                Hit::ConfirmOk => self.confirm_ok(),
                Hit::ConfirmCancel => {
                    self.confirm = None;
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        match hit {
            Hit::EmptyTrash => {
                self.confirm = Some(Confirm::Empty);
                return effects;
            }
            Hit::PickerAccept=>return self.picker_accept(),
            Hit::PickerCancel=>return vec![Effect::Close],
            Hit::PickerName=>{if let Some(p)=self.picker.as_mut(){p.name_focus=true;}},
            Hit::PickerFilter=>{self.picker_next_filter();},
            Hit::PickerChoice(i)=>self.picker_next_choice(i),
            Hit::Close => effects.push(Effect::Close),
            Hit::Minimize => effects.push(Effect::Minimize),
            Hit::Zoom => effects.push(Effect::ToggleMaximize),
            Hit::Back => effects.extend(self.go_back()),
            Hit::Forward => effects.extend(self.go_forward()),
            Hit::ViewIcons => self.set_view(View::Icons),
            Hit::ViewList => self.set_view(View::List),
            Hit::Search => {
                self.search_focus = true;
                let (sx, _, sw, _) = self.search_rect();
                self.search.click(&mut self.fonts, x - sx - 28.0, sw - 40.0, 13.0, false, mods.shift);
            },
            Hit::Place(i) => {
                let path = self.places[i].path.clone();
                if self.picker.is_none() && path == fsops::trash_files_dir() {
                    let _ = std::fs::create_dir_all(&path);
                }
                effects.extend(self.navigate(path, true));
            }
            Hit::Crumb(path) => effects.extend(self.navigate(path, true)),
            Hit::Toolbar if double => effects.push(Effect::ToggleMaximize),
            Hit::Toolbar => effects.push(Effect::Move),
            Hit::Item(i) if double => {
                self.last_click = None;
                effects.extend(self.open(i));
            }
            Hit::Item(i) => self.click_item(i, selection_mods),
            Hit::Content => {
                if self.picker.as_ref().is_some_and(|p|!p.config.multiple){self.selected.clear();self.cursor=None;return effects;}
                self.pressed = Some((Hit::Content, (x, y)));
                self.rubber_base = if mods.ctrl || mods.shift { self.selected.clone() } else { HashSet::new() };
                if !(mods.ctrl || mods.shift) {
                    self.selected.clear();
                    self.cursor = None;
                }
            }
            Hit::Menu(_) | Hit::ConfirmOk | Hit::ConfirmCancel | Hit::Title | Hit::PropsClose | Hit::Nothing => {}
        }
        effects
    }

    fn click_item(&mut self, i: usize, mods: Mods) {
        let path = self.entry(i).path.clone();
        if mods.shift {
            let a = self.anchor.unwrap_or(i);
            if !mods.ctrl {
                self.selected.clear();
            }
            for j in a.min(i)..=a.max(i) {
                self.selected.insert(self.entry(j).path.clone());
            }
        } else if mods.ctrl {
            if !self.selected.remove(&path) {
                self.selected.insert(path);
            }
            self.anchor = Some(i);
        } else {
            if !self.selected.contains(&path) || self.selected.len() > 1 {
                self.selected = HashSet::from([path]);
            } else if self.cursor == Some(i) {
                // A second slow click on the selected name renames, like Finder
                self.pressed = Some((Hit::Item(i), self.pointer));
            }
            self.anchor = Some(i);
        }
        self.cursor = Some(i);
        if self.selected.contains(&self.entry(i).path) {
            self.drag_origin = Some(self.pointer);
        }
    }

    pub fn release(&mut self, button: u32, x: f32, y: f32) -> Vec<Effect> {
        if self.picker.is_some(){self.pressed=None;}
        if button != BTN_LEFT {
            return Vec::new();
        }
        self.drag_origin = None;
        if self.rubber.take().is_some() {
            self.dirty = true;
        }
        if let Some((Hit::Item(i), _)) = self.pressed.take() {
            let (_, ry, _, rh) = self.item_rect(i);
            let on_name = match self.view {
                View::Icons => y > ry + 10.0 + GRID_ICON,
                View::List => y >= ry && y < ry + rh && x > SIDEBAR_W + 36.0,
            };
            let quick = self.last_click.as_ref().is_some_and(|(t, _)| t.elapsed() < DOUBLE_CLICK);
            if on_name && !quick && self.selected.len() == 1 {
                self.start_rename();
            }
        }
        Vec::new()
    }

    pub fn scroll(&mut self, dy: f32) {
        if let Some(p) = self.preview.as_mut() {
            if let Some(Ok(crate::preview::Content::Text(lines, _))) = &p.result {
                let max = lines.len().saturating_sub(1);
                let old = p.scroll;
                p.scroll = (p.scroll as f32 + dy / 18.0).round().clamp(0.0, max as f32) as usize;
                self.dirty |= p.scroll != old;
            }
            return;
        }
        if self.menu.is_some() {
            return;
        }
        let before = self.scroll;
        self.scroll += dy;
        self.clamp_scroll();
        if self.rubber.is_some() {
            self.update_rubber();
        }
        if self.scroll != before {
            self.dirty = true;
            let (x, y) = self.pointer;
            self.hover = self.hit(x, y);
        }
    }

    /// A drag with files is over the window: returns the folder a drop would go to
    pub fn drag_over(&mut self, x: f32, y: f32) -> PathBuf {
        self.pointer = (x, y);
        let target = match self.hit(x, y) {
            Hit::Item(i) if self.entry(i).is_dir => self.entry(i).path.clone(),
            Hit::Place(i) => self.places[i].path.clone(),
            Hit::Crumb(path) => path,
            _ => self.dir.clone(),
        };
        if self.drop_target.as_ref() != Some(&target) {
            self.drop_target = Some(target.clone());
            self.dirty = true;
        }
        target
    }

    pub fn drag_leave(&mut self) {
        if self.drop_target.take().is_some() {
            self.dirty = true;
        }
    }

    /// Files dropped into `dir`: same filesystem moves (unless the program chose copy),
    /// elsewhere copies; dropping items back where they are does nothing
    pub fn drop_files(&mut self, paths: Vec<PathBuf>, dir: PathBuf, move_allowed: bool) -> Vec<Effect> {
        if self.picker.is_some(){return Vec::new();}
        self.drag_leave();
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.parent() != Some(dir.as_path()) && *p != dir).collect();
        if paths.is_empty() {
            return Vec::new();
        }
        let cut = move_allowed && paths.iter().all(|p| p.parent().is_some_and(|parent| fsops::same_device(parent, &dir)));
        self.flash(format!("{} {}…", if cut { "Перемещение" } else { "Копирование" }, objects(paths.len())));
        vec![Effect::Paste(paths, dir, cut)]
    }

    /// Image shown under the pointer while dragging: the first icon, plus a count badge
    pub fn drag_image(&mut self, paths: &[PathBuf], scale: f32) -> Option<(u32, Vec<u8>)> {
        let first = paths.first()?;
        let entry = self.entries.iter().find(|e| e.path == *first)?.clone();
        let icon = self.icon_for(&entry);
        let size = 72.0;
        let px = (size * scale).ceil() as u32;
        let mut pm = tiny_skia::Pixmap::new(px, px)?;
        let before = theme::ui_scale();
        theme::set_ui_scale(scale);
        {
            let mut m = pm.as_mut();
            self.icons.draw(&mut m, &mut self.fonts, &icon, 4.0, 4.0, 64.0);
            if paths.len() > 1 {
                let label = paths.len().to_string();
                let w = self.fonts.measure_text(&label, 11.0, true) + 10.0;
                theme::fill_rrect(&mut m, size - w - 2.0, 2.0, w, 18.0, 9.0, rgba(255, 59, 48, 255));
                self.fonts.draw_text_centered(&mut m, &label, size - w * 0.5 - 2.0, 4.5, 11.0, rgba(255, 255, 255, 255), true);
            }
        }
        theme::set_ui_scale(before);
        Some((px, pm.take()))
    }

    fn open_menu(&mut self, hit: Hit, x: f32, y: f32) {
        let item = |s: &str, a: MenuAction| (s.to_string(), Some(a));
        let sep = || (String::new(), None);
        let items = match hit {
            Hit::Item(i) => {
                let path = self.entry(i).path.clone();
                if !self.selected.contains(&path) {
                    self.selected = HashSet::from([path]);
                    self.anchor = Some(i);
                }
                self.cursor = Some(i);
                let mut items = vec![item("Открыть", MenuAction::Open)];
                if self.in_trash() {
                    items.extend([sep(), item("Вернуть", MenuAction::Restore), item("Скопировать", MenuAction::Copy), item("Свойства", MenuAction::Properties), sep(), item("Удалить навсегда…", MenuAction::DeleteForever)]);
                    self.menu = Some(Menu { x, y, items });
                    self.menu_hover = None;
                    return;
                }
                if self.entry(i).is_dir {
                    items.push(item("Открыть в терминале", MenuAction::Terminal));
                }
                items.extend([sep(), item("Переименовать", MenuAction::Rename), item("Скопировать", MenuAction::Copy), item("Вырезать", MenuAction::Cut), item("Свойства", MenuAction::Properties)]);
                if !self.dir.starts_with(fsops::trash_files_dir()) {
                    items.extend([sep(), item("Переместить в Корзину", MenuAction::Trash)]);
                }
                items
            }
            Hit::Content | Hit::Nothing => {
                self.selected.clear();
                let mut items = vec![item("Новая папка", MenuAction::NewFolder)];
                if self.clipboard.is_some() {
                    items.push(item("Вставить", MenuAction::Paste));
                }
                items.extend([
                    item("Открыть в терминале", MenuAction::Terminal),
                    item("Свойства папки", MenuAction::Properties),
                    sep(),
                    item(if self.show_hidden { "✓ Скрытые файлы" } else { "Скрытые файлы" }, MenuAction::ToggleHidden),
                    item(if self.view == View::Icons { "✓ Значки" } else { "Значки" }, MenuAction::ShowIcons),
                    item(if self.view == View::List { "✓ Список" } else { "Список" }, MenuAction::ShowList),
                ]);
                if self.in_trash() && !self.entries.is_empty() {
                    items.extend([sep(), item("Очистить Корзину…", MenuAction::EmptyTrash)]);
                }
                items
            }
            _ => return,
        };
        self.menu = Some(Menu { x, y, items });
        self.menu_hover = None;
    }

    fn menu_rect(&self, menu: &Menu) -> (f32, f32, f32, f32) {
        let w = 220.0;
        let h = 12.0 + menu.items.iter().map(|(_, a)| if a.is_some() { 24.0 } else { 9.0 }).sum::<f32>();
        let x = menu.x.min(self.w - w - 6.0).max(6.0);
        let y = if menu.y + h > self.h - 6.0 { (menu.y - h).max(6.0) } else { menu.y };
        (x, y, w, h)
    }

    fn menu_action(&mut self, action: MenuAction) -> Vec<Effect> {
        self.dirty = true;
        match action {
            MenuAction::Open => self.open_selection(),
            MenuAction::Terminal => {
                let target = self.cursor.filter(|_| !self.selected.is_empty()).map(|c| self.entry(c).clone()).filter(|e| e.is_dir).map(|e| e.path).unwrap_or_else(|| self.dir.clone());
                fsops::open_terminal(&target);
                Vec::new()
            }
            MenuAction::Rename => {
                self.start_rename();
                Vec::new()
            }
            MenuAction::Copy => self.copy_selection(false),
            MenuAction::Cut => self.copy_selection(true),
            MenuAction::Paste => self.paste(),
            MenuAction::Trash => self.trash_selection(),
            MenuAction::Restore => {
                let paths = self.selected_paths();
                if paths.is_empty() {
                    return Vec::new();
                }
                self.flash(format!("Возвращено: {}", objects(paths.len())));
                vec![Effect::Restore(paths)]
            }
            MenuAction::DeleteForever => {
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    self.confirm = Some(Confirm::Delete(paths));
                }
                Vec::new()
            }
            MenuAction::EmptyTrash => {
                self.confirm = Some(Confirm::Empty);
                Vec::new()
            }
            MenuAction::Properties => {
                let path = self.cursor.filter(|_| !self.selected.is_empty()).map(|c| self.entry(c).path.clone()).unwrap_or_else(|| self.dir.clone());
                self.props = Some(Props { path: path.clone(), ..Default::default() });
                vec![Effect::Inspect(path)]
            }
            MenuAction::NewFolder => self.new_folder(),
            MenuAction::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.refilter();
                Vec::new()
            }
            MenuAction::ShowIcons => {
                self.set_view(View::Icons);
                Vec::new()
            }
            MenuAction::ShowList => {
                self.set_view(View::List);
                Vec::new()
            }
        }
    }

    /// `sym` is an XKB keysym, `text` the produced characters
    pub fn key(&mut self, sym: u32, text: Option<&str>, mods: Mods) -> Vec<Effect> {
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        self.dirty = true;
        let raw = sym;
        let sym = K::new(sym);
        if self.props.is_some() {
            if matches!(sym, K::Escape | K::Return | K::KP_Enter | K::space) {
                self.props = None;
            }
            return Vec::new();
        }
        if self.preview.is_some() {
            match sym {
                K::Escape | K::space => self.preview = None,
                K::Return | K::KP_Enter => { self.preview = None; return self.open_selection(); }
                K::Left | K::Right | K::Up | K::Down => {
                    let key = if matches!(sym, K::Left | K::Up) { K::Left } else { K::Right };
                    // Follow the visible list order in both icon and list views.
                    let current = self.cursor.unwrap_or(0);
                    let next = if key == K::Left { current.saturating_sub(1) } else { (current + 1).min(self.visible.len().saturating_sub(1)) };
                    if !self.visible.is_empty() { self.select_only(next); return self.open_preview(); }
                }
                _ => {}
            }
            return Vec::new();
        }
        if let Some(input) = self.path_edit.as_mut() {
            match sym {
                K::Escape => self.path_edit = None,
                K::Return | K::KP_Enter => return self.commit_path(),
                _ => {
                    input.key(raw, text, mods.ctrl, mods.shift);
                }
            }
            return Vec::new();
        }
        if self.picker.is_none() && mods.ctrl && (matches!(sym, K::l | K::L) || (mods.shift && matches!(sym, K::g | K::G))) {
            self.open_path_edit();
            return Vec::new();
        }
        if self.confirm.is_some() {
            return match sym {
                K::Return | K::KP_Enter => self.confirm_ok(),
                K::Escape => {
                    self.confirm = None;
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        if let Some(effects)=self.picker_key(sym,text,mods){return effects;}
        // Editing a name or the search field
        if let Some((_, name)) = self.rename.as_mut() {
            match sym {
                K::Return | K::KP_Enter => return self.commit_rename(),
                K::Escape => self.rename = None,
                _ => { name.key(raw, text, mods.ctrl, mods.shift); }
            }
            return Vec::new();
        }
        if self.menu.is_some() {
            if sym == K::Escape {
                self.menu = None;
            }
            return Vec::new();
        }
        if self.search_focus {
            match sym {
                K::Escape => {
                    self.search.clear();
                    self.search_focus = false;
                }
                K::Return | K::KP_Enter | K::Down => {
                    self.search_focus = false;
                    if !self.visible.is_empty() {
                        self.select_only(0);
                    }
                }
                _ => {
                    if self.search.key(raw, text, mods.ctrl, mods.shift) != feather_ui::input::Edit::Changed {
                        return Vec::new();
                    }
                }
            }
            self.scroll = 0.0;
            self.refilter();
            return Vec::new();
        }
        let ctrl = mods.ctrl;
        match sym {
            K::space if !mods.ctrl && !mods.alt => return self.open_preview(),
            K::Return | K::KP_Enter if mods.alt || self.selected.len() != 1 => return self.open_selection(),
            K::Return | K::KP_Enter => return self.open_selection(),
            K::F2 => self.start_rename(),
            K::BackSpace => return self.go_up(),
            K::Up if mods.alt => return self.go_up(),
            K::Down if mods.alt || ctrl => return self.open_selection(),
            K::Left if mods.alt => return self.go_back(),
            K::Right if mods.alt => return self.go_forward(),
            K::bracketleft if ctrl => return self.go_back(),
            K::bracketright if ctrl => return self.go_forward(),
            K::Delete if self.in_trash() => return self.menu_action(MenuAction::DeleteForever),
            K::Delete => return self.trash_selection(),
            K::F5 => return self.reload(),
            K::Escape => {
                self.selected.clear();
                self.search.clear();
                self.refilter();
            }
            K::Left | K::Right | K::Up | K::Down | K::Home | K::End | K::Page_Up | K::Page_Down => self.move_cursor(sym, mods.shift&&self.picker.as_ref().is_none_or(|p|p.config.multiple)),
            _ if ctrl => match text.map(str::to_lowercase).as_deref().or(latin(sym)) {
                Some("a" | "ф") => {
                    self.selected = self.visible.iter().map(|&i| self.entries[i].path.clone()).collect();
                }
                Some("c" | "с") => return self.copy_selection(false),
                Some("x" | "ч") => return self.copy_selection(true),
                Some("v" | "м") => return self.paste(),
                Some("h" | "р") => {
                    self.show_hidden = !self.show_hidden;
                    self.refilter();
                }
                Some("f" | "а") => self.search_focus = true,
                Some("l" | "д") => self.search_focus = true,
                Some("1") => self.set_view(View::Icons),
                Some("2") => self.set_view(View::List),
                Some("n" | "т") if mods.shift => return self.new_folder(),
                Some("w" | "ц" | "q" | "й") => return vec![Effect::Close],
                Some("r" | "к") => return self.reload(),
                _ => {}
            },
            _ => {
                // Type-ahead: jump to the first name starting with the typed letters
                if let Some(t) = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control)) {
                    let (buffer, at) = &mut self.type_ahead;
                    if at.elapsed() > Duration::from_millis(900) {
                        buffer.clear();
                    }
                    buffer.push_str(&t.to_lowercase());
                    *at = Instant::now();
                    let prefix = buffer.clone();
                    if let Some(i) = (0..self.visible.len()).find(|&i| self.entry(i).name.to_lowercase().starts_with(&prefix)) {
                        self.select_only(i);
                    }
                }
            }
        }
        Vec::new()
    }

    fn select_only(&mut self, i: usize) {
        self.selected = HashSet::from([self.entry(i).path.clone()]);
        self.cursor = Some(i);
        self.anchor = Some(i);
        self.scroll_to(i);
    }

    fn move_cursor(&mut self, sym: smithay_client_toolkit::seat::keyboard::Keysym, extend: bool) {
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        let n = self.visible.len();
        if n == 0 {
            return;
        }
        let cols = if self.view == View::Icons { self.columns() } else { 1 };
        let page = ((self.content().3 / if self.view == View::Icons { CELL_H } else { ROW_H }) as usize).max(1) * cols;
        let next = match (self.cursor, sym) {
            (None, K::End) => n - 1,
            (None, _) => 0,
            (Some(c), K::Left) if self.view == View::Icons => c.saturating_sub(1),
            (Some(c), K::Right) if self.view == View::Icons => (c + 1).min(n - 1),
            (Some(c), K::Up) => c.saturating_sub(cols),
            (Some(c), K::Down) => (c + cols).min(n - 1),
            (Some(c), K::Page_Up) => c.saturating_sub(page),
            (Some(c), K::Page_Down) => (c + page).min(n - 1),
            (Some(_), K::Home) => 0,
            (Some(_), K::End) => n - 1,
            (Some(c), _) => c,
        };
        if extend {
            let a = *self.anchor.get_or_insert(self.cursor.unwrap_or(next));
            self.selected = (a.min(next)..=a.max(next)).map(|j| self.entry(j).path.clone()).collect();
            self.cursor = Some(next);
            self.scroll_to(next);
        } else {
            self.select_only(next);
        }
    }

    /// Clears an expired status message; true if a repaint is needed
    pub fn tick(&mut self) -> bool {
        if self.status.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4)) {
            self.status = None;
            self.dirty = true;
        }
        self.dirty
    }

    pub fn status_deadline(&self) -> Option<Instant> {
        self.status.as_ref().map(|(_, t)| *t + Duration::from_secs(4))
    }

    // ---------- painting ----------

    pub fn paint(&mut self, pm: &mut PixmapMut) {
        self.dirty = false;
        pm.fill(colors::window());
        self.paint_content(pm);
        self.paint_toolbar(pm);
        self.paint_pathbar(pm);
        self.paint_sidebar(pm);
        self.paint_menu(pm);
        self.paint_confirm(pm);
        self.paint_props(pm);
        self.paint_picker(pm);
        self.paint_preview(pm);
    }

    fn paint_sidebar(&mut self, pm: &mut PixmapMut) {
        // Floating sidebar panel, as in Tahoe
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
        let mut section = "";
        let rects = self.place_rects();
        for (i, py) in rects {
            let place = &self.places[i];
            if place.section != section {
                section = place.section;
                self.fonts.draw_text(pm, section, 22.0, py - 19.0, 11.0, colors::tertiary(), true);
            }
            let active = self.dir == place.path || self.drop_target.as_ref() == Some(&place.path);
            if active {
                theme::fill_rrect(pm, 14.0, py, SIDEBAR_W - 28.0, PLACE_H - 2.0, 7.0, colors::selected_gray());
            } else if self.hover == Hit::Place(i) {
                theme::fill_rrect(pm, 14.0, py, SIDEBAR_W - 28.0, PLACE_H - 2.0, 7.0, colors::hover());
            }
            draw_glyph(pm, place.glyph, 24.0, py + 5.0, 16.0, colors::accent());
            let name = self.fonts.ellipsize(&place.name, SIDEBAR_W - 84.0, 13.0, false);
            self.fonts.draw_text(pm, &name, 48.0, py + 5.5, 13.0, colors::text(), false);
        }
    }

    fn paint_toolbar(&mut self, pm: &mut PixmapMut) {
        let x = SIDEBAR_W;
        theme::fill_rect(pm, x, 0.0, self.w - x, TOOLBAR_H, colors::window());
        // Back/forward capsule
        theme::fill_rrect(pm, x + 10.0, 12.0, 64.0, 28.0, 14.0, colors::control());
        for (hit, bx, enabled, dir) in [
            (Hit::Back, x + 10.0, self.hist_pos > 0, -1.0),
            (Hit::Forward, x + 42.0, self.hist_pos + 1 < self.history.len(), 1.0),
        ] {
            if enabled && self.hover == hit {
                theme::fill_rrect(pm, bx + 2.0, 14.0, 28.0, 24.0, 12.0, colors::hover());
            }
            let ink = if enabled { colors::text() } else { colors::tertiary() };
            let cx = bx + 16.0;
            theme::polyline(pm, &[(cx - 2.5 * dir, 21.0), (cx + 2.5 * dir, 26.0), (cx - 2.5 * dir, 31.0)], 1.8, ink);
        }
        if let Some(input) = self.path_edit.clone() {
            let (tx, ty, tw, th) = self.title_rect();
            theme::fill_rrect(pm, tx, ty, tw, th, 8.0, colors::window());
            theme::stroke_rrect(pm, tx - 1.0, ty - 1.0, tw + 2.0, th + 2.0, 9.0, 2.5, rgba(10, 132, 255, 110));
            theme::stroke_rrect(pm, tx, ty, tw, th, 8.0, 0.6, colors::separator());
            input.draw(pm, &mut self.fonts, tx + 10.0, ty + 7.0, tw - 20.0, 13.0, false, colors::text(), true, colors::accent());
        } else {
            let title_w = self.w - 196.0 - 84.0 - (x + 86.0) - 10.0;
            let title = self.fonts.ellipsize(&self.title(), title_w, 15.0, true);
            self.fonts.draw_text(pm, &title, x + 86.0, 16.5, 15.0, colors::text(), true);
        }
        // View switch
        let [(_, (vx, vy, _, _)), _] = self.view_rects();
        theme::fill_rrect(pm, vx, vy, 72.0, 28.0, 14.0, colors::control());
        for (hit, (rx, ry, rw, rh)) in self.view_rects() {
            let active = (hit == Hit::ViewIcons) == (self.view == View::Icons);
            if active {
                theme::fill_rrect(pm, rx + 2.0, ry + 2.0, rw - 4.0, rh - 4.0, 12.0, colors::window());
                theme::stroke_rrect(pm, rx + 2.0, ry + 2.0, rw - 4.0, rh - 4.0, 12.0, 0.6, colors::separator());
            } else if self.hover == hit {
                theme::fill_rrect(pm, rx + 2.0, ry + 2.0, rw - 4.0, rh - 4.0, 12.0, colors::hover());
            }
            let ink = colors::text();
            let (cx, cy) = (rx + rw * 0.5, ry + rh * 0.5);
            if hit == Hit::ViewIcons {
                for (dx, dy) in [(-4.5, -4.5), (1.0, -4.5), (-4.5, 1.0), (1.0, 1.0)] {
                    theme::fill_rrect(pm, cx + dx, cy + dy, 3.8, 3.8, 1.0, ink);
                }
            } else {
                for dy in [-4.0, 0.0, 4.0] {
                    theme::fill_circle(pm, cx - 5.0, cy + dy, 1.0, ink);
                    theme::line(pm, cx - 2.5, cy + dy, cx + 5.5, cy + dy, 1.4, ink);
                }
            }
        }
        // Search field
        let (sx, sy, sw, sh) = self.search_rect();
        theme::fill_rrect(pm, sx, sy, sw, sh, 14.0, colors::control());
        if self.search_focus {
            theme::stroke_rrect(pm, sx - 1.0, sy - 1.0, sw + 2.0, sh + 2.0, 15.0, 2.5, rgba(10, 132, 255, 110));
        }
        let ink = colors::secondary();
        theme::stroke_circle(pm, sx + 15.0, sy + 13.0, 4.5, 1.5, ink);
        theme::line(pm, sx + 18.5, sy + 16.5, sx + 21.5, sy + 19.5, 1.6, ink);
        if self.search.is_empty() && !self.search_focus {
            self.fonts.draw_text(pm, "Поиск", sx + 28.0, sy + 6.5, 13.0, colors::tertiary(), false);
        } else {
            self.search.draw(pm, &mut self.fonts, sx + 28.0, sy + 6.5, sw - 40.0, 13.0, false, colors::text(), self.search_focus, colors::accent());
        }
        theme::fill_rect(pm, x, TOOLBAR_H - 1.0, self.w - x, 1.0, colors::separator());
    }

    fn paint_content(&mut self, pm: &mut PixmapMut) {
        let (cx, cy, cw, ch) = self.content();
        if let Some(err) = self.error.clone() {
            self.fonts.draw_text_centered(pm, "Не удаётся открыть папку", cx + cw * 0.5, cy + ch * 0.4, 15.0, colors::secondary(), true);
            let err = self.fonts.ellipsize(&err, cw - 40.0, 12.0, false);
            self.fonts.draw_text_centered(pm, &err, cx + cw * 0.5, cy + ch * 0.4 + 24.0, 12.0, colors::tertiary(), false);
            return;
        }
        if self.visible.is_empty() && !self.loading {
            let text = if self.search.is_empty() { "Папка пуста" } else { "Ничего не найдено" };
            self.fonts.draw_text_centered(pm, text, cx + cw * 0.5, cy + ch * 0.42, 14.0, colors::tertiary(), false);
        }
        match self.view {
            View::Icons => self.paint_icons(pm),
            View::List => self.paint_list(pm),
        }
        if let Some((start, end)) = self.rubber {
            let (x0, x1) = (start.0.min(end.0), start.0.max(end.0));
            let sy = start.1 - self.scroll;
            let (y0, y1) = (sy.min(end.1).max(cy), sy.max(end.1).min(cy + ch));
            if y1 > y0 {
                theme::fill_rect(pm, x0, y0, x1 - x0, y1 - y0, rgba(10, 132, 255, 38));
                theme::stroke_rrect(pm, x0 + 0.5, y0 + 0.5, x1 - x0 - 1.0, y1 - y0 - 1.0, 0.0, 1.0, rgba(10, 132, 255, 170));
            }
        }
        if self.drop_target.as_ref() == Some(&self.dir) {
            theme::stroke_rrect(pm, cx + 4.0, cy + 4.0, cw - 8.0, ch - 8.0, 10.0, 3.0, rgba(10, 132, 255, 150));
        }
        // Thin overlay scrollbar when the folder does not fit
        let total = self.content_height();
        if total > ch + 1.0 {
            let track = ch - 8.0;
            let thumb = (track * ch / total).max(28.0);
            let y = cy + 4.0 + (track - thumb) * self.scroll / (total - ch);
            theme::fill_rrect(pm, self.w - 9.0, y, 5.0, thumb, 2.5, colors::secondary());
        }
    }

    fn visible_range(&self) -> std::ops::Range<usize> {
        let (_, _, _, ch) = self.content();
        match self.view {
            View::Icons => {
                let cols = self.columns();
                let first = ((self.scroll - 10.0) / CELL_H).floor().max(0.0) as usize * cols;
                let rows = (ch / CELL_H).ceil() as usize + 2;
                first.min(self.visible.len())..(first + rows * cols).min(self.visible.len())
            }
            View::List => {
                let first = (self.scroll / ROW_H).floor().max(0.0) as usize;
                first.min(self.visible.len())..(first + (ch / ROW_H) as usize + 2).min(self.visible.len())
            }
        }
    }

    fn icon_for(&self, e: &Entry) -> Icon {
        if e.is_dir {
            Icon::Folder(self.special(&e.path))
        } else {
            Icon::File(e.kind, e.ext.clone())
        }
    }

    fn paint_icons(&mut self, pm: &mut PixmapMut) {
        let (_, cy, _, ch) = self.content();
        for i in self.visible_range() {
            let (x, y, w, _) = self.item_rect(i);
            if y > cy + ch || y + CELL_H < cy {
                continue;
            }
            let e = self.entry(i).clone();
            let selected = self.selected.contains(&e.path) || self.drop_target.as_ref() == Some(&e.path);
            let icon_x = x + (w - GRID_ICON) * 0.5;
            if selected {
                theme::fill_rrect(pm, icon_x - 6.0, y + 2.0, GRID_ICON + 12.0, GRID_ICON + 8.0, 8.0, colors::selected_gray());
            }
            let icon = self.icon_for(&e);
            self.icons.draw(pm, &mut self.fonts, &icon, icon_x, y + 6.0, GRID_ICON);
            let renaming = self.rename.as_ref().filter(|(p, _)| *p == e.path).map(|(_, n)| n.clone());
            let name_y = y + 10.0 + GRID_ICON;
            if let Some(name) = renaming {
                theme::fill_rrect(pm, x + 5.0, name_y - 2.0, w - 10.0, 19.0, 4.0, colors::window());
                theme::stroke_rrect(pm, x + 5.0, name_y - 2.0, w - 10.0, 19.0, 4.0, 2.0, rgba(10, 132, 255, 160));
                name.draw(pm, &mut self.fonts, x + 9.0, name_y, w - 18.0, 12.0, false, colors::text(), true, colors::accent());
                continue;
            }
            let lines = self.fonts.wrap_two_lines(&e.name, w - 10.0, 12.0, false);
            for (k, line) in lines.iter().enumerate() {
                let ly = name_y + k as f32 * 15.0;
                let tw = self.fonts.measure_text(line, 12.0, false);
                let ink = if selected {
                    theme::fill_rrect(pm, x + (w - tw) * 0.5 - 4.0, ly - 1.0, tw + 8.0, 16.0, 4.0, if self.focused { colors::accent() } else { colors::inactive_selection() });
                    if self.focused { rgba(255, 255, 255, 255) } else { colors::text() }
                } else if e.hidden {
                    colors::secondary()
                } else {
                    colors::text()
                };
                self.fonts.draw_text_centered(pm, line, x + w * 0.5, ly, 12.0, ink, false);
            }
        }
    }

    fn paint_list(&mut self, pm: &mut PixmapMut) {
        let (cx, cy, cw, ch) = self.content();
        let date_w = 160.0;
        let size_w = 90.0;
        let kind_w = if cw > 640.0 { 130.0 } else { 0.0 };
        let name_w = cw - date_w - size_w - kind_w - 40.0;
        let col_date = cx + 20.0 + name_w;
        let col_size = col_date + date_w;
        let col_kind = col_size + size_w;
        for i in self.visible_range() {
            let (_, y, _, _) = self.item_rect(i);
            if y + ROW_H < cy + HEADER_H || y > cy + ch {
                continue;
            }
            let e = self.entry(i).clone();
            let selected = self.selected.contains(&e.path) || self.drop_target.as_ref() == Some(&e.path);
            if selected {
                let fill = if self.focused { colors::accent() } else { colors::inactive_selection() };
                theme::fill_rrect(pm, cx + 8.0, y, cw - 16.0, ROW_H, 6.0, fill);
            } else if i % 2 == 1 {
                theme::fill_rrect(pm, cx + 8.0, y, cw - 16.0, ROW_H, 6.0, colors::stripe());
            }
            let (ink, sub) = if selected && self.focused {
                (rgba(255, 255, 255, 255), rgba(255, 255, 255, 220))
            } else {
                (if e.hidden { colors::secondary() } else { colors::text() }, colors::secondary())
            };
            let icon = self.icon_for(&e);
            self.icons.draw(pm, &mut self.fonts, &icon, cx + 18.0, y + 4.0, 18.0);
            let renaming = self.rename.as_ref().filter(|(p, _)| *p == e.path).map(|(_, n)| n.clone());
            if let Some(name) = renaming {
                theme::fill_rrect(pm, cx + 40.0, y + 3.0, name_w - 30.0, ROW_H - 6.0, 4.0, colors::window());
                theme::stroke_rrect(pm, cx + 40.0, y + 3.0, name_w - 30.0, ROW_H - 6.0, 4.0, 2.0, rgba(10, 132, 255, 160));
                name.draw(pm, &mut self.fonts, cx + 45.0, y + 5.5, name_w - 40.0, 13.0, false, colors::text(), true, colors::accent());
            } else {
                self.fonts.draw_text_clipped(pm, &e.name, cx + 44.0, y + 5.5, name_w - 30.0, 13.0, ink, false);
            }
            let date = fsops::human_date(e.modified);
            self.fonts.draw_text_clipped(pm, &date, col_date, y + 5.5, date_w - 10.0, 12.0, sub, false);
            let size = if e.is_dir { "—".to_string() } else { fsops::human_size(e.size) };
            self.fonts.draw_text_right(pm, &size, col_size + size_w - 18.0, y + 5.5, 12.0, sub, false);
            if kind_w > 0.0 {
                self.fonts.draw_text_clipped(pm, e.kind.label(), col_kind, y + 5.5, kind_w - 10.0, 12.0, sub, false);
            }
        }
        // Sticky header over the scrolled rows
        theme::fill_rect(pm, cx, cy, cw, HEADER_H, colors::window());
        for (label, x) in [("Имя", cx + 44.0), ("Дата изменения", col_date), ("Размер", col_size + 4.0), ("Тип", col_kind)] {
            if label == "Тип" && kind_w == 0.0 {
                continue;
            }
            self.fonts.draw_text(pm, label, x, cy + 7.0, 11.5, colors::secondary(), true);
        }
        theme::fill_rect(pm, cx + 8.0, cy + HEADER_H - 1.0, cw - 16.0, 1.0, colors::separator());
    }

    fn paint_pathbar(&mut self, pm: &mut PixmapMut) {
        let y = self.h - PATHBAR_H-self.picker_height();
        theme::fill_rect(pm, SIDEBAR_W, y, self.w - SIDEBAR_W, PATHBAR_H, colors::window());
        theme::fill_rect(pm, SIDEBAR_W, y, self.w - SIDEBAR_W, 1.0, colors::separator());
        self.layout_crumbs();
        let crumbs = self.crumb_layout.clone();
        let last = crumbs.len().saturating_sub(1);
        for (k, (path, name, x, _)) in crumbs.iter().enumerate() {
            let glyph = if *path == fsops::home() { Glyph::Home } else if path.parent().is_none() { Glyph::Computer } else { Glyph::Folder };
            let ink = if self.hover == Hit::Crumb(path.clone()) { colors::accent() } else if k == last { colors::text() } else { colors::secondary() };
            draw_glyph(pm, glyph, *x, y + 8.0, 12.0, if glyph == Glyph::Folder { rgba(76, 155, 228, 255) } else { ink });
            self.fonts.draw_text(pm, name, x + 16.0, y + 7.5, 11.5, ink, k == last);
            if k < last {
                let w = self.fonts.measure_text(name, 11.5, k == last);
                let sx = x + 16.0 + w + 5.0;
                theme::polyline(pm, &[(sx, y + 10.5), (sx + 3.0, y + 14.0), (sx, y + 17.5)], 1.2, colors::tertiary());
            }
        }
        let info = if let Some((text, _)) = &self.status {
            text.clone()
        } else if self.loading {
            "Загрузка…".into()
        } else if !self.selected.is_empty() {
            format!("Выбрано {} из {}", self.selected.len(), self.visible.len())
        } else {
            objects(self.visible.len())
        };
        let info = self.fonts.ellipsize(&info, 260.0, 11.5, false);
        let mut right = self.w - 14.0;
        if let Some((bx, by, bw, bh)) = self.empty_button() {
            let hover = self.hover == Hit::EmptyTrash;
            theme::fill_rrect(pm, bx, by, bw, bh, bh / 2.0, if hover { colors::selected_gray() } else { colors::control() });
            self.fonts.draw_text_centered(pm, "Очистить", bx + bw / 2.0, by + (bh - 11.5) / 2.0 - 1.0, 11.5, colors::text(), false);
            right = bx - 10.0;
        }
        self.fonts.draw_text_right(pm, &info, right, y + 7.5, 11.5, colors::secondary(), false);
    }

    fn paint_preview(&mut self, pm: &mut PixmapMut) {
        let Some(preview) = self.preview.as_ref() else { return; };
        let (x, y, w, h) = self.preview_rect();
        theme::fill_rect(pm, 0.0, 0.0, self.w, self.h, rgba(0, 0, 0, 70));
        for i in 1..=6 {
            let e = i as f32 * 2.0;
            theme::fill_rrect(pm, x - e, y - e + 6.0, w + e * 2.0, h + e * 2.0, 14.0 + e, rgba(0, 0, 0, 7));
        }
        theme::fill_rrect(pm, x, y, w, h, 14.0, colors::window());
        theme::stroke_rrect(pm, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 13.5, 1.0, colors::separator());
        theme::line(pm, x + 20.0, y + 19.0, x + 28.0, y + 27.0, 1.5, colors::secondary());
        theme::line(pm, x + 28.0, y + 19.0, x + 20.0, y + 27.0, 1.5, colors::secondary());
        let name = preview.path.file_name().unwrap_or_default().to_string_lossy();
        self.fonts.draw_text_clipped_centered(pm, &name, x + w * 0.5, y + 16.0, w - 100.0, 14.0, colors::text(), true);
        theme::fill_rect(pm, x + 1.0, y + 46.0, w - 2.0, 1.0, colors::separator());
        let (bx, by, bw, bh) = (x + 20.0, y + 60.0, w - 40.0, h - 116.0);
        let mut detail = "Пробел — закрыть · ← → — другой файл".to_string();
        match preview.result.as_ref() {
            None => { self.fonts.draw_text_centered(pm, "Загрузка…", x + w * 0.5, y + h * 0.5, 14.0, colors::secondary(), false); }
            Some(Err(error)) => { self.fonts.draw_text_clipped_centered(pm, error, x + w * 0.5, y + h * 0.5, bw, 13.0, colors::secondary(), false); }
            Some(Ok(crate::preview::Content::Info(info))) => { self.fonts.draw_text_clipped_centered(pm, info, x + w * 0.5, y + h * 0.5, bw, 14.0, colors::secondary(), false); }
            Some(Ok(crate::preview::Content::Text(lines, truncated))) => {
                if lines.is_empty() { self.fonts.draw_text_centered(pm, "Пустой файл", x + w * 0.5, y + h * 0.5, 14.0, colors::secondary(), false); }
                for (row, text) in lines.iter().skip(preview.scroll).take((bh / 18.0) as usize).enumerate() {
                    self.fonts.draw_text_clipped(pm, text, bx, by + row as f32 * 18.0, bw, 12.5, colors::text(), false);
                }
                if *truncated { detail = "Показана начальная часть файла".into(); }
            }
            Some(Ok(crate::preview::Content::Image(image, iw, ih))) => {
                let scale = (bw / image.width() as f32).min(bh / image.height() as f32);
                let (dw, dh) = (image.width() as f32 * scale, image.height() as f32 * scale);
                let (dx, dy) = (bx + (bw - dw) * 0.5, by + (bh - dh) * 0.5);
                let paint = tiny_skia::Paint { shader: tiny_skia::Pattern::new(image.as_ref(), tiny_skia::SpreadMode::Pad, tiny_skia::FilterQuality::Bilinear, 1.0, tiny_skia::Transform::from_scale(scale, scale).post_translate(dx, dy)), ..Default::default() };
                if let Some(rect) = tiny_skia::Rect::from_xywh(dx, dy, dw, dh) {
                    pm.fill_rect(rect, &paint, tiny_skia::Transform::from_scale(theme::ui_scale(), theme::ui_scale()), None);
                }
                detail = format!("{iw} × {ih}");
            }
        }
        self.fonts.draw_text_clipped(pm, &detail, x + 20.0, y + h - 29.0, w - 230.0, 10.5, colors::secondary(), false);
        theme::fill_rrect(pm, x + w - 190.0, y + h - 39.0, 174.0, 29.0, 8.0, colors::control());
        self.fonts.draw_text_centered(pm, if self.picker.is_some() { "Открыть" } else { "Открыть в приложении" }, x + w - 103.0, y + h - 32.0, 11.5, colors::text(), false);
    }

    fn paint_props(&mut self, pm: &mut PixmapMut) {
        let Some(p) = self.props.clone() else { return };
        theme::fill_rect(pm, 0.0, 0.0, self.w, self.h, rgba(0, 0, 0, 40));
        let (x, y, w, h) = self.props_rect();
        for i in 1..=6 {
            let e = i as f32 * 2.0;
            theme::fill_rrect(pm, x - e, y - e + 6.0, w + 2.0 * e, h + 2.0 * e, 14.0 + e, rgba(0, 0, 0, 7));
        }
        theme::fill_rrect(pm, x, y, w, h, 14.0, colors::glass());
        theme::stroke_rrect(pm, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 13.5, 1.0, colors::separator());
        let name = p.path.file_name().map_or_else(|| p.path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
        let glyph = if p.is_dir { Glyph::Folder } else { Glyph::Documents };
        draw_glyph(pm, glyph, x + 20.0, y + 20.0, 32.0, rgba(76, 155, 228, 255));
        let title = self.fonts.ellipsize(&name, w - 84.0, 15.0, true);
        self.fonts.draw_text(pm, &title, x + 64.0, y + 20.0, 15.0, colors::text(), true);
        let size_text = match p.size {
            Some(bytes) => {
                let human = human_size(bytes);
                let exact = format!("{bytes} байт").chars().rev().collect::<Vec<_>>().chunks(3).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ").chars().rev().collect::<String>();
                format!("{}{human} ({exact})", if p.partial { "более " } else { "" })
            }
            None => "Подсчёт…".into(),
        };
        self.fonts.draw_text(pm, &size_text, x + 64.0, y + 40.0, 12.0, colors::secondary(), false);
        theme::fill_rect(pm, x + 20.0, y + 70.0, w - 40.0, 1.0, colors::separator());
        let date = |t: Option<std::time::SystemTime>| t.map_or("—".into(), |t| chrono::DateTime::<chrono::Local>::from(t).format("%d.%m.%Y, %H:%M").to_string());
        let mode = p.mode;
        let bits: String = (0..9).map(|i| if mode & (0o400 >> i) != 0 { b"rwxrwxrwx"[i] as char } else { '-' }).collect();
        let mut rows = vec![("Тип", p.kind.clone())];
        if p.is_dir {
            rows.push(("Содержит", p.items.map_or("Подсчёт…".into(), |n| format!("{}{}", if p.partial { "более " } else { "" }, objects(n as usize)))));
        }
        rows.extend([
            ("Расположение", p.path.parent().map_or("/".into(), |d| d.to_string_lossy().into_owned())),
            ("Создан", date(p.created)),
            ("Изменён", date(p.modified)),
            ("Открыт", date(p.accessed)),
            ("Права", format!("{bits}  ·  {}", p.owner)),
        ]);
        for (i, (k, v)) in rows.iter().enumerate() {
            let ry = y + 84.0 + i as f32 * 24.0;
            self.fonts.draw_text_right(pm, k, x + 128.0, ry, 12.0, colors::secondary(), false);
            let v = self.fonts.ellipsize(v, w - 160.0, 12.0, false);
            self.fonts.draw_text(pm, &v, x + 140.0, ry, 12.0, colors::text(), false);
        }
        let (bx, by) = (x + w - 116.0, y + h - 46.0);
        theme::fill_rrect(pm, bx, by, 96.0, 30.0, 15.0, colors::accent());
        if self.hover == Hit::PropsClose {
            theme::fill_rrect(pm, bx, by, 96.0, 30.0, 15.0, rgba(255, 255, 255, 36));
        }
        self.fonts.draw_text_centered(pm, "Готово", bx + 48.0, by + 7.5, 13.0, rgba(255, 255, 255, 255), false);
    }

    fn paint_confirm(&mut self, pm: &mut PixmapMut) {
        let Some(confirm) = self.confirm.clone() else { return };
        theme::fill_rect(pm, 0.0, 0.0, self.w, self.h, rgba(0, 0, 0, 40));
        let ((x, y, w, h), cancel, ok) = self.confirm_rects();
        for i in 1..=6 {
            let e = i as f32 * 2.0;
            theme::fill_rrect(pm, x - e, y - e + 6.0, w + 2.0 * e, h + 2.0 * e, 14.0 + e, rgba(0, 0, 0, 7));
        }
        theme::fill_rrect(pm, x, y, w, h, 14.0, colors::glass());
        theme::stroke_rrect(pm, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 13.5, 1.0, colors::separator());
        let (title, body, action) = match &confirm {
            Confirm::Delete(paths) if paths.len() == 1 => (
                format!("Удалить «{}» безвозвратно?", paths[0].file_name().unwrap_or_default().to_string_lossy()),
                "Объект будет удалён сразу. Это действие нельзя отменить.".to_string(),
                "Удалить",
            ),
            Confirm::Delete(paths) => (
                format!("Удалить безвозвратно {}?", objects(paths.len())),
                "Объекты будут удалены сразу. Это действие нельзя отменить.".to_string(),
                "Удалить",
            ),
            Confirm::Empty => (
                "Удалить объекты из Корзины безвозвратно?".to_string(),
                format!("{} будут удалены. Это действие нельзя отменить.", objects(self.entries.len())),
                "Очистить Корзину",
            ),
        };
        self.fonts.draw_text_clipped(pm, &title, x + 20.0, y + 20.0, w - 40.0, 14.0, colors::text(), true);
        for (i, line) in self.fonts.wrap_two_lines(&body, w - 40.0, 12.0, false).iter().enumerate() {
            self.fonts.draw_text(pm, line, x + 20.0, y + 48.0 + i as f32 * 16.0, 12.0, colors::secondary(), false);
        }
        let (cx, cy, cw, ch) = cancel;
        theme::fill_rrect(pm, cx, cy, cw, ch, ch / 2.0, colors::control());
        self.fonts.draw_text_centered(pm, "Отменить", cx + cw / 2.0, cy + 7.5, 13.0, colors::text(), false);
        let (ax, ay, aw, ah) = ok;
        theme::fill_rrect(pm, ax, ay, aw, ah, ah / 2.0, colors::accent());
        self.fonts.draw_text_centered(pm, action, ax + aw / 2.0, ay + 7.5, 13.0, rgba(255, 255, 255, 255), false);
    }

    fn paint_menu(&mut self, pm: &mut PixmapMut) {
        let Some(menu) = self.menu.as_ref() else { return };
        let (x, y, w, h) = self.menu_rect(menu);
        let items = menu.items.clone();
        theme::fill_rrect(pm, x, y + 3.0, w, h, 10.0, rgba(0, 0, 0, 30));
        theme::fill_rrect(pm, x, y, w, h, 10.0, colors::glass());
        theme::stroke_rrect(pm, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 9.5, 1.0, colors::separator());
        let mut iy = y + 6.0;
        for (i, (label, action)) in items.iter().enumerate() {
            if action.is_none() {
                theme::fill_rect(pm, x + 12.0, iy + 4.0, w - 24.0, 1.0, colors::separator());
                iy += 9.0;
                continue;
            }
            let hovered = self.menu_hover == Some(i);
            if hovered {
                theme::fill_rrect(pm, x + 5.0, iy, w - 10.0, 24.0, 6.0, colors::accent());
            }
            let ink = if hovered { rgba(255, 255, 255, 255) } else { colors::text() };
            self.fonts.draw_text(pm, label, x + 16.0, iy + 4.5, 13.0, ink, false);
            iy += 24.0;
        }
    }
}

fn latin(sym: smithay_client_toolkit::seat::keyboard::Keysym) -> Option<&'static str> {
    use smithay_client_toolkit::seat::keyboard::Keysym as K;
    Some(match sym {
        K::a | K::A => "a",
        K::c | K::C => "c",
        K::x | K::X => "x",
        K::v | K::V => "v",
        K::h | K::H => "h",
        K::f | K::F => "f",
        K::l | K::L => "l",
        K::n | K::N => "n",
        K::w | K::W => "w",
        K::q | K::Q => "q",
        K::r | K::R => "r",
        K::_1 => "1",
        K::_2 => "2",
        _ => return None,
    })
}

/// 1536 → "1,5 КБ"
pub fn human_size(bytes: u64) -> String {
    let units = ["байт", "КБ", "МБ", "ГБ", "ТБ"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} байт") } else { format!("{:.1} {}", v, units[u]).replace('.', ",") }
}

/// "1 объект", "3 объекта", "12 объектов"
pub fn objects(n: usize) -> String {
    let word = match (n % 10, n % 100) {
        (1, r) if r != 11 => "объект",
        (2..=4, r) if !(12..=14).contains(&r) => "объекта",
        _ => "объектов",
    };
    format!("{n} {word}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_in(dir: &Path) -> App {
        let (mut app, effects) = App::new(FontSystem::new(), dir.to_path_buf(), None, false);
        for e in effects {
            if let Effect::List(path, generation) = e {
                app.listed(generation, fsops::list(&path).map_err(|e| e.to_string()));
            }
        }
        app
    }

    fn run(app: &mut App, effects: Vec<Effect>) {
        for e in effects {
            if let Effect::List(path, generation) = e {
                app.listed(generation, fsops::list(&path).map_err(|e| e.to_string()));
            }
        }
    }

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("feather-files-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Альфа")).unwrap();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        for name in ["file2.txt", "file10.txt", "photo.png", ".hidden"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        root
    }

    #[test]
    fn native_picker_filters_multiple_directories_save_confirmation_and_cancel() {
        use feather_ui::picker::{Config,Filter,Mode};
        let root=fixture("picker");
        let mut app=app_in(&root);
        app.set_picker(Config {filters:vec![Filter{name:"PNG".into(),rules:vec![(1,"image/png".into())]}],..Default::default()});
        assert!(app.visible.iter().all(|&i|app.entries[i].is_dir || app.entries[i].name=="photo.png"));
        app.selected.insert(root.join("photo.png"));
        assert!(matches!(app.picker_accept().as_slice(),[Effect::Picked(s)] if s.paths.len()==1));
        app.set_picker(Config{mode:Mode::Directory,multiple:true,..Default::default()});
        app.selected.clear(); app.selected.extend([root.join("Альфа"),root.join("beta")]);
        assert!(matches!(app.picker_accept().as_slice(),[Effect::Picked(s)] if s.paths.len()==2));
        app.set_picker(Config{mode:Mode::Save,name:"file2.txt".into(),..Default::default()});
        assert!(matches!(app.picker_accept().as_slice(),[Effect::CheckSave(_)]));
        assert!(app.save_checked(root.join("file2.txt"),Ok(Some(false))).is_empty());
        assert!(matches!(app.picker_accept().as_slice(),[Effect::Picked(s)] if s.paths[0]==feather_ui::picker::path_bytes(&root.join("file2.txt"))));
        let p=app.picker.as_mut().unwrap();p.name="  spaces.txt  ".into();
        let effects=app.picker_accept();
        assert!(matches!(effects.as_slice(),[Effect::CheckSave(p)] if p.file_name().unwrap()=="  spaces.txt  "));
        assert!(app.save_checked(root.join("stale"),Ok(None)).is_empty());
        assert!(matches!(app.save_checked(root.join("  spaces.txt  "),Ok(None)).as_slice(),[Effect::Picked(_)]));
        app.set_picker(Config::default());
        assert!(matches!(app.picker_key(smithay_client_toolkit::seat::keyboard::Keysym::Escape,None,Mods::default()).unwrap().as_slice(),[Effect::Close]));
        assert_eq!(std::fs::read_to_string(root.join("file2.txt")).unwrap(),"file2.txt");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// FEATHER_FILES_PREVIEW=/dir cargo test -p feather-files preview -- --ignored
    #[test]
    #[ignore = "writes preview PNGs"]
    fn preview() {
        let Some(out) = std::env::var_os("FEATHER_FILES_PREVIEW").map(PathBuf::from) else { return };
        let mut app = app_in(&fsops::home());
        app.resize(1000.0, 640.0);
        theme::set_ui_scale(2.0);
        let mut pm = tiny_skia::Pixmap::new(2000, 1280).unwrap();
        app.select_only(2.min(app.visible.len().saturating_sub(1)));
        app.hover = Hit::Place(3);
        app.paint(&mut pm.as_mut());
        pm.save_png(out.join("icons.png")).unwrap();
        app.set_view(View::List);
        app.paint(&mut pm.as_mut());
        pm.save_png(out.join("list.png")).unwrap();
        app.set_view(View::Icons);
        app.open_menu(Hit::Item(1), 520.0, 200.0);
        app.menu_hover = Some(3);
        app.paint(&mut pm.as_mut());
        pm.save_png(out.join("menu.png")).unwrap();
        app.menu = None;
        for view in [View::Icons, View::List] {
            app.set_view(view);
            app.paint(&mut pm.as_mut());
            let t = Instant::now();
            for _ in 0..50 {
                app.paint(&mut pm.as_mut());
            }
            eprintln!("{view:?}: {:.2} мс на полный кадр 2000×1280", t.elapsed().as_secs_f64() * 1e3 / 50.0);
        }
        theme::set_ui_scale(1.0);
    }

    #[test]
    fn plural_forms() {
        assert_eq!([objects(1), objects(3), objects(5), objects(11), objects(22)], ["1 объект", "3 объекта", "5 объектов", "11 объектов", "22 объекта"]);
    }

    #[test]
    fn quick_look_space_arrows_stale_reply_and_text_editing_are_separate() {
        feather_ui::scale::with_render_settings(1.0, feather_ui::scale::Appearance::Light, || {
            let root = fixture("quick-look");
            let mut app = app_in(&root);
            app.resize(1000.0, 640.0);
            let index = (0..app.visible.len()).find(|&i| app.entry(i).name == "file2.txt").unwrap();
            app.select_only(index);
            let effects = app.key(0x20, Some(" "), Mods::default());
            let [Effect::Preview(path, generation)] = effects.as_slice() else { panic!("Space did not request preview") };
            assert_eq!(path, &root.join("file2.txt"));
            let first_generation = *generation;
            app.preview_ready(first_generation, crate::preview::load(path));
            assert!(matches!(app.preview.as_ref().unwrap().result, Some(Ok(crate::preview::Content::Text(_, _)))));
            let mut image = tiny_skia::Pixmap::new(1000, 640).unwrap();
            app.paint(&mut image.as_mut());
            std::fs::create_dir_all("/tmp/feather-inputs").unwrap();
            image.save_png("/tmp/feather-inputs/quick-look-text.png").unwrap();
            let effects = app.key(0xff53, None, Mods::default());
            assert!(matches!(effects.as_slice(), [Effect::Preview(path, _)] if path == &root.join("file10.txt")));
            app.preview_ready(first_generation, Ok(crate::preview::Content::Info("stale".into())));
            assert!(app.preview.as_ref().unwrap().result.is_none());
            app.key(0x20, Some(" "), Mods::default());
            assert!(app.preview.is_none());
            app.preview_ready(first_generation, Ok(crate::preview::Content::Info("closed".into())));
            assert!(app.preview.is_none());
            app.search_focus = true;
            assert!(app.key(0x20, Some(" "), Mods::default()).is_empty());
            assert_eq!(app.search.text(), " ");
            assert!(app.preview.is_none());
            app.key(0xff1b, None, Mods::default());
            app.select_only(index);
            app.start_rename();
            app.key(0x20, Some(" "), Mods::default());
            assert_eq!(app.rename.as_ref().unwrap().1.text(), " ");
            assert!(app.preview.is_none());
            app.key(0xff1b, None, Mods::default());
            let image_path = root.join("photo.png");
            image::RgbImage::from_fn(180, 120, |x, y| image::Rgb([x as u8, y as u8, 160])).save(&image_path).unwrap();
            let index = (0..app.visible.len()).find(|&i| app.entry(i).name == "photo.png").unwrap();
            app.select_only(index);
            app.open_preview();
            let generation = app.preview.as_ref().unwrap().generation;
            app.preview_ready(generation, crate::preview::load(&image_path));
            app.paint(&mut image.as_mut());
            image.save_png("/tmp/feather-inputs/quick-look-image.png").unwrap();
            app.key(0xff1b, None, Mods::default());
            assert!(app.preview.is_none());
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn dark_theme_paints_views_and_rename_at_fractional_scales() {
        let root = fixture("dark-theme");
        std::fs::create_dir_all("/tmp/feather-inputs").unwrap();
        for scale in [1.0, 1.25, 2.0] {
            feather_ui::scale::with_render_settings(scale, feather_ui::scale::Appearance::Dark, || {
                let mut app = app_in(&root);
                app.resize(1000.0, 640.0);
                for view in [View::Icons, View::List] {
                    app.set_view(view);
                    let index = (0..app.visible.len()).find(|&i| app.entry(i).name == "file2.txt").unwrap();
                    app.select_only(index);
                    app.start_rename();
                    let mut image = tiny_skia::Pixmap::new((1000.0 * scale) as u32, (640.0 * scale) as u32).unwrap();
                    app.paint(&mut image.as_mut());
                    let bg = image.pixel((900.0 * scale) as u32, (550.0 * scale) as u32).unwrap();
                    assert!(bg.red() < 60 && bg.green() < 60 && bg.blue() < 60, "white content in dark mode");
                    image.save_png(format!("/tmp/feather-inputs/files-dark-{view:?}-{scale}.png")).unwrap();
                    app.rename = None;
                }
            });
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_and_rename_edit_selection_caret_and_mouse_in_both_views() {
        feather_ui::scale::with_render_settings(1.0, feather_ui::scale::Appearance::Light, || {
        let root = fixture("text-inputs");
        let mut app = app_in(&root);
        app.resize(1000.0, 640.0);
        let ctrl = Mods { ctrl: true, ..Default::default() };
        app.key(0x66, Some("f"), ctrl);
        app.key(0, Some("file"), Mods::default());
        assert_eq!(app.visible.len(), 2);
        app.key(0x61, Some("a"), ctrl);
        assert_eq!(app.search.selected_text(), Some("file"));
        assert_eq!(app.visible.len(), 2, "select-all changed the filter");
        app.key(0, Some("photo"), Mods::default());
        assert_eq!(app.visible.len(), 1);
        let (x, y, _, _) = app.search_rect();
        app.press(BTN_LEFT, x + 28.0, y + 12.0, Mods::default());
        assert_eq!(app.search.caret(), 0);
        app.key(0xffff, None, Mods::default());
        assert_eq!(app.search.text(), "hoto");
        app.key(0xff1b, None, Mods::default());
        for view in [View::Icons, View::List] {
            app.set_view(view);
            let index = (0..app.visible.len()).find(|&i| app.entry(i).name == "file2.txt").unwrap();
            app.select_only(index);
            app.key(0xffbf, None, Mods::default()); // F2
            assert_eq!(app.rename.as_ref().unwrap().1.selected_text(), Some("file2.txt"));
            app.key(0, Some("имя.txt"), Mods::default());
            let (rx, ry, _, _) = app.item_rect(index);
            let (x, y) = if view == View::Icons { (rx + 9.0, ry + 10.0 + GRID_ICON) } else { (SIDEBAR_W + 45.0, ry + 5.5) };
            app.press(BTN_LEFT, x, y + 2.0, Mods::default());
            assert_eq!(app.rename.as_ref().unwrap().1.caret(), 0);
            app.key(0xff53, None, Mods { shift: true, ..Default::default() });
            assert_eq!(app.rename.as_ref().unwrap().1.selected_text(), Some("и"));
            app.key(0, Some("И"), Mods::default());
            assert_eq!(app.rename.as_ref().unwrap().1.text(), "Имя.txt");
            assert!(root.join("file2.txt").exists(), "editing already renamed the file");
            let mut image = tiny_skia::Pixmap::new(1000, 640).unwrap();
            app.paint(&mut image.as_mut());
            std::fs::create_dir_all("/tmp/feather-inputs").unwrap();
            image.save_png(format!("/tmp/feather-inputs/files-{view:?}.png")).unwrap();
            app.key(0xff1b, None, Mods::default());
            assert!(root.join("file2.txt").exists());
        }
        app.start_rename();
        app.key(0, Some("Переименовано.txt"), Mods::default());
        let effects = app.key(0xff0d, None, Mods::default());
        run(&mut app, effects);
        assert!(root.join("Переименовано.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn listing_navigation_selection_and_paint() {
        let root = fixture("nav");
        let mut app = app_in(&root);
        let names: Vec<_> = (0..app.visible.len()).map(|i| app.entry(i).name.clone()).collect();
        assert_eq!(names, ["beta", "Альфа", "file2.txt", "file10.txt", "photo.png"]);
        // Hidden files toggle and type-ahead
        let e = app.key(0x0068, Some("h"), Mods { ctrl: true, ..Default::default() });
        run(&mut app, e);
        assert_eq!(app.visible.len(), 6);
        let e = app.key(0x0062, Some("b"), Mods::default());
        run(&mut app, e);
        assert_eq!(app.selected_paths(), [root.join("beta")]);
        // Enter opens the folder, Backspace returns and reselects it
        let e = app.key(0xff0d, None, Mods::default());
        run(&mut app, e);
        assert_eq!(app.dir(), root.join("beta"));
        let e = app.key(0xff08, None, Mods::default());
        run(&mut app, e);
        assert_eq!(app.dir(), root);
        assert_eq!(app.selected_paths(), [root.join("beta")]);
        // Rubber band over the first grid row selects what it crosses
        app.resize(1000.0, 640.0);
        let (x0, y0) = (SIDEBAR_W + 2.0, TOOLBAR_H + 4.0);
        assert!(app.press(BTN_LEFT, x0, y0, Mods::default()).is_empty());
        app.motion(SIDEBAR_W + 2.0 + CELL_W * 2.2, TOOLBAR_H + 50.0);
        app.release(BTN_LEFT, 0.0, 0.0);
        assert_eq!(app.selected.len(), 2, "{:?}", app.selected);
        // Moving over files does not repaint; over the sidebar it does
        app.dirty = false;
        let (ix, iy, _, _) = app.item_rect(3);
        app.motion(ix + 50.0, iy + 30.0);
        assert!(!app.dirty);
        app.motion(60.0, 200.0);
        assert!(app.dirty);
        // Search filters
        app.search_focus = true;
        for ch in ["f", "i", "l", "e"] {
            app.key(0, Some(ch), Mods::default());
        }
        assert_eq!(app.visible.len(), 2);
        // Paints at 2× without panicking, list view too
        theme::set_ui_scale(2.0);
        let mut pm = tiny_skia::Pixmap::new(2000, 1280).unwrap();
        app.paint(&mut pm.as_mut());
        app.set_view(View::List);
        app.paint(&mut pm.as_mut());
        theme::set_ui_scale(1.0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn new_folder_rename_and_trash_effect() {
        let root = fixture("edit");
        let mut app = app_in(&root);
        let mods = Mods { ctrl: true, shift: true, ..Default::default() };
        assert!(app.key(0x004e, Some("N"), mods).is_empty());
        assert!(root.join("Новая папка").is_dir());
        for _ in 0.."Новая папка".chars().count() {
            app.key(0xff08, None, Mods::default());
        }
        for ch in ["Г", "а", "м", "м", "а"] {
            app.key(0, Some(ch), Mods::default());
        }
        let e = app.key(0xff0d, None, Mods::default());
        run(&mut app, e);
        assert!(root.join("Гамма").is_dir() && !root.join("Новая папка").exists());
        assert_eq!(app.selected_paths(), [root.join("Гамма")]);
        assert_eq!(app.key(0xffff, None, Mods::default()), vec![Effect::Trash(vec![root.join("Гамма")])]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn trash_folder_asks_before_deleting_and_offers_restore() {
        let root = std::env::temp_dir().join(format!("feather-files-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        let trash = fsops::trash_files_dir();
        std::fs::create_dir_all(&trash).unwrap();
        std::fs::write(trash.join("старое.txt"), "x").unwrap();
        let mut app = app_in(&trash);
        assert!(app.in_trash());
        assert!(app.empty_button().is_some());
        let ctrl = Mods { ctrl: true, ..Default::default() };
        app.key(0x0061, Some("a"), ctrl);
        assert_eq!(app.selected_paths(), [trash.join("старое.txt")]);
        // Delete asks; Escape keeps everything.
        assert!(app.key(0xffff, None, Mods::default()).is_empty());
        assert!(app.confirm.is_some());
        assert!(app.key(0xff1b, None, Mods::default()).is_empty());
        assert!(app.confirm.is_none());
        // Delete + Return deletes for good; the menu offers "Вернуть".
        app.key(0xffff, None, Mods::default());
        assert_eq!(app.key(0xff0d, None, Mods::default()), vec![Effect::DeleteForever(vec![trash.join("старое.txt")])]);
        app.open_menu(Hit::Item(0), 10.0, 10.0);
        let labels: Vec<_> = app.menu.as_ref().unwrap().items.iter().map(|(l, _)| l.clone()).collect();
        assert!(labels.iter().any(|l| l == "Вернуть") && !labels.iter().any(|l| l == "Переместить в Корзину"));
        app.menu = None;
        assert_eq!(app.menu_action(MenuAction::Restore), vec![Effect::Restore(vec![trash.join("старое.txt")])]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn path_field_and_properties() {
        let root = fixture("path");
        let mut app = app_in(&root);
        let ctrl = Mods { ctrl: true, ..Default::default() };
        assert!(app.key(0x6c, Some("l"), ctrl).is_empty());
        assert_eq!(app.path_edit.as_ref().unwrap().selected_text(), Some(root.to_str().unwrap()));
        // Typing replaces the selected path; a relative name opens inside the current folder
        for ch in ["А", "л", "ь", "ф", "а"] {
            app.key(0, Some(ch), Mods::default());
        }
        let effects = app.key(0xff0d, None, Mods::default());
        assert!(effects.iter().any(|e| matches!(e, Effect::List(p, _) if *p == root.join("Альфа"))));
        assert!(app.path_edit.is_none());
        app.key(0x6c, Some("l"), ctrl);
        app.key(0, Some("/нет/такой"), Mods::default());
        assert!(app.key(0xff0d, None, Mods::default()).is_empty());
        assert_eq!(app.dir, root.join("Альфа"));
        assert_eq!(app.menu_action(MenuAction::Properties), vec![Effect::Inspect(root.join("Альфа"))]);
        assert!(app.props.is_some());
        app.key(0xff1b, None, Mods::default());
        assert!(app.props.is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
