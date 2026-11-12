//! What Spotlight finds besides programs: open windows, sections of the settings, files in the
//! home folder and notes. There is no index and no background work: a short-lived thread walks
//! the disk for the latest query only, stops as soon as the query changes, and exits when
//! nobody has typed for a while.
use std::collections::VecDeque;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use smithay::reexports::calloop::channel;

use crate::apps::{AppAction, IconKind};
use crate::shell::{AppIcon, ShellClickAction};
use crate::system_settings::Page;
use crate::FeatherCompositor;

/// Rows at most: long lists are slower to read than a better query
const MAX_ROWS: usize = 10;
const MAX_APPS: usize = 5;
const MAX_WINDOWS: usize = 3;
const MAX_SETTINGS: usize = 3;
const MAX_NOTES: usize = 2;
const MAX_FILES: usize = 6;
/// Typing faster than this does not start a walk for every letter
const TYPING_PAUSE: Duration = Duration::from_millis(90);
const WORKER_IDLE: Duration = Duration::from_secs(20);
const WALK_BUDGET: Duration = Duration::from_millis(600);
const WALK_ENTRIES: usize = 200_000;
const NOTE_READ_LIMIT: usize = 64 * 1024;
/// Build products and caches: thousands of names nobody looks for
const SKIP_DIRS: &[&str] = &["node_modules", "target", "__pycache__", "venv", "dist-packages", "site-packages"];

/// One row of the result list
#[derive(Debug, Clone)]
pub struct Hit {
    pub name: String,
    pub sub: String,
    pub icon: AppIcon,
    /// Emoji panel: the glyph, instead of the icon
    pub emoji: Option<char>,
    /// Optional right-aligned badge (e.g. keyboard shortcut pill in Menu HUD)
    pub badge: String,
    pub action: ShellClickAction,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FoundFile {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    score: u8,
    depth: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FoundNote {
    pub id: String,
    pub title: String,
    pub line: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub files: Vec<FoundFile>,
    pub notes: Vec<FoundNote>,
}

/// Compositor's side of the disk search
pub struct DiskSearch {
    queries: Option<mpsc::Sender<(u64, String)>>,
    generation: Arc<AtomicU64>,
    results: channel::SyncSender<(u64, Found)>,
    pub found: Found,
}

/// Each word of the query is somewhere in the name, ignoring case. A better score for a name
/// that is the query, then for one that starts with it.
fn score_name(name: &str, query: &str) -> u8 {
    let name = name.to_lowercase();
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
    if !query.split_whitespace().all(|word| name.contains(word)) {
        0
    } else if stem == query || name == query {
        3
    } else if name.starts_with(query) {
        2
    } else {
        1
    }
}

fn better(a: &FoundFile, b: &FoundFile) -> std::cmp::Ordering {
    b.score.cmp(&a.score).then(a.depth.cmp(&b.depth)).then(a.name.len().cmp(&b.name.len())).then_with(|| a.name.cmp(&b.name))
}

/// Breadth first: what is near the top of the home folder is found first, and a walk cut short
/// by its budget has still seen the likely places. Stays on one filesystem.
fn walk_home(home: &Path, query: &str, cancelled: &dyn Fn() -> bool, mut report: impl FnMut(&[FoundFile])) -> Vec<FoundFile> {
    let started = Instant::now();
    let device = std::fs::metadata(home).map(|m| m.dev()).ok();
    let mut queue: VecDeque<(PathBuf, u16)> = VecDeque::from([(home.to_path_buf(), 0)]);
    let mut best: Vec<FoundFile> = Vec::new();
    let mut seen = 0usize;
    let mut last_report = Instant::now();
    let mut changed = false;
    while let Some((dir, depth)) = queue.pop_front() {
        if cancelled() || started.elapsed() > WALK_BUDGET || seen > WALK_ENTRIES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else { continue };
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && !SKIP_DIRS.contains(&name) {
                let path = entry.path();
                if device.is_none() || std::fs::metadata(&path).map(|m| m.dev()).ok() == device {
                    queue.push_back((path, depth + 1));
                }
            }
            let score = score_name(name, query);
            if score == 0 {
                continue;
            }
            let candidate = FoundFile { path: entry.path(), name: name.to_string(), is_dir: kind.is_dir(), score, depth };
            let at = best.partition_point(|f| better(f, &candidate).is_lt());
            if at < MAX_FILES {
                best.insert(at, candidate);
                best.truncate(MAX_FILES);
                changed = true;
            }
        }
        if changed && last_report.elapsed() > Duration::from_millis(60) {
            report(&best);
            changed = false;
            last_report = Instant::now();
        }
    }
    best
}

fn search_notes(dir: &Path, query: &str, cancelled: &dyn Fn() -> bool) -> Vec<FoundNote> {
    let mut notes = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return notes };
    for entry in entries.flatten() {
        if cancelled() || notes.len() >= MAX_NOTES {
            break;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else { continue };
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(NOTE_READ_LIMIT)]);
        if let Some(found) = match_note(&id, &text, query) {
            notes.push(found);
        }
    }
    notes
}

