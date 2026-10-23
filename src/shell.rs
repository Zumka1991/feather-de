//! System UI of Feather DE in the style of macOS Tahoe: menu bar, Dock, menus, Spotlight,
//! Control Center, notifications and the Alt+Tab app switcher.
//!
//! Everything is rasterized on the CPU into a few small buffers only when it changes, and
//! only the changed regions are re-uploaded to the GPU.

use std::time::{Duration, Instant};

use chrono::{Datelike, Local, Timelike};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Buffer, Rectangle, Transform},
};
use tiny_skia::{Color, PixmapMut};

use crate::apps::{draw_app_icon, eval_spotlight_math, AppAction, AppEntry, AppRegistry, BuiltinWindowKind, IconKind};
use crate::icons::{draw_icon, IconCache};
use crate::text::FontSystem;
use crate::theme::{
    appearance, draw_control_center_icon, draw_feather_logo, draw_glass_panel, draw_search_icon,
    draw_wifi_icon, fill_circle, fill_rounded_rect, palette, render_wallpaper, rgba,
    stroke_line, stroke_rounded_rect, to_phys, ui_scale, with_alpha, Appearance, WallpaperTheme,
    DOCK_BOTTOM_MARGIN, DOCK_ICON_BASE, DOCK_ICON_GAP, DOCK_PAD_X, DOCK_PAD_Y, DOCK_RADIUS,
    TOP_BAR_HEIGHT,
};

/// The wallpaper is a smooth gradient: render it at 1/WALLPAPER_DIV resolution and let the GPU
/// upscale it with linear filtering (16x less CPU work than a full-res 4K raster).
pub const WALLPAPER_DIV: i32 = 4;
/// Logical height of the Dock buffer (room for magnified icons and the hover label)
const DOCK_BUF_H: i32 = 136;
/// Maximum Dock magnification
const DOCK_MAGNIFY: f32 = 0.5;

pub fn wallpaper_buffer_size(w: i32, h: i32) -> (i32, i32) {
    (
        (to_phys(w) + WALLPAPER_DIV - 1) / WALLPAPER_DIV,
        (to_phys(h) + WALLPAPER_DIV - 1) / WALLPAPER_DIV,
    )
}

fn new_buffer(w: i32, h: i32) -> MemoryRenderBuffer {
    MemoryRenderBuffer::new(Fourcc::Abgr8888, (w.max(1), h.max(1)), 1, Transform::Normal, None)
}

/// Logical rectangle (x, y, w, h)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    fn grow(&self, m: f32) -> Self {
        Self::new(self.x - m, self.y - m, self.w + 2.0 * m, self.h + 2.0 * m)
    }
    /// Physical buffer rectangle clamped to `(pw, ph)`
    fn to_buffer(self, pw: i32, ph: i32) -> Option<Rectangle<i32, Buffer>> {
        let s = ui_scale();
        let x0 = ((self.x * s).floor() as i32).clamp(0, pw);
        let y0 = ((self.y * s).floor() as i32).clamp(0, ph);
        let x1 = (((self.x + self.w) * s).ceil() as i32).clamp(0, pw);
        let y1 = (((self.y + self.h) * s).ceil() as i32).clamp(0, ph);
        (x1 > x0 && y1 > y0).then(|| Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into()))
    }
}

/// Clears the given physical rectangles of an RGBA buffer
fn clear_rects(buf: &mut [u8], pw: i32, rects: &[Rectangle<i32, Buffer>]) {
    let stride = pw as usize * 4;
    for r in rects {
        for row in r.loc.y..r.loc.y + r.size.h {
            let start = row as usize * stride + r.loc.x as usize * 4;
            buf[start..start + r.size.w as usize * 4].fill(0);
        }
    }
}

/// Icon of an app: a themed icon if the app has one, else a procedural macOS-style one
#[derive(Debug, Clone, PartialEq)]
pub struct AppIcon {
    pub kind: IconKind,
    pub name: Option<String>,
}

impl AppIcon {
    pub fn of(entry: &AppEntry) -> Self {
        Self {
            kind: entry.icon,
            name: entry.icon_name.clone(),
        }
    }
}

/// Draws an app icon at logical `(x, y)`; themed icons are cached in a few size buckets
#[allow(clippy::too_many_arguments)]
pub fn draw_any_icon(
    pixmap: &mut PixmapMut,
    fonts: &mut FontSystem,
    icons: &mut IconCache,
    icon: &AppIcon,
    label: &str,
    x: f32,
    y: f32,
    size: f32,
) {
    if let Some(name) = icon.name.as_deref() {
        let needed = (size * ui_scale()).ceil() as u32;
        // Few shared buckets: Dock, Alt+Tab and Launchpad reuse the same rasterized icon, so
        // the switcher never waits for SVG rasterization on first use
        let bucket = match needed {
            0..=48 => 64,
            49..=192 => 192,
            _ => 512,
        };
        if let Some(pm) = icons.get(name, bucket) {
            draw_icon(pixmap, pm, x, y, size);
            return;
        }
    }
    draw_app_icon(pixmap, fonts, icon.kind, label, x, y, size);
}

