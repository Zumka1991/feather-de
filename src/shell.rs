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

use crate::apps::{ eval_spotlight_math, AppAction, AppEntry, AppRegistry, BuiltinWindowKind, IconKind};
use crate::icons::{draw_icon, IconCache};
use crate::text::FontSystem;
use crate::theme::{
    appearance, draw_control_center_icon, draw_feather_logo, draw_glass_panel, draw_search_icon,
    draw_wifi_icon, fill_circle, fill_rounded_rect, palette, rgba,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    _fonts: &mut FontSystem,
    icons: &mut IconCache,
    icon: &AppIcon,
    label: &str,
    x: f32,
    y: f32,
    size: f32,
) {
    draw_cached_icon(pixmap, icons, icon, label, x, y, size, true);
}

/// Magnified Dock icons change size with every pointer move: never queue one exact
/// raster per transient size, resample a bucket image instead.
pub fn draw_resampled_icon(pixmap: &mut PixmapMut, icons: &mut IconCache, icon: &AppIcon, label: &str, x: f32, y: f32, size: f32) {
    draw_cached_icon(pixmap, icons, icon, label, x, y, size, false);
}

fn draw_cached_icon(pixmap: &mut PixmapMut, icons: &mut IconCache, icon: &AppIcon, label: &str, x: f32, y: f32, size: f32, exact: bool) {
    let phys = size * ui_scale();
    // Whole-pixel sizes (resting Dock, Spotlight, switcher) get an icon rasterized at that
    // exact size on the worker and are only copied here. Resampling a bucket image on the
    // event loop cost ~0.5–1 ms per icon at 4K. Magnified/fractional sizes keep resampling.
    let key = match crate::icons::exact_icon_px(phys).filter(|_| exact) {
        Some(px) => {
            let key = crate::icons::IconKey::new(icon, label, px);
            if let Some(pm) = icons.get(&key) {
                // An evicted entry keeps only its small preview, which still needs scaling.
                if pm.width() == px { crate::icons::blit_icon(pixmap, pm, x, y); }
                else { draw_icon(pixmap, pm, x, y, size); }
                return;
            }
            key
        }
        None => {
            let key = crate::icons::IconKey::new(icon, label, crate::icons::icon_bucket(phys.ceil() as u32));
            if let Some(pm) = icons.get(&key) { draw_icon(pixmap, pm, x, y, size); return; }
            key
        }
    };
    // Until it arrives (scale change, another surface's size), scale any cached size of
    // the same icon rather than flashing a placeholder; only the request above is queued.
    if let Some(pm) = icons.other_size(&key, phys.ceil() as u32) { draw_icon(pixmap, pm, x, y, size); }
    else { fill_rounded_rect(pixmap, x, y, size, size, size * 0.22, palette().control_fill); }
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
    /// The Trash, always last
    Trash,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DockItem {
    pub target: DockTarget,
    pub label: String,
    pub icon: AppIcon,
    /// Open windows of the app (dots under the icon, up to three)
    pub windows: u8,
    /// Kept in the Dock by the user (left part, reorderable)
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockMenuAction {
    Tray(usize),
    Open,
    KeepInDock,
    RemoveFromDock,
    Quit,
    /// Raise this window of the app (window list in the menu)
    Window(u64),
    /// Start another copy of the app
    NewWindow,
    /// Dock Trash: asks before deleting everything in it
    EmptyTrash,
    /// Desktop context menu (the menu's Dock target is unused)
    Desktop(crate::desktop::MenuAction),
}

#[derive(Debug, Clone)]
pub struct DockMenu {
    pub target: DockTarget,
    follow_dock: bool,
    pub items: Vec<(String, DockMenuAction)>,
    pub rect: R,
    pub hovered: Option<usize>,
}

/// macOS-style launch bounce of a Dock icon. Only a GPU offset changes per frame; the Dock
/// buffer is repainted twice (hide the icon, show it again).
pub struct Bounce {
    pub target: DockTarget,
    pub app: usize,
    pub started: Instant,
    /// Windows with an id from here on count as the launched app's
    pub first_new_window: u64,
    /// The window appeared: finish the current jump, then stop
    finish_at: Option<Instant>,
    pub buffer: MemoryRenderBuffer,
    pub px: i32,
}

pub const BOUNCE_PERIOD: f32 = 0.55;
pub const BOUNCE_HEIGHT: f32 = 22.0;

impl Bounce {
    pub fn finish(&mut self, now: Instant) {
        if self.finish_at.is_none() {
            let period = BOUNCE_PERIOD as f64;
            let cycle = (now.duration_since(self.started).as_secs_f64() / period).floor();
            self.finish_at = Some(self.started + std::time::Duration::from_secs_f64((cycle + 1.0) * period));
        }
    }

    /// Jump height (logical) and whether the animation is over
    pub fn offset(&self, now: Instant) -> (f32, bool) {
        let t = now.duration_since(self.started).as_secs_f32() / BOUNCE_PERIOD;
        // A launch that never shows a window still stops after ~5 s
        let cycles = t.floor();
        let over = self.finish_at.is_some_and(|end| now >= end) || t >= 9.0;
        let f = t - cycles;
        (if over { 0.0 } else { BOUNCE_HEIGHT * 4.0 * f * (1.0 - f) }, over)
    }
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
    Tray(usize),
    TrayOverflow,
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
    /// «Об этом компьютере»: show it at login or not
    ToggleShowAbout,
    SystemSettings(crate::system_settings::Action),
    Control(crate::controls::Control),
    NetworkSettings,
    BluetoothSettings,
    AppMenuActivate { generation: u64, id: i32 },
    LaunchApp(AppAction, String),
    LaunchpadLaunch(usize),
    Tray {key:String,action:crate::tray::Action,x:i32,y:i32},
    TrayMenu {generation:u64,id:i32},
    TrayOverflow,
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
    Input,
    Client(i32),
    Tray(i32),
    Feather,
    App,
    Window,
    Help,
}

/// Modal layer whose appearance has its own spring (content changes keep the same key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    Menu(TopMenu),
    ControlCenter,
    Spotlight,
    DockMenu,
}

/// One row of a menu; `action == None` renders a disabled item
#[derive(Debug, Clone)]
pub enum MenuRow {
    Item {
        label: String,
        shortcut: String,
        action: Option<ShellClickAction>,
        checked: bool,
        radio: bool,
        submenu: Option<i32>,
    },
    Separator,
}

fn item(label: impl Into<String>, shortcut: &'static str, action: Option<ShellClickAction>) -> MenuRow {
    MenuRow::Item {
        label: label.into(),
        shortcut: shortcut.into(),
        action,
        checked: false,
        radio: false,
        submenu: None,
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
        TopMenu::Client(_) | TopMenu::Tray(_) | TopMenu::Input => Vec::new(),
        TopMenu::Feather => vec![
            item("Об этом компьютере", "", builtin(BuiltinWindowKind::AboutFeather, "Об этом компьютере")),
            Separator,
            item("Системные настройки…", "", builtin(BuiltinWindowKind::SystemSettings, "Системные настройки")),
            item("Мониторинг системы", "", builtin(BuiltinWindowKind::ActivityMonitor, "Мониторинг системы")),
            item("Программы", "⌘A", Some(ShellClickAction::LaunchApp(crate::apps::AppAction::OpenLaunchpad, "Программы".into()))),
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
                        shortcut: String::new(),
                        action: Some(ShellClickAction::FocusWindow(*id)),
                        checked: *focused,
                        radio: false,
                        submenu: None,
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

pub struct CcBackdrop {
    bounds: Rectangle<i32, Buffer>,
    pixels: tiny_skia::Pixmap,
}

pub struct MacShell {
    /// Logical desktop dimensions (in macOS points)
    pub screen_w: i32,
    pub screen_h: i32,

    pub theme: WallpaperTheme,
    pub wallpaper_image: Option<crate::wallpaper::ImageSource>,
    pub applied_wallpaper_image: Option<crate::wallpaper::ImageSource>,
    pub wallpaper_texture: Option<smithay::backend::renderer::element::texture::TextureBuffer<smithay::backend::renderer::gles::GlesTexture>>,
    pub wallpaper_texture_size:(i32,i32),
    wallpaper_pending: Option<(Vec<u8>,(i32,i32))>,
    pub wallpaper_dirty: bool,
    cc_backdrop: Option<CcBackdrop>,
    cc_backdrop_requested: bool,

    pub topbar_buffer: MemoryRenderBuffer,
    pub topbar_dirty: bool,
    topbar_hits: Vec<(f32, f32, TopbarItem)>,
    status_hover: Option<TopbarItem>,
    pub tray_items: Vec<crate::tray::Item>,
    pub input_sources: Vec<crate::settings::InputSource>,
    pub input_selected: usize,
    input_shortcut: &'static str,
    tray_menu: Option<(String,u64,f32,Option<std::sync::Arc<crate::appmenu::MenuNode>>)>,

    pub dock_buffer: MemoryRenderBuffer,
    pub dock_buf_w: i32,
    pub dock_buf_h: i32,
    pub dock_dirty: bool,
    pub dock_zoom_enabled: bool,
    dock_items: Vec<DockItem>,
    dock_cursor_x: Option<f32>,
    /// Keep the icon under the pointer still when a minimized tile is added/removed.
    dock_anchor: Option<(DockTarget, f32)>,
    dock_slots: Vec<DockSlot>,
    dock_bar: R,
    dock_painted: Vec<Rectangle<i32, Buffer>>,

    pub spotlight_open: bool,
    pub spotlight_query: String,
    /// Caret/selection of the Spotlight field; `spotlight_query` mirrors its text
    pub spotlight_input: feather_ui::input::TextInput,
    pub spotlight_selected: usize,

    pub control_center_open: bool,
    pub cc: crate::control_panel::Panel,
    pub wifi_enabled: bool,
    pub dnd_enabled: bool,

    pub open_menu: Option<TopMenu>,
    pub menu_hovered: Option<usize>,
    pub menu_ctx: MenuContext,
    pub client_menu: Option<std::sync::Arc<crate::appmenu::MenuNode>>,
    pub client_generation: u64,
    pub client_submenus: Vec<i32>,
    client_hovered: Option<(usize, usize)>,
    client_scroll: std::collections::HashMap<i32, usize>,

    pub toast: Option<NotificationToast>,

    pub overlay_buffer: MemoryRenderBuffer,
    pub overlay_dirty: bool,
    overlay_painted: Vec<Rectangle<i32, Buffer>>,
    overlay_allocated: bool,
    overlay_geometry: Rectangle<i32, Buffer>,

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
    launchpad_bg_requested: Option<crate::raster::BackgroundKey>,
    pub raster: Option<crate::raster::RasterSender>,
    pub desktop: crate::desktop::Desktop,
    /// Launch feedback: the Dock icon jumps until the app's window appears
    pub bounce: Option<Bounce>,
    pub pending_bounce: Option<Instant>,
    raster_launchpad_open: bool,
    raster_reclaim_pending: bool,

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

        let mut shell = Self {
            screen_w: w,
            screen_h: h,
            theme: WallpaperTheme::for_appearance(appearance()),
            wallpaper_image:None,applied_wallpaper_image:None,wallpaper_texture:None,wallpaper_texture_size:(1,1),wallpaper_pending:None,
            wallpaper_dirty: true,
            cc_backdrop: None,
            cc_backdrop_requested: false,
            topbar_buffer: new_buffer(to_phys(w), to_phys(TOP_BAR_HEIGHT)),
            topbar_dirty: true,
            topbar_hits: Vec::new(),status_hover:None,
            tray_items: Vec::new(),
            input_sources: crate::settings::Keyboard::default().sources(),input_selected:0,input_shortcut:"Alt + Shift",
            tray_menu: None,
            dock_buffer: new_buffer(to_phys(w), to_phys(DOCK_BUF_H)),
            dock_buf_w: w,
            dock_buf_h: DOCK_BUF_H,
            dock_dirty: true,
            dock_zoom_enabled: true,
            dock_items: Vec::new(),
            dock_cursor_x: None,
            dock_anchor: None,
            dock_slots: Vec::new(),
            dock_bar: R::new(0.0, 0.0, 0.0, 0.0),
            dock_painted: Vec::new(),
            spotlight_open: false,
            spotlight_query: String::new(),
            spotlight_input: Default::default(),
            spotlight_selected: 0,
            control_center_open: false,
            cc: crate::control_panel::Panel::default(),
            wifi_enabled: false,
            dnd_enabled: false,
            open_menu: None,
            menu_hovered: None,
            menu_ctx: MenuContext::default(),
            client_menu: None,
            client_generation: 0,
            client_submenus: Vec::new(),
            client_hovered: None,
            client_scroll: std::collections::HashMap::new(),
            toast: None,
            overlay_buffer: new_buffer(1, 1),
            overlay_dirty: true,
            overlay_painted: Vec::new(),
            overlay_allocated: false,
            overlay_geometry: Rectangle::from_size((1, 1).into()),
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
            launchpad_bg_requested: None,
            raster: None,
            desktop: crate::desktop::Desktop::new(),
            bounce: None,
            pending_bounce: None,
            raster_launchpad_open: false,
            raster_reclaim_pending: false,
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
        if let Some(worker) = &self.raster { worker.invalidate(); }
        self.launchpad_bg_key = None;
        self.launchpad_bg_requested = None;
        self.cc_backdrop = None;
        self.cc_backdrop_requested = false;
        crate::theme::set_glass_background(None);
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
        self.desktop.invalidate();
    }

    pub fn reallocate_buffers(&mut self, logical_w: i32, logical_h: i32) {
        let w = logical_w.max(640);
        let h = logical_h.max(480);
        self.screen_w = w;
        self.screen_h = h;
        self.topbar_buffer = new_buffer(to_phys(w), to_phys(TOP_BAR_HEIGHT));
        self.dock_buf_w = w;
        self.dock_anchor = None;
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
        self.tray_menu=None;
        if self.dock_menu.take().is_some() {
            self.overlay_dirty = true;
        }
        if self.open_menu.is_some() || self.control_center_open || self.spotlight_open {
            self.open_menu = None;
            self.menu_hovered = None;
            self.client_submenus.clear();
            self.client_hovered = None;
            self.control_center_open = false;
            self.spotlight_open = false;
            self.overlay_dirty = true;
            self.topbar_dirty = true;
        }
    }

    // ───────────────────────── Dock ─────────────────────────

    pub fn set_dock_items(&mut self, items: Vec<DockItem>) {
        if items != self.dock_items {
            let layout_changed = !items.iter().map(|i| i.target).eq(self.dock_items.iter().map(|i| i.target));
            if layout_changed && self.dock_anchor.is_none() {
                self.dock_anchor = self.dock_cursor_x.and_then(|cx| {
                    self.dock_slots.iter()
                        .find(|s| cx >= s.x && cx < s.x + s.size && items.iter().any(|i| i.target == s.target))
                        .map(|s| (s.target, s.x + s.size * 0.5))
                });
            }
            if self.dock_anchor.is_some_and(|(target, _)| !items.iter().any(|i| i.target == target)) {
                self.dock_anchor = None;
            }
            self.dock_items = items;
            self.dock_dirty = true;
        }
    }

    pub fn anchor_dock_target(&mut self, target: DockTarget) {
        if self.dock_cursor_x.is_some() {
            self.dock_anchor = self.dock_slots.iter().find(|s|s.target==target)
                .map(|s|(target,s.x+s.size*0.5));
        }
    }

    fn dock_menu_rect(&self, anchor: R, w: f32, h: f32) -> R {
        let x = (anchor.x + anchor.w * 0.5 - w * 0.5).clamp(6.0, (self.screen_w as f32 - w - 6.0).max(6.0));
        let y = (anchor.y - h - 10.0).max(TOP_BAR_HEIGHT as f32 + 4.0);
        R::new(x,y,w,h)
    }

    fn update_dock_menu_anchor(&mut self) {
        let Some(menu) = self.dock_menu.as_ref().filter(|m|m.follow_dock) else {return};
        let Some((x,y,size)) = self.dock_slot_global(menu.target) else {return};
        let rect = self.dock_menu_rect(R::new(x,y,size,size),menu.rect.w,menu.rect.h);
        if rect != menu.rect {
            let menu=self.dock_menu.as_mut().unwrap();
            menu.rect=rect;
            menu.hovered=None;
            self.overlay_dirty=true;
        }
    }

    /// Global icon destination for minimizing; layout is updated before animation sampling.
    pub fn minimized_target(&self, id: u64) -> R {
        let (ox, oy) = self.dock_origin();
        self.dock_slots.iter().find(|s| s.target == DockTarget::Minimized(id))
            .map(|s| R::new(ox + s.x, oy + s.y, s.size, s.size))
            .unwrap_or_else(|| R::new(self.screen_w as f32 * 0.5 - 24.0, self.screen_h as f32 - 64.0, 48.0, 48.0))
    }

    /// Crop cached transparent margins before GPU animation, limiting output damage.
    pub fn overlay_region(&self) -> Option<Rectangle<i32, Buffer>> {
        self.overlay_allocated.then_some(self.overlay_geometry)
    }

    /// Identity and pivot of the modal overlay. Hover/query changes do not restart motion.
    pub fn animation_overlay(&self) -> Option<(OverlayKind, R)> {
        if let Some(m) = self.open_menu { return Some((OverlayKind::Menu(m), self.menu_rect(m))); }
        if self.control_center_open { return Some((OverlayKind::ControlCenter, self.cc_layout().0)); }
        if self.spotlight_open {
            return Some((OverlayKind::Spotlight, self.spotlight_bar_rect()));
        }
        self.dock_menu.as_ref().map(|m| (OverlayKind::DockMenu, m.rect))
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
        let rect = self.dock_menu_rect(anchor,w,h);
        self.close_menus();
        self.dock_menu = Some(DockMenu {
            target,
            follow_dock: true,
            items,
            rect,
            hovered: None,
        });
        self.overlay_dirty = true;
    }

    /// Context menu at the pointer (desktop), drawn like the Dock menu
    pub fn open_context_menu(&mut self, items: Vec<(String, DockMenuAction)>, x: f32, y: f32, fonts: &mut FontSystem) {
        let w = items.iter().map(|(l, _)| fonts.measure_text(l, 13.0, false)).fold(160.0f32, f32::max) + 28.0;
        let h = MENU_PAD * 2.0 + items.len() as f32 * MENU_ITEM_H;
        let x = x.min(self.screen_w as f32 - w - 6.0).max(6.0);
        let y = if y + h > self.screen_h as f32 - 6.0 { (y - h).max(TOP_BAR_HEIGHT as f32 + 4.0) } else { y };
        self.close_menus();
        self.dock_menu = Some(DockMenu { target: DockTarget::Window(0), follow_dock: false, items, rect: R::new(x, y, w, h), hovered: None });
        self.overlay_dirty = true;
    }

    pub fn attach_raster(&mut self, worker: crate::raster::RasterSender) {
        self.icons.attach(worker.clone());
        self.raster = Some(worker);
    }

    pub fn install_controls(&mut self, update:crate::controls::Update) -> bool {
        match update {
            crate::controls::Update::Wifi(wifi) => {
                if self.cc.wifi==wifi {return false;}
                self.wifi_enabled=wifi.available&&wifi.enabled&&wifi.hardware;
                self.cc.wifi=wifi;
                self.topbar_dirty=true;
            }
            crate::controls::Update::Audio(audio) => {
                if self.cc.audio==audio {return false;}
                self.cc.audio=audio;
            }
            crate::controls::Update::Error(title,error) => {
                if self.control_center_open {self.cc.open(if title=="Звук" {crate::control_panel::Page::Sound} else {crate::control_panel::Page::Wifi});}
                self.cc.error=Some(error.clone());
                if !self.control_center_open {self.notify(title,error);return true;}
            }
        }
        if self.control_center_open {self.overlay_dirty=true;}
        self.control_center_open||self.topbar_dirty
    }

    /// Install a CPU-only result. No rasterization, file lookup, or GPU access here.
    pub fn install_asset(&mut self, asset: crate::raster::Asset) -> bool {
        match asset {
            crate::raster::Asset::Settings(_,_)=>return false,
            crate::raster::Asset::Retry => {}
            crate::raster::Asset::Reclaimed => return false,
            crate::raster::Asset::CcBackdrop(backdrop) => {
                self.cc_backdrop = Some(backdrop);
                self.cc_backdrop_requested = false;
                if !self.control_center_open { return false; }
                self.overlay_dirty = true;
            }
            crate::raster::Asset::WallpaperError(_)=>return false,
            crate::raster::Asset::Wallpaper { pixels,size,image, glass } => {
                let (w, h) = (glass.width(), glass.height());
                self.applied_wallpaper_image=image;
                self.wallpaper_pending=Some((pixels,size));
                crate::theme::set_glass_background(Some((std::rc::Rc::new(glass), self.screen_w as f32 / w as f32, self.screen_h as f32 / h as f32)));
                self.dock_painted.clear(); self.overlay_painted.clear();
                self.dock_dirty = true; self.overlay_dirty = true; self.switcher_dirty = true;
            }
            crate::raster::Asset::Frosted(key, buffer) => {
                self.launchpad_bg_requested = None;
                if self.launchpad.is_none() { return false; }
                self.launchpad_bg_key = Some(key);
                self.launchpad_bg = buffer;
            }
            crate::raster::Asset::Icon(key, pixmap) => {
                if !self.tray_items.is_empty(){self.topbar_dirty=true;}
                self.icons.install(key, pixmap);
                self.dock_dirty = true; self.overlay_dirty = true; self.switcher_dirty = true; self.ghost_dirty = true;
            }
            crate::raster::Asset::DesktopTile(pixels) => return self.desktop.install(pixels),
            crate::raster::Asset::Tile { ticket, app, buffer } => {
                return self.launchpad.as_mut().is_some_and(|lp| lp.install_tile(&ticket, app, buffer));
            }
        }
        true
    }

    pub fn ensure_launchpad_bg(&mut self) {
        self.raster_launchpad_open = true;
        let (w, h) = ((to_phys(self.screen_w) / 8).max(8), (to_phys(self.screen_h) / 8).max(8));
        let key = (self.theme, w, h, appearance() == Appearance::Dark);
        if self.launchpad_bg_key == Some(key) || self.launchpad_bg_requested == Some(key) { return; }
        if self.raster.as_ref().is_some_and(|worker| worker.submit(crate::raster::Job::Frosted(key))) {
            self.launchpad_bg_requested = Some(key);
        }
    }

    /// Drop CPU/GPU references before scene construction, so this frame can release
    /// old textures. Reclaim allocator pages only after that frame has been submitted.
    pub fn release_closed_launchpad(&mut self) {
        if self.raster_launchpad_open && self.launchpad.is_none() {
            self.launchpad_bg = new_buffer(1, 1);
            self.launchpad_bg_key = None;
            self.launchpad_bg_requested = None;
            self.raster_launchpad_open = false;
            self.raster_reclaim_pending = true;
        }
    }

    pub fn reclaim_after_frame(&mut self) {
        if self.raster_reclaim_pending {
            if self.raster.as_ref().is_some_and(|worker| worker.submit(crate::raster::Job::Reclaim)) {
                self.raster_reclaim_pending = false;
            }
        }
    }

    pub fn request_raster_reclaim(&mut self) {
        self.raster_reclaim_pending = true;
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

    /// Starts the launch bounce of app `app` if it has a Dock icon
    pub fn start_bounce(&mut self, app: usize, first_new_window: u64, fonts: &mut FontSystem) {
        let target = DockTarget::App(app);
        let Some(item) = self.dock_items.iter().find(|i| i.target == target).cloned() else { return };
        let px = to_phys(DOCK_ICON_BASE as i32);
        let Some(mut pm) = tiny_skia::Pixmap::new(px as u32, px as u32) else { return };
        draw_any_icon(&mut pm.as_mut(), fonts, &mut self.icons, &item.icon, &item.label, 0.0, 0.0, DOCK_ICON_BASE);
        let buffer = MemoryRenderBuffer::from_slice(pm.data(), smithay::backend::allocator::Fourcc::Abgr8888, (px, px), 1, smithay::utils::Transform::Normal, None);
        self.bounce = Some(Bounce { target, app, started: Instant::now(), first_new_window, finish_at: None, buffer, px });
        self.dock_dirty = true;
    }

    /// Global logical top-left and size of a Dock slot
    pub fn dock_slot_global(&self, target: DockTarget) -> Option<(f32, f32, f32)> {
        let (ox, oy) = self.dock_origin();
        self.dock_slots.iter().find(|s| s.target == target).map(|s| (ox + s.x, oy + s.y, s.size))
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

    fn rows_for_menu(&self, menu: TopMenu) -> Vec<MenuRow> {
        match menu {
            TopMenu::Input=>{
                let mut rows:Vec<_>=self.input_sources.iter().enumerate().map(|(i,s)|MenuRow::Item{label:s.label.into(),shortcut:String::new(),checked:i==self.input_selected,radio:false,submenu:None,action:Some(ShellClickAction::SystemSettings(crate::system_settings::Action::SelectSource(i)))}).collect();
                rows.push(MenuRow::Separator);
                rows.push(item(&format!("Переключение: {}",self.input_shortcut),"",None));
                rows.push(item("Настройки клавиатуры…","",Some(ShellClickAction::SystemSettings(crate::system_settings::Action::Page(crate::system_settings::Page::Keyboard)))));
                rows
            }
            TopMenu::Client(id) | TopMenu::Tray(id) => self.native_menu_root(menu).and_then(|root| root.find(id))
                .map(|node| {
                    let count = node.children.iter().filter(|n| n.visible).count();
                    let capacity = self.client_row_capacity();
                    let offset = self.client_scroll.get(&id).copied().unwrap_or(0).min(count.saturating_sub(capacity));
                    let mut rows = Vec::new();
                    if offset > 0 { rows.push(item("↑", "", None)); }
                    rows.extend(node.children.iter().filter(|n| n.visible).skip(offset).take(capacity).map(|n| {
                    if n.separator { MenuRow::Separator } else { MenuRow::Item {
                        label: n.label.clone(), shortcut: n.shortcut.clone(), checked: n.checked,
                        radio: n.radio, submenu: n.submenu.then_some(n.id),
                        action: n.enabled.then(||if matches!(menu,TopMenu::Tray(_)) {
                            ShellClickAction::TrayMenu{generation:self.tray_menu.as_ref().map_or(0,|m|m.1),id:n.id}
                        }else{ShellClickAction::AppMenuActivate { generation: self.client_generation, id: n.id }}),
                    }}
                    }));
                    if offset + capacity < count { rows.push(item("↓", "", None)); }
                    if rows.is_empty() { rows.push(item("Нет пунктов", "", None)); }
                    rows
                }).unwrap_or_else(||if matches!(menu,TopMenu::Tray(_)){vec![item("Загрузка…","",None)]}else{Vec::new()}),
            _ => menu_rows(menu, &self.menu_ctx),
        }
    }

    pub fn set_client_menu(&mut self, generation: u64, root: Option<std::sync::Arc<crate::appmenu::MenuNode>>) -> bool {
        if self.client_generation == generation && self.client_menu == root { return false; }
        if generation != self.client_generation || root.is_none() {
            if matches!(self.open_menu, Some(TopMenu::Client(_))) { self.close_menus(); }
        }
        self.client_generation = generation;
        self.client_menu = root;
        if let Some(TopMenu::Client(id)) = self.open_menu {
            let mut parent = self.client_menu.as_ref().and_then(|root| root.find(id))
                .filter(|node| node.enabled && node.visible);
            if parent.is_none() { self.close_menus(); }
            else {
                let mut valid = 0;
                for id in &self.client_submenus {
                    parent = parent.and_then(|node| node.children.iter().find(|child| child.id == *id && child.enabled && child.visible && child.submenu));
                    if parent.is_none() { break; }
                    valid += 1;
                }
                self.client_submenus.truncate(valid);
            }
        }
        self.topbar_dirty = true;
        self.overlay_dirty = true;
        true
    }

    pub fn client_open_ids(&self) -> Vec<i32> {
        let Some(TopMenu::Client(id)) = self.open_menu else { return Vec::new() };
        std::iter::once(id).chain(self.client_submenus.iter().copied()).collect()
    }

    fn native_menu_root(&self,menu:TopMenu)->Option<&crate::appmenu::MenuNode>{
        if matches!(menu,TopMenu::Tray(_)){self.tray_menu.as_ref()?.3.as_deref()}else{self.client_menu.as_deref()}
    }
    pub fn set_input_sources(&mut self,sources:Vec<crate::settings::InputSource>,selected:usize,shortcut:&'static str){
        self.input_sources=sources;self.input_selected=selected;self.input_shortcut=shortcut;
        self.topbar_dirty=true;if self.open_menu==Some(TopMenu::Input){self.overlay_dirty=true;}
    }
    pub fn debug_input_hit(&self,x:f64)->bool{self.topbar_item_at(x as f32)==Some(TopbarItem::Layout)}
    pub fn input_menu_key(&mut self,direction:i32)->Option<ShellClickAction>{
        let rows=self.rows_for_menu(TopMenu::Input);
        let enabled:Vec<_>=rows.iter().enumerate().filter_map(|(i,r)|matches!(r,MenuRow::Item{action:Some(_),..}).then_some(i)).collect();
        if direction==0 {let action=self.menu_hovered.and_then(|i|rows.get(i)).and_then(|r|match r{MenuRow::Item{action,..}=>action.clone(),_=>None});if action.is_some(){self.close_menus();}return action;}
        if enabled.is_empty(){return None;}
        let current=self.menu_hovered.and_then(|i|enabled.iter().position(|n|*n==i));
        let next=match current{Some(i)=>(i as i32+direction).rem_euclid(enabled.len() as i32) as usize,None=>if direction>0{0}else{enabled.len()-1}};
        self.menu_hovered=Some(enabled[next]);self.overlay_dirty=true;None
    }
    pub fn tray_open_ids(&self)->Vec<i32>{
        let Some(TopMenu::Tray(id))=self.open_menu else{return Vec::new()};
        std::iter::once(id).chain(self.client_submenus.iter().copied()).collect()
    }
    pub fn set_tray_items(&mut self,items:Vec<crate::tray::Item>){
        if self.tray_menu.as_ref().is_some_and(|m|!items.iter().any(|i|i.key==m.0)){self.close_menus();}
        if self.tray_items!=items{
            if self.dock_menu.as_ref().is_some_and(|m|m.items.iter().any(|(_,a)|matches!(a,DockMenuAction::Tray(_)))){self.close_menus();}
            self.tray_items=items;self.topbar_dirty=true;
        }
    }
    pub fn prepare_tray_menu(&mut self,key:String,generation:u64,x:f32){
        self.tray_menu=Some((key,generation,x,None));self.open_menu=Some(TopMenu::Tray(0));self.overlay_dirty=true;
    }
    pub fn install_tray_menu(&mut self,key:&str,generation:u64,root:Option<std::sync::Arc<crate::appmenu::MenuNode>>)->bool {
        let Some(menu)=self.tray_menu.as_mut() else{return false};
        if menu.0!=key||menu.1!=generation{return false;}
        let Some(root)=root else{self.close_menus();return true};
        self.open_menu=Some(TopMenu::Tray(root.id));menu.3=Some(root);self.overlay_dirty=true;true
    }
    pub fn tray_hit(&self,x:f64,y:f64)->Option<(&crate::tray::Item,R)>{
        if y<0.0||y>=TOP_BAR_HEIGHT as f64{return None;}
        let (x0,x1,index)=self.topbar_hits.iter().find_map(|&(x0,x1,hit)|match hit{TopbarItem::Tray(i)if x>=x0 as f64&&x<x1 as f64=>Some((x0,x1,i)),_=>None})?;
        Some((self.tray_items.get(index)?,R::new(x0,0.0,x1-x0,TOP_BAR_HEIGHT as f32)))
    }
    fn tray_visible_count(&self)->usize{self.tray_items.len().min(((self.screen_w as f32-500.0)/28.0).max(1.0) as usize).min(12)}

    fn client_row_capacity(&self) -> usize {
        (((self.screen_h - TOP_BAR_HEIGHT - 16) as f32 / MENU_ITEM_H) as usize).saturating_sub(2).max(1)
    }

    /// Consume scrolling while a native menu is open, even outside its panel.
    pub fn scroll_client_menu(&mut self, px: f32, py: f32, delta: f64) -> bool {
        let Some(menu @ (TopMenu::Client(root)|TopMenu::Tray(root))) = self.open_menu else { return false };
        if delta == 0.0 { return true; }
        let panels = self.menu_panels();
        if let Some(depth) = panels.iter().rposition(|(r, _, _)| r.contains(px, py)) {
            let id = if depth == 0 { root } else { self.client_submenus[depth - 1] };
            let count = self.native_menu_root(menu).and_then(|r| r.find(id)).map_or(0, |n| n.children.iter().filter(|n| n.visible).count());
            let maximum = count.saturating_sub(self.client_row_capacity());
            let offset = self.client_scroll.entry(id).or_default();
            let next = if delta > 0.0 { (*offset + 3).min(maximum) } else { offset.saturating_sub(3) };
            if next != *offset {
                *offset = next;
                self.client_submenus.truncate(depth);
                self.client_hovered = None;
                self.overlay_dirty = true;
            }
        }
        true
    }

    /// Menu navigation changes only shell state. The caller defers returned actions until
    /// after KeyboardHandle::input has released its lock.
    pub fn client_menu_key(&mut self, direction: i32) -> Option<ShellClickAction> {
        let panels = self.menu_panels();
        let depth = self.client_hovered.map_or(panels.len().saturating_sub(1), |(d, _)| d.min(panels.len().saturating_sub(1)));
        let panel = panels.get(depth)?;
        if direction == -2 { // left: close one submenu
            if self.client_submenus.pop().is_none() { self.close_menus(); }
            self.client_hovered = None;
        } else if direction.abs() == 1 {
            let enabled: Vec<_> = panel.1.iter().enumerate().filter_map(|(i, r)| matches!(r, MenuRow::Item { action: Some(_), .. }).then_some(i)).collect();
            if enabled.is_empty() { return None; }
            let current = self.client_hovered.filter(|(d, _)| *d == depth).and_then(|(_, row)| enabled.iter().position(|i| *i == row));
            let next = match (current, direction) {
                (Some(i), 1) => (i + 1) % enabled.len(),
                (Some(i), _) => (i + enabled.len() - 1) % enabled.len(),
                (None, 1) => 0,
                _ => enabled.len() - 1,
            };
            self.client_hovered = Some((depth, enabled[next]));
        } else if let Some((d, row)) = self.client_hovered {
            if let Some(MenuRow::Item { submenu, action, .. }) = panels.get(d).and_then(|p| p.1.get(row)) {
                if let Some(id) = submenu {
                    self.client_submenus.truncate(d);
                    self.client_submenus.push(*id);
                    self.client_hovered = None;
                } else if direction == 0 {
                    let action = action.clone(); self.close_menus(); return action;
                }
            }
        }
        self.overlay_dirty = true;
        None
    }

    /// Exercise our own menu hit testing in screenshot/integration runs, without host input.
    pub fn debug_client_menu_path(&mut self, path: &str, activate: bool, registry: &AppRegistry) -> Option<ShellClickAction> {
        let parts: Vec<_> = path.split('/').collect();
        let root = self.client_menu.as_ref()?;
        let label = *parts.first()?;
        let node = root.children.iter().find(|n| n.label == label)?;
        let menu = TopMenu::Client(node.id);
        let (x0, x1, _) = *self.topbar_hits.iter().find(|(_, _, hit)| *hit == TopbarItem::Menu(menu))?;
        self.handle_pointer_click(((x0 + x1) * 0.5) as f64, 12.0, registry);
        for (depth, label) in parts.iter().skip(1).enumerate() {
            let panels = self.menu_panels();
            let panel = panels.get(depth)?;
            let row = panel.1.iter().position(|r| matches!(r, MenuRow::Item { label: text, .. } if text == label))?;
            let x = panel.0.x + 40.0;
            let y = panel.0.y + MENU_PAD + panel.1.iter().take(row).map(Self::row_height).sum::<f32>() + MENU_ITEM_H * 0.5;
            self.update_pointer_hover(x as f64, y as f64);
            if activate && depth + 2 == parts.len() {
                return self.handle_pointer_click(x as f64, y as f64, registry);
            }
        }
        Some(ShellClickAction::None)
    }

    fn row_height(row: &MenuRow) -> f32 {
        if matches!(row, MenuRow::Separator) { MENU_SEP_H } else { MENU_ITEM_H }
    }

    fn menu_size(rows: &[MenuRow]) -> (f32, f32) {
        let h = MENU_PAD * 2.0 + rows.iter().map(Self::row_height).sum::<f32>();
        let w = rows.iter().filter_map(|r| match r {
            MenuRow::Item { label, shortcut, .. } => Some(label.chars().count() as f32 * 7.3 + shortcut.chars().count() as f32 * 9.0 + 64.0),
            _ => None,
        }).fold(MENU_MIN_W, f32::max).min(420.0);
        (w, h)
    }

    /// Root menu plus cascaded submenus, in back-to-front order.
    fn menu_panels(&self) -> Vec<(R, Vec<MenuRow>, Option<usize>)> {
        let Some(menu) = self.open_menu else { return Vec::new() };
        let root = (self.menu_rect(menu), self.rows_for_menu(menu), self.menu_hovered);
        if !matches!(menu, TopMenu::Client(_)|TopMenu::Tray(_)) { return vec![root]; }
        let mut panels = vec![root];
        for (depth, id) in self.client_submenus.iter().enumerate() {
            let (parent, rows, _) = &panels[depth];
            let Some(row_index) = rows.iter().position(|r| matches!(r, MenuRow::Item { submenu: Some(child), action: Some(_), .. } if child == id)) else { break };
            let rows = self.rows_for_menu(if matches!(menu,TopMenu::Tray(_)){TopMenu::Tray(*id)}else{TopMenu::Client(*id)});
            let (w, h) = Self::menu_size(&rows);
            let w = w.min((self.screen_w as f32 - 8.0).max(1.0));
            let x = if parent.x + parent.w + w <= self.screen_w as f32 - 4.0 { parent.x + parent.w - 2.0 } else { (parent.x - w + 2.0).max(4.0) };
            let y = parent.y + MENU_PAD + panels[depth].1.iter().take(row_index).map(Self::row_height).sum::<f32>();
            let y = y.min((self.screen_h as f32 - h - 4.0).max(TOP_BAR_HEIGHT as f32 + 3.0));
            panels[depth].2 = Some(row_index);
            panels.push((R::new(x, y, w, h), rows, None));
        }
        if let Some((depth, row)) = self.client_hovered {
            if let Some(p) = panels.get_mut(depth) { p.2 = Some(row); }
        }
        panels
    }

    fn panel_item_at(&self, px: f32, py: f32) -> Option<(usize, usize)> {
        for (depth, (rect, rows, _)) in self.menu_panels().iter().enumerate().rev() {
            if !rect.contains(px, py) { continue; }
            let mut y = rect.y + MENU_PAD;
            for (i, row) in rows.iter().enumerate() {
                let h = Self::row_height(row);
                if py >= y && py < y + h {
                    return matches!(row, MenuRow::Item { action: Some(_), .. }).then_some((depth, i));
                }
                y += h;
            }
            return None;
        }
        None
    }

    fn hover_client_menu(&mut self, px: f32, py: f32) {
        let hovered = self.panel_item_at(px, py);
        if self.client_hovered != hovered { self.client_hovered = hovered; self.overlay_dirty = true; }
        if let Some((depth, row)) = hovered {
            let panels = self.menu_panels();
            let submenu = match &panels[depth].1[row] { MenuRow::Item { submenu, .. } => *submenu, _ => None };
            let mut path = self.client_submenus.clone();
            path.truncate(depth);
            if let Some(id) = submenu { path.push(id); }
            if path != self.client_submenus { self.client_submenus = path; self.overlay_dirty = true; }
        }
    }

    fn menu_rect(&self, menu: TopMenu) -> R {
        let rows = self.rows_for_menu(menu);
        let (w, h) = Self::menu_size(&rows);
        let w = w.min((self.screen_w as f32 - 8.0).max(1.0));
        let anchor = self
            .topbar_hits
            .iter()
            .find(|(_, _, it)| *it == TopbarItem::Menu(menu)||(menu==TopMenu::Input&&*it==TopbarItem::Layout))
            .map(|(x0, _, _)| *x0)
            .unwrap_or(6.0);
        let anchor=if matches!(menu,TopMenu::Tray(_)){self.tray_menu.as_ref().map_or(anchor,|m|m.2-w*0.5)}else{anchor};
        let x = (anchor - 2.0).clamp(4.0, (self.screen_w as f32 - w - 4.0).max(4.0));
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
        for (i, row) in self.rows_for_menu(menu).iter().enumerate() {
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
        Self::cc_layout_for(self.screen_w)
    }

    fn cc_layout_for(screen_w: i32) -> (R, Vec<(R, CcItem)>) {
        let w = 324.0;
        let x = screen_w as f32 - w - 8.0;
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

    pub fn ensure_cc_backdrop(&mut self) {
        if self.cc_backdrop.is_none() && !self.cc_backdrop_requested && crate::theme::has_glass_background() {
            if self.raster.as_ref().is_some_and(|r| r.submit(crate::raster::Job::CcBackdrop { screen: (self.screen_w, self.screen_h) })) {
                self.cc_backdrop_requested = true;
            }
        }
    }

    /// Pure static plate: glass, borders and card surfaces. Text and controls
    /// remain live. Toast is hidden while CC is open, so only one origin is needed.
    pub fn raster_cc_backdrop(screen: (i32, i32)) -> CcBackdrop {
        let (panel, items) = Self::cc_layout_for(screen.0);
        let bounds = panel.grow(16.0).to_buffer(to_phys(screen.0), to_phys(screen.1)).unwrap();
        let (ox, oy) = (bounds.loc.x as f32 / ui_scale(), bounds.loc.y as f32 / ui_scale());
        crate::theme::set_glass_origin(ox, oy);
        let mut pixels = tiny_skia::Pixmap::new(bounds.size.w as u32, bounds.size.h as u32).unwrap();
        let mut pm = pixels.as_mut();
        draw_glass_panel(&mut pm, panel.x - ox, panel.y - oy, panel.w, panel.h, 22.0, false);
        let card = if appearance() == Appearance::Dark { rgba(255, 255, 255, 20) } else { rgba(255, 255, 255, 140) };
        for (r, _) in items { fill_rounded_rect(&mut pm, r.x - ox, r.y - oy, r.w, r.h, 14.0, card); }
        CcBackdrop { bounds, pixels }
    }

    fn spotlight_bar_rect(&self) -> R {
        let w = 640.0;
        let x = (self.screen_w as f32 - w) * 0.5;
        let y = (self.screen_h as f32 * 0.22).max(TOP_BAR_HEIGHT as f32 + 40.0);
        R::new(x, y, w, 52.0)
    }

    /// (search bar, results panel, app result count, has calculator row)
    fn spotlight_layout(&self, registry: &AppRegistry) -> (R, Option<R>, usize, bool) {
        let bar = self.spotlight_bar_rect();
        let (x, y, w) = (bar.x, bar.y, bar.w);
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

    fn toast_visible(&self)->bool {
        self.toast.is_some()&&!self.control_center_open&&!matches!(self.open_menu,Some(TopMenu::Tray(_)))
    }
    fn toast_rect(&self) -> R { Self::toast_rect_for(self.screen_w) }

    fn toast_rect_for(screen_w: i32) -> R {
        R::new(screen_w as f32 - 344.0 - 10.0, TOP_BAR_HEIGHT as f32 + 8.0, 344.0, 72.0)
    }

    // ───────────────────────── Pointer ─────────────────────────

    pub fn update_pointer_hover(&mut self, px: f64, py: f64) {
        let hover=if py>=0.0&&py<TOP_BAR_HEIGHT as f64{self.topbar_item_at(px as f32)}else{None};
        if hover!=self.status_hover{self.status_hover=hover;self.topbar_dirty=true;}
        if self.control_center_open {
            let (panel, items) = self.cc_layout();
            let pos = (px as f32, py as f32);
            let inside = panel.contains(pos.0, pos.1).then_some(pos);
            let key = |cc: &crate::control_panel::Panel| {
                if cc.page == crate::control_panel::Page::Main {
                    cc.hover.and_then(|(x, y)| items.iter().position(|(r, _)| r.contains(x, y))).map(|i| i as u32 + 1000)
                } else {
                    cc.hover_target(panel).map(|(k, _)| k)
                }
            };
            let before = key(&self.cc);
            self.cc.hover = inside;
            if key(&self.cc) != before {
                self.overlay_dirty = true;
            }
        } else {
            self.cc.hover = None;
        }
        if self.open_menu.is_some() {
            // Like macOS: with a menu open, sliding along the menu bar switches menus
            if py < TOP_BAR_HEIGHT as f64 {
                if let Some(TopbarItem::Menu(m)) = self.topbar_item_at(px as f32) {
                    if Some(m) != self.open_menu {
                        self.open_menu = Some(m);
                        self.menu_hovered = None;
                        self.client_submenus.clear();
                        self.client_hovered = None;
                        self.overlay_dirty = true;
                        self.topbar_dirty = true;
                    }
                }else if self.topbar_item_at(px as f32)==Some(TopbarItem::Layout)&&self.open_menu!=Some(TopMenu::Input){
                    self.open_menu=Some(TopMenu::Input);self.menu_hovered=None;self.client_submenus.clear();self.client_hovered=None;self.overlay_dirty=true;self.topbar_dirty=true;
                }
            }
            if matches!(self.open_menu, Some(TopMenu::Client(_)|TopMenu::Tray(_))) {
                self.hover_client_menu(px as f32, py as f32);
                return;
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
            if new_cursor.is_none() {
                self.dock_anchor = None;
            }
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
                Some(TopbarItem::Clock)=>return Some(ShellClickAction::SystemSettings(crate::system_settings::Action::Page(crate::system_settings::Page::DateTime))),
                Some(TopbarItem::ControlCenter) | Some(TopbarItem::Wifi) => {
                    self.control_center_open = !was.1
                }
                Some(TopbarItem::Spotlight) => self.spotlight_open = !was.2,
                Some(TopbarItem::Layout) => {
                    self.open_menu=(was.0!=Some(TopMenu::Input)).then_some(TopMenu::Input);
                    self.topbar_dirty=true;self.overlay_dirty=true;
                }
                Some(TopbarItem::Tray(index)) => {
                    if let Some(item)=self.tray_items.get(index){return Some(ShellClickAction::Tray{key:item.key.clone(),action:crate::tray::Action::Activate,x:mx as i32,y:my as i32});}
                }
                Some(TopbarItem::TrayOverflow) => {
                    return Some(ShellClickAction::TrayOverflow);
                }
                None => {}
            }
            self.overlay_dirty = true;
            self.topbar_dirty = true;
            return Some(ShellClickAction::None);
        }

        if matches!(self.open_menu, Some(TopMenu::Client(_)|TopMenu::Tray(_))) {
            if let Some((depth, row)) = self.panel_item_at(mx, my) {
                if let Some(MenuRow::Item { submenu, action, .. }) = self.menu_panels().get(depth).and_then(|p| p.1.get(row)).cloned() {
                    if submenu.is_some() { self.hover_client_menu(mx, my); }
                    else { self.close_menus(); return action; }
                    return Some(ShellClickAction::None);
                }
            }
            self.close_menus();
            return Some(ShellClickAction::None);
        }
        if self.open_menu.is_some() {
            let idx = self.menu_item_at(mx, my);
            let rows = self.open_menu.map(|m| self.rows_for_menu(m)).unwrap_or_default();
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
            if self.cc.page!=crate::control_panel::Page::Main {return Some(self.cc.click(panel,mx,my));}
            if let Some((r, item)) = items.iter().find(|(r, _)| r.contains(mx, my)) {
                match item {
                    CcItem::Wifi => { self.cc.open(crate::control_panel::Page::Wifi); return Some(ShellClickAction::Control(crate::controls::Control::Scan)); }
                    CcItem::Bluetooth => return Some(ShellClickAction::BluetoothSettings),
                    CcItem::Focus => self.dnd_enabled = !self.dnd_enabled,
                    CcItem::DarkMode => return Some(ShellClickAction::ToggleAppearance),
                    CcItem::Brightness => { self.cc.error=Some("Яркость этого дисплея недоступна".into()); }
                    CcItem::Volume => {
                        if my<r.y+28.0 {
                            if mx<r.x+45.0 {return Some(self.cc.mute());}
                            self.cc.open(crate::control_panel::Page::Sound);
                        } else {self.cc.start_slider(R::new(r.x+14.0,r.y+30.0,r.w-28.0,20.0),mx);}
                    }
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

        if self.toast_visible() && self.toast_rect().contains(mx, my) {
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
        if self.raster.as_ref().is_some_and(|worker| worker.submit(crate::raster::Job::Wallpaper {
            logical: (self.screen_w, self.screen_h), theme: self.theme,image:self.wallpaper_image.clone(),
        })) { self.wallpaper_dirty = false; }
    }

    pub fn upload_wallpaper(&mut self,renderer:&mut smithay::backend::renderer::gles::GlesRenderer){
        use smithay::backend::renderer::{ImportMem,element::texture::TextureBuffer};
        if let Some((pixels,size))=self.wallpaper_pending.take(){
            match renderer.import_memory(&pixels,Fourcc::Abgr8888,size.into(),false){Ok(texture)=>{self.wallpaper_texture=Some(TextureBuffer::from_texture(renderer,texture,1,Transform::Normal,None));self.wallpaper_texture_size=size;},Err(e)=>tracing::warn!("wallpaper upload: {e:?}")}
            self.request_raster_reclaim();
        }
    }

    pub fn ensure_topbar_rendered(&mut self, fonts: &mut FontSystem, active_app_title: &str, _active_layout: &str) {
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
        let mut hits = Vec::with_capacity(16);
        let tray=self.tray_items.clone();let visible_tray=self.tray_visible_count();
        let tray_width=visible_tray as f32*28.0+if tray.len()>visible_tray{30.0}else{0.0};
        let input=self.input_sources.get(self.input_selected).copied().unwrap_or(crate::settings::SOURCES[0]);
        let hover=self.status_hover;
        let mut menus = vec![(active_app_title.to_string(), TopMenu::App, true, true)];
        if let Some(root) = self.client_menu.as_ref().filter(|root| root.children.iter().any(|n| n.visible)) {
            menus.extend(root.children.iter().filter(|n| n.visible && !n.separator).map(|n| (n.label.clone(), TopMenu::Client(n.id), false, n.enabled)));
        } else {
            menus.extend([("Окно".into(), TopMenu::Window, false, true), ("Справка".into(), TopMenu::Help, false, true)]);
        }

        let icons=&mut self.icons;
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
                let mut x = 46.0;
                let right_reserved = fonts.measure_text(&clock, 13.0, false).max(146.0) + 162.0+tray_width;
                for (label, menu, bold, enabled) in &menus {
                    let available = lw - right_reserved - x - 12.0;
                    if available < 24.0 { break; }
                    let max_width = if *bold { available.min(180.0) } else { available };
                    let label = fonts.ellipsize(label, max_width, 13.0, *bold);
                    let w = fonts.measure_text(&label, 13.0, *bold);
                    if open_menu == Some(*menu) { highlight(&mut pm, x - 8.0, x + w + 8.0); }
                    fonts.draw_text(&mut pm, &label, x, text_y, 13.0,
                        if *enabled { p.menubar_text } else { p.text_tertiary }, *bold);
                    if *enabled { hits.push((x - 9.0, x + w + 9.0, TopbarItem::Menu(*menu))); }
                    x += w + 20.0;
                }

                // A fixed grid keeps targets aligned when the source or clock changes.
                let mut rx=lw-14.0;
                let cw=fonts.measure_text(&clock,13.0,false);
                let clock_width=cw.max(146.0);
                if hover==Some(TopbarItem::Clock){highlight(&mut pm,rx-clock_width-4.0,lw-5.0);}
                fonts.draw_text(&mut pm,&clock,rx-cw,text_y,13.0,p.menubar_text,false);
                hits.push((rx-clock_width-4.0,lw,TopbarItem::Clock));rx-=clock_width+14.0;
                for (hit,opened) in [(TopbarItem::ControlCenter,cc_open),(TopbarItem::Spotlight,sp_open),(TopbarItem::Wifi,cc_open)]{
                    if opened||hover==Some(hit){highlight(&mut pm,rx-29.0,rx-1.0);}
                    let center=rx-15.0;
                    match hit {
                        TopbarItem::ControlCenter=>draw_control_center_icon(&mut pm,center,lh*0.5,p.menubar_text),
                        TopbarItem::Spotlight=>draw_search_icon(&mut pm,center,lh*0.5,5.0,p.menubar_text),
                        TopbarItem::Wifi=>draw_wifi_icon(&mut pm,center,lh*0.5-2.0,p.menubar_text,wifi),_=>{}
                    }
                    hits.push((rx-30.0,rx,hit));rx-=30.0;
                }
                if open_menu==Some(TopMenu::Input)||hover==Some(TopbarItem::Layout){highlight(&mut pm,rx-31.0,rx-1.0);}
                crate::system_settings::draw_input_badge(&mut pm,fonts,input.badge,rx-27.0,(lh-16.0)*0.5,22.0,16.0,p.menubar_text);
                hits.push((rx-32.0,rx,TopbarItem::Layout));rx-=40.0;
                for (index,item) in tray.iter().take(visible_tray).enumerate(){
                    let x=rx-23.0;let y=(lh-18.0)*0.5;
                    if hover==Some(TopbarItem::Tray(index)){highlight(&mut pm,rx-27.0,rx-1.0);}
                    // A client pixmap is authoritative; an unresolved theme name
                    // must not replace it with the generic application fallback.
                    let icon=if item.pixels.is_none()&&!item.icon_name.is_empty(){
                        icons.get(&crate::icons::IconKey{icon:AppIcon{kind:crate::apps::IconKind::GenericApp,name:Some(item.icon_name.clone())},initial:String::new(),px:to_phys(18) as u32,dark:appearance()==Appearance::Dark})
                    }else{None};
                    if let Some(icon)=icon {crate::icons::blit_icon(&mut pm,icon,x,y);}
                    else if let Some(image)=&item.pixels {
                        let size=18.0*ui_scale();
                        pm.draw_pixmap(0,0,image.as_ref().as_ref(),&tiny_skia::PixmapPaint{quality:tiny_skia::FilterQuality::Bilinear,..Default::default()},tiny_skia::Transform::from_scale(size/image.width() as f32,size/image.height() as f32).post_translate((x*ui_scale()).round(),(y*ui_scale()).round()),None);
                    }else{
                        crate::theme::stroke_circle(&mut pm,x+9.0,y+9.0,6.0,1.3,p.menubar_text);
                        let label=item.title.chars().next().unwrap_or('•').to_string();
                        fonts.draw_text_centered(&mut pm,&label,x+9.0,y+2.0,10.0,p.menubar_text,false);
                    }
                    if item.attention{fill_circle(&mut pm,x+17.0,y+2.0,2.0,p.accent);}
                    hits.push((rx-28.0,rx,TopbarItem::Tray(index)));rx-=28.0;
                }
                if tray.len()>visible_tray {
                    let label=format!("+{}",tray.len()-visible_tray);
                    fonts.draw_text_centered(&mut pm,&label,rx-14.0,text_y,11.0,p.menubar_text,false);
                    hits.push((rx-30.0,rx,TopbarItem::TrayOverflow));
                }
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
        // Separators: apps | minimized windows | Trash (the Trash keeps its own section).
        let first_min = self.dock_items.iter().position(|i| matches!(i.target, DockTarget::Minimized(_)));
        let trash_at = self.dock_items.iter().position(|i| i.target == DockTarget::Trash);
        let mut boundaries: Vec<usize> = Vec::with_capacity(2);
        if let Some(start) = first_min.or(trash_at).filter(|&s| s > 0) {
            boundaries.push(start);
        }
        if let (Some(_), Some(t)) = (first_min, trash_at) {
            boundaries.push(t);
        }
        const SEP: f32 = 14.0;
        let sep_w = SEP * boundaries.len() as f32;
        let buf_w = self.dock_buf_w as f32;
        let buf_h = self.dock_buf_h as f32;

        // Base layout (unmagnified) to know where the cursor is relative to icons
        let base_total = n as f32 * DOCK_ICON_BASE + n.saturating_sub(1) as f32 * DOCK_ICON_GAP + sep_w;
        let base_x0 = (buf_w - base_total) * 0.5;
        let base_center = |i: usize| {
            base_x0
                + i as f32 * (DOCK_ICON_BASE + DOCK_ICON_GAP)
                + SEP * boundaries.iter().filter(|&&b| i >= b).count() as f32
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
        let mut bar = R::new((buf_w - bar_w) * 0.5, buf_h - bar_h - DOCK_BOTTOM_MARGIN as f32, bar_w, bar_h);
        let baseline = bar.y + bar.h - DOCK_PAD_Y;

        let mut slots = Vec::with_capacity(n);
        let mut x = bar.x + DOCK_PAD_X;
        let mut sep_x: Vec<f32> = Vec::with_capacity(2);
        for (i, item) in self.dock_items.iter().enumerate() {
            if gap == Some(i) {
                x += gap_w;
            }
            if boundaries.contains(&i) {
                sep_x.push(x + SEP * 0.5 - DOCK_ICON_GAP * 0.5);
                x += SEP;
            }
            slots.push(DockSlot {
                target: item.target,
                x,
                y: baseline - sizes[i],
                size: sizes[i],
            });
            x += sizes[i] + DOCK_ICON_GAP;
        }

        if let Some((target, center)) = self.dock_anchor {
            if let Some(slot) = slots.iter().find(|s| s.target == target) {
                let shift = center - (slot.x + slot.size * 0.5);
                bar.x += shift;
                for slot in &mut slots { slot.x += shift; }
                for sx in &mut sep_x { *sx += shift; }
            }
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
        let bouncing = self.bounce.as_ref().map(|b| b.target);
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

                for &sx in &sep_x {
                    stroke_line(&mut pm, sx, bar.y + 10.0, sx, bar.y + bar.h - 10.0, 1.0, p.separator);
                }

                for (slot, item) in slots.iter().zip(items.iter()) {
                    let n = item.windows.min(3) as f32;
                    for k in 0..n as usize {
                        let dx = (k as f32 - (n - 1.0) * 0.5) * 5.0;
                        fill_circle(&mut pm, slot.x + slot.size * 0.5 + dx, bar.y + bar.h - 3.0, 1.9, p.dock_dot);
                    }
                    // The bouncing icon is drawn as its own GPU element above the Dock
                    if bouncing == Some(item.target) {
                        continue;
                    }
                    if slot.size == DOCK_ICON_BASE {
                        draw_any_icon(&mut pm, fonts, icons, &item.icon, &item.label, slot.x, slot.y, slot.size);
                    } else {
                        draw_resampled_icon(&mut pm, icons, &item.icon, &item.label, slot.x, slot.y, slot.size);
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
        self.update_dock_menu_anchor();
    }

    /// Logical rectangles of all currently visible overlay panels
    fn overlay_panel_rects(&self, registry: &AppRegistry) -> Vec<R> {
        let mut rects = Vec::with_capacity(4);
        rects.extend(self.menu_panels().into_iter().map(|(r, _, _)| r));
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
        if self.toast_visible() {
            rects.push(self.toast_rect());
        }
        if let Some(m) = &self.dock_menu {
            rects.push(m.rect);
        }
        rects
    }

    pub fn ensure_overlay_rendered(&mut self, fonts: &mut FontSystem, registry: &AppRegistry, _active_layout: &str) {
        if self.overlay_allocated && !self.overlay_dirty {
            return;
        }
        self.overlay_dirty = false;

        let screen_pw = to_phys(self.screen_w);
        let screen_ph = to_phys(self.screen_h);
        let current: Vec<_> = self
            .overlay_panel_rects(registry)
            .into_iter()
            .filter_map(|r| r.grow(16.0).to_buffer(screen_pw, screen_ph))
            .collect();
        let Some(bounds) = current.iter().copied().reduce(|a, b| a.merge(b)) else { return };
        let fresh = !self.overlay_allocated || self.overlay_geometry != bounds;
        if fresh {
            // Cache only the painted panels, never a screen-sized transparent canvas.
            self.overlay_buffer = new_buffer(bounds.size.w, bounds.size.h);
            self.overlay_geometry = bounds;
            self.overlay_allocated = true;
            self.overlay_painted.clear();
        }
        let (pw, ph) = (bounds.size.w, bounds.size.h);
        let mut damage = std::mem::take(&mut self.overlay_painted);
        damage.extend(current.iter().copied());
        self.overlay_painted = current;
        for rect in &mut damage { rect.loc -= bounds.loc; }
        // MemoryRenderBuffer::new is already transparent. Do not fault in and
        // clear a second time the large menu+toast canvas on its first paint.
        let clear = if fresh { Vec::new() } else { damage.clone() };
        let (ox, oy) = (bounds.loc.x as f32 / ui_scale(), bounds.loc.y as f32 / ui_scale());
        let local = |mut r: R| { r.x -= ox; r.y -= oy; r };

        let p = palette();
        let menu_panels: Vec<_> = self.menu_panels().into_iter().map(|(r, rows, hovered)| (local(r), rows, hovered)).collect();
        let (panel, items) = self.cc_layout();
        let cc = (local(panel), items.into_iter().map(|(r, item)| (local(r), item)).collect::<Vec<_>>());
        let (bar, list, count, math) = self.spotlight_layout(registry);
        let sp = (local(bar), list.map(local), count, math);
        let toast_rect = local(self.toast_rect());
        let toast = self.toast.clone().filter(|_| self.toast_visible());
        let input_badges:Vec<_>=if self.open_menu==Some(TopMenu::Input){self.input_sources.iter().map(|s|s.badge).collect()}else{Vec::new()};
        let active_input_label=self.input_sources.get(self.input_selected).map_or("ABC",|s|s.label);
        let (cc_open, sp_open) = (self.control_center_open, self.spotlight_open);
        let query = self.spotlight_query.clone();
        let spotlight_input = self.spotlight_input.clone();
        let selected = self.spotlight_selected;
        let (wifi, dnd, vol) = (self.wifi_enabled, self.dnd_enabled, self.cc.volume());
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
        let mut dock_menu = self.dock_menu.clone();
        if let Some(m) = dock_menu.as_mut() { m.rect = local(m.rect); }
        let cc_backdrop = self.cc_backdrop.as_ref().filter(|_| menu_panels.is_empty() && !sp_open && dock_menu.is_none());
        let icons = &mut self.icons;
        let controls = &self.cc;
        crate::theme::set_glass_origin(ox, oy);

        let mut ctx = self.overlay_buffer.render();
        let _ = ctx.draw(|buf| {
            clear_rects(buf, pw, &clear);
            let Some(mut pm) = PixmapMut::from_bytes(buf, pw as u32, ph as u32) else {
                return Result::<_, ()>::Ok(damage);
            };

            for (r, menu_rows_now, hovered) in &menu_panels {
                draw_glass_panel(&mut pm, r.x, r.y, r.w, r.h, 12.0, true);
                let mut y = r.y + MENU_PAD;
                for (i, row) in menu_rows_now.iter().enumerate() {
                    match row {
                        MenuRow::Separator => {
                            let sy = y + MENU_SEP_H * 0.5;
                            stroke_line(&mut pm, r.x + 14.0, sy, r.x + r.w - 14.0, sy, 1.0, p.separator);
                            y += MENU_SEP_H;
                        }
                        MenuRow::Item { label, shortcut, action, checked, radio, submenu } => {
                            let enabled = action.is_some();
                            let hot = enabled && *hovered == Some(i);
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
                                fonts.draw_text(&mut pm, if *radio { "●" } else { "✓" }, r.x + 10.0, y + 3.5, 13.0, col, false);
                            }
                            if submenu.is_some() {
                                fonts.draw_text_right(&mut pm, "›", r.x + r.w - 12.0, y + 2.0, 16.0, col, false);
                            }
                            let sw = if shortcut.is_empty() {
                                0.0
                            } else {
                                let sc = if hot { p.on_accent } else { p.text_tertiary };
                                fonts.draw_text_right(&mut pm, shortcut, r.x + r.w - 14.0, y + 3.5, 13.0, sc, false) + 16.0
                            };
                            let inset=if let Some(badge)=input_badges.get(i){
                                crate::system_settings::draw_input_badge(&mut pm,fonts,badge,r.x+27.0,y+5.0,22.0,16.0,col);58.0
                            }else{24.0};
                            fonts.draw_text_clipped(&mut pm, label, r.x + inset, y + 3.5, r.w - inset-14.0 - sw, 13.0, col, false);
                            y += MENU_ITEM_H;
                        }
                    }
                }
            }

            if cc_open && controls.page!=crate::control_panel::Page::Main {
                controls.paint(&mut pm,fonts,cc.0);
            } else if cc_open {
                let (panel, items) = &cc;
                if let Some(backdrop) = cc_backdrop {
                    let stride = pw as usize * 4;
                    let source_stride = backdrop.bounds.size.w as usize * 4;
                    let x = (backdrop.bounds.loc.x - bounds.loc.x) as usize;
                    let y = (backdrop.bounds.loc.y - bounds.loc.y) as usize;
                    for (row, source) in backdrop.pixels.data().chunks_exact(source_stride).enumerate() {
                        let start = (y + row) * stride + x * 4;
                        pm.data_mut()[start..start + source_stride].copy_from_slice(source);
                    }
                } else {
                    draw_glass_panel(&mut pm, panel.x, panel.y, panel.w, panel.h, 22.0, false);
                }
                let card = if dark { rgba(255, 255, 255, 20) } else { rgba(255, 255, 255, 140) };
                // Hover: global pointer → this buffer's local coordinates
                let cc_hover = controls.hover.map(|(x, y)| (x - ox, y - oy));
                for (r, item) in items {
                    if cc_backdrop.is_none() { fill_rounded_rect(&mut pm, r.x, r.y, r.w, r.h, 14.0, card); }
                    if cc_hover.is_some_and(|(x, y)| r.contains(x, y)) {
                        fill_rounded_rect(&mut pm, r.x, r.y, r.w, r.h, 14.0, if dark { rgba(255, 255, 255, 18) } else { rgba(255, 255, 255, 110) });
                    }
                    let toggle = |pm: &mut PixmapMut, fonts: &mut FontSystem, on: bool, title: &str, sub: &str, icon:crate::control_panel::Icon| {
                        let (cx, cy) = (r.x + 25.0, r.y + r.h * 0.5);
                        fill_circle(pm, cx, cy, 14.0, if on { p.accent } else { p.control_fill });
                        crate::control_panel::icon(pm,icon,cx,cy,if on {p.on_accent} else {p.text_secondary},false);
                        fonts.draw_text_clipped(pm, title, r.x + 46.0, r.y + r.h * 0.5 - 15.0, r.w - 52.0, 13.0, p.text, true);
                        fonts.draw_text_clipped(pm, sub, r.x + 46.0, r.y + r.h * 0.5 + 1.0, r.w - 52.0, 11.5, p.text_secondary, false);
                    };
                    match item {
                        CcItem::Wifi => toggle(&mut pm, fonts, wifi, "Wi‑Fi", controls.wifi.subtitle(),crate::control_panel::Icon::Wifi),
                        CcItem::Bluetooth => toggle(&mut pm, fonts, false, "Bluetooth", "Настроить…",crate::control_panel::Icon::Bluetooth),
                        CcItem::Focus => toggle(&mut pm, fonts, dnd, "Не беспокоить", if dnd { "Вкл." } else { "Выкл." },crate::control_panel::Icon::Moon),
                        CcItem::DarkMode => toggle(&mut pm, fonts, dark, "Тёмный режим", if dark { "Вкл." } else { "Выкл." },crate::control_panel::Icon::Contrast),
                        CcItem::Brightness => {
                            crate::control_panel::icon(&mut pm,crate::control_panel::Icon::Sun,r.x+25.0,r.y+27.0,p.text_secondary,false);
                            fonts.draw_text(&mut pm,"Дисплей",r.x+46.0,r.y+10.0,13.0,p.text,true);
                            fonts.draw_text_clipped(&mut pm,"Яркость недоступна",r.x+46.0,r.y+29.0,r.w-58.0,11.5,p.text_secondary,false);
                        }
                        CcItem::Volume => {
                            crate::control_panel::icon(&mut pm,crate::control_panel::Icon::Speaker,r.x+25.0,r.y+16.0,p.text,controls.audio.current().is_some_and(|s|s.muted));
                            fonts.draw_text(&mut pm,"Звук",r.x+46.0,r.y+7.0,13.0,p.text,true);
                            let sub=controls.audio.current().map(|s| if s.muted {"Без звука".into()} else {format!("{}% · Устройства ›",(vol*100.0).round() as u32)}).unwrap_or_else(||"Нет устройств ›".into());
                            fonts.draw_text_right(&mut pm,&sub,r.x+r.w-14.0,r.y+8.0,11.0,p.text_secondary,false);
                            crate::control_panel::slider(&mut pm,R::new(r.x+14.0,r.y+30.0,r.w-28.0,20.0),vol,controls.audio.current().is_some());
                        }
                        CcItem::Scale => {
                            crate::control_panel::icon(&mut pm,crate::control_panel::Icon::Scale,r.x+23.0,r.y+25.0,p.text_secondary,false);
                            fonts.draw_text(&mut pm, "Масштаб", r.x + 42.0, r.y + 8.0, 13.0, p.text, true);
                            fonts.draw_text(&mut pm, &scale_pct, r.x + 42.0, r.y + 26.0, 12.0, p.text_secondary, false);
                        }
                        CcItem::Wallpaper => {
                            crate::control_panel::icon(&mut pm,crate::control_panel::Icon::Wallpaper,r.x+23.0,r.y+25.0,p.text_secondary,false);
                            fonts.draw_text(&mut pm, "Обои", r.x + 42.0, r.y + 8.0, 13.0, p.text, true);
                            fonts.draw_text_clipped(&mut pm, theme_name, r.x + 42.0, r.y + 26.0, r.w - 50.0, 12.0, p.text_secondary, false);
                        }
                        CcItem::Layout => {
                            crate::control_panel::icon(&mut pm,crate::control_panel::Icon::Keyboard,r.x+25.0,r.y+20.0,p.text_secondary,false);
                            fonts.draw_text(&mut pm, "Источник ввода", r.x + 46.0, r.y + 11.5, 13.0, p.text, true);
                            let name = active_input_label;
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
                    let mut field = spotlight_input.clone();
                    if field.text() != query {
                        field.set(&query);
                    }
                    field.draw(&mut pm, fonts, bar.x + 50.0, bar.y + 14.0, bar.w - 70.0, 20.0, false, p.text, true, p.accent);
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

    pub fn switcher_hit(&self,x:f64,y:f64)->Option<usize>{
        let sw=self.switcher.as_ref().filter(|s|s.visible)?;
        let n=sw.items.len();if n==0{return None;}
        let cell=((self.screen_w as f32-108.0)/n as f32).clamp(40.0,96.0);
        let rect=self.switcher_rect;
        let (x,y)=(x as f32-rect.x-14.0,y as f32-rect.y-14.0);
        (x>=0.0&&x<cell*n as f32&&y>=0.0&&y<cell).then_some((x.max(0.0)/cell) as usize)
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

#[cfg(test)]
mod dock_tests {
    use super::*;
    use std::time::Duration;

    fn item(target: DockTarget) -> DockItem {
        DockItem { target, label: "Test".into(), icon: AppIcon { kind: crate::apps::IconKind::Terminal, name: None }, windows: 1, pinned: true }
    }

    #[test]
    fn minimizing_and_restoring_keep_the_hovered_app_under_the_same_point() {
        let mut fonts = FontSystem::new();
        for zoom in [false, true] {
            let mut shell = MacShell::new(1440, 900);
            shell.dock_zoom_enabled = zoom;
            let items: Vec<_> = (0..9).map(|i| item(DockTarget::App(i))).collect();
            shell.set_dock_items(items.clone());
            shell.ensure_dock_rendered(&mut fonts);
            let slot = shell.dock_slots[3].clone();
            shell.dock_cursor_x = Some(slot.x + slot.size * 0.5);
            shell.dock_dirty = true;
            shell.ensure_dock_rendered(&mut fonts);
            let slot = shell.dock_slots[3].clone();
            let center = slot.x + slot.size * 0.5;
            let (ox, oy) = shell.dock_origin();
            let point = ((ox + center) as f64, (oy + slot.y + slot.size * 0.5) as f64);
            for _ in 0..4 {
                let mut minimized = items.clone();
                minimized.push(item(DockTarget::Minimized(99)));
                for model in [minimized, items.clone()] {
                    shell.set_dock_items(model);
                    shell.ensure_dock_rendered(&mut fonts);
                    let slot = &shell.dock_slots[3];
                    assert!((slot.x + slot.size * 0.5 - center).abs() < 0.01);
                    assert_eq!(shell.dock_target_at(point.0, point.1).map(|(t, _)| t), Some(DockTarget::App(3)));
                }
            }
        }
    }

    fn bounce(started: Instant) -> Bounce {
        Bounce { target: DockTarget::App(0), app: 0, started, first_new_window: 1,
            finish_at: None, buffer: new_buffer(1, 1), px: 1 }
    }

    #[test]
    fn dock_menu_follows_the_icon_when_magnification_disappears() {
        let mut fonts = FontSystem::new();
        let mut shell = MacShell::new(1440,900);
        shell.set_dock_items((0..9).map(|i|item(DockTarget::App(i))).collect());
        shell.ensure_dock_rendered(&mut fonts);
        let s=&shell.dock_slots[3];
        let (ox,oy)=shell.dock_origin();
        shell.update_pointer_hover((ox+s.x+s.size*0.5) as f64,(oy+s.y+s.size*0.5) as f64);
        shell.ensure_dock_rendered(&mut fonts);
        let (x,y,size)=shell.dock_slot_global(DockTarget::App(3)).unwrap();
        shell.open_dock_menu(DockTarget::App(3),R::new(x,y,size,size),vec![("Open".into(),DockMenuAction::Open)],&mut fonts);
        let before=shell.dock_menu.as_ref().unwrap().rect;
        shell.update_pointer_hover(20.0,60.0);
        shell.ensure_dock_rendered(&mut fonts);
        let (x,y,size)=shell.dock_slot_global(DockTarget::App(3)).unwrap();
        let rect=shell.dock_menu.as_ref().unwrap().rect;
        assert_ne!(before,rect);
        assert!((rect.y+rect.h+10.0-y).abs()<0.01);
        assert!((rect.x+rect.w*0.5-x-size*0.5).abs()<0.01);
        assert!(shell.overlay_dirty);
        // A desktop context menu has a pointer anchor and must stay where it opened.
        shell.open_context_menu(vec![("Open".into(),DockMenuAction::Open)],300.0,400.0,&mut fonts);
        let rect=shell.dock_menu.as_ref().unwrap().rect;
        shell.dock_dirty=true;
        shell.ensure_dock_rendered(&mut fonts);
        assert_eq!(rect,shell.dock_menu.as_ref().unwrap().rect);
    }

    #[test]
    fn bounce_finishes_at_landing_even_when_frames_skip_the_landing() {
        let start = Instant::now();
        let mut b = bounce(start);
        assert_eq!(b.offset(start), (0.0, false));
        let peak = start + Duration::from_secs_f32(BOUNCE_PERIOD * 0.5);
        assert!((b.offset(peak).0 - BOUNCE_HEIGHT).abs() < 0.01);
        b.finish(peak);
        b.finish(start + Duration::from_secs(1)); // Must not postpone the landing.
        assert_eq!(b.offset(start + Duration::from_millis(630)), (0.0, true));
        let b = bounce(start);
        assert_eq!(b.offset(start + Duration::from_secs(6)), (0.0, true));
    }
}

#[cfg(test)]
mod appmenu_tests {
    use super::*;
    use crate::appmenu::MenuNode;
    use std::sync::Arc;

    fn node(id: i32, label: &str, children: Vec<MenuNode>) -> MenuNode {
        MenuNode { id, label: label.into(), shortcut: String::new(), enabled: true, visible: true,
            separator: false, checked: false, radio: false, submenu: !children.is_empty(), children }
    }
    fn shell() -> MacShell {
        let mut shell = MacShell::new(1000, 700);
        let mut disabled = node(3, "Disabled", vec![]); disabled.enabled = false;
        let mut hidden = node(4, "Hidden", vec![]); hidden.visible = false;
        let tree = node(0, "", vec![node(1, "File", vec![node(2, "Run", vec![]), disabled, hidden, node(5, "Nested", vec![node(6, "Child", vec![])])])]);
        shell.set_client_menu(7, Some(Arc::new(tree)));
        shell.topbar_hits.push((40.0, 90.0, TopbarItem::Menu(TopMenu::Client(1))));
        shell
    }

    #[test]
    fn client_menu_hit_testing_opens_cascade_and_dispatches_the_selected_leaf() {
        let mut shell = shell();
        let registry = AppRegistry::new();
        assert!(shell.debug_client_menu_path("File/Nested", false, &registry).is_some());
        assert_eq!(shell.client_open_ids(), vec![1, 5]);
        let panels = shell.menu_panels();
        assert_eq!(panels.len(), 2);
        assert!(panels.iter().all(|(r, _, _)| r.x >= 0.0 && r.y >= TOP_BAR_HEIGHT as f32));
        shell.close_menus();
        let action = shell.debug_client_menu_path("File/Nested/Child", true, &registry);
        assert!(matches!(action, Some(ShellClickAction::AppMenuActivate { generation: 7, id: 6 })));
        assert!(shell.client_open_ids().is_empty());
    }

    #[test]
    fn source_indicator_has_stable_targets_and_menu_selects_or_opens_keyboard_settings(){
        let mut fonts=FontSystem::new();let registry=AppRegistry::new();let mut shell=MacShell::new(1000,700);
        shell.ensure_topbar_rendered(&mut fonts,"Терминал","EN");
        let rect=shell.topbar_hits.iter().find(|(_,_,h)|*h==TopbarItem::Layout).copied().unwrap();
        let slots=shell.topbar_hits.iter().filter(|(_,_,h)|!matches!(h,TopbarItem::Menu(_))).cloned().collect::<Vec<_>>();
        shell.set_input_sources(crate::settings::Keyboard::default().sources(),1,"Ctrl + Space");
        shell.ensure_topbar_rendered(&mut fonts,"Терминал","RU");
        assert_eq!(slots,shell.topbar_hits.iter().filter(|(_,_,h)|!matches!(h,TopbarItem::Menu(_))).cloned().collect::<Vec<_>>());
        shell.handle_pointer_click(((rect.0+rect.1)/2.0) as f64,12.0,&registry);assert_eq!(shell.open_menu,Some(TopMenu::Input));
        let rows=shell.rows_for_menu(TopMenu::Input);assert!(matches!(rows[1],MenuRow::Item{checked:true,..}));
        shell.input_menu_key(1);assert!(matches!(shell.input_menu_key(0),Some(ShellClickAction::SystemSettings(crate::system_settings::Action::SelectSource(0)))));
        assert!(shell.open_menu.is_none());shell.open_menu=Some(TopMenu::Input);
        for _ in 0..3{shell.input_menu_key(1);}
        assert!(matches!(shell.input_menu_key(0),Some(ShellClickAction::SystemSettings(crate::system_settings::Action::Page(crate::system_settings::Page::Keyboard)))));
    }

    #[test]
    fn disabled_hidden_and_previous_window_items_cannot_activate() {
        let mut shell = shell();
        let rows = shell.rows_for_menu(TopMenu::Client(1));
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[1], MenuRow::Item { action: None, .. }));
        shell.open_menu = Some(TopMenu::Client(1));
        shell.client_submenus.push(5);
        assert!(shell.set_client_menu(8, None));
        assert!(shell.client_open_ids().is_empty());
        assert!(shell.client_menu.is_none());
        assert!(!shell.set_client_menu(8, None));
    }

    #[test]
    fn tray_menu_uses_native_cascades_and_rejects_closed_or_stale_replies() {
        let mut shell=shell();let root=shell.client_menu.as_ref().unwrap().children[0].clone();
        shell.notify("Test","Toast must not cover the tray menu");assert!(shell.toast_visible());
        shell.prepare_tray_menu("tray".into(),12,960.0);
        assert!(!shell.toast_visible());
        assert!(!shell.install_tray_menu("tray",11,Some(Arc::new(root.clone()))));
        assert!(shell.install_tray_menu("tray",12,Some(Arc::new(root))));
        assert_eq!(shell.tray_open_ids(),vec![1]);assert!(shell.client_open_ids().is_empty());
        shell.client_menu_key(1); // Run
        shell.client_menu_key(1); // Nested, skips disabled and hidden
        shell.client_menu_key(2); // open submenu
        assert_eq!(shell.tray_open_ids(),vec![1,5]);
        shell.client_menu_key(1);
        assert!(matches!(shell.client_menu_key(0),Some(ShellClickAction::TrayMenu{generation:12,id:6})));
        assert!(!shell.install_tray_menu("tray",12,None));
        assert!(shell.toast_visible());
        assert!(shell.tray_open_ids().is_empty());
    }

    #[test]
    fn tall_client_menu_is_bounded_and_scrolls_without_forwarding_to_client() {
        let mut shell = shell();
        shell.screen_h = 480;
        shell.screen_w = 180; // narrower than MENU_MIN_W: must not panic in clamp()
        let children = (10..90).map(|id| node(id, &format!("Item {id}"), vec![])).collect();
        shell.set_client_menu(9, Some(Arc::new(node(0, "", vec![node(1, "File", children)]))));
        shell.open_menu = Some(TopMenu::Client(1));
        let (r, before, _) = shell.menu_panels().remove(0);
        assert!(r.y + r.h < shell.screen_h as f32);
        assert!(r.x >= 0.0 && r.x + r.w <= shell.screen_w as f32);
        assert!(shell.scroll_client_menu(r.x + 5.0, r.y + 10.0, 1.0));
        let after = shell.rows_for_menu(TopMenu::Client(1));
        assert!(!matches!((&before[0], &after[0]), (MenuRow::Item { label: a, .. }, MenuRow::Item { label: b, .. }) if a == b));
        assert!(shell.overlay_dirty);
    }
}

#[cfg(test)]
mod cc_backdrop_tests {
    use super::*;

    fn pixels(shell: &mut MacShell) -> Vec<u8> {
        let mut result = Vec::new();
        let mut ctx = shell.overlay_buffer.render();
        let _ = ctx.draw(|data| {
            result.extend_from_slice(data);
            Ok::<_, ()>(Vec::new())
        });
        result
    }

    #[test]
    fn prepared_cc_matches_live_paint_and_controls_stay_live() {
        let registry = AppRegistry::new();
        let mut fonts = FontSystem::new();
        for scale in [1.0, 1.25, 2.0] {
            for dark in [false, true] {
                crate::theme::with_render_settings(scale,
                    if dark { Appearance::Dark } else { Appearance::Light }, || {
                    let mut bg = tiny_skia::Pixmap::new(32, 24).unwrap();
                    for (i, p) in bg.pixels_mut().iter_mut().enumerate() {
                        *p = rgba((i * 7) as u8, (i * 13) as u8, (i * 19) as u8, 255).premultiply().to_color_u8();
                    }
                    crate::theme::set_glass_background(Some((std::rc::Rc::new(bg), 40.0, 800.0 / 24.0)));
                    let mut shell = MacShell::new(1280, 800);
                    shell.cc.audio=crate::controls::Audio {default:"test".into(),sinks:vec![crate::controls::Sink {name:"test".into(),channels:vec![65536],..Default::default()}],error:None};
                    shell.control_center_open = true;
                    // Toast is present in the model but must not cover the controls.
                    shell.ensure_overlay_rendered(&mut fonts, &registry, "EN");
                    let expected = pixels(&mut shell);
                    shell.cc_backdrop = Some(MacShell::raster_cc_backdrop((1280, 800)));
                    shell.overlay_dirty = true;
                    shell.ensure_overlay_rendered(&mut fonts, &registry, "EN");
                    let prepared = pixels(&mut shell);
                    assert_eq!(prepared.len(), expected.len());
                    let difference = prepared.iter().zip(&expected).map(|(a,b)| a.abs_diff(*b)).max().unwrap();
                    assert!(difference <= 2, "prepared plate phase changed at scale {scale}, dark {dark}: {difference}");
                    shell.cc.audio.sinks[0].channels=vec![8192];
                    shell.overlay_dirty = true;
                    shell.ensure_overlay_rendered(&mut fonts, &registry, "EN");
                    assert!(pixels(&mut shell) != prepared, "cached plate froze the slider");
                    // Toast expiry must not change CC geometry or paint.
                    shell.toast = None;
                    let plates = shell.cc_backdrop.take();
                    shell.overlay_dirty = true;
                    shell.ensure_overlay_rendered(&mut fonts, &registry, "EN");
                    let alone = pixels(&mut shell);
                    shell.cc_backdrop = plates;
                    shell.overlay_dirty = true;
                    shell.ensure_overlay_rendered(&mut fonts, &registry, "EN");
                    let difference = pixels(&mut shell).iter().zip(&alone).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                    assert!(difference <= 2, "CC alone phase changed at scale {scale}, dark {dark}: {difference}");
                    shell.invalidate_all();
                    assert!(shell.cc_backdrop.is_none());
                    assert!(!shell.cc_backdrop_requested);
                    assert!(!crate::theme::has_glass_background());
                });
            }
        }
    }
}