fn match_note(id: &str, text: &str, query: &str) -> Option<FoundNote> {
    let lower = text.to_lowercase();
    if !query.split_whitespace().all(|word| lower.contains(word)) {
        return None;
    }
    let title = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(60).collect();
    let first = query.split_whitespace().next().unwrap_or(query);
    let line = text.lines().find(|l| l.to_lowercase().contains(first)).unwrap_or("").trim().chars().take(80).collect();
    Some(FoundNote { id: id.to_string(), title, line })
}

fn notes_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("feather-notes"))
}

fn worker(queries: mpsc::Receiver<(u64, String)>, generation: Arc<AtomicU64>, results: channel::SyncSender<(u64, Found)>) {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let notes = notes_dir();
    while let Ok((mut stamp, mut query)) = queries.recv_timeout(WORKER_IDLE) {
        // The newest query after a pause in typing
        while let Ok((next, text)) = queries.recv_timeout(TYPING_PAUSE) {
            stamp = next;
            query = text;
        }
        let cancelled = || generation.load(Ordering::Relaxed) != stamp;
        if cancelled() {
            continue;
        }
        let mut found = Found { notes: notes.as_deref().map_or_else(Vec::new, |d| search_notes(d, &query, &cancelled)), files: Vec::new() };
        if let Some(home) = home.as_deref() {
            let partial = found.notes.clone();
            let results_now = results.clone();
            found.files = walk_home(home, &query, &cancelled, |files| {
                let _ = results_now.try_send((stamp, Found { files: files.to_vec(), notes: partial.clone() }));
            });
        }
        if !cancelled() {
            let _ = results.send((stamp, found));
        }
    }
}

impl DiskSearch {
    pub fn new(handle: &smithay::reexports::calloop::LoopHandle<'static, FeatherCompositor>) -> Result<Self, Box<dyn std::error::Error>> {
        let (results, source) = channel::sync_channel(4);
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg((stamp, found)) = event {
                if stamp == state.disk_search.generation.load(Ordering::Relaxed) && state.disk_search.found != found {
                    state.disk_search.found = found;
                    state.spotlight_refresh();
                    state.needs_redraw = true;
                }
            }
        })?;
        Ok(Self { queries: None, generation: Arc::new(AtomicU64::new(0)), results, found: Found::default() })
    }

    /// Stops the walk in progress and, for a query worth a walk, starts the next one
    fn search(&mut self, query: &str) {
        let stamp = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.found = Found::default();
        if query.chars().count() < 2 {
            return;
        }
        let mut message = (stamp, query.to_string());
        if let Some(queries) = &self.queries {
            match queries.send(message) {
                Ok(()) => return,
                // The worker went idle and exited
                Err(mpsc::SendError(back)) => message = back,
            }
        }
        let (tx, rx) = mpsc::channel();
        let (generation, results) = (self.generation.clone(), self.results.clone());
        if std::thread::Builder::new().name("feather-spotlight".into()).stack_size(256 * 1024).spawn(move || worker(rx, generation, results)).is_ok() {
            let _ = tx.send(message);
            self.queries = Some(tx);
        }
    }
}