#[derive(Debug, Clone)]
pub struct NotificationToast {
    pub title: String,
    pub body: String,
    pub created_at: Instant,
    pub duration: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockTarget {
    /// An app from the registry (pinned, or running and known)
    App(usize),
    /// A running window with no matching .desktop entry (grouped by app_id)
    Window(u64),
    /// A minimized window, shown right of the separator like on macOS
    Minimized(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DockItem {
    pub target: DockTarget,
    pub label: String,
    pub icon: AppIcon,
    pub running: bool,
    /// Kept in the Dock by the user (left part, reorderable)
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockMenuAction {
    Open,
    KeepInDock,
    RemoveFromDock,
    Quit,
}

#[derive(Debug, Clone)]
pub struct DockMenu {
    pub target: DockTarget,
    pub items: Vec<(String, DockMenuAction)>,
    pub rect: R,
    pub hovered: Option<usize>,
}

/// Icon following the pointer while an app is dragged (Dock / Launchpad)
#[derive(Debug, Clone)]
pub struct Ghost {
    pub icon: AppIcon,
    pub label: String,
}

pub const GHOST_SIZE: f32 = 64.0;

#[derive(Debug, Clone)]
struct DockSlot {
    target: DockTarget,
    x: f32,
    y: f32,
    size: f32,
}

/// Key identifying an app in the switcher (registry index or a window for unknown apps)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppKey {
    App(usize),
    Window(u64),
}

#[derive(Debug, Clone)]
pub struct SwitcherItem {
    pub key: AppKey,
    pub label: String,
    pub icon: AppIcon,
}

#[derive(Debug, Clone)]
pub struct Switcher {
    pub items: Vec<SwitcherItem>,
    pub selected: usize,
    pub opened_at: Instant,
    /// Shown only after a short hold, so a quick Alt+Tab tap switches without a flash
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopbarItem {
    Menu(TopMenu),
    Layout,
    Wifi,
    Spotlight,
    ControlCenter,
    Clock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CcItem {
    Wifi,
    Bluetooth,
    Focus,
    DarkMode,
    Brightness,
    Volume,
    Scale,
    Wallpaper,
    Layout,
}

#[derive(Debug, Clone)]
pub enum ShellClickAction {
    None,
    LaunchApp(AppAction, String),
    ActivateApp(usize),
    /// Press on a Dock icon: becomes a click or a drag depending on what the pointer does
    DockPress(DockTarget),
    DockMenu(DockMenuAction, DockTarget),
    ActivateWindow(u64),
    RestoreMinimizedWindow(u64),
    ToggleLayout,
    CycleTheme,
    ToggleAppearance,
    CycleHiDpiScale,
    ToggleDockZoom,
    HotReloadShell,
    MinimizeAllWindows,
    QuitCompositor,
    // Menu bar commands acting on the frontmost app / window
    HideApp,
    HideOthers,
    ShowAllWindows,
    QuitFocusedApp,
    MinimizeFocused,
    ZoomFocused,
    FullscreenFocused,
    CloseFocused,
    FocusWindow(u64),
    ShowShortcuts,
    TogglePinFocused,
}

/// Menus of the menu bar
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TopMenu {
    Feather,
    App,
    Window,
    Help,
}

/// One row of a menu; `action == None` renders a disabled item
#[derive(Debug, Clone)]
pub enum MenuRow {
    Item {
        label: String,
        shortcut: &'static str,
        action: Option<ShellClickAction>,
        checked: bool,
    },
    Separator,
}

fn item(label: impl Into<String>, shortcut: &'static str, action: Option<ShellClickAction>) -> MenuRow {
    MenuRow::Item {
        label: label.into(),
        shortcut,
        action,
        checked: false,
    }
}

/// What the menu bar needs to know about the frontmost app (set by the compositor)
#[derive(Debug, Clone, Default)]
pub struct MenuContext {
    /// Name of the frontmost app; empty when only the desktop is focused
    pub app_name: String,
    /// Windows of the frontmost app: (id, title, focused, minimized)
    pub windows: Vec<(u64, String, bool, bool)>,
    pub focused_is_wayland: bool,
    /// Some(pinned?) when the frontmost app has a .desktop entry and can be kept in the Dock
    pub pinned: Option<bool>,
}

const MENU_ITEM_H: f32 = 22.0;
const MENU_SEP_H: f32 = 9.0;
const MENU_PAD: f32 = 5.0;
const MENU_MIN_W: f32 = 220.0;

fn builtin(kind: BuiltinWindowKind, name: &str) -> Option<ShellClickAction> {
    Some(ShellClickAction::LaunchApp(AppAction::BuiltinWindow(kind), name.into()))
}

fn menu_rows(menu: TopMenu, ctx: &MenuContext) -> Vec<MenuRow> {
    use MenuRow::Separator;
    let has_app = !ctx.app_name.is_empty();
    let has_window = ctx.windows.iter().any(|w| w.2);
    let on = |cond: bool, a: ShellClickAction| cond.then_some(a);
    match menu {
        TopMenu::Feather => vec![
            item("Об этом компьютере", "", builtin(BuiltinWindowKind::AboutFeather, "Об этом компьютере")),
            Separator,
            item("Системные настройки…", "", builtin(BuiltinWindowKind::SystemSettings, "Системные настройки")),
            item("Мониторинг системы", "", builtin(BuiltinWindowKind::ActivityMonitor, "Мониторинг системы")),
            Separator,
            item("Перезагрузить оболочку", "⇧⌘R", Some(ShellClickAction::HotReloadShell)),
            item("Свернуть все окна", "", Some(ShellClickAction::MinimizeAllWindows)),
            Separator,
            item("Выйти из Пёрышка…", "", Some(ShellClickAction::QuitCompositor)),
        ],
        TopMenu::App => {
            let name = if has_app { ctx.app_name.as_str() } else { "Пёрышко" };
            vec![
                item(format!("Скрыть «{name}»"), "⌘H", on(has_app, ShellClickAction::HideApp)),
                item("Скрыть остальные", "⌥⌘H", on(has_app, ShellClickAction::HideOthers)),
                item("Показать все", "", Some(ShellClickAction::ShowAllWindows)),
                Separator,
                match ctx.pinned {
                    Some(true) => item("Удалить из Dock", "", Some(ShellClickAction::TogglePinFocused)),
                    Some(false) => item("Оставить в Dock", "", Some(ShellClickAction::TogglePinFocused)),
                    None => item("Оставить в Dock", "", None),
                },
                Separator,
                item(format!("Завершить «{name}»"), "⌘Q", on(has_app, ShellClickAction::QuitFocusedApp)),
            ]
        }
        TopMenu::Window => {
            let mut rows = vec![
                item("Свернуть", "⌘M", on(has_window, ShellClickAction::MinimizeFocused)),
                item("Увеличить", "⌘F", on(has_window, ShellClickAction::ZoomFocused)),
                item(
                    "Войти в полноэкранный режим",
                    "",
                    on(has_window && ctx.focused_is_wayland, ShellClickAction::FullscreenFocused),
                ),
            ];
            if !ctx.windows.is_empty() {
                rows.push(Separator);
                for (id, title, focused, minimized) in &ctx.windows {
                    let label = if *minimized { format!("{title} (свёрнуто)") } else { title.clone() };
                    rows.push(MenuRow::Item {
                        label,
                        shortcut: "",
                        action: Some(ShellClickAction::FocusWindow(*id)),
                        checked: *focused,
                    });
                }
            }
            rows.push(Separator);
            rows.push(item("Закрыть окно", "⌘W", on(has_window, ShellClickAction::CloseFocused)));
            rows
        }
        TopMenu::Help => vec![
            item("Горячие клавиши Пёрышка", "", Some(ShellClickAction::ShowShortcuts)),
            item("Об этом компьютере", "", builtin(BuiltinWindowKind::AboutFeather, "Об этом компьютере")),
        ],
    }
}

pub struct MacShell {
    /// Logical desktop dimensions (in macOS points)
    pub screen_w: i32,
    pub screen_h: i32,

    pub theme: WallpaperTheme,
    pub wallpaper_buffer: MemoryRenderBuffer,
    pub wallpaper_dirty: bool,

    pub topbar_buffer: MemoryRenderBuffer,
    pub topbar_dirty: bool,
    topbar_hits: Vec<(f32, f32, TopbarItem)>,

    pub dock_buffer: MemoryRenderBuffer,
    pub dock_buf_w: i32,
    pub dock_buf_h: i32,
    pub dock_dirty: bool,
    pub dock_zoom_enabled: bool,
    dock_items: Vec<DockItem>,
    dock_cursor_x: Option<f32>,
    dock_slots: Vec<DockSlot>,
    dock_bar: R,
    dock_painted: Vec<Rectangle<i32, Buffer>>,

    pub spotlight_open: bool,
    pub spotlight_query: String,
    pub spotlight_selected: usize,

    pub control_center_open: bool,
    pub wifi_enabled: bool,
    pub bt_enabled: bool,
    pub dnd_enabled: bool,
    pub brightness_level: f32,
    pub volume_level: f32,

    pub open_menu: Option<TopMenu>,
    pub menu_hovered: Option<usize>,
    pub menu_ctx: MenuContext,

    pub toast: Option<NotificationToast>,

    pub overlay_buffer: MemoryRenderBuffer,
    pub overlay_dirty: bool,
    overlay_painted: Vec<Rectangle<i32, Buffer>>,
    overlay_allocated: bool,

    pub switcher: Option<Switcher>,
    pub switcher_buffer: MemoryRenderBuffer,
    pub switcher_rect: R,
    pub switcher_dirty: bool,

    /// While dragging an app: index in the Dock where an empty slot opens
    pub dock_gap: Option<usize>,
    pub dock_dragging: bool,
    pub dock_menu: Option<DockMenu>,
    pub ghost: Option<Ghost>,
    pub ghost_buffer: MemoryRenderBuffer,
    ghost_dirty: bool,
    pub launchpad: Option<crate::launchpad::Launchpad>,
    /// Frosted wallpaper behind Launchpad, cached per wallpaper/size/appearance
    pub launchpad_bg: MemoryRenderBuffer,
    pub launchpad_bg_key: Option<(WallpaperTheme, i32, i32, bool)>,
    glass_key: Option<(WallpaperTheme, i32, i32, bool)>,

    pub icons: IconCache,
    pub last_clock_str: String,
    pub last_rss_mb: f32,
    pub last_rss_check: Instant,
    pub reload_count: u32,
}

impl MacShell {
    pub fn new(logical_w: i32, logical_h: i32) -> Self {
        let w = logical_w.max(640);
        let h = logical_h.max(480);
        let (ww, wh) = wallpaper_buffer_size(w, h);

        let mut shell = Self {
            screen_w: w,
            screen_h: h,
            theme: WallpaperTheme::for_appearance(appearance()),
            wallpaper_buffer: new_buffer(ww, wh),
            wallpaper_dirty: true,
            topbar_buffer: new_buffer(to_phys(w), to_phys(TOP_BAR_HEIGHT)),
            topbar_dirty: true,
            topbar_hits: Vec::new(),
            dock_buffer: new_buffer(to_phys(w), to_phys(DOCK_BUF_H)),
            dock_buf_w: w,
            dock_buf_h: DOCK_BUF_H,
            dock_dirty: true,
            dock_zoom_enabled: true,
            dock_items: Vec::new(),
            dock_cursor_x: None,
            dock_slots: Vec::new(),
            dock_bar: R::new(0.0, 0.0, 0.0, 0.0),
            dock_painted: Vec::new(),
            spotlight_open: false,
            spotlight_query: String::new(),
            spotlight_selected: 0,
            control_center_open: false,
            wifi_enabled: true,
            bt_enabled: true,
            dnd_enabled: false,
            brightness_level: 0.85,
            volume_level: 0.72,
            open_menu: None,
            menu_hovered: None,
            menu_ctx: MenuContext::default(),
            toast: None,
            overlay_buffer: new_buffer(1, 1),
            overlay_dirty: true,
            overlay_painted: Vec::new(),
            overlay_allocated: false,
            switcher: None,
            switcher_buffer: new_buffer(1, 1),
            switcher_rect: R::new(0.0, 0.0, 0.0, 0.0),
            switcher_dirty: false,
            dock_gap: None,
            dock_dragging: false,
            dock_menu: None,
            ghost: None,
            ghost_buffer: new_buffer(1, 1),
            ghost_dirty: false,
            launchpad: None,
            launchpad_bg: new_buffer(1, 1),
            launchpad_bg_key: None,
            glass_key: None,
            icons: IconCache::new(),
            last_clock_str: String::new(),
            last_rss_mb: Self::read_process_rss_mb(),
            last_rss_check: Instant::now(),
            reload_count: 0,
        };
        shell.notify(
            "Добро пожаловать в Пёрышко",
            "Alt+Tab — приложения • Super+Space — Spotlight • Alt+Shift — язык",
        );
        shell
    }

    /// Private memory of the compositor (RssAnon). Shared GPU driver libraries (Mesa/LLVM,
    /// ~150 MB file-backed) are shared with every GL app and are not counted.
    pub fn read_process_rss_mb() -> f32 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|l| l.starts_with("RssAnon:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|kb| kb.parse::<f32>().ok())
            })
            .map(|kb| kb / 1024.0)
            .unwrap_or(0.0)
    }

    pub fn notify(&mut self, title: impl Into<String>, body: impl Into<String>) {
        if self.dnd_enabled {
            return;
        }
        self.toast = Some(NotificationToast {
            title: title.into(),
            body: body.into(),
            created_at: Instant::now(),
            duration: Duration::from_secs(5),
        });
        self.overlay_dirty = true;
    }

    /// Everything must be redrawn (appearance, wallpaper or scale changed)
    pub fn invalidate_all(&mut self) {
        self.wallpaper_dirty = true;
        self.topbar_dirty = true;
        self.dock_dirty = true;
        self.overlay_dirty = true;
        self.switcher_dirty = true;
        // Launchpad tiles are rasterized for one scale/size/appearance: rebuild them lazily
        if let Some(lp) = self.launchpad.as_mut() {
            lp.invalidate(self.screen_w, self.screen_h);
        }
        self.dock_painted.clear();
        self.overlay_painted.clear();
        self.icons.clear_raster();
    }

    pub fn reallocate_buffers(&mut self, logical_w: i32, logical_h: i32) {
        let w = logical_w.max(640);
        let h = logical_h.max(480);
        self.screen_w = w;
        self.screen_h = h;
        let (ww, wh) = wallpaper_buffer_size(w, h);
        self.wallpaper_buffer = new_buffer(ww, wh);
        self.topbar_buffer = new_buffer(to_phys(w), to_phys(TOP_BAR_HEIGHT));
        self.dock_buf_w = w;
        self.dock_buffer = new_buffer(to_phys(w), to_phys(self.dock_buf_h));
        self.overlay_buffer = new_buffer(1, 1);
        self.overlay_allocated = false;
        self.switcher_rect = R::new(0.0, 0.0, 0.0, 0.0);
        self.invalidate_all();
    }

    pub fn format_ru_clock() -> String {
        let now = Local::now();
        let weekday = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"][now.weekday().num_days_from_monday() as usize];
        let month = [
            "янв.", "февр.", "мар.", "апр.", "мая", "июн.", "июл.", "авг.", "сент.", "окт.", "нояб.", "дек.",
        ][now.month0() as usize];
        format!("{} {} {}  {}:{:02}", weekday, now.day(), month, now.hour(), now.minute())
    }

    /// Periodic housekeeping (clock, RSS, toast expiry). Returns true if anything needs a redraw.
    pub fn tick(&mut self) -> bool {
        let was_dirty = self.topbar_dirty || self.overlay_dirty;
        let clock = Self::format_ru_clock();
        if clock != self.last_clock_str {
            self.last_clock_str = clock;
            self.topbar_dirty = true;
        }
        if self.last_rss_check.elapsed() >= Duration::from_secs(3) {
            self.last_rss_check = Instant::now();
            self.last_rss_mb = Self::read_process_rss_mb();
        }
        if let Some(ref t) = self.toast {
            if t.created_at.elapsed() >= t.duration {
                self.toast = None;
                self.overlay_dirty = true;
            }
        }
        if self.overlay_allocated && !self.has_visible_overlay() {
            // Free the full-screen overlay buffer while nothing is shown
            self.overlay_buffer = new_buffer(1, 1);
            self.overlay_allocated = false;
            self.overlay_painted.clear();
        }
        !was_dirty && (self.topbar_dirty || self.overlay_dirty)
    }

    pub fn has_visible_overlay(&self) -> bool {
        self.spotlight_open
            || self.control_center_open
            || self.open_menu.is_some()
            || self.toast.is_some()
            || self.dock_menu.is_some()
    }

    pub fn close_menus(&mut self) {
        if self.dock_menu.take().is_some() {
            self.overlay_dirty = true;
        }
        if self.open_menu.is_some() || self.control_center_open || self.spotlight_open {
            self.open_menu = None;
            self.menu_hovered = None;
            self.control_center_open = false;
            self.spotlight_open = false;
            self.overlay_dirty = true;
            self.topbar_dirty = true;
        }
    }

    // ───────────────────────── Dock ─────────────────────────

    pub fn set_dock_items(&mut self, items: Vec<DockItem>) {
        if items != self.dock_items {
            self.dock_items = items;
            self.dock_dirty = true;
        }
    }

    /// Dock icon under a global point
    pub fn dock_target_at(&self, px: f64, py: f64) -> Option<(DockTarget, R)> {
        let (ox, oy) = self.dock_origin();
        let (lx, ly) = (px as f32 - ox, py as f32 - oy);
        self.dock_slots
            .iter()
            .find(|s| R::new(s.x, s.y, s.size, s.size).contains(lx, ly))
            .map(|s| (s.target, R::new(s.x + ox, s.y + oy, s.size, s.size)))
    }

    /// Area where dropping a dragged app pins it
    pub fn dock_in_drop_zone(&self, px: f64, py: f64) -> bool {
        let (ox, oy) = self.dock_origin();
        let (lx, ly) = (px as f32 - ox, py as f32 - oy);
        let b = self.dock_bar;
        lx >= b.x - 60.0 && lx <= b.x + b.w + 60.0 && ly >= b.y - 40.0
    }

    /// Insertion index among pinned items for a pointer at global `px`
    pub fn dock_insert_index(&self, px: f64) -> usize {
        let (ox, _) = self.dock_origin();
        let lx = px as f32 - ox;
        self.dock_slots
            .iter()
            .zip(self.dock_items.iter())
            .filter(|(_, item)| item.pinned)
            .filter(|(slot, _)| slot.x + slot.size * 0.5 < lx)
            .count()
    }

    pub fn set_dock_gap(&mut self, gap: Option<usize>) {
        if gap != self.dock_gap {
            self.dock_gap = gap;
            self.dock_dirty = true;
        }
    }

    pub fn set_ghost(&mut self, ghost: Option<Ghost>) {
        self.ghost_dirty = ghost.is_some();
        self.ghost = ghost;
    }

    pub fn open_dock_menu(&mut self, target: DockTarget, anchor: R, items: Vec<(String, DockMenuAction)>, fonts: &mut FontSystem) {
        let w = items
            .iter()
            .map(|(l, _)| fonts.measure_text(l, 13.0, false))
            .fold(140.0f32, f32::max)
            + 28.0;
        let h = MENU_PAD * 2.0 + items.len() as f32 * MENU_ITEM_H;
        let x = (anchor.x + anchor.w * 0.5 - w * 0.5).clamp(6.0, self.screen_w as f32 - w - 6.0);
        let y = anchor.y - h - 10.0;
        self.close_menus();
        self.dock_menu = Some(DockMenu {
            target,
            items,
            rect: R::new(x, y, w, h),
            hovered: None,
        });
        self.overlay_dirty = true;
    }

    /// Blurred wallpaper used under all glass panels (1/8 resolution, ~0.5 MB at 4K)
    pub fn ensure_glass_bg(&mut self) {
        let (w, h) = ((to_phys(self.screen_w) / 8).max(8), (to_phys(self.screen_h) / 8).max(8));
        let dark = appearance() == Appearance::Dark;
        let key = (self.theme, w, h, dark);
        if self.glass_key == Some(key) {
            return;
        }
        self.glass_key = Some(key);
        let data = crate::theme::render_frosted_wallpaper(w, h, self.theme, if dark { 0.8 } else { 1.0 });
        let pixmap = tiny_skia::IntSize::from_wh(w as u32, h as u32)
            .and_then(|size| tiny_skia::Pixmap::from_vec(data, size));
        crate::theme::set_glass_background(pixmap.map(|pm| {
            (
                std::rc::Rc::new(pm),
                self.screen_w as f32 / w as f32,
                self.screen_h as f32 / h as f32,
            )
        }));
        // Everything glassy must pick up the new background
        self.dock_dirty = true;
        self.overlay_dirty = true;
        self.switcher_dirty = true;
        self.dock_painted.clear();
        self.overlay_painted.clear();
    }

    pub fn ensure_launchpad_bg(&mut self) {
        let (w, h) = ((to_phys(self.screen_w) / 8).max(8), (to_phys(self.screen_h) / 8).max(8));
        let key = (self.theme, w, h, appearance() == Appearance::Dark);
        if self.launchpad_bg_key == Some(key) {
            return;
        }
        self.launchpad_bg_key = Some(key);
        let darken = if key.3 { 0.55 } else { 0.72 };
        let data = crate::theme::render_frosted_wallpaper(w, h, self.theme, darken);
        // Opaque: lets the renderer skip everything underneath (all windows)
        self.launchpad_bg = MemoryRenderBuffer::from_slice(
            &data,
            Fourcc::Abgr8888,
            (w, h),
            1,
            Transform::Normal,
            Some(vec![Rectangle::from_size((w, h).into())]),
        );
    }

    pub fn ensure_ghost_rendered(&mut self, fonts: &mut FontSystem) {
        if !self.ghost_dirty {
            return;
        }
        self.ghost_dirty = false;
        let Some(g) = self.ghost.clone() else { return };
        let px = to_phys(GHOST_SIZE as i32);
        self.ghost_buffer = new_buffer(px, px);
        let icons = &mut self.icons;
        let mut ctx = self.ghost_buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            if let Some(mut pm) = PixmapMut::from_bytes(buf, px as u32, px as u32) {
                draw_any_icon(&mut pm, fonts, icons, &g.icon, &g.label, 0.0, 0.0, GHOST_SIZE);
            }
            Result::<_, ()>::Ok(vec![Rectangle::from_size((px, px).into())])
        });
    }

    fn dock_origin(&self) -> (f32, f32) {
        (
            ((self.screen_w - self.dock_buf_w) / 2) as f32,
            (self.screen_h - self.dock_buf_h) as f32,
        )
    }

    // ───────────────────────── Layouts (shared by drawing and hit testing) ─────────────

    /// Updates what the App/Window menus show; redraws if something visible changed
    pub fn set_menu_context(&mut self, ctx: MenuContext) {
        let changed_name = ctx.app_name != self.menu_ctx.app_name;
        self.menu_ctx = ctx;
        if changed_name {
            self.topbar_dirty = true;
        }
        if self.open_menu.is_some() {
            self.overlay_dirty = true;
        }
    }

    fn open_menu_rows(&self) -> Vec<MenuRow> {
        self.open_menu.map(|m| menu_rows(m, &self.menu_ctx)).unwrap_or_default()
    }

    fn menu_rect(&self, menu: TopMenu) -> R {
        let rows = menu_rows(menu, &self.menu_ctx);
        let h = MENU_PAD * 2.0
            + rows
                .iter()
                .map(|r| if matches!(r, MenuRow::Separator) { MENU_SEP_H } else { MENU_ITEM_H })
                .sum::<f32>();
        // Width estimate from text length (exact text is clipped to it when drawn)
        let w = rows
            .iter()
            .filter_map(|r| match r {
                MenuRow::Item { label, shortcut, .. } => {
                    Some(label.chars().count() as f32 * 7.3 + shortcut.chars().count() as f32 * 9.0 + 64.0)
                }
                MenuRow::Separator => None,
            })
            .fold(MENU_MIN_W, f32::max)
            .min(420.0);
        let anchor = self
            .topbar_hits
            .iter()
            .find(|(_, _, it)| *it == TopbarItem::Menu(menu))
            .map(|(x0, _, _)| *x0)
            .unwrap_or(6.0);
        let x = (anchor - 2.0).clamp(4.0, self.screen_w as f32 - w - 4.0);
        R::new(x, TOP_BAR_HEIGHT as f32 + 3.0, w, h)
    }

    /// Index (into the open menu's rows) of the enabled item under a point
    fn menu_item_at(&self, px: f32, py: f32) -> Option<usize> {
        let menu = self.open_menu?;
        let r = self.menu_rect(menu);
        if !r.contains(px, py) {
            return None;
        }
        let mut y = r.y + MENU_PAD;
        for (i, row) in menu_rows(menu, &self.menu_ctx).iter().enumerate() {
            let h = if matches!(row, MenuRow::Separator) { MENU_SEP_H } else { MENU_ITEM_H };
            if py >= y && py < y + h {
                return matches!(row, MenuRow::Item { action: Some(_), .. }).then_some(i);
            }
            y += h;
        }
        None
    }

    fn topbar_item_at(&self, px: f32) -> Option<TopbarItem> {
        self.topbar_hits
            .iter()
            .find(|(x0, x1, _)| px >= *x0 && px < *x1)
            .map(|(_, _, item)| *item)
    }

    fn cc_layout(&self) -> (R, Vec<(R, CcItem)>) {
        let w = 324.0;
        let x = self.screen_w as f32 - w - 8.0;
        let y = TOP_BAR_HEIGHT as f32 + 5.0;
        let p = 12.0;
        let col = (w - p * 3.0) * 0.5;
        let mut items = Vec::new();
        let mut cy = y + p;
        items.push((R::new(x + p, cy, col, 50.0), CcItem::Wifi));
        items.push((R::new(x + p, cy + 56.0, col, 50.0), CcItem::Bluetooth));
        items.push((R::new(x + p * 2.0 + col, cy, col, 50.0), CcItem::Focus));
        items.push((R::new(x + p * 2.0 + col, cy + 56.0, col, 50.0), CcItem::DarkMode));
        cy += 106.0 + p;
        items.push((R::new(x + p, cy, w - p * 2.0, 58.0), CcItem::Brightness));
        cy += 58.0 + p;
        items.push((R::new(x + p, cy, w - p * 2.0, 58.0), CcItem::Volume));
        cy += 58.0 + p;
        items.push((R::new(x + p, cy, col, 50.0), CcItem::Scale));
        items.push((R::new(x + p * 2.0 + col, cy, col, 50.0), CcItem::Wallpaper));
        cy += 50.0 + p;
        items.push((R::new(x + p, cy, w - p * 2.0, 40.0), CcItem::Layout));
        cy += 40.0 + p;
        (R::new(x, y, w, cy - y), items)
    }

    /// (search bar, results panel, app result count, has calculator row)
    fn spotlight_layout(&self, registry: &AppRegistry) -> (R, Option<R>, usize, bool) {
        let w = 640.0;
        let x = (self.screen_w as f32 - w) * 0.5;
        let y = (self.screen_h as f32 * 0.22).max(TOP_BAR_HEIGHT as f32 + 40.0);
        let bar = R::new(x, y, w, 52.0);
        let n = if self.spotlight_query.trim().is_empty() {
            0
        } else {
            registry.search(&self.spotlight_query).len()
        };
        let math = eval_spotlight_math(&self.spotlight_query).is_some();
        let rows = n + usize::from(math);
        let list = (rows > 0).then(|| R::new(x, y + 52.0 + 8.0, w, 12.0 + rows as f32 * 40.0));
        (bar, list, n, math)
    }

    fn toast_rect(&self) -> R {
        R::new(self.screen_w as f32 - 344.0 - 10.0, TOP_BAR_HEIGHT as f32 + 8.0, 344.0, 72.0)
    }

    // ───────────────────────── Pointer ─────────────────────────

    pub fn update_pointer_hover(&mut self, px: f64, py: f64) {
        if self.open_menu.is_some() {
            // Like macOS: with a menu open, sliding along the menu bar switches menus
            if py < TOP_BAR_HEIGHT as f64 {
                if let Some(TopbarItem::Menu(m)) = self.topbar_item_at(px as f32) {
                    if Some(m) != self.open_menu {
                        self.open_menu = Some(m);
                        self.menu_hovered = None;
                        self.overlay_dirty = true;
                        self.topbar_dirty = true;
                    }
                }
            }
            let hovered = self.menu_item_at(px as f32, py as f32);
            if hovered != self.menu_hovered {
                self.menu_hovered = hovered;
                self.overlay_dirty = true;
            }
        }

        if let Some(menu) = self.dock_menu.as_mut() {
            let r = menu.rect;
            let hovered = r
                .contains(px as f32, py as f32)
                .then(|| ((py as f32 - r.y - MENU_PAD) / MENU_ITEM_H).floor() as isize)
                .filter(|i| *i >= 0 && (*i as usize) < menu.items.len())
                .map(|i| i as usize);
            if hovered != menu.hovered {
                menu.hovered = hovered;
                self.overlay_dirty = true;
            }
        }

        let (ox, oy) = self.dock_origin();
        let local_x = px as f32 - ox;
        let local_y = py as f32 - oy;
        let bar = self.dock_bar;
        let in_dock = !self.dock_dragging
            && local_y >= bar.y - 10.0
            && local_y <= self.dock_buf_h as f32
            && local_x >= bar.x - 20.0
            && local_x <= bar.x + bar.w + 20.0;

        let new_cursor = in_dock.then_some(local_x);
        let changed = match (self.dock_cursor_x, new_cursor) {
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
            (Some(old_x), Some(new_x)) => (old_x - new_x).abs() >= 1.0,
        };
        if changed {
            self.dock_cursor_x = new_cursor;
            self.dock_dirty = true;
        }
    }

    pub fn handle_pointer_click(&mut self, px: f64, py: f64, registry: &AppRegistry) -> Option<ShellClickAction> {
        let mx = px as f32;
        let my = py as f32;

        if let Some(menu) = self.dock_menu.take() {
            self.overlay_dirty = true;
            if menu.rect.contains(mx, my) {
                let idx = ((my - menu.rect.y - MENU_PAD) / MENU_ITEM_H).floor();
                if idx >= 0.0 {
                    if let Some((_, action)) = menu.items.get(idx as usize) {
                        return Some(ShellClickAction::DockMenu(*action, menu.target));
                    }
                }
            }
            return Some(ShellClickAction::None);
        }

        // Menu bar
        if my >= 0.0 && my < TOP_BAR_HEIGHT as f32 {
            let hit = self.topbar_item_at(mx);
            let was = (self.open_menu, self.control_center_open, self.spotlight_open);
            self.close_menus();
            match hit {
                Some(TopbarItem::Menu(m)) => {
                    if was.0 != Some(m) {
                        self.open_menu = Some(m);
                    }
                }
                Some(TopbarItem::ControlCenter) | Some(TopbarItem::Wifi) | Some(TopbarItem::Clock) => {
                    self.control_center_open = !was.1
                }
                Some(TopbarItem::Spotlight) => self.spotlight_open = !was.2,
                Some(TopbarItem::Layout) => return Some(ShellClickAction::ToggleLayout),
                None => {}
            }
            self.overlay_dirty = true;
            self.topbar_dirty = true;
            return Some(ShellClickAction::None);
        }

        if self.open_menu.is_some() {
            let idx = self.menu_item_at(mx, my);
            let rows = self.open_menu_rows();
            self.close_menus();
            if let Some(MenuRow::Item { action: Some(action), .. }) = idx.and_then(|i| rows.get(i)) {
                return Some(action.clone());
            }
            return Some(ShellClickAction::None);
        }

        if self.control_center_open {
            let (panel, items) = self.cc_layout();
            if !panel.contains(mx, my) {
                self.close_menus();
                return Some(ShellClickAction::None);
            }
            self.overlay_dirty = true;
            if let Some((r, item)) = items.iter().find(|(r, _)| r.contains(mx, my)) {
                let slider = ((mx - r.x - 14.0) / (r.w - 28.0)).clamp(0.0, 1.0);
                match item {
                    CcItem::Wifi => self.wifi_enabled = !self.wifi_enabled,
                    CcItem::Bluetooth => self.bt_enabled = !self.bt_enabled,
                    CcItem::Focus => self.dnd_enabled = !self.dnd_enabled,
                    CcItem::DarkMode => return Some(ShellClickAction::ToggleAppearance),
                    CcItem::Brightness => self.brightness_level = slider.max(0.1),
                    CcItem::Volume => self.volume_level = slider,
                    CcItem::Scale => return Some(ShellClickAction::CycleHiDpiScale),
                    CcItem::Wallpaper => return Some(ShellClickAction::CycleTheme),
                    CcItem::Layout => return Some(ShellClickAction::ToggleLayout),
                }
                if matches!(item, CcItem::Wifi) {
                    self.topbar_dirty = true;
                }
            }
            return Some(ShellClickAction::None);
        }

        if self.spotlight_open {
            let (bar, list, n, math) = self.spotlight_layout(registry);
            if let Some(list) = list.filter(|l| l.contains(mx, my)) {
                let row = ((my - list.y - 6.0) / 40.0).floor().max(0.0) as usize;
                let app_row = row.checked_sub(usize::from(math));
                if let Some(app) = app_row
                    .filter(|r| *r < n)
                    .and_then(|r| registry.search(&self.spotlight_query).get(r).copied())
                {
                    let action = ShellClickAction::LaunchApp(app.action.clone(), app.name.clone());
                    self.spotlight_query.clear();
                    self.close_menus();
                    return Some(action);
                }
                return Some(ShellClickAction::None);
            }
            if !bar.contains(mx, my) {
                self.close_menus();
            }
            return Some(ShellClickAction::None);
        }

        if self.toast.is_some() && self.toast_rect().contains(mx, my) {
            self.toast = None;
            self.overlay_dirty = true;
            return Some(ShellClickAction::None);
        }

        // Dock
        let (ox, oy) = self.dock_origin();
        let lx = mx - ox;
        let ly = my - oy;
        let on_icon = self
            .dock_slots
            .iter()
            .any(|s| R::new(s.x, s.y, s.size, s.size).contains(lx, ly));
        if self.dock_bar.contains(lx, ly) || on_icon {
            let slot = self
                .dock_slots
                .iter()
                .find(|s| lx >= s.x - DOCK_ICON_GAP * 0.5 && lx < s.x + s.size + DOCK_ICON_GAP * 0.5);
            return Some(match slot.map(|s| s.target) {
                Some(target) => ShellClickAction::DockPress(target),
                None => ShellClickAction::None,
            });
        }

        None
    }

    // ───────────────────────── Rendering ─────────────────────────

    pub fn ensure_wallpaper_rendered(&mut self) {
        if !self.wallpaper_dirty {
            return;
        }
        self.wallpaper_dirty = false;
        let (pw, ph) = wallpaper_buffer_size(self.screen_w, self.screen_h);
        let theme = self.theme;
        let mut ctx = self.wallpaper_buffer.render();
        let _ = ctx.draw(|buf| {
            render_wallpaper(buf, pw, ph, theme);
            Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
        });
    }

    pub fn ensure_topbar_rendered(&mut self, fonts: &mut FontSystem, active_app_title: &str, active_layout: &str) {
        if !self.topbar_dirty {
            return;
        }
        self.topbar_dirty = false;

        let lw = self.screen_w as f32;
        let lh = TOP_BAR_HEIGHT as f32;
        let pw = to_phys(self.screen_w);
        let ph = to_phys(TOP_BAR_HEIGHT);
        if self.last_clock_str.is_empty() {
            self.last_clock_str = Self::format_ru_clock();
        }
        let clock = self.last_clock_str.clone();
        let (open_menu, cc_open, sp_open, wifi) =
            (self.open_menu, self.control_center_open, self.spotlight_open, self.wifi_enabled);
        let p = palette();
        let mut hits = Vec::with_capacity(8);

        let mut ctx = self.topbar_buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            if let Some(mut pm) = PixmapMut::from_bytes(buf, pw as u32, ph as u32) {
                // Tahoe: the menu bar is (almost) transparent over the wallpaper
                fill_rounded_rect(&mut pm, 0.0, 0.0, lw, lh, 0.0, p.menubar_tint);
                let text_y = (lh - 13.0) * 0.5 - 1.5;
                let highlight = |pm: &mut PixmapMut, x0: f32, x1: f32| {
                    fill_rounded_rect(pm, x0, 2.0, x1 - x0, lh - 4.0, 6.0, p.menubar_highlight);
                };

                // Left: logo, app menu, Window, Help — all of them real menus
                if open_menu == Some(TopMenu::Feather) {
                    highlight(&mut pm, 8.0, 36.0);
                }
                draw_feather_logo(&mut pm, 22.0, lh * 0.5, 0.95, p.menubar_text);
                hits.push((0.0, 38.0, TopbarItem::Menu(TopMenu::Feather)));
                let name = if active_app_title.chars().count() > 40 {
                    active_app_title.chars().take(40).collect::<String>() + "…"
                } else {
                    active_app_title.to_string()
                };
                let mut x = 46.0;
                for (label, menu, bold) in [
                    (name.as_str(), TopMenu::App, true),
                    ("Окно", TopMenu::Window, false),
                    ("Справка", TopMenu::Help, false),
                ] {
                    let w = fonts.measure_text(label, 13.0, bold);
                    if open_menu == Some(menu) {
                        highlight(&mut pm, x - 8.0, x + w + 8.0);
                    }
                    fonts.draw_text(&mut pm, label, x, text_y, 13.0, p.menubar_text, bold);
                    hits.push((x - 9.0, x + w + 9.0, TopbarItem::Menu(menu)));
                    x += w + 20.0;
                }

                // Right: clock, Control Center, Spotlight, Wi-Fi, input source
                let mut rx = lw - 12.0;
                let cw = fonts.measure_text(&clock, 13.0, false);
                fonts.draw_text(&mut pm, &clock, rx - cw, text_y, 13.0, p.menubar_text, false);
                hits.push((rx - cw - 6.0, lw, TopbarItem::Clock));
                rx -= cw + 14.0;

                if cc_open {
                    highlight(&mut pm, rx - 28.0, rx);
                }
                draw_control_center_icon(&mut pm, rx - 14.0, lh * 0.5, p.menubar_text);
                hits.push((rx - 28.0, rx, TopbarItem::ControlCenter));
                rx -= 32.0;

                if sp_open {
                    highlight(&mut pm, rx - 28.0, rx);
                }
                draw_search_icon(&mut pm, rx - 14.0, lh * 0.5, 4.8, p.menubar_text);
                hits.push((rx - 28.0, rx, TopbarItem::Spotlight));
                rx -= 32.0;

                draw_wifi_icon(&mut pm, rx - 14.0, lh * 0.5 - 2.0, p.menubar_text, wifi);
                hits.push((rx - 28.0, rx, TopbarItem::Wifi));
                rx -= 34.0;

                // Input source badge, like macOS ("RU" / "EN" in a rounded box)
                let lw_text = fonts.measure_text(active_layout, 10.5, true);
                let bw = lw_text + 10.0;
                stroke_rounded_rect(&mut pm, rx - bw, 5.5, bw, lh - 11.0, 3.5, 1.2, p.menubar_text);
                fonts.draw_text(&mut pm, active_layout, rx - bw + 5.0, 6.8, 10.5, p.menubar_text, true);
                hits.push((rx - bw - 6.0, rx + 4.0, TopbarItem::Layout));
            }
            Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
        });
        drop(ctx);
        self.topbar_hits = hits;
    }

    pub fn ensure_dock_rendered(&mut self, fonts: &mut FontSystem) {
        if !self.dock_dirty {
            return;
        }
        self.dock_dirty = false;

        let p = palette();
        let n = self.dock_items.len();
        let minimized_start = self
            .dock_items
            .iter()
            .position(|i| matches!(i.target, DockTarget::Minimized(_)))
            .unwrap_or(n);
        let has_sep = minimized_start > 0 && minimized_start < n;
        let sep_w = if has_sep { 14.0 } else { 0.0 };
        let buf_w = self.dock_buf_w as f32;
        let buf_h = self.dock_buf_h as f32;

        // Base layout (unmagnified) to know where the cursor is relative to icons
        let base_total = n as f32 * DOCK_ICON_BASE + n.saturating_sub(1) as f32 * DOCK_ICON_GAP + sep_w;
        let base_x0 = (buf_w - base_total) * 0.5;
        let base_center = |i: usize| {
            base_x0
                + i as f32 * (DOCK_ICON_BASE + DOCK_ICON_GAP)
                + if has_sep && i >= minimized_start { sep_w } else { 0.0 }
                + DOCK_ICON_BASE * 0.5
        };

        let sizes: Vec<f32> = (0..n)
            .map(|i| match (self.dock_zoom_enabled, self.dock_cursor_x) {
                (true, Some(cx)) => {
                    let d = ((cx - base_center(i)) / (DOCK_ICON_BASE * 2.6)).abs();
                    let f = if d < 1.0 { (d * std::f32::consts::FRAC_PI_2).cos().powi(2) } else { 0.0 };
                    DOCK_ICON_BASE * (1.0 + DOCK_MAGNIFY * f)
                }
                _ => DOCK_ICON_BASE,
            })
            .collect();

        // An empty slot opens where a dragged app would land
        let gap = self.dock_gap;
        let gap_w = if gap.is_some() { DOCK_ICON_BASE + DOCK_ICON_GAP } else { 0.0 };
        let content_w: f32 =
            sizes.iter().sum::<f32>() + n.saturating_sub(1) as f32 * DOCK_ICON_GAP + sep_w + gap_w;
        let bar_h = DOCK_ICON_BASE + DOCK_PAD_Y * 2.0;
        let bar_w = content_w + DOCK_PAD_X * 2.0;
        let bar = R::new((buf_w - bar_w) * 0.5, buf_h - bar_h - DOCK_BOTTOM_MARGIN as f32, bar_w, bar_h);
        let baseline = bar.y + bar.h - DOCK_PAD_Y;

        let mut slots = Vec::with_capacity(n);
        let mut x = bar.x + DOCK_PAD_X;
        let mut sep_x = None;
        for (i, item) in self.dock_items.iter().enumerate() {
            if gap == Some(i) {
                x += gap_w;
            }
            if has_sep && i == minimized_start {
                sep_x = Some(x + sep_w * 0.5 - DOCK_ICON_GAP * 0.5);
                x += sep_w;
            }
            slots.push(DockSlot {
                target: item.target,
                x,
                y: baseline - sizes[i],
                size: sizes[i],
            });
            x += sizes[i] + DOCK_ICON_GAP;
        }

        let hovered = self.dock_cursor_x.and_then(|cx| {
            slots
                .iter()
                .position(|s| cx >= s.x - DOCK_ICON_GAP * 0.5 && cx < s.x + s.size + DOCK_ICON_GAP * 0.5)
        });

        // Damage: previous and current painted areas only
        let label = hovered.map(|h| self.dock_items[h].label.clone());
        let label_w = label.as_ref().map(|l| fonts.measure_text(l, 13.0, false) + 20.0).unwrap_or(0.0);
        let top_icon = slots.iter().map(|s| s.y).fold(bar.y, f32::min);
        let painted = R::new(bar.x - 30.0, top_icon - 40.0, bar.w + 60.0, buf_h - top_icon + 40.0).grow(label_w * 0.5);
        let pw = to_phys(self.dock_buf_w);
        let ph = to_phys(self.dock_buf_h);
        let current: Vec<_> = painted.to_buffer(pw, ph).into_iter().collect();
        let mut damage = std::mem::take(&mut self.dock_painted);
        damage.extend(current.iter().copied());
        self.dock_painted = current;

        let items = self.dock_items.clone();
        let (dock_ox, dock_oy) = self.dock_origin();
        crate::theme::set_glass_origin(dock_ox, dock_oy);
        let icons = &mut self.icons;
        let clear = damage.clone();
        let mut ctx = self.dock_buffer.render();
        let _ = ctx.draw(|buf| {
            clear_rects(buf, pw, &clear);
            if let Some(mut pm) = PixmapMut::from_bytes(buf, pw as u32, ph as u32) {
                // Liquid-glass shelf over the frosted desktop
                fill_rounded_rect(&mut pm, bar.x, bar.y + 2.0, bar.w, bar.h, DOCK_RADIUS, rgba(0, 0, 0, 22));
                crate::theme::fill_frosted(&mut pm, bar.x, bar.y, bar.w, bar.h, DOCK_RADIUS);
                fill_rounded_rect(&mut pm, bar.x, bar.y, bar.w, bar.h, DOCK_RADIUS, p.dock_fill);
                stroke_rounded_rect(&mut pm, bar.x + 0.75, bar.y + 0.75, bar.w - 1.5, bar.h - 1.5, DOCK_RADIUS - 0.75, 1.0, p.glass_edge);
                stroke_rounded_rect(&mut pm, bar.x - 0.25, bar.y - 0.25, bar.w + 0.5, bar.h + 0.5, DOCK_RADIUS + 0.25, 0.5, p.glass_outline);

                if let Some(sx) = sep_x {
                    stroke_line(&mut pm, sx, bar.y + 10.0, sx, bar.y + bar.h - 10.0, 1.0, p.separator);
                }

                for (slot, item) in slots.iter().zip(items.iter()) {
                    draw_any_icon(&mut pm, fonts, icons, &item.icon, &item.label, slot.x, slot.y, slot.size);
                    if item.running {
                        fill_circle(&mut pm, slot.x + slot.size * 0.5, bar.y + bar.h - 3.0, 1.9, p.dock_dot);
                    }
                }

                if let (Some(h), Some(label)) = (hovered, label.as_ref()) {
                    let slot = &slots[h];
                    let lw = fonts.measure_text(label, 13.0, false) + 20.0;
                    let lx = (slot.x + slot.size * 0.5 - lw * 0.5).clamp(4.0, buf_w - lw - 4.0);
                    let ly = slot.y - 34.0;
                    draw_glass_panel(&mut pm, lx, ly, lw, 24.0, 8.0, true);
                    fonts.draw_text(&mut pm, label, lx + 10.0, ly + 4.5, 13.0, p.text, false);
                }
            }
            Result::<_, ()>::Ok(damage)
        });
        drop(ctx);
        self.dock_slots = slots;
        self.dock_bar = bar;
    }

    /// Logical rectangles of all currently visible overlay panels
    fn overlay_panel_rects(&self, registry: &AppRegistry) -> Vec<R> {
        let mut rects = Vec::with_capacity(4);
        if let Some(m) = self.open_menu {
            rects.push(self.menu_rect(m));
        }
        if self.control_center_open {
            rects.push(self.cc_layout().0);
        }
        if self.spotlight_open {
            let (bar, list, _, _) = self.spotlight_layout(registry);
            rects.push(bar);
            if let Some(l) = list {
                rects.push(l);
            }
        }
        if self.toast.is_some() {
            rects.push(self.toast_rect());
        }
        if let Some(m) = &self.dock_menu {
            rects.push(m.rect);
        }
        rects
    }

    pub fn ensure_overlay_rendered(&mut self, fonts: &mut FontSystem, registry: &AppRegistry, active_layout: &str) {
        if !self.overlay_allocated {
            self.overlay_buffer = new_buffer(to_phys(self.screen_w), to_phys(self.screen_h));
            self.overlay_allocated = true;
            self.overlay_painted.clear();
            self.overlay_dirty = true;
        }
        if !self.overlay_dirty {
            return;
        }
        self.overlay_dirty = false;

        let pw = to_phys(self.screen_w);
        let ph = to_phys(self.screen_h);
        let current: Vec<_> = self
            .overlay_panel_rects(registry)
            .into_iter()
            .filter_map(|r| r.grow(16.0).to_buffer(pw, ph))
            .collect();
        let mut damage = std::mem::take(&mut self.overlay_painted);
        damage.extend(current.iter().copied());
        self.overlay_painted = current;
        let clear = damage.clone();

        let p = palette();
        let menu_rect = self.open_menu.map(|m| self.menu_rect(m));
        let menu_rows_now = self.open_menu_rows();
        let hovered = self.menu_hovered;
        let cc = self.cc_layout();
        let sp = self.spotlight_layout(registry);
        let toast_rect = self.toast_rect();
        let toast = self.toast.clone();
        let (cc_open, sp_open) = (self.control_center_open, self.spotlight_open);
        let query = self.spotlight_query.clone();
        let selected = self.spotlight_selected;
        let (wifi, bt, dnd, bright, vol) =
            (self.wifi_enabled, self.bt_enabled, self.dnd_enabled, self.brightness_level, self.volume_level);
        let theme_name = self.theme.name();
        let dark = appearance() == Appearance::Dark;
        let scale_pct = format!("{}%", (ui_scale() * 100.0).round() as i32);
        let results: Vec<(String, String, AppIcon)> = if sp_open && !query.trim().is_empty() {
            registry
                .search(&query)
                .iter()
                .map(|a| (a.name.clone(), a.subtitle.clone(), AppIcon::of(a)))
                .collect()
        } else {
            Vec::new()
        };
        let dock_menu = self.dock_menu.clone();
        let icons = &mut self.icons;
        crate::theme::set_glass_origin(0.0, 0.0);

        let mut ctx = self.overlay_buffer.render();
        let _ = ctx.draw(|buf| {
            clear_rects(buf, pw, &clear);
            let Some(mut pm) = PixmapMut::from_bytes(buf, pw as u32, ph as u32) else {
                return Result::<_, ()>::Ok(damage);
            };

            if let Some(r) = menu_rect {
                draw_glass_panel(&mut pm, r.x, r.y, r.w, r.h, 12.0, true);
                let mut y = r.y + MENU_PAD;
                for (i, row) in menu_rows_now.iter().enumerate() {
                    match row {
                        MenuRow::Separator => {
                            let sy = y + MENU_SEP_H * 0.5;
                            stroke_line(&mut pm, r.x + 14.0, sy, r.x + r.w - 14.0, sy, 1.0, p.separator);
                            y += MENU_SEP_H;
                        }
                        MenuRow::Item { label, shortcut, action, checked } => {
                            let enabled = action.is_some();
                            let hot = enabled && hovered == Some(i);
                            if hot {
                                fill_rounded_rect(&mut pm, r.x + 5.0, y, r.w - 10.0, MENU_ITEM_H, 6.0, p.accent);
                            }
                            let col = if hot {
                                p.on_accent
                            } else if enabled {
                                p.text
                            } else {
                                p.text_tertiary
                            };
                            if *checked {
                                fonts.draw_text(&mut pm, "✓", r.x + 10.0, y + 3.5, 13.0, col, false);
                            }
                            let sw = if shortcut.is_empty() {
                                0.0
                            } else {
                                let sc = if hot { p.on_accent } else { p.text_tertiary };
                                fonts.draw_text_right(&mut pm, shortcut, r.x + r.w - 14.0, y + 3.5, 13.0, sc, false) + 16.0
                            };
                            fonts.draw_text_clipped(&mut pm, label, r.x + 24.0, y + 3.5, r.w - 38.0 - sw, 13.0, col, false);
                            y += MENU_ITEM_H;
                        }
                    }
                }
            }

            if cc_open {
                let (panel, items) = &cc;
                draw_glass_panel(&mut pm, panel.x, panel.y, panel.w, panel.h, 22.0, false);
                let card = if dark { rgba(255, 255, 255, 20) } else { rgba(255, 255, 255, 140) };
                for (r, item) in items {
                    fill_rounded_rect(&mut pm, r.x, r.y, r.w, r.h, 14.0, card);
                    let toggle = |pm: &mut PixmapMut, fonts: &mut FontSystem, on: bool, title: &str, sub: &str| {
                        let (cx, cy) = (r.x + 25.0, r.y + r.h * 0.5);
                        fill_circle(pm, cx, cy, 14.0, if on { p.accent } else { p.control_fill });
                        fill_circle(pm, cx, cy, 4.0, if on { p.on_accent } else { p.text_secondary });
                        fonts.draw_text_clipped(pm, title, r.x + 46.0, r.y + r.h * 0.5 - 15.0, r.w - 52.0, 13.0, p.text, true);
                        fonts.draw_text_clipped(pm, sub, r.x + 46.0, r.y + r.h * 0.5 + 1.0, r.w - 52.0, 11.5, p.text_secondary, false);
                    };
                    let slider = |pm: &mut PixmapMut, fonts: &mut FontSystem, title: &str, v: f32| {
                        fonts.draw_text(pm, title, r.x + 14.0, r.y + 8.0, 13.0, p.text, true);
                        let (sx, sy, sw, sh) = (r.x + 14.0, r.y + 30.0, r.w - 28.0, 20.0);
                        fill_rounded_rect(pm, sx, sy, sw, sh, sh * 0.5, p.control_fill);
                        fill_rounded_rect(pm, sx, sy, (sw * v).max(sh), sh, sh * 0.5, rgba(255, 255, 255, 245));
                        stroke_rounded_rect(pm, sx, sy, sw, sh, sh * 0.5, 0.5, p.glass_outline);
                    };
                    match item {
                        CcItem::Wifi => toggle(&mut pm, fonts, wifi, "Wi‑Fi", if wifi { "Подключено" } else { "Выкл." }),
                        CcItem::Bluetooth => toggle(&mut pm, fonts, bt, "Bluetooth", if bt { "Вкл." } else { "Выкл." }),
                        CcItem::Focus => toggle(&mut pm, fonts, dnd, "Не беспокоить", if dnd { "Вкл." } else { "Выкл." }),
                        CcItem::DarkMode => toggle(&mut pm, fonts, dark, "Тёмный режим", if dark { "Вкл." } else { "Выкл." }),
                        CcItem::Brightness => slider(&mut pm, fonts, "Дисплей", bright),
                        CcItem::Volume => slider(&mut pm, fonts, "Звук", vol),
                        CcItem::Scale => {
                            fonts.draw_text(&mut pm, "Масштаб", r.x + 14.0, r.y + 8.0, 13.0, p.text, true);
                            fonts.draw_text(&mut pm, &scale_pct, r.x + 14.0, r.y + 26.0, 12.0, p.text_secondary, false);
                        }
                        CcItem::Wallpaper => {
                            fonts.draw_text(&mut pm, "Обои", r.x + 14.0, r.y + 8.0, 13.0, p.text, true);
                            fonts.draw_text_clipped(&mut pm, theme_name, r.x + 14.0, r.y + 26.0, r.w - 28.0, 12.0, p.text_secondary, false);
                        }
                        CcItem::Layout => {
                            fonts.draw_text(&mut pm, "Источник ввода", r.x + 14.0, r.y + 11.5, 13.0, p.text, true);
                            let name = if active_layout == "RU" { "Русская" } else { "ABC" };
                            fonts.draw_text_right(&mut pm, name, r.x + r.w - 14.0, r.y + 11.5, 13.0, p.text_secondary, false);
                        }
                    }
                }
            }

            if sp_open {
                let (bar, list, _, math) = &sp;
                draw_glass_panel(&mut pm, bar.x, bar.y, bar.w, bar.h, bar.h * 0.5, true);
                draw_search_icon(&mut pm, bar.x + 26.0, bar.y + bar.h * 0.5, 7.5, p.text_secondary);
                if query.is_empty() {
                    fonts.draw_text(&mut pm, "Поиск Spotlight", bar.x + 50.0, bar.y + 14.0, 20.0, p.text_tertiary, false);
                } else {
                    let qw = fonts.draw_text(&mut pm, &query, bar.x + 50.0, bar.y + 14.0, 20.0, p.text, false);
                    fill_rounded_rect(&mut pm, bar.x + 51.0 + qw, bar.y + 13.0, 2.0, 26.0, 1.0, p.accent);
                }
                if let Some(list) = list {
                    draw_glass_panel(&mut pm, list.x, list.y, list.w, list.h, 18.0, true);
                    let mut y = list.y + 6.0;
                    if *math {
                        if let Some(ans) = eval_spotlight_math(&query) {
                            fonts.draw_text(&mut pm, &format!("= {ans}"), list.x + 20.0, y + 9.0, 18.0, p.text, true);
                            fonts.draw_text_right(&mut pm, "Калькулятор", list.x + list.w - 20.0, y + 12.0, 12.0, p.text_secondary, false);
                            y += 40.0;
                        }
                    }
                    let sel = selected.min(results.len().saturating_sub(1));
                    for (i, (name, sub, icon)) in results.iter().enumerate() {
                        let hot = i == sel;
                        if hot {
                            fill_rounded_rect(&mut pm, list.x + 6.0, y, list.w - 12.0, 40.0, 10.0, p.accent);
                        }
                        draw_any_icon(&mut pm, fonts, icons, icon, name, list.x + 14.0, y + 5.0, 30.0);
                        let fg = if hot { p.on_accent } else { p.text };
                        let fg2 = if hot { with_alpha(p.on_accent, 0.8) } else { p.text_secondary };
                        let nw = fonts.draw_text(&mut pm, name, list.x + 54.0, y + 11.0, 14.0, fg, false);
                        fonts.draw_text_clipped(&mut pm, sub, list.x + 66.0 + nw, y + 12.5, list.w - 140.0 - nw, 12.0, fg2, false);
                        y += 40.0;
                    }
                }
            }

            if let Some(m) = dock_menu.as_ref() {
                let r = m.rect;
                draw_glass_panel(&mut pm, r.x, r.y, r.w, r.h, 12.0, true);
                for (i, (label, _)) in m.items.iter().enumerate() {
                    let y = r.y + MENU_PAD + i as f32 * MENU_ITEM_H;
                    let hot = m.hovered == Some(i);
                    if hot {
                        fill_rounded_rect(&mut pm, r.x + 5.0, y, r.w - 10.0, MENU_ITEM_H, 6.0, p.accent);
                    }
                    fonts.draw_text(&mut pm, label, r.x + 14.0, y + 3.5, 13.0, if hot { p.on_accent } else { p.text }, false);
                }
            }

            if let Some(t) = toast.as_ref() {
                let r = toast_rect;
                draw_glass_panel(&mut pm, r.x, r.y, r.w, r.h, 18.0, false);
                crate::theme::fill_squircle_gradient(&mut pm, r.x + 14.0, r.y + 18.0, 36.0, rgba(110, 200, 255, 255), rgba(20, 110, 235, 255));
                draw_feather_logo(&mut pm, r.x + 32.0, r.y + 36.0, 1.3, Color::WHITE);
                fonts.draw_text_clipped(&mut pm, &t.title, r.x + 62.0, r.y + 16.0, r.w - 120.0, 13.0, p.text, true);
                fonts.draw_text_right(&mut pm, "сейчас", r.x + r.w - 14.0, r.y + 16.5, 11.5, p.text_secondary, false);
                fonts.draw_text_clipped(&mut pm, &t.body, r.x + 62.0, r.y + 36.0, r.w - 76.0, 12.5, p.text_secondary, false);
            }

            Result::<_, ()>::Ok(damage)
        });
    }

    // ───────────────────────── App switcher (Alt+Tab) ─────────────────────────

    pub fn open_switcher(&mut self, items: Vec<SwitcherItem>, reverse: bool) {
        if items.is_empty() {
            return;
        }
        let n = items.len();
        let selected = if n == 1 {
            0
        } else if reverse {
            n - 1
        } else {
            1
        };
        self.switcher = Some(Switcher {
            items,
            selected,
            opened_at: Instant::now(),
            visible: false,
        });
        self.switcher_dirty = true;
    }

    pub fn switcher_step(&mut self, delta: i32) {
        if let Some(sw) = self.switcher.as_mut() {
            let n = sw.items.len() as i32;
            sw.selected = ((sw.selected as i32 + delta).rem_euclid(n)) as usize;
            sw.visible = true;
            self.switcher_dirty = true;
        }
    }

    pub fn ensure_switcher_rendered(&mut self, fonts: &mut FontSystem) {
        let Some(sw) = self.switcher.as_ref() else {
            return;
        };
        if !self.switcher_dirty || !sw.visible {
            return;
        }
        self.switcher_dirty = false;

        let n = sw.items.len() as f32;
        let pad = 14.0;
        let cell = ((self.screen_w as f32 - 80.0 - pad * 2.0) / n).clamp(40.0, 96.0);
        let icon = cell - 18.0;
        let w = (pad * 2.0 + cell * n).ceil();
        let h = (pad + cell + 34.0).ceil();
        let rect = R::new(((self.screen_w as f32 - w) * 0.5).round(), ((self.screen_h as f32 - h) * 0.5).round(), w, h);
        let (pw, ph) = (to_phys(w as i32), to_phys(h as i32));
        if self.switcher_rect.w != w || self.switcher_rect.h != h {
            self.switcher_buffer = new_buffer(pw, ph);
        }
        self.switcher_rect = rect;

        let p = palette();
        let items = sw.items.clone();
        let selected = sw.selected;
        crate::theme::set_glass_origin(rect.x, rect.y);
        let icons = &mut self.icons;
        let mut ctx = self.switcher_buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            if let Some(mut pm) = PixmapMut::from_bytes(buf, pw as u32, ph as u32) {
                draw_glass_panel(&mut pm, 0.5, 0.5, w - 1.0, h - 1.0, 24.0, true);
                for (i, item) in items.iter().enumerate() {
                    let cx = pad + i as f32 * cell;
                    if i == selected {
                        fill_rounded_rect(&mut pm, cx, pad, cell, cell, 16.0, p.control_fill);
                        stroke_rounded_rect(&mut pm, cx, pad, cell, cell, 16.0, 0.5, p.glass_outline);
                    }
                    draw_any_icon(&mut pm, fonts, icons, &item.icon, &item.label, cx + 9.0, pad + 9.0, icon);
                }
                let label = &items[selected].label;
                let center = pad + selected as f32 * cell + cell * 0.5;
                let lw = fonts.measure_text(label, 13.0, false).min(w - 20.0);
                let lx = (center - lw * 0.5).clamp(10.0, w - lw - 10.0);
                fonts.draw_text_clipped(&mut pm, label, lx, pad + cell + 8.0, w - 20.0, 13.0, p.text, false);
            }
            Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
        });
    }
}