/// Words that lead to a section of the settings, besides its title
fn page_words(page: Page) -> &'static str {
    match page {
        Page::Appearance => "тема тёмная светлая цвет акцент dark light theme accent appearance",
        Page::Accessibility => "контраст прозрачность залипание крупный текст contrast accessibility",
        Page::Language => "перевод регион language region",
        Page::Network => "интернет wi-fi wifi вайфай vpn ethernet network",
        Page::Bluetooth => "блютуз наушники bluetooth",
        Page::Keyboard => "раскладка сочетания клавиши автоповтор keyboard shortcuts layout",
        Page::Pointer => "мышь трекпад курсор тачпад mouse trackpad cursor",
        Page::Displays => "монитор экран разрешение масштаб ночной яркость display monitor resolution scale night",
        Page::Desktop => "обои dock док углы wallpaper desktop",
        Page::Defaults => "браузер автозапуск программы по умолчанию сеанс browser autostart default",
        Page::Privacy => "конфиденциальность запись экрана privacy screen",
        Page::DateTime => "часы часовой пояс время дата clock timezone",
        Page::Power => "сон питание блокировка пароль батарея power lock sleep battery",
        Page::Notifications => "не беспокоить баннеры notifications dnd",
        Page::Sound => "звук громкость микрофон колонки наушники sound volume microphone",
    }
}

fn matches_page(page: Page, query: &str) -> bool {
    let title = feather_ui::i18n::tr(page.title()).to_lowercase();
    let russian = page.title().to_lowercase();
    query.split_whitespace().all(|word| title.contains(word) || russian.contains(word) || page_words(page).contains(word))
}

fn space_mode_words(mode: crate::space_layout::SpaceMode) -> &'static str {
    use crate::space_layout::SpaceMode;
    match mode {
        SpaceMode::Free => "режим стола рабочего свободные окна плавающие обычный macos floating free windows",
        SpaceMode::Tile => "режим стола рабочего авто-плитка плитка тайлинг сетка bsp dwindle tile tiling",
        SpaceMode::Tape => "режим стола рабочего бесконечная лента колонки прокрутка niri tape ribbon scroll",
    }
}

fn matches_space_mode(mode: crate::space_layout::SpaceMode, query: &str) -> bool {
    let title = feather_ui::i18n::tr(mode.label()).to_lowercase();
    let russian = mode.label().to_lowercase();
    let words = space_mode_words(mode);
    query
        .split_whitespace()
        .all(|word| title.contains(word) || russian.contains(word) || words.contains(word))
}

fn matches_drop_shelf(query: &str) -> bool {
    let title = feather_ui::i18n::tr("Полка перетаскивания").to_lowercase();
    let words = "полка перетаскивания перетаскивание файлы буфер drop shelf dropover yoink dnd";
    query
        .split_whitespace()
        .all(|word| title.contains(word) || "полка перетаскивания".contains(word) || words.contains(word))
}

fn matches_menu_hud(query: &str) -> bool {
    let title = feather_ui::i18n::tr("Поиск по меню").to_lowercase();
    let words = "меню команды поиск активного окна hud menu command palette";
    query
        .split_whitespace()
        .all(|word| title.contains(word) || "поиск по меню".contains(word) || words.contains(word))
}

fn matches_pip(query: &str) -> bool {
    let title = feather_ui::i18n::tr("Картинка в картинке (PiP)").to_lowercase();
    let words = "pip картинка в картинке поверх всех окон призрачный сквозь курсор picture in picture ghost";
    query
        .split_whitespace()
        .all(|word| title.contains(word) || "картинка в картинке".contains(word) || words.contains(word))
}

/// Icon for a file by its extension; folders and everything else look like the file manager
fn file_icon(file: &FoundFile) -> IconKind {
    if file.is_dir {
        return IconKind::Finder;
    }
    let ext = file.name.rsplit_once('.').map_or(String::new(), |(_, e)| e.to_lowercase());
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" | "pdf" | "tiff" => IconKind::Preview,
        "txt" | "md" | "rs" | "py" | "sh" | "toml" | "json" | "yaml" | "yml" | "conf" | "log" | "c" | "h" | "cpp" | "js" | "ts" | "html" | "css" => IconKind::TextEdit,
        _ => IconKind::Finder,
    }
}

/// `~/Documents/report` for a path in the home folder
fn tilde(path: &Path) -> String {
    let parent = path.parent().unwrap_or(path);
    match std::env::var_os("HOME").map(PathBuf::from).and_then(|h| parent.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => parent.display().to_string(),
    }
}

impl FeatherCompositor {
    /// The query changed: new rows now from memory, the disk answers a moment later
    pub fn spotlight_query_changed(&mut self) {
        let query = self.shell.spotlight_query.trim().to_lowercase();
        if self.shell.spotlight_mode == crate::shell::SpotlightMode::Search {
            self.disk_search.search(&query);
        }
        self.spotlight_refresh();
    }

    /// Rebuilds the result rows. Programs first, then what is open, then what is on disk.
    pub fn spotlight_refresh(&mut self) {
        let query = self.shell.spotlight_query.trim().to_lowercase();
        match self.shell.spotlight_mode {
            crate::shell::SpotlightMode::Search => {}
            crate::shell::SpotlightMode::Clipboard => {
                self.shell.spotlight_hits = self.clipboard_hits(&query);
                self.shell.overlay_dirty = true;
                return;
            }
            crate::shell::SpotlightMode::Emoji => {
                self.shell.spotlight_hits = self.emoji_hits(&query);
                self.shell.overlay_dirty = true;
                return;
            }
            crate::shell::SpotlightMode::MenuHud => {
                self.shell.spotlight_hits = self.menu_hud_hits(&query);
                if self.shell.spotlight_selected >= self.shell.spotlight_hits.len() {
                    self.shell.spotlight_selected = 0;
                }
                self.shell.sync_spotlight_scroll();
                self.shell.overlay_dirty = true;
                return;
            }
        }
        let mut hits: Vec<Hit> = Vec::with_capacity(MAX_ROWS);
        if !query.is_empty() {
            for app in self.app_registry.search(&query).into_iter().take(MAX_APPS) {
                hits.push(Hit {
                    name: app.name.clone(),
                    sub: app.subtitle.clone(),
                    icon: AppIcon::of(app),
                    emoji: None,
                    badge: String::new(),
                    action: ShellClickAction::LaunchApp(app.action.clone(), app.name.clone()),
                });
            }
            let window_label = feather_ui::i18n::tr("Открытое окно").into_owned();
            for win in self.windows.iter().rev().filter(|w| w.mapped && !w.larvis && w.title.to_lowercase().contains(&query)).take(MAX_WINDOWS) {
                let app = self.app_registry.app_for_app_id(&win.app_id).map(|i| &self.app_registry.apps[i]);
                hits.push(Hit {
                    name: win.title.chars().take(60).collect(),
                    sub: window_label.clone(),
                    icon: app.map_or(AppIcon { kind: IconKind::GenericApp, name: None }, AppIcon::of),
                    emoji: None,
                    badge: String::new(),
                    action: ShellClickAction::FocusWindow(win.id),
                });
            }
            let settings_label = feather_ui::i18n::tr("Системные настройки").into_owned();
            for page in Page::ALL.into_iter().filter(|p| matches_page(*p, &query)).take(MAX_SETTINGS) {
                hits.push(Hit {
                    name: feather_ui::i18n::tr(page.title()).into_owned(),
                    sub: settings_label.clone(),
                    icon: AppIcon { kind: IconKind::Settings, name: None },
                    emoji: None,
                    badge: String::new(),
                    action: ShellClickAction::OpenSettingsPage(page),
                });
            }
            let mode_sub = feather_ui::i18n::tr("Режим рабочего стола").into_owned();
            for mode in crate::space_layout::SpaceMode::ALL
                .into_iter()
                .filter(|m| matches_space_mode(*m, &query))
            {
                hits.push(Hit {
                    name: feather_ui::i18n::tr(mode.label()).into_owned(),
                    sub: mode_sub.clone(),
                    icon: AppIcon { kind: IconKind::Launchpad, name: None },
                    emoji: None,
                    badge: String::new(),
                    action: ShellClickAction::SetSpaceMode(mode),
                });
            }
            if matches_drop_shelf(&query) {
                hits.push(Hit {
                    name: feather_ui::i18n::tr("Полка перетаскивания").into_owned(),
                    sub: feather_ui::i18n::tr("Перетащите файлы сюда").into_owned(),
                    icon: AppIcon { kind: IconKind::Finder, name: None },
                    emoji: None,
                    badge: String::new(),
                    action: ShellClickAction::ToggleDropShelf,
                });
            }
            if matches_menu_hud(&query) {
                hits.push(Hit {
                    name: feather_ui::i18n::tr("Поиск по меню").into_owned(),
                    sub: feather_ui::i18n::tr("Команды активного окна").into_owned(),
                    icon: AppIcon { kind: IconKind::Settings, name: None },
                    emoji: None,
                    badge: "⌘/".into(),
                    action: ShellClickAction::OpenMenuHud,
                });
            }
            if matches_pip(&query) {
                hits.push(Hit {
                    name: feather_ui::i18n::tr("Картинка в картинке (PiP)").into_owned(),
                    sub: feather_ui::i18n::tr("Поверх всех столов").into_owned(),
                    icon: AppIcon { kind: IconKind::Launchpad, name: None },
                    emoji: None,
                    badge: "⌘P".into(),
                    action: ShellClickAction::TogglePipFocused,
                });
            }
            let notes_cmd = self.app_registry.apps.iter().find_map(|a| match &a.action {
                AppAction::Exec(cmd) if a.icon == IconKind::Notes => Some(cmd.clone()),
                _ => None,
            });
            for note in self.disk_search.found.notes.iter().filter(|_| notes_cmd.is_some()) {
                let quoted_id = format!("'{}'", note.id.replace('\'', "'\\''"));
                hits.push(Hit {
                    name: if note.title.is_empty() { feather_ui::i18n::tr("Заметка").into_owned() } else { note.title.clone() },
                    sub: note.line.clone(),
                    icon: AppIcon { kind: IconKind::Notes, name: None },
                    emoji: None,
                    badge: String::new(),
                    action: ShellClickAction::LaunchApp(AppAction::Exec(format!("{} --note {}", notes_cmd.as_deref().unwrap_or_default(), quoted_id)), feather_ui::i18n::tr("Заметки").into_owned()),
                });
            }
            for file in &self.disk_search.found.files {
                if hits.len() >= MAX_ROWS {
                    break;
                }
                hits.push(Hit {
                    name: file.name.chars().take(60).collect(),
                    sub: tilde(&file.path),
                    icon: AppIcon { kind: file_icon(file), name: None },
                    emoji: None,
                    badge: String::new(),
                    action: ShellClickAction::OpenPath(file.path.clone()),
                });
            }
            hits.truncate(MAX_ROWS);
        }
        self.shell.spotlight_hits = hits;
        self.shell.spotlight_scroll = 0;
        self.shell.overlay_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("feather-spotlight-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn exact_names_beat_prefixes_beat_the_rest() {
        assert_eq!(score_name("Отчёт.pdf", "отчёт"), 3);
        assert_eq!(score_name("отчёт за год.pdf", "отчёт"), 2);
        assert_eq!(score_name("годовой отчёт.pdf", "отчёт"), 1);
        assert_eq!(score_name("смета.pdf", "отчёт"), 0);
        assert_eq!(score_name("годовой отчёт.pdf", "отчёт год"), 1, "words in any order");
    }

    #[test]
    fn the_walk_skips_hidden_and_build_folders_and_prefers_shallow_names() {
        let home = temp("walk");
        std::fs::create_dir_all(home.join("a/b")).unwrap();
        std::fs::create_dir_all(home.join("target/x")).unwrap();
        std::fs::create_dir_all(home.join(".hidden")).unwrap();
        for f in ["a/b/plan.txt", "a/plan.txt", "plan.md", "target/x/plan.txt", ".hidden/plan.txt", "other.txt"] {
            std::fs::write(home.join(f), "").unwrap();
        }
        let found = walk_home(&home, "plan", &|| false, |_| {});
        let names: Vec<_> = found.iter().map(|f| f.path.strip_prefix(&home).unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["plan.md", "a/plan.txt", "a/b/plan.txt"]);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_changed_query_stops_the_walk() {
        let home = temp("cancel");
        std::fs::write(home.join("plan.md"), "").unwrap();
        assert!(walk_home(&home, "plan", &|| true, |_| {}).is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn notes_match_by_content_and_show_the_line() {
        let note = match_note("7", "Покупки\n\nмолоко и Хлеб\n", "хлеб").unwrap();
        assert_eq!((note.id.as_str(), note.title.as_str(), note.line.as_str()), ("7", "Покупки", "молоко и Хлеб"));
        assert!(match_note("7", "Покупки", "хлеб").is_none());
    }

    #[test]
    fn settings_are_found_by_title_and_by_what_is_inside() {
        assert!(matches_page(Page::Sound, "громкость"));
        assert!(matches_page(Page::Network, "wi-fi"));
        assert!(matches_page(Page::Displays, "яркость"));
        assert!(!matches_page(Page::Sound, "обои"));
    }
}
