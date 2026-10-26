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
    DOCK_BOTTOM_MARGIN, DOCK_ICON_GAP, DOCK_PAD_X, DOCK_PAD_Y, DOCK_RADIUS, dock_icon,
    TOP_BAR_HEIGHT,
};

/// The wallpaper is a smooth gradient: render it at 1/WALLPAPER_DIV resolution and let the GPU
/// upscale it with linear filtering (16x less CPU work than a full-res 4K raster).
pub const WALLPAPER_DIV: i32 = 4;
/// Logical height of the Dock buffer (room for magnified icons and the hover label)
fn dock_buf_height() -> i32 {
    (crate::theme::dock_icon() * (1.0 + DOCK_MAGNIFY)) as i32 + 61
}
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

/// A 4×4 black buffer, stretched (with `src`) to dim the desktop behind Mission Control
fn black_buffer() -> MemoryRenderBuffer {
    let mut b = new_buffer(4, 4);
    let _ = b.render().draw(|buf| {
        buf.chunks_exact_mut(4).for_each(|px| px.copy_from_slice(&[0, 0, 0, 255]));
        Result::<_, ()>::Ok(vec![Rectangle::from_size((4, 4).into())])
    });
    b
}

/// Menu bar colours for the wallpaper under it (macOS Tahoe adapts them the same way)
#[derive(Clone, Copy)]
pub struct BarColors {
    pub tint: Color,
    pub text: Color,
    pub dim: Color,
    pub highlight: Color,
    /// Behind a coloured tray icon that would not stand out
    pub chip: Color,
    pub dark: bool,
}

impl BarColors {
    pub fn for_background(dark: bool) -> Self {
        let opaque = crate::theme::reduce_transparency();
        let hc = crate::theme::high_contrast();
        if dark {
            Self {
                tint: if opaque { rgba(34, 34, 38, 255) } else { rgba(0, 0, 0, 36) },
                text: rgba(255, 255, 255, if hc { 255 } else { 240 }),
                dim: rgba(255, 255, 255, if hc { 170 } else { 120 }),
                highlight: rgba(255, 255, 255, if hc { 64 } else { 42 }),
                chip: rgba(255, 255, 255, 50),
                dark,
            }
        } else {
            Self {
                tint: if opaque { rgba(244, 244, 246, 255) } else { rgba(255, 255, 255, 70) },
                text: rgba(0, 0, 0, if hc { 255 } else { 225 }),
                dim: rgba(0, 0, 0, if hc { 155 } else { 105 }),
                highlight: rgba(0, 0, 0, if hc { 44 } else { 28 }),
                chip: rgba(0, 0, 0, 34),
                dark,
            }
        }
    }
}

/// Mean luminance (0…1) of the wallpaper under the menu bar, from the blurred copy
pub fn bar_luma(glass: &tiny_skia::Pixmap, screen_h: i32) -> f32 {
    let rows = ((TOP_BAR_HEIGHT as f32 / screen_h.max(1) as f32) * glass.height() as f32).ceil().max(1.0) as usize;
    let w = glass.width() as usize;
    let data = glass.data();
    let (mut sum, mut n) = (0.0f32, 0.0f32);
    for px in data[..(rows.min(glass.height() as usize) * w * 4)].chunks_exact(4) {
        let a = px[3] as f32;
        if a < 1.0 { continue; }
        // Unpremultiplied sRGB, Rec. 709 weights: good enough to pick black or white text
        let c = |v: u8| v as f32 / a;
        sum += 0.2126 * c(px[0]) + 0.7152 * c(px[1]) + 0.0722 * c(px[2]);
        n += 1.0;
    }
    if n == 0.0 { 1.0 } else { sum / n }
}

/// True when a tray icon is too close in brightness to the bar to stand out; it then gets
/// a soft chip behind it. Icons keep their own colours (white symbolic ones included).
pub fn tray_icon_needs_chip(icon: &tiny_skia::Pixmap, dark_bar: bool) -> bool {
    let (mut n, mut luma) = (0.0f32, 0.0f32);
    for px in icon.data().chunks_exact(4) {
        let a = px[3] as f32;
        if a < 64.0 { continue; }
        luma += 0.2126 * px[0] as f32 / a + 0.7152 * px[1] as f32 / a + 0.0722 * px[2] as f32 / a;
        n += 1.0;
    }
    if n < 4.0 {
        return false;
    }
    let bar = if dark_bar { 0.2 } else { 0.85 };
    (luma / n - bar).abs() < 0.3
}

/// The Dock is laid out along its own axes: x along the bar, y towards the screen edge
/// (the bottom Dock's buffer as is). A side Dock's buffer is that layout turned on its
/// side; icons and text stay upright, only positions are mapped.
#[derive(Clone, Copy, Debug)]
pub struct DockMap {
    pub edge: crate::theme::DockEdge,
    /// Cross-axis depth of the layout (the bottom Dock's buffer height)
    pub depth: f32,
}

impl DockMap {
    pub fn point(self, x: f32, y: f32) -> (f32, f32) {
        use crate::theme::DockEdge::*;
        match self.edge { Bottom => (x, y), Left => (self.depth - y, x), Right => (y, x) }
    }
    pub fn rect(self, r: R) -> R {
        use crate::theme::DockEdge::*;
        match self.edge {
            Bottom => r,
            Left => R::new(self.depth - (r.y + r.h), r.x, r.h, r.w),
            Right => R::new(r.y, r.x, r.h, r.w),
        }
    }
    /// Buffer point back to layout axes
    pub fn unpoint(self, bx: f32, by: f32) -> (f32, f32) {
        use crate::theme::DockEdge::*;
        match self.edge { Bottom => (bx, by), Left => (by, self.depth - bx), Right => (by, bx) }
    }
}

/// One desktop tile (or «+») of the Mission Control strip
#[derive(Debug, Clone)]
pub struct MissionTile {
    pub label: String,
    pub rect: R,
    /// Desktop index; None for «+»
    pub space: Option<u32>,
    /// The close button is shown (hovered tile, more than one desktop)
    pub removable: bool,
    pub remove_hover: bool,
    pub current: bool,
    pub hover: bool,
    pub add: bool,
    pub windows: usize,
}

#[derive(Debug, Clone)]
pub struct MissionOverlay {
    pub tiles: Vec<MissionTile>,
    /// Title of the hovered window and its overview rectangle
    pub title: Option<(String, R)>,
}

impl MissionOverlay {
    fn title_rect(r: R) -> R {
        R::new(r.x + r.w * 0.5 - 130.0, r.y + r.h + 8.0, 260.0, 26.0)
    }
    /// Tile, its label below and the close button corner
    fn rects(&self) -> Vec<R> {
        self.tiles
            .iter()
            .map(|t| R::new(t.rect.x - 8.0, t.rect.y - 8.0, t.rect.w + 16.0, t.rect.h + 32.0))
            .chain(self.title.iter().map(|(_, r)| Self::title_rect(*r)))
            .collect()
    }
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

/// `minute` lies in [from, to), the window wrapping past midnight when to < from
pub fn in_window(minute: u16, from: u16, to: u16) -> bool {
    if from <= to { (from..to).contains(&minute) } else { minute >= from || minute < to }
}

/// Offered «Не беспокоить» windows, cycled by a click
pub const DND_WINDOWS: [(u16, u16); 5] = [(22 * 60, 7 * 60), (23 * 60, 8 * 60), (21 * 60, 7 * 60), (0, 9 * 60), (13 * 60, 14 * 60)];

pub fn dnd_window_label((from, to): (u16, u16)) -> String {
    format!("{}:{:02} – {}:{:02}", from / 60, from % 60, to / 60, to % 60)
}

#[derive(Debug, Clone)]
pub struct NotificationToast {
    /// D-Bus id; 0 for the shell's own messages, which emit no signals
    pub id: u32,
    pub title: String,
    pub body: String,
    pub icon: Option<AppIcon>,
    pub app_label: String,
    pub image: Option<std::sync::Arc<tiny_skia::Pixmap>>,
    /// (key, label); "default" is the click on the banner
    pub actions: Vec<(String, String)>,
    pub created_at: Instant,
    /// None: stays until closed
    pub duration: Option<Duration>,
    pub resident: bool,
    /// Not kept in the notification center
    pub transient: bool,
    pub posted: chrono::DateTime<chrono::Local>,
}

impl NotificationToast {
    fn buttons(&self) -> impl Iterator<Item = (usize, &(String, String))> {
        self.actions.iter().enumerate().filter(|(_, (k, _))| k != "default").take(3)
    }
    fn height(&self) -> f32 {
        if self.buttons().next().is_some() { TOAST_H + TOAST_BUTTON_H + 10.0 } else { TOAST_H }
    }
}

const TOAST_W: f32 = 344.0;
const TOAST_H: f32 = 72.0;
const TOAST_BUTTON_H: f32 = 26.0;
const TOAST_GAP: f32 = 8.0;
/// Banners shown at once; an older one moves to the notification center
const MAX_TOASTS: usize = 3;
/// Notification center history; the oldest is closed (expired) beyond it
const MAX_HISTORY: usize = 50;
const CENTER_HEADER_H: f32 = 40.0;
const CENTER_GROUP_H: f32 = 26.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CenterHit {
    ClearAll,
    /// Clear the group whose newest entry has this history index
    ClearGroup(usize),
    Card(usize, ToastPart),
}

/// A drawn center row, detached from the shell for the render closure
enum CenterItem {
    Group(usize, String),
    Card(usize, NotificationToast),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CenterRow {
    Group(usize),
    Card(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastPart {
    Body,
    Close,
    Action(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationEvent {
    Closed(u32, crate::notifications::Reason),
    Invoked(u32, String),
    Sound(crate::sounds::Sound),
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
    ToggleDockLock,
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
    /// A mapped representative window; registry/group indices can change during a hold.
    pub window_id: u64,
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
    /// Keyboard selection wins until the mouse deliberately moves at least six points.
    pub pointer_anchor: Option<(f64, f64)>,
}
impl Switcher {
    pub fn allow_pointer(&mut self, x: f64, y: f64) -> bool {
        if let Some((ax, ay)) = self.pointer_anchor {
            if (x-ax)*(x-ax)+(y-ay)*(y-ay) < 36.0 { return false; }
            self.pointer_anchor = None;
        }
        true
    }
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
    ToggleDoNotDisturb,
    SetBrightness(f32),
    FirstRun(crate::first_run::Action),
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
            item("Мастер приветствия…", "", builtin(BuiltinWindowKind::FirstRun, "Добро пожаловать в Пёрышко")),
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
    dock_slot_gap: f32,
    dock_painted: Vec<Rectangle<i32, Buffer>>,

    pub spotlight_open: bool,
    pub spotlight_query: String,
    /// Caret/selection of the Spotlight field; `spotlight_query` mirrors its text
    pub spotlight_input: feather_ui::input::TextInput,
    pub spotlight_selected: usize,

    pub control_center_open: bool,
    pub cc: crate::control_panel::Panel,
    pub wifi_enabled: bool,
    /// «Не беспокоить» turned on by hand; see `dnd_active` for the effective state
    pub dnd_enabled: bool,
    /// Daily window in minutes after midnight (from, to); may cross midnight
    pub dnd_schedule: Option<(u16, u16)>,
    dnd_in_window: bool,
    /// Turned off by hand inside the window: stays off until the window ends
    dnd_skipped: bool,

    pub open_menu: Option<TopMenu>,
    pub menu_hovered: Option<usize>,
    pub menu_ctx: MenuContext,
    pub client_menu: Option<std::sync::Arc<crate::appmenu::MenuNode>>,
    pub client_generation: u64,
    pub client_submenus: Vec<i32>,
    client_hovered: Option<(usize, usize)>,
    client_scroll: std::collections::HashMap<i32, usize>,

    pub toasts: Vec<NotificationToast>,
    toast_hover: Option<(usize, ToastPart)>,
    /// Notification center: history newest first
    pub history: Vec<NotificationToast>,
    pub center_open: bool,
    center_first: usize,
    center_hover: Option<CenterHit>,
    /// Closes and actions for D-Bus clients; drained by the compositor
    pub notification_events: Vec<NotificationEvent>,
    /// Per-program delivery, in first-seen order; `notify_apps_changed` asks for a save
    pub notify_apps: Vec<crate::notifications::AppPolicy>,
    pub notify_apps_changed: bool,
    pub notify_sounds: bool,

    pub overlay_buffer: MemoryRenderBuffer,
    pub overlay_dirty: bool,
    overlay_painted: Vec<Rectangle<i32, Buffer>>,
    overlay_allocated: bool,
    overlay_geometry: Rectangle<i32, Buffer>,

    pub switcher: Option<Switcher>,
    pub switcher_keys: std::collections::HashSet<smithay::input::keyboard::Keycode>,
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
    /// Mission Control strip and title (CPU part; windows move on the GPU)
    pub mission: Option<MissionOverlay>,
    pub mission_dim: MemoryRenderBuffer,
    /// Auto-hidden Dock is out of the way (it slides back at the bottom edge)
    pub dock_hidden: bool,
    /// Wallpaper under the menu bar is dark (None until the first wallpaper arrives)
    pub bar_on_dark: Option<bool>,
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
            dock_buffer: new_buffer(to_phys(w), to_phys(dock_buf_height())),
            dock_buf_w: w,
            dock_buf_h: dock_buf_height(),
            dock_dirty: true,
            dock_zoom_enabled: true,
            dock_items: Vec::new(),
            dock_cursor_x: None,
            dock_anchor: None,
            dock_slots: Vec::new(),
            dock_bar: R::new(0.0, 0.0, 0.0, 0.0),
            dock_slot_gap: DOCK_ICON_GAP,
            dock_painted: Vec::new(),
            spotlight_open: false,
            spotlight_query: String::new(),
            spotlight_input: Default::default(),
            spotlight_selected: 0,
            control_center_open: false,
            cc: crate::control_panel::Panel::default(),
            wifi_enabled: false,
            dnd_enabled: false,
            dnd_schedule: None,
            dnd_in_window: false,
            dnd_skipped: false,
            open_menu: None,
            menu_hovered: None,
            menu_ctx: MenuContext::default(),
            client_menu: None,
            client_generation: 0,
            client_submenus: Vec::new(),
            client_hovered: None,
            client_scroll: std::collections::HashMap::new(),
            toasts: Vec::new(),
            toast_hover: None,
            history: Vec::new(),
            center_open: false,
            center_first: 0,
            center_hover: None,
            notification_events: Vec::new(),
            notify_apps: Vec::new(),
            notify_apps_changed: false,
            notify_sounds: true,
            overlay_buffer: new_buffer(1, 1),
            overlay_dirty: true,
            overlay_painted: Vec::new(),
            overlay_allocated: false,
            overlay_geometry: Rectangle::from_size((1, 1).into()),
            switcher: None,
            switcher_keys: Default::default(),
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
            mission: None,
            mission_dim: black_buffer(),
            dock_hidden: false,
            bar_on_dark: None,
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
        // Buffer and axes for the saved Dock edge (a side Dock's is turned)
        shell.resize_dock();
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

    pub fn dnd_active(&self) -> bool {
        self.dnd_enabled || (self.dnd_in_window && !self.dnd_skipped)
    }

    /// The switch in Control Center and Settings: off ends a scheduled window early
    pub fn toggle_dnd(&mut self) {
        if self.dnd_active() {
            self.dnd_enabled = false;
            self.dnd_skipped = self.dnd_in_window;
        } else {
            self.dnd_enabled = true;
        }
        self.topbar_dirty = true;
        self.overlay_dirty = true;
    }

    /// Re-evaluates the schedule for `minute` (after midnight); true when the effective
    /// state may have changed
    pub fn update_dnd_schedule(&mut self, minute: u16) -> bool {
        let inside = self.dnd_schedule.is_some_and(|(from, to)| in_window(minute, from, to));
        if inside == self.dnd_in_window { return false; }
        self.dnd_in_window = inside;
        if !inside { self.dnd_skipped = false; }
        self.topbar_dirty = true;
        self.overlay_dirty = true;
        true
    }

    /// The shell's own message; it replaces the previous one of its kind
    pub fn notify(&mut self, title: impl Into<String>, body: impl Into<String>) {
        if self.dnd_active() {
            return;
        }
        self.toasts.retain(|t| t.id != 0);
        self.push_toast(NotificationToast {
            id: 0,
            title: title.into(),
            body: body.into(),
            icon: None,
            app_label: String::new(),
            image: None,
            actions: Vec::new(),
            created_at: Instant::now(),
            duration: Some(crate::notifications::DEFAULT_TIMEOUT),
            resident: false,
            transient: true,
            posted: chrono::Local::now(),
        });
    }

    /// A client notification (org.freedesktop.Notifications). With «Не беспокоить» or an
    /// open center only critical ones show a banner; the rest go straight to the center.
    pub fn show_notification(&mut self, n: crate::notifications::Incoming, registry: &AppRegistry) {
        let app = [n.desktop_entry.as_str(), n.app_name.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .find_map(|s| registry.app_for_app_id(s).or_else(|| registry.app_for_app_id(&s.to_lowercase())))
            .map(|i| &registry.apps[i]);
        let label = app.map_or_else(|| n.app_name.clone(), |a| a.name.clone());
        let delivery = self.app_delivery(&n, &label);
        let icon = if !n.app_icon.is_empty() {
            Some(AppIcon { kind: crate::apps::IconKind::GenericApp, name: Some(n.app_icon.clone()) })
        } else {
            app.map(AppIcon::of)
        };
        let toast = NotificationToast {
            id: n.id,
            title: if n.summary.is_empty() { n.app_name.clone() } else { n.summary },
            body: n.body,
            icon,
            app_label: label,
            image: n.image,
            actions: n.actions,
            created_at: Instant::now(),
            duration: n.timeout,
            resident: n.resident,
            transient: n.transient,
            posted: chrono::Local::now(),
        };
        if delivery == crate::notifications::Delivery::Off {
            // A replaced one disappears too; the client hears it was dismissed
            self.toasts.retain(|t| t.id != toast.id);
            self.history.retain(|t| t.id != toast.id);
            self.toast_closed(toast.id, crate::notifications::Reason::Dismissed);
            self.overlay_dirty = true;
            return;
        }
        if let Some(i) = self.toasts.iter().position(|t| t.id == toast.id) {
            self.toasts[i] = toast;
            self.overlay_dirty = true;
            return;
        }
        if let Some(i) = self.history.iter().position(|t| t.id == toast.id) {
            // A replaced entry moves up as the newest one
            self.history.remove(i);
            self.history.insert(0, toast);
            self.overlay_dirty = true;
            return;
        }
        let quiet = self.dnd_active() || self.center_open || delivery == crate::notifications::Delivery::CenterOnly;
        if quiet && !n.critical {
            self.to_history(toast);
            return;
        }
        // Only a new banner chimes: updates (replaces_id) and silent routes do not
        if let Some(sound) = n.sound.filter(|_| self.notify_sounds && !n.replaces) {
            self.notification_events.push(NotificationEvent::Sound(sound));
        }
        self.push_toast(toast);
    }

    /// The sender's setting; a new sender is remembered with the default
    fn app_delivery(&mut self, n: &crate::notifications::Incoming, label: &str) -> crate::notifications::Delivery {
        use crate::notifications::{clean_field, AppPolicy, MAX_APPS};
        let key = clean_field(if !n.desktop_entry.is_empty() { &n.desktop_entry } else if !n.app_name.is_empty() { &n.app_name } else { label });
        if key.is_empty() { return Default::default(); }
        let label = match clean_field(label) { l if l.is_empty() => key.clone(), l => l };
        match self.notify_apps.iter_mut().find(|a| a.key == key) {
            Some(a) => {
                if a.label != label { a.label = label; self.notify_apps_changed = true; }
                a.delivery
            }
            None => {
                if self.notify_apps.len() < MAX_APPS {
                    self.notify_apps.push(AppPolicy { key, label, delivery: Default::default() });
                    self.notify_apps_changed = true;
                }
                Default::default()
            }
        }
    }

    /// A banner that left the screen: kept in the center unless the client said transient
    fn to_history(&mut self, toast: NotificationToast) {
        if toast.id == 0 {
            return;
        }
        if toast.transient {
            self.toast_closed(toast.id, crate::notifications::Reason::Expired);
            return;
        }
        self.history.insert(0, toast);
        while self.history.len() > MAX_HISTORY {
            let old = self.history.pop().unwrap();
            self.toast_closed(old.id, crate::notifications::Reason::Expired);
        }
        if self.center_open { self.overlay_dirty = true; }
    }

    fn push_toast(&mut self, toast: NotificationToast) {
        // Newest on top, like macOS; the oldest banner leaves when the stack is full.
        self.toasts.insert(0, toast);
        while self.toasts.len() > MAX_TOASTS {
            let old = self.toasts.pop().unwrap();
            self.to_history(old);
        }
        self.toast_hover = None;
        self.overlay_dirty = true;
    }

    fn toast_closed(&mut self, id: u32, reason: crate::notifications::Reason) {
        if id != 0 {
            self.notification_events.push(NotificationEvent::Closed(id, reason));
        }
    }

    fn remove_toast(&mut self, index: usize, reason: crate::notifications::Reason) {
        let t = self.toasts.remove(index);
        self.toast_closed(t.id, reason);
        self.toast_hover = None;
        self.overlay_dirty = true;
    }

    /// CloseNotification from the client: true when it was shown or kept in the center
    pub fn close_notification(&mut self, id: u32) -> bool {
        if id == 0 { return false; }
        if let Some(i) = self.toasts.iter().position(|t| t.id == id) {
            self.remove_toast(i, crate::notifications::Reason::Closed);
            return true;
        }
        match self.history.iter().position(|t| t.id == id) {
            Some(i) => { self.remove_history(i, crate::notifications::Reason::Closed); true }
            None => false,
        }
    }

    fn remove_history(&mut self, index: usize, reason: crate::notifications::Reason) {
        let t = self.history.remove(index);
        self.toast_closed(t.id, reason);
        self.center_hover = None;
        self.overlay_dirty = true;
    }

    pub fn toggle_center(&mut self) {
        let open = !self.center_open;
        self.close_menus();
        self.center_open = open;
        self.center_first = 0;
        if open {
            // Banners on screen join the center at once, like on macOS
            for t in std::mem::take(&mut self.toasts).into_iter().rev() { self.to_history(t); }
            self.toast_hover = None;
        }
        self.overlay_dirty = true;
        self.topbar_dirty = true;
    }

    /// History grouped by app in order of each app's newest entry
    fn center_rows(&self) -> Vec<CenterRow> {
        let mut seen: Vec<&str> = Vec::new();
        let mut rows = Vec::new();
        for (i, t) in self.history.iter().enumerate() {
            if seen.contains(&t.app_label.as_str()) { continue; }
            seen.push(&t.app_label);
            rows.push(CenterRow::Group(i));
            rows.extend(self.history.iter().enumerate().filter(|(_, o)| o.app_label == t.app_label).map(|(j, _)| CenterRow::Card(j)));
        }
        rows
    }

    /// Panel, visible rows with their rectangles and the «clear all» target. Rows scroll
    /// whole, so nothing needs clipping; `center_first` is the first shown row.
    fn center_layout(&self) -> (R, Vec<(R, CenterRow)>, Option<R>) {
        let x = self.screen_w as f32 - TOAST_W - 10.0;
        let y = TOP_BAR_HEIGHT as f32 + 8.0;
        let max_h = (self.screen_h as f32 - y - 96.0).max(160.0);
        let rows = self.center_rows();
        if rows.is_empty() {
            return (R::new(x, y, TOAST_W, 120.0), Vec::new(), None);
        }
        let mut cy = y + CENTER_HEADER_H;
        let mut out = Vec::new();
        for row in rows.into_iter().skip(self.center_first) {
            let h = match row {
                CenterRow::Group(_) => CENTER_GROUP_H,
                CenterRow::Card(i) => self.history[i].height(),
            };
            if cy + h > y + max_h - 10.0 && !out.is_empty() { break; }
            out.push((R::new(x + 8.0, cy, TOAST_W - 16.0, h), row));
            cy += h + if matches!(row, CenterRow::Card(_)) { 8.0 } else { 0.0 };
        }
        let clear = R::new(x + TOAST_W - 112.0, y + 10.0, 100.0, 22.0);
        (R::new(x, y, TOAST_W, cy - y + 4.0), out, Some(clear))
    }

    fn center_at(&self, x: f32, y: f32) -> Option<CenterHit> {
        if !self.center_open { return None; }
        let (panel, rows, clear) = self.center_layout();
        if clear.is_some_and(|c| c.contains(x, y)) { return Some(CenterHit::ClearAll); }
        for (r, row) in rows {
            match row {
                CenterRow::Group(i) => {
                    if R::new(r.x + r.w - 26.0, r.y + 2.0, 24.0, 22.0).contains(x, y) { return Some(CenterHit::ClearGroup(i)); }
                }
                CenterRow::Card(i) => {
                    if self.center_hover.is_some_and(|h| matches!(h, CenterHit::Card(j, _) if j == i)) && Self::toast_close_rect(r).contains(x, y) {
                        return Some(CenterHit::Card(i, ToastPart::Close));
                    }
                    if r.contains(x, y) {
                        let part = Self::toast_button_rects(&self.history[i], r).into_iter()
                            .find(|(_, b)| b.contains(x, y))
                            .map_or(ToastPart::Body, |(a, _)| ToastPart::Action(a));
                        return Some(CenterHit::Card(i, part));
                    }
                }
            }
        }
        let _ = panel;
        None
    }

    /// Wheel over the open center: one row per notch
    pub fn scroll_center(&mut self, px: f32, py: f32, delta: f64) -> bool {
        if !self.center_open || delta == 0.0 || !self.center_layout().0.contains(px, py) { return false; }
        let total = self.center_rows().len();
        let next = if delta > 0.0 { (self.center_first + 1).min(total.saturating_sub(1)) } else { self.center_first.saturating_sub(1) };
        if next != self.center_first {
            self.center_first = next;
            self.center_hover = None;
            self.overlay_dirty = true;
        }
        true
    }

    fn center_click(&mut self, hit: CenterHit) {
        use crate::notifications::Reason;
        match hit {
            CenterHit::ClearAll => {
                for t in std::mem::take(&mut self.history) { self.toast_closed(t.id, Reason::Dismissed); }
            }
            CenterHit::ClearGroup(i) => {
                let app = self.history[i].app_label.clone();
                let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.history).into_iter().partition(|t| t.app_label == app);
                self.history = keep;
                for t in gone { self.toast_closed(t.id, Reason::Dismissed); }
            }
            CenterHit::Card(i, part) => {
                let t = &self.history[i];
                let key = match part {
                    ToastPart::Close => None,
                    ToastPart::Body => t.actions.iter().any(|(k, _)| k == "default").then(|| "default".to_string()),
                    ToastPart::Action(a) => Some(t.actions[a].0.clone()),
                };
                let resident = t.resident;
                if let Some(key) = key {
                    self.notification_events.push(NotificationEvent::Invoked(t.id, key));
                    if resident { return; }
                    // The app comes forward: the center gets out of the way
                    self.remove_history(i, Reason::Dismissed);
                    self.close_menus();
                    return;
                }
                self.remove_history(i, Reason::Dismissed);
            }
        }
        let total = self.center_rows().len();
        self.center_first = self.center_first.min(total.saturating_sub(1));
        self.center_hover = None;
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

    /// Only the wallpaper image/theme changed: keep icon caches and desktop tiles intact
    pub fn invalidate_wallpaper(&mut self) {
        self.launchpad_bg_key = None;
        self.launchpad_bg_requested = None;
        self.cc_backdrop = None;
        self.cc_backdrop_requested = false;
        self.wallpaper_dirty = true;
    }

    pub fn reallocate_buffers(&mut self, logical_w: i32, logical_h: i32) {
        let w = logical_w.max(640);
        let h = logical_h.max(480);
        self.screen_w = w;
        self.screen_h = h;
        self.topbar_buffer = new_buffer(to_phys(w), to_phys(TOP_BAR_HEIGHT));
        (self.dock_buf_w, self.dock_buf_h) = self.dock_axes();
        self.dock_anchor = None;
        let (bw, bh) = self.dock_buffer_size();
        self.dock_buffer = new_buffer(to_phys(bw), to_phys(bh));
        self.overlay_buffer = new_buffer(1, 1);
        self.overlay_allocated = false;
        self.switcher_rect = R::new(0.0, 0.0, 0.0, 0.0);
        self.invalidate_all();
    }

    pub fn format_ru_clock() -> String {
        let now = Local::now();
        feather_ui::i18n::format_topbar_clock(
            now.weekday().num_days_from_monday() as usize,
            now.day(),
            now.month0() as usize,
            now.hour(),
            now.minute(),
        )
    }

    /// Periodic housekeeping (clock, RSS, toast expiry). Returns true if anything needs a redraw.
    pub fn tick(&mut self) -> bool {
        let was_dirty = self.topbar_dirty || self.overlay_dirty;
        {
            use chrono::Timelike;
            let now = chrono::Local::now();
            self.update_dnd_schedule((now.hour() * 60 + now.minute()) as u16);
        }
        let clock = Self::format_ru_clock();
        if clock != self.last_clock_str {
            self.last_clock_str = clock;
            self.topbar_dirty = true;
            // «N мин назад» in the open center follows the minute
            if self.center_open && !self.history.is_empty() { self.overlay_dirty = true; }
        }
        if self.last_rss_check.elapsed() >= Duration::from_secs(3) {
            self.last_rss_check = Instant::now();
            self.last_rss_mb = Self::read_process_rss_mb();
        }
        // A banner under the pointer waits; it gets its full time again on leave.
        let hovered = self.toast_hover.map(|(i, _)| i);
        // Oldest first, so the center stays ordered newest first
        while let Some(i) = self.toasts.iter().enumerate().rposition(|(i, t)| {
            Some(i) != hovered && t.duration.is_some_and(|d| t.created_at.elapsed() >= d)
        }) {
            let t = self.toasts.remove(i);
            self.toast_hover = None;
            self.overlay_dirty = true;
            self.to_history(t);
        }
        if self.overlay_allocated && !self.has_visible_overlay() {
            // Free the full-screen overlay buffer while nothing is shown
            self.overlay_buffer = new_buffer(1, 1);
            self.overlay_allocated = false;
            self.overlay_painted.clear();
        }
        !was_dirty && (self.topbar_dirty || self.overlay_dirty)
    }

    pub fn bar_colors(&self) -> BarColors {
        let dark = if crate::theme::reduce_transparency() {
            appearance() == Appearance::Dark
        } else {
            self.bar_on_dark.unwrap_or(appearance() == Appearance::Dark)
        };
        BarColors::for_background(dark)
    }

    pub fn has_visible_overlay(&self) -> bool {
        self.spotlight_open
            || self.control_center_open
            || self.open_menu.is_some()
            || !self.toasts.is_empty()
            || self.center_open
            || self.dock_menu.is_some()
            || self.mission.is_some()
    }

    pub fn close_menus(&mut self) {
        self.tray_menu=None;
        if self.dock_menu.take().is_some() {
            self.overlay_dirty = true;
        }
        if self.center_open {
            self.center_open = false;
            self.center_hover = None;
            self.overlay_dirty = true;
            self.topbar_dirty = true;
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
        use crate::theme::DockEdge::*;
        // Opens away from the screen edge, like macOS
        let side_y = (anchor.y + anchor.h * 0.5 - h * 0.5).clamp(TOP_BAR_HEIGHT as f32 + 4.0, (self.screen_h as f32 - h - 6.0).max(TOP_BAR_HEIGHT as f32 + 4.0));
        match crate::theme::dock_edge() {
            Bottom => {
                let x = (anchor.x + anchor.w * 0.5 - w * 0.5).clamp(6.0, (self.screen_w as f32 - w - 6.0).max(6.0));
                let y = (anchor.y - h - 10.0).max(TOP_BAR_HEIGHT as f32 + 4.0);
                R::new(x,y,w,h)
            }
            Left => R::new(anchor.x + anchor.w + 10.0, side_y, w, h),
            Right => R::new(anchor.x - w - 10.0, side_y, w, h),
        }
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
        self.dock_slots.iter().find(|s| s.target == DockTarget::Minimized(id))
            .map(|s| self.dock_global_rect(R::new(s.x, s.y, s.size, s.size)))
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
        let (lx, ly) = self.dock_local(px, py);
        self.dock_slots
            .iter()
            .find(|s| R::new(s.x, s.y, s.size, s.size).contains(lx, ly))
            .map(|s| (s.target, self.dock_global_rect(R::new(s.x, s.y, s.size, s.size))))
    }

    /// The visible Dock background, without the wider app drop margin.
    pub fn dock_contains(&self, px: f64, py: f64) -> bool {
        let (lx, ly) = self.dock_local(px, py);
        self.dock_bar.contains(lx, ly)
    }

    /// Area where dropping a dragged app pins it
    pub fn dock_in_drop_zone(&self, px: f64, py: f64) -> bool {
        let (lx, ly) = self.dock_local(px, py);
        let b = self.dock_bar;
        lx >= b.x - 60.0 && lx <= b.x + b.w + 60.0 && ly >= b.y - 40.0
    }

    /// Insertion index among pinned items for a pointer at a global point
    pub fn dock_insert_index(&self, px: f64, py: f64) -> usize {
        let (lx, _) = self.dock_local(px, py);
        self.dock_slots
            .iter()
            .zip(self.dock_items.iter())
            .filter(|(_, item)| item.pinned)
            .filter(|(slot, _)| slot.x + slot.size * 0.5 < lx)
            .count()
    }

    /// Running apps become pinned only when deliberately moved into the pinned section.
    pub fn dock_in_pinned_section(&self, px: f64, py: f64) -> bool {
        let (lx, _) = self.dock_local(px, py);
        self.dock_slots.iter().zip(&self.dock_items)
            .filter(|(_, item)| item.pinned)
            .next_back()
            .is_some_and(|(slot, _)| lx <= slot.x + slot.size + self.dock_slot_gap * 0.5)
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

    /// Along-the-edge safe bounds `(min_along, max_along)` for the Dock bar
    fn dock_along_bounds(&self) -> (f32, f32) {
        let buf_w = self.dock_buf_w as f32;
        let min_along = if crate::theme::dock_edge().vertical() {
            TOP_BAR_HEIGHT as f32 + 6.0
        } else {
            6.0
        };
        let max_along = (buf_w - 6.0).max(min_along + 1.0);
        (min_along, max_along)
    }

    /// Separator indices (before first minimized window and/or before Trash)
    fn dock_separator_boundaries(&self) -> Vec<usize> {
        let first_min = self.dock_items.iter().position(|i| matches!(i.target, DockTarget::Minimized(_)));
        let trash_at = self.dock_items.iter().position(|i| i.target == DockTarget::Trash);
        let mut boundaries: Vec<usize> = Vec::with_capacity(2);
        if let Some(start) = first_min.or(trash_at).filter(|&s| s > 0) {
            boundaries.push(start);
        }
        if let (Some(_), Some(t)) = (first_min, trash_at) {
            boundaries.push(t);
        }
        boundaries
    }

    /// Proportional scale factor (`<= 1.0`) that shrinks the resting Dock so all items fit on screen, like macOS
    pub fn dock_fit_scale(&self) -> f32 {
        const SEP: f32 = 14.0;
        let (min_along, max_along) = self.dock_along_bounds();
        let avail_span = (max_along - min_along).max(1.0);
        let total_slots = self.dock_items.len() + usize::from(self.dock_gap.is_some());
        let num_seps = self.dock_separator_boundaries().len() as f32;
        let raw_base_bar_w = total_slots as f32 * dock_icon()
            + total_slots.saturating_sub(1) as f32 * DOCK_ICON_GAP
            + num_seps * SEP
            + DOCK_PAD_X * 2.0;
        if raw_base_bar_w > avail_span {
            (avail_span / raw_base_bar_w).clamp(0.15, 1.0)
        } else {
            1.0
        }
    }

    pub fn dock_base_icon(&self) -> f32 {
        dock_icon() * self.dock_fit_scale()
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
            crate::controls::Update::Bluetooth(bluetooth) => {
                if self.cc.bluetooth == bluetooth {
                    return false;
                }
                self.cc.bluetooth = bluetooth;
            }
            crate::controls::Update::AudioFeedback(audio) => {
                self.cc.keyboard_audio_updated(audio);
            }
            crate::controls::Update::Audio(audio) => {
                if self.cc.audio==audio {return false;}
                self.cc.audio=audio;
                self.cc.audio_updated();
            }
            crate::controls::Update::Error(title,error) => {
                if self.control_center_open {
                    self.cc.open(match title {
                        "Звук" => crate::control_panel::Page::Sound,
                        "Bluetooth" => crate::control_panel::Page::Bluetooth,
                        _ => crate::control_panel::Page::Wifi,
                    });
                }
                if title == "Bluetooth" {
                    self.cc.bluetooth.connecting = None;
                }
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
                let dark = bar_luma(&glass, self.screen_h) < 0.5;
                if self.bar_on_dark != Some(dark) {
                    self.bar_on_dark = Some(dark);
                    self.topbar_dirty = true;
                }
                self.applied_wallpaper_image=image;
                self.wallpaper_pending=Some((pixels,size));
                crate::theme::set_glass_background(Some((std::rc::Rc::new(glass), self.screen_w as f32 / w as f32, self.screen_h as f32 / h as f32)));
                self.launchpad_bg_key = None;
                self.launchpad_bg_requested = None;
                self.cc_backdrop = None;
                self.cc_backdrop_requested = false;
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
        let base_size = self.dock_base_icon().round().max(16.0);
        let px = to_phys(base_size as i32).max(1);
        let Some(mut pm) = tiny_skia::Pixmap::new(px as u32, px as u32) else { return };
        draw_any_icon(&mut pm.as_mut(), fonts, &mut self.icons, &item.icon, &item.label, 0.0, 0.0, base_size);
        let buffer = MemoryRenderBuffer::from_slice(pm.data(), smithay::backend::allocator::Fourcc::Abgr8888, (px, px), 1, smithay::utils::Transform::Normal, None);
        self.bounce = Some(Bounce { target, app, started: Instant::now(), first_new_window, finish_at: None, buffer, px });
        self.dock_dirty = true;
    }

    /// Global logical top-left and size of a Dock slot
    pub fn dock_slot_global(&self, target: DockTarget) -> Option<(f32, f32, f32)> {
        self.dock_slots.iter().find(|s| s.target == target).map(|s| {
            let r = self.dock_global_rect(R::new(s.x, s.y, s.size, s.size));
            (r.x, r.y, s.size)
        })
    }

    pub fn dock_map(&self) -> DockMap {
        DockMap { edge: crate::theme::dock_edge(), depth: self.dock_buf_h as f32 }
    }

    /// Logical size of the Dock buffer on screen (a side Dock's is turned)
    pub fn dock_buffer_size(&self) -> (i32, i32) {
        if crate::theme::dock_edge().vertical() { (self.dock_buf_h, self.dock_buf_w) } else { (self.dock_buf_w, self.dock_buf_h) }
    }

    /// Top-left of the Dock buffer when shown
    pub fn dock_shown_origin(&self) -> (f32, f32) {
        use crate::theme::DockEdge::*;
        let (bw, bh) = self.dock_buffer_size();
        match crate::theme::dock_edge() {
            Bottom => (((self.screen_w - bw) / 2) as f32, (self.screen_h - bh) as f32),
            Left => (0.0, ((self.screen_h - bh) / 2) as f32),
            Right => ((self.screen_w - bw) as f32, ((self.screen_h - bh) / 2) as f32),
        }
    }

    pub fn dock_origin(&self) -> (f32, f32) {
        // A hidden Dock sits beyond its screen edge: every hit test misses it by itself
        if !self.dock_hidden {
            return self.dock_shown_origin();
        }
        let (ox, oy) = self.dock_shown_origin();
        let (bw, _) = self.dock_buffer_size();
        use crate::theme::DockEdge::*;
        match crate::theme::dock_edge() {
            Bottom => (ox, self.screen_h as f32),
            Left => (-(bw as f32) - 1.0, oy),
            Right => (self.screen_w as f32 + 1.0, oy),
        }
    }

    /// Global point in Dock layout axes
    pub fn dock_local(&self, px: f64, py: f64) -> (f32, f32) {
        let (ox, oy) = self.dock_origin();
        self.dock_map().unpoint(px as f32 - ox, py as f32 - oy)
    }

    /// Layout rectangle on screen
    pub fn dock_global_rect(&self, r: R) -> R {
        let (ox, oy) = self.dock_origin();
        let b = self.dock_map().rect(r);
        R::new(b.x + ox, b.y + oy, b.w, b.h)
    }

    /// Along-the-bar length and depth of the layout for the current edge and screen
    fn dock_axes(&self) -> (i32, i32) {
        if crate::theme::dock_edge().vertical() {
            // Room beside the bar for the hover label
            (self.screen_h, dock_buf_height() + 200)
        } else {
            (self.screen_w, dock_buf_height())
        }
    }

    /// New icon size or edge: the buffer follows (magnification and label room scale too)
    pub fn resize_dock(&mut self) {
        (self.dock_buf_w, self.dock_buf_h) = self.dock_axes();
        let (bw, bh) = self.dock_buffer_size();
        self.dock_buffer = new_buffer(to_phys(bw), to_phys(bh));
        self.dock_painted.clear();
        self.dock_anchor = None;
        self.dock_dirty = true;
        self.desktop.invalidate();
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

    pub(crate) fn control_panel_rect(&self) -> R {
        self.cc_layout().0
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
        let hc = crate::theme::high_contrast();
        let card = if appearance() == Appearance::Dark {
            rgba(255, 255, 255, if hc { 38 } else { 20 })
        } else {
            rgba(255, 255, 255, if hc { 215 } else { 140 })
        };
        for (r, _) in items {
            fill_rounded_rect(&mut pm, r.x - ox, r.y - oy, r.w, r.h, 14.0, card);
            if hc {
                stroke_rounded_rect(&mut pm, r.x - ox + 0.5, r.y - oy + 0.5, r.w - 1.0, r.h - 1.0, 13.5, 0.75, palette().separator);
            }
        }
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
        !self.toasts.is_empty()&&!self.control_center_open&&!self.center_open&&!matches!(self.open_menu,Some(TopMenu::Tray(_)))
    }

    /// «сейчас», «12 мин назад», «14:05», «вчера», «21.10»
    fn posted_label(t: &NotificationToast) -> String {
        let now = chrono::Local::now();
        let minutes = (now - t.posted).num_minutes();
        if minutes < 1 { return "сейчас".into(); }
        if minutes < 60 { return format!("{minutes} мин назад"); }
        let (day, today) = (t.posted.date_naive(), now.date_naive());
        if day == today { t.posted.format("%H:%M").to_string() }
        else if today.pred_opt() == Some(day) { "вчера".into() }
        else { t.posted.format("%d.%m").to_string() }
    }

    /// One banner or center card: icon, title, time, two body lines, action buttons and,
    /// on hover, the close button at the top-left corner
    #[allow(clippy::too_many_arguments)]
    fn draw_toast(pm: &mut PixmapMut, fonts: &mut FontSystem, icons: &mut IconCache, t: &NotificationToast, r: R, hover: Option<ToastPart>, when: &str, banner: bool) {
        let p = palette();
        if banner { draw_glass_panel(pm, r.x, r.y, r.w, r.h, 18.0, false); }
        else { fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 14.0, p.control_fill); }
        let (ix, iy) = (r.x + 14.0, r.y + 18.0);
        if let Some(img) = &t.image {
            let k = 36.0 / img.width().max(img.height()) as f32;
            let (w, h) = (img.width() as f32 * k, img.height() as f32 * k);
            crate::icons::draw_icon(pm, img, ix + (36.0 - w) * 0.5, iy + (36.0 - h) * 0.5, w);
        } else if let Some(icon) = &t.icon {
            draw_any_icon(pm, fonts, icons, icon, &t.app_label, ix, iy, 36.0);
        } else {
            crate::theme::fill_squircle_gradient(pm, ix, iy, 36.0, rgba(110, 200, 255, 255), rgba(20, 110, 235, 255));
            draw_feather_logo(pm, ix + 18.0, iy + 18.0, 1.3, Color::WHITE);
        }
        let when_w = fonts.measure_text(when, 11.5, false);
        fonts.draw_text_clipped(pm, &t.title, r.x + 62.0, r.y + 13.0, r.w - 62.0 - when_w - 24.0, 13.0, p.text, true);
        fonts.draw_text_right(pm, when, r.x + r.w - 14.0, r.y + 13.5, 11.5, p.text_secondary, false);
        for (k, line) in fonts.wrap_two_lines(&t.body, r.w - 76.0, 12.5, false).iter().enumerate() {
            fonts.draw_text(pm, line, r.x + 62.0, r.y + 32.0 + k as f32 * 16.0, 12.5, p.text_secondary, false);
        }
        for (a, b) in Self::toast_button_rects(t, r) {
            let hot = hover == Some(ToastPart::Action(a));
            let fill = if hot { p.accent } else if banner { p.control_fill } else { p.window_bg };
            fill_rounded_rect(pm, b.x, b.y, b.w, b.h, 7.0, fill);
            let label = fonts.ellipsize(&t.actions[a].1, b.w - 12.0, 12.0, false);
            fonts.draw_text_centered(pm, &label, b.x + b.w * 0.5, b.y + 5.5, 12.0, if hot { p.on_accent } else { p.text }, false);
        }
        if hover.is_some() {
            let c = Self::toast_close_rect(r);
            draw_glass_panel(pm, c.x, c.y, c.w, c.h, c.w * 0.5, false);
            let col = if hover == Some(ToastPart::Close) { p.text } else { p.text_secondary };
            fonts.draw_text_centered(pm, "×", c.x + c.w * 0.5, c.y + 1.0, 14.0, col, true);
        }
    }

    /// Banner rectangles, newest first, stacked down from the menu bar
    fn toast_rects(&self) -> Vec<R> {
        let x = self.screen_w as f32 - TOAST_W - 10.0;
        let mut y = TOP_BAR_HEIGHT as f32 + 8.0;
        self.toasts.iter().map(|t| {
            let r = R::new(x, y, TOAST_W, t.height());
            y += r.h + TOAST_GAP;
            r
        }).collect()
    }

    fn toast_close_rect(r: R) -> R { R::new(r.x - 6.0, r.y - 6.0, 20.0, 20.0) }

    fn toast_button_rects(t: &NotificationToast, r: R) -> Vec<(usize, R)> {
        let buttons: Vec<_> = t.buttons().map(|(i, _)| i).collect();
        let n = buttons.len().max(1) as f32;
        let (x0, w) = (r.x + 62.0, r.w - 62.0 - 12.0);
        let bw = (w - (n - 1.0) * 8.0) / n;
        buttons.into_iter().enumerate()
            .map(|(k, i)| (i, R::new(x0 + k as f32 * (bw + 8.0), r.y + TOAST_H, bw, TOAST_BUTTON_H)))
            .collect()
    }

    fn toast_at(&self, x: f32, y: f32) -> Option<(usize, ToastPart)> {
        if !self.toast_visible() { return None; }
        for (i, r) in self.toast_rects().into_iter().enumerate() {
            if self.toast_hover.is_some_and(|(h, _)| h == i) && Self::toast_close_rect(r).contains(x, y) {
                return Some((i, ToastPart::Close));
            }
            if r.contains(x, y) {
                let part = Self::toast_button_rects(&self.toasts[i], r).into_iter()
                    .find(|(_, b)| b.contains(x, y))
                    .map_or(ToastPart::Body, |(a, _)| ToastPart::Action(a));
                return Some((i, part));
            }
        }
        None
    }

    // ───────────────────────── Pointer ─────────────────────────

    pub fn update_pointer_hover(&mut self, px: f64, py: f64) {
        let hover=if py>=0.0&&py<TOP_BAR_HEIGHT as f64{self.topbar_item_at(px as f32)}else{None};
        if hover!=self.status_hover{self.status_hover=hover;self.topbar_dirty=true;}
        let center = self.center_at(px as f32, py as f32);
        if center != self.center_hover {
            self.center_hover = center;
            self.overlay_dirty = true;
        }
        let toast = self.toast_at(px as f32, py as f32);
        if toast != self.toast_hover {
            let left = self.toast_hover.map(|(i, _)| i).filter(|i| toast.map(|(j, _)| j) != Some(*i));
            if let Some(t) = left.and_then(|i| self.toasts.get_mut(i)) { t.created_at = Instant::now(); }
            self.toast_hover = toast;
            self.overlay_dirty = true;
        }
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
            if px>=0.0 && px<self.screen_w as f64 && py>=0.0 && py < TOP_BAR_HEIGHT as f64 {
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

        let (local_x, local_y) = self.dock_local(px, py);
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
        if mx>=0.0 && mx<self.screen_w as f32 && my >= 0.0 && my < TOP_BAR_HEIGHT as f32 {
            let hit = self.topbar_item_at(mx);
            let was = (self.open_menu, self.control_center_open, self.spotlight_open, self.center_open);
            self.close_menus();
            match hit {
                Some(TopbarItem::Menu(m)) => {
                    if was.0 != Some(m) {
                        self.open_menu = Some(m);
                    }
                }
                Some(TopbarItem::Clock)=>{ if !was.3 { self.toggle_center(); } }
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
                    CcItem::Bluetooth => {
                        self.cc.open(crate::control_panel::Page::Bluetooth);
                        return Some(ShellClickAction::Control(crate::controls::Control::ScanBluetooth));
                    }
                    CcItem::Focus => return Some(ShellClickAction::ToggleDoNotDisturb),
                    CcItem::DarkMode => return Some(ShellClickAction::ToggleAppearance),
                    CcItem::Brightness => {
                        match self.cc.start_brightness(R::new(r.x+14.0,r.y+30.0,r.w-28.0,20.0),mx) {
                            Some(v) => return Some(ShellClickAction::SetBrightness(v)),
                            None => self.cc.error=Some("Яркость этого дисплея недоступна: нет подсветки и монитор не отвечает по DDC/CI".into()),
                        }
                    }
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

        if self.center_open && my >= TOP_BAR_HEIGHT as f32 {
            match self.center_at(mx, my) {
                Some(hit) => self.center_click(hit),
                None if self.center_layout().0.contains(mx, my) => {}
                None => self.close_menus(),
            }
            return Some(ShellClickAction::None);
        }
        if let Some((i, part)) = self.toast_at(mx, my) {
            let t = &self.toasts[i];
            let (id, resident) = (t.id, t.resident);
            let key = match part {
                ToastPart::Close => None,
                ToastPart::Body => t.actions.iter().any(|(k, _)| k == "default").then(|| "default".to_string()),
                ToastPart::Action(a) => Some(t.actions[a].0.clone()),
            };
            match key {
                Some(key) if id != 0 => {
                    self.notification_events.push(NotificationEvent::Invoked(id, key));
                    if !resident { self.remove_toast(i, crate::notifications::Reason::Dismissed); }
                }
                _ => self.remove_toast(i, crate::notifications::Reason::Dismissed),
            }
            return Some(ShellClickAction::None);
        }

        // Dock
        let (lx, ly) = self.dock_local(px, py);
        let on_icon = self
            .dock_slots
            .iter()
            .any(|s| R::new(s.x, s.y, s.size, s.size).contains(lx, ly));
        if self.dock_bar.contains(lx, ly) || on_icon {
            let half_gap = self.dock_slot_gap * 0.5;
            let slot = self
                .dock_slots
                .iter()
                .find(|s| lx >= s.x - half_gap && lx < s.x + s.size + half_gap);
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
            self.wallpaper_texture = None;
            match renderer.import_memory(&pixels,Fourcc::Abgr8888,size.into(),false){Ok(texture)=>{self.wallpaper_texture=Some(TextureBuffer::from_texture(renderer,texture,1,Transform::Normal,None));self.wallpaper_texture_size=size;},Err(e)=>tracing::warn!("wallpaper upload: {e:?}")}
            drop(pixels);
            #[cfg(target_os = "linux")]
            unsafe { libc::malloc_trim(0); }
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
        let bar = self.bar_colors();
        let mut hits = Vec::with_capacity(16);
        let tray=self.tray_items.clone();let visible_tray=self.tray_visible_count();
        let tray_width=visible_tray as f32*28.0+if tray.len()>visible_tray{30.0}else{0.0};
        let input=self.input_sources.get(self.input_selected).copied().unwrap_or(crate::settings::SOURCES[0]);
        let hover=self.status_hover;
        let (center_open,dnd)=(self.center_open,self.dnd_active());
        let mut menus = vec![(active_app_title.to_string(), TopMenu::App, true, true)];
        if let Some(root) = self.client_menu.as_ref().filter(|root| root.children.iter().any(|n| n.visible)) {
            menus.extend(root.children.iter().filter(|n| n.visible && !n.separator).map(|n| (n.label.clone(), TopMenu::Client(n.id), false, n.enabled)));
        } else {
            menus.extend([("Окно".into(), TopMenu::Window, false, true), ("Справка".into(), TopMenu::Help, false, true)]);
        }

        let icons=&mut self.icons;
        let mut ctx = self.topbar_buffer.render();
        let _ = ctx.draw(|buf| {
            // Tahoe: fill the menu bar tint directly in a single pass instead of
            // zeroing the whole buffer and rasterizing a full-screen tiny_skia mask.
            let tc = bar.tint.premultiply().to_color_u8();
            let px = [tc.red(), tc.green(), tc.blue(), tc.alpha()];
            for chunk in buf.chunks_exact_mut(4) {
                chunk.copy_from_slice(&px);
            }
            if let Some(mut pm) = PixmapMut::from_bytes(buf, pw as u32, ph as u32) {
                let text_y = (lh - 13.0) * 0.5 - 1.5;
                let highlight = |pm: &mut PixmapMut, x0: f32, x1: f32| {
                    fill_rounded_rect(pm, x0, 2.0, x1 - x0, lh - 4.0, 6.0, bar.highlight);
                };

                // Left: logo, app menu, Window, Help — all of them real menus
                if open_menu == Some(TopMenu::Feather) {
                    highlight(&mut pm, 8.0, 36.0);
                }
                draw_feather_logo(&mut pm, 22.0, lh * 0.5, 0.95, bar.text);
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
                        if *enabled { bar.text } else { bar.dim }, *bold);
                    if *enabled { hits.push((x - 9.0, x + w + 9.0, TopbarItem::Menu(*menu))); }
                    x += w + 20.0;
                }

                // A fixed grid keeps targets aligned when the source or clock changes.
                let mut rx=lw-14.0;
                let cw=fonts.measure_text(&clock,13.0,false);
                let clock_width=cw.max(146.0);
                if center_open||hover==Some(TopbarItem::Clock){highlight(&mut pm,rx-clock_width-4.0,lw-5.0);}
                fonts.draw_text(&mut pm,&clock,rx-cw,text_y,13.0,bar.text,false);
                if dnd {crate::control_panel::icon(&mut pm,crate::control_panel::Icon::Moon,rx-cw-12.0,lh*0.5,bar.text,false);}
                hits.push((rx-clock_width-4.0,lw,TopbarItem::Clock));rx-=clock_width+14.0;
                for (hit,opened) in [(TopbarItem::ControlCenter,cc_open),(TopbarItem::Spotlight,sp_open),(TopbarItem::Wifi,cc_open)]{
                    if opened||hover==Some(hit){highlight(&mut pm,rx-29.0,rx-1.0);}
                    let center=rx-15.0;
                    match hit {
                        TopbarItem::ControlCenter=>draw_control_center_icon(&mut pm,center,lh*0.5,bar.text),
                        TopbarItem::Spotlight=>draw_search_icon(&mut pm,center,lh*0.5,5.0,bar.text),
                        TopbarItem::Wifi=>draw_wifi_icon(&mut pm,center,lh*0.5-2.0,bar.text,wifi),_=>{}
                    }
                    hits.push((rx-30.0,rx,hit));rx-=30.0;
                }
                if open_menu==Some(TopMenu::Input)||hover==Some(TopbarItem::Layout){highlight(&mut pm,rx-31.0,rx-1.0);}
                crate::system_settings::draw_input_badge(&mut pm,fonts,input.badge,rx-27.0,(lh-16.0)*0.5,22.0,16.0,bar.text);
                hits.push((rx-32.0,rx,TopbarItem::Layout));rx-=40.0;
                for (index,item) in tray.iter().take(visible_tray).enumerate(){
                    let x=rx-23.0;let y=(lh-18.0)*0.5;
                    if hover==Some(TopbarItem::Tray(index)){highlight(&mut pm,rx-27.0,rx-1.0);}
                    // A client pixmap is authoritative; an unresolved theme name
                    // must not replace it with the generic application fallback.
                    let icon=if item.pixels.is_none()&&!item.icon_name.is_empty(){
                        icons.get(&crate::icons::IconKey{icon:AppIcon{kind:crate::apps::IconKind::GenericApp,name:Some(item.icon_name.clone())},initial:String::new(),px:to_phys(18) as u32,dark:appearance()==Appearance::Dark})
                    }else{None};
                    let source=icon.or(item.pixels.as_deref());
                    if let Some(image)=source {
                        // An icon that would vanish into the wallpaper gets a soft chip
                        let size=(18.0*ui_scale()).round().max(1.0);
                        if let Some(mut tile)=tiny_skia::Pixmap::new(size as u32,size as u32) {
                            tile.draw_pixmap(0,0,image.as_ref(),&tiny_skia::PixmapPaint{quality:tiny_skia::FilterQuality::Bilinear,..Default::default()},tiny_skia::Transform::from_scale(size/image.width() as f32,size/image.height() as f32),None);
                            if tray_icon_needs_chip(&tile,bar.dark) {
                                // A soft disc, like a macOS status item's selection, not a box
                                fill_rounded_rect(&mut pm,x-2.0,y-2.0,22.0,22.0,11.0,bar.chip);
                            }
                            pm.draw_pixmap((x*ui_scale()).round() as i32,(y*ui_scale()).round() as i32,tile.as_ref(),&tiny_skia::PixmapPaint::default(),tiny_skia::Transform::identity(),None);
                        }
                    }else{
                        crate::theme::stroke_circle(&mut pm,x+9.0,y+9.0,6.0,1.3,bar.text);
                        let label=item.title.chars().next().unwrap_or('•').to_string();
                        fonts.draw_text_centered(&mut pm,&label,x+9.0,y+2.0,10.0,bar.text,false);
                    }
                    if item.attention{fill_circle(&mut pm,x+17.0,y+2.0,2.0,p.accent);}
                    hits.push((rx-28.0,rx,TopbarItem::Tray(index)));rx-=28.0;
                }
                if tray.len()>visible_tray {
                    let label=format!("+{}",tray.len()-visible_tray);
                    fonts.draw_text_centered(&mut pm,&label,rx-14.0,text_y,11.0,bar.text,false);
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
        let boundaries = self.dock_separator_boundaries();
        const SEP: f32 = 14.0;
        let num_seps = boundaries.len() as f32;
        let buf_w = self.dock_buf_w as f32;
        let buf_h = self.dock_buf_h as f32;
        let (min_along, max_along) = self.dock_along_bounds();
        let avail_span = (max_along - min_along).max(1.0);
        let clamp_along = |w: f32| ((buf_w - w) * 0.5).clamp(min_along, (max_along - w).max(min_along));

        // Base layout (unmagnified), shrunk proportionally when needed so a huge Dock fits on screen like macOS
        let gap = self.dock_gap;
        let base_fit = self.dock_fit_scale();
        let base_icon = dock_icon() * base_fit;
        let base_gap = DOCK_ICON_GAP * base_fit;
        let base_sep = SEP * base_fit;
        let base_pad_x = DOCK_PAD_X * base_fit;
        let base_gap_w = if gap.is_some() { base_icon + base_gap } else { 0.0 };
        let base_total = n as f32 * base_icon + n.saturating_sub(1) as f32 * base_gap + num_seps * base_sep;
        let base_bar_w = base_total + base_gap_w + base_pad_x * 2.0;
        let base_x0 = clamp_along(base_bar_w) + base_pad_x;
        let base_center = |i: usize| {
            base_x0
                + if gap.is_some_and(|g| i >= g) { base_gap_w } else { 0.0 }
                + i as f32 * (base_icon + base_gap)
                + base_sep * boundaries.iter().filter(|&&b| i >= b).count() as f32
                + base_icon * 0.5
        };

        let mag_peak = (dock_icon() * (1.0 + DOCK_MAGNIFY)).min(base_icon * 2.2);
        let mut sizes: Vec<f32> = (0..n)
            .map(|i| match (self.dock_zoom_enabled, self.dock_cursor_x) {
                (true, Some(cx)) => {
                    let d = ((cx - base_center(i)) / (base_icon * 2.6)).abs();
                    let f = if d < 1.0 { (d * std::f32::consts::FRAC_PI_2).cos().powi(2) } else { 0.0 };
                    base_icon + (mag_peak - base_icon) * f
                }
                _ => base_icon,
            })
            .collect();

        // If hover magnification would push the Dock past the screen edges, compress proportionally so all icons stay on screen
        let raw_content_w: f32 =
            sizes.iter().sum::<f32>() + n.saturating_sub(1) as f32 * base_gap + num_seps * base_sep + base_gap_w;
        let raw_bar_w = raw_content_w + base_pad_x * 2.0;
        let mag_fit = if raw_bar_w > avail_span {
            (avail_span / raw_bar_w).clamp(0.15, 1.0)
        } else {
            1.0
        };
        if mag_fit < 1.0 {
            for s in &mut sizes {
                *s *= mag_fit;
            }
        }
        let fit = base_fit * mag_fit;
        let icon_gap = DOCK_ICON_GAP * fit;
        let sep_step = SEP * fit;
        let pad_x = DOCK_PAD_X * fit;
        let pad_y = DOCK_PAD_Y * fit;
        let shelf_icon = dock_icon() * fit;
        let gap_w = base_gap_w * mag_fit;
        let content_w: f32 =
            sizes.iter().sum::<f32>() + n.saturating_sub(1) as f32 * icon_gap + num_seps * sep_step + gap_w;
        let bar_h = shelf_icon + pad_y * 2.0;
        let bar_w = content_w + pad_x * 2.0;
        let mut bar = R::new(clamp_along(bar_w), buf_h - bar_h - DOCK_BOTTOM_MARGIN as f32, bar_w, bar_h);
        let baseline = bar.y + bar.h - pad_y;

        let mut slots = Vec::with_capacity(n);
        let mut x = bar.x + pad_x;
        let mut sep_x: Vec<f32> = Vec::with_capacity(2);
        for (i, item) in self.dock_items.iter().enumerate() {
            if gap == Some(i) {
                x += gap_w;
            }
            if boundaries.contains(&i) {
                sep_x.push(x + sep_step * 0.5 - icon_gap * 0.5);
                x += sep_step;
            }
            slots.push(DockSlot {
                target: item.target,
                x,
                y: baseline - sizes[i],
                size: sizes[i],
            });
            x += sizes[i] + icon_gap;
        }

        if let Some((target, center)) = self.dock_anchor {
            if let Some(slot) = slots.iter().find(|s| s.target == target) {
                let shift = (center - (slot.x + slot.size * 0.5))
                    .clamp(min_along - bar.x, (max_along - (bar.x + bar.w)).max(min_along - bar.x));
                bar.x += shift;
                for slot in &mut slots { slot.x += shift; }
                for sx in &mut sep_x { *sx += shift; }
            }
        }

        let hovered = self.dock_cursor_x.and_then(|cx| {
            slots
                .iter()
                .position(|s| cx >= s.x - icon_gap * 0.5 && cx < s.x + s.size + icon_gap * 0.5)
        });

        // Damage: previous and current painted areas only
        let label = hovered.map(|h| self.dock_items[h].label.clone());
        let label_w = label.as_ref().map(|l| fonts.measure_text(l, 13.0, false) + 20.0).unwrap_or(0.0);
        let top_icon = slots.iter().map(|s| s.y).fold(bar.y, f32::min);
        let map = self.dock_map();
        let painted = R::new(bar.x - 30.0, top_icon - 40.0, bar.w + 60.0, buf_h - top_icon + 40.0).grow(label_w * 0.5);
        // A side Dock's label sits beside the icon, outside the turned layout area
        let painted = if map.edge.vertical() { map.rect(painted).grow(label_w + 20.0) } else { painted };
        let (bw, bh) = self.dock_buffer_size();
        let pw = to_phys(bw);
        let ph = to_phys(bh);
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
        let radius = (DOCK_RADIUS * fit).max(8.0);
        let sep_pad = (10.0 * fit).clamp(4.0, 10.0);
        let dot_scale = fit.clamp(0.5, 1.0);
        let mut ctx = self.dock_buffer.render();
        let _ = ctx.draw(|buf| {
            clear_rects(buf, pw, &clear);
            if let Some(mut pm) = PixmapMut::from_bytes(buf, pw as u32, ph as u32) {
                // Liquid-glass shelf over the frosted desktop (positions mapped for a side Dock)
                let b = map.rect(bar);
                let shadow = map.rect(R::new(bar.x, bar.y + 2.0, bar.w, bar.h));
                fill_rounded_rect(&mut pm, shadow.x, shadow.y, shadow.w, shadow.h, radius, rgba(0, 0, 0, 22));
                crate::theme::fill_frosted(&mut pm, b.x, b.y, b.w, b.h, radius);
                fill_rounded_rect(&mut pm, b.x, b.y, b.w, b.h, radius, p.dock_fill);
                stroke_rounded_rect(&mut pm, b.x + 0.75, b.y + 0.75, b.w - 1.5, b.h - 1.5, radius - 0.75, 1.0, p.glass_edge);
                stroke_rounded_rect(&mut pm, b.x - 0.25, b.y - 0.25, b.w + 0.5, b.h + 0.5, radius + 0.25, 0.5, p.glass_outline);

                for &sx in &sep_x {
                    let (x0, y0) = map.point(sx, bar.y + sep_pad);
                    let (x1, y1) = map.point(sx, bar.y + bar.h - sep_pad);
                    stroke_line(&mut pm, x0, y0, x1, y1, 1.0, p.separator);
                }

                for (slot, item) in slots.iter().zip(items.iter()) {
                    let n = item.windows.min(3) as f32;
                    for k in 0..n as usize {
                        let dx = (k as f32 - (n - 1.0) * 0.5) * 5.0 * dot_scale;
                        let (cx, cy) = map.point(slot.x + slot.size * 0.5 + dx, bar.y + bar.h - (3.0 * dot_scale).max(1.5));
                        fill_circle(&mut pm, cx, cy, 1.9 * dot_scale, p.dock_dot);
                    }
                    // The bouncing icon is drawn as its own GPU element above the Dock
                    if bouncing == Some(item.target) {
                        continue;
                    }
                    let r = map.rect(R::new(slot.x, slot.y, slot.size, slot.size));
                    if slot.size == dock_icon() {
                        draw_any_icon(&mut pm, fonts, icons, &item.icon, &item.label, r.x, r.y, slot.size);
                    } else {
                        draw_resampled_icon(&mut pm, icons, &item.icon, &item.label, r.x, r.y, slot.size);
                    }
                }

                if let (Some(h), Some(label)) = (hovered, label.as_ref()) {
                    let slot = &slots[h];
                    let lw = fonts.measure_text(label, 13.0, false) + 20.0;
                    let (lx, ly) = if map.edge.vertical() {
                        // Beside the icon, on the screen's inner side
                        let r = map.rect(R::new(slot.x, slot.y, slot.size, slot.size));
                        let ly = (r.y + r.h * 0.5 - 12.0).clamp(min_along, (buf_w - 28.0).max(min_along));
                        if map.edge == crate::theme::DockEdge::Left { (r.x + r.w + 12.0, ly) } else { (r.x - lw - 12.0, ly) }
                    } else {
                        ((slot.x + slot.size * 0.5 - lw * 0.5).clamp(4.0, buf_w - lw - 4.0), slot.y - 34.0)
                    }
                    ;
                    draw_glass_panel(&mut pm, lx, ly, lw, 24.0, 8.0, true);
                    fonts.draw_text(&mut pm, label, lx + 10.0, ly + 4.5, 13.0, p.text, false);
                }
            }
            Result::<_, ()>::Ok(damage)
        });
        drop(ctx);
        self.dock_slots = slots;
        self.dock_bar = bar;
        self.dock_slot_gap = icon_gap;
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
            rects.extend(self.toast_rects());
        }
        if self.center_open {
            rects.push(self.center_layout().0);
        }
        if let Some(m) = &self.mission {
            rects.extend(m.rects());
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
        let cc = if self.control_center_open {
            let (panel, items) = self.cc_layout();
            (local(panel), items.into_iter().map(|(r, item)| (local(r), item)).collect::<Vec<_>>())
        } else {
            (R::new(0.0, 0.0, 0.0, 0.0), Vec::new())
        };
        let sp = if self.spotlight_open {
            let (bar, list, count, math) = self.spotlight_layout(registry);
            (local(bar), list.map(local), count, math)
        } else {
            (R::new(0.0, 0.0, 0.0, 0.0), None, 0, false)
        };
        let toasts: Vec<_> = if self.toast_visible() {
            self.toasts.iter().cloned().zip(self.toast_rects().into_iter().map(local)).collect()
        } else {
            Vec::new()
        };
        let toast_hover = self.toast_hover;
        let mission = self.mission.clone().map(|mut m| {
            for t in &mut m.tiles { t.rect = local(t.rect); }
            if let Some((_, r)) = m.title.as_mut() { *r = local(*r); }
            m
        });
        let center_hover = self.center_hover;
        let center = self.center_open.then(|| {
            let (panel, rows, clear) = self.center_layout();
            let rows: Vec<_> = rows.into_iter().map(|(r, row)| (local(r), match row {
                CenterRow::Group(i) => CenterItem::Group(i, self.history[i].app_label.clone()),
                CenterRow::Card(i) => CenterItem::Card(i, self.history[i].clone()),
            })).collect();
            (local(panel), rows, clear.filter(|_| !self.history.is_empty()).map(local))
        });
        let input_badges:Vec<_>=if self.open_menu==Some(TopMenu::Input){self.input_sources.iter().map(|s|s.badge).collect()}else{Vec::new()};
        let active_input_label=self.input_sources.get(self.input_selected).map_or("ABC",|s|s.label);
        let (cc_open, sp_open) = (self.control_center_open, self.spotlight_open);
        let query = self.spotlight_query.clone();
        let spotlight_input = self.spotlight_input.clone();
        let selected = self.spotlight_selected;
        let (wifi, dnd, vol) = (self.wifi_enabled, self.dnd_active(), self.cc.volume());
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
                        CcItem::Bluetooth => toggle(
                            &mut pm,
                            fonts,
                            controls.bluetooth.available && controls.bluetooth.powered,
                            "Bluetooth",
                            controls.bluetooth.subtitle(),
                            crate::control_panel::Icon::Bluetooth,
                        ),
                        CcItem::Focus => toggle(&mut pm, fonts, dnd, "Не беспокоить", if dnd { "Вкл." } else { "Выкл." },crate::control_panel::Icon::Moon),
                        CcItem::DarkMode => toggle(&mut pm, fonts, dark, "Тёмный режим", if dark { "Вкл." } else { "Выкл." },crate::control_panel::Icon::Contrast),
                        CcItem::Brightness => match controls.shown_brightness() {
                            Some(b) => {
                                crate::control_panel::icon(&mut pm,crate::control_panel::Icon::Sun,r.x+25.0,r.y+16.0,p.text,false);
                                fonts.draw_text(&mut pm,"Дисплей",r.x+46.0,r.y+7.0,13.0,p.text,true);
                                fonts.draw_text_right(&mut pm,&format!("{}%",(b*100.0).round() as u32),r.x+r.w-14.0,r.y+8.0,11.0,p.text_secondary,false);
                                crate::control_panel::slider(&mut pm,R::new(r.x+14.0,r.y+30.0,r.w-28.0,20.0),b,true);
                            }
                            None => {
                                crate::control_panel::icon(&mut pm,crate::control_panel::Icon::Sun,r.x+25.0,r.y+27.0,p.text_secondary,false);
                                fonts.draw_text(&mut pm,"Дисплей",r.x+46.0,r.y+10.0,13.0,p.text,true);
                                fonts.draw_text_clipped(&mut pm,"Яркость недоступна",r.x+46.0,r.y+29.0,r.w-58.0,11.5,p.text_secondary,false);
                            }
                        },
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

            // Bottom of the stack first: the newest banner's close button overlaps its neighbour.
            for (i, (t, r)) in toasts.iter().enumerate().rev() {
                let hover = toast_hover.filter(|(h, _)| *h == i).map(|(_, part)| part);
                Self::draw_toast(&mut pm, fonts, icons, t, *r, hover, "сейчас", true);
            }

            if let Some(m) = mission.as_ref() {
                for t in &m.tiles {
                    let r = t.rect;
                    if t.add {
                        draw_glass_panel(&mut pm, r.x, r.y, r.w, r.h, r.w * 0.5, false);
                        fonts.draw_text_centered(&mut pm, "+", r.x + r.w * 0.5, r.y + r.h * 0.5 - 14.0, 26.0, if t.hover { p.accent } else { p.text }, false);
                        continue;
                    }
                    // A frosted piece of the wallpaper, like a desktop seen from afar
                    crate::theme::fill_frosted(&mut pm, r.x, r.y, r.w, r.h, 10.0);
                    if t.hover { fill_rounded_rect(&mut pm, r.x, r.y, r.w, r.h, 10.0, rgba(255, 255, 255, 60)); }
                    stroke_rounded_rect(&mut pm, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, 9.5, if t.current { 2.5 } else { 1.0 }, if t.current { p.accent } else { rgba(255, 255, 255, 140) });
                    // Below the tile: window thumbnails cover its inside
                    let pill = R::new(r.x + 14.0, r.y + r.h + 4.0, r.w - 28.0, 20.0);
                    fill_rounded_rect(&mut pm, pill.x, pill.y, pill.w, pill.h, 10.0, rgba(0, 0, 0, 80));
                    fonts.draw_text_centered(&mut pm, &t.label, pill.x + pill.w * 0.5, pill.y + 3.0, 11.5, Color::WHITE, true);
                    if t.removable {
                        let c = R::new(r.x - 7.0, r.y - 7.0, 22.0, 22.0);
                        draw_glass_panel(&mut pm, c.x, c.y, c.w, c.h, c.w * 0.5, true);
                        fonts.draw_text_centered(&mut pm, "×", c.x + c.w * 0.5, c.y + 2.0, 14.0, if t.remove_hover { p.text } else { p.text_secondary }, true);
                    }
                    // The current desktop's windows are large below; others get thumbnails
                    if t.current && t.windows > 0 {
                        let n = t.windows.min(9);
                        for i in 0..n {
                            let x = r.x + r.w * 0.5 - (n as f32 - 1.0) * 6.0 + i as f32 * 12.0;
                            fill_rounded_rect(&mut pm, x - 4.0, r.y + 22.0, 8.0, 8.0, 2.0, rgba(255, 255, 255, 210));
                        }
                    }
                }
                if let Some((title, r)) = m.title.as_ref() {
                    let pill = MissionOverlay::title_rect(*r);
                    draw_glass_panel(&mut pm, pill.x, pill.y, pill.w, pill.h, 13.0, true);
                    fonts.draw_text_clipped_centered(&mut pm, title, pill.x + pill.w * 0.5, pill.y + 5.0, pill.w - 20.0, 12.5, p.text, true);
                }
            }

            if let Some((r, rows, clear)) = center.as_ref() {
                let r = *r;
                draw_glass_panel(&mut pm, r.x, r.y, r.w, r.h, 18.0, false);
                fonts.draw_text(&mut pm, "Уведомления", r.x + 16.0, r.y + 12.0, 15.0, p.text, true);
                if rows.is_empty() {
                    fonts.draw_text_centered(&mut pm, "Нет уведомлений", r.x + r.w * 0.5, r.y + 66.0, 13.0, p.text_secondary, false);
                }
                if let Some(c) = clear {
                    let hot = center_hover == Some(CenterHit::ClearAll);
                    fonts.draw_text_right(&mut pm, "Очистить все", c.x + c.w, c.y + 4.0, 12.0, if hot { p.accent } else { p.text_secondary }, false);
                }
                for (rr, row) in rows {
                    let rr = *rr;
                    match row {
                        CenterItem::Group(i, label) => {
                            fonts.draw_text_clipped(&mut pm, label, rr.x + 6.0, rr.y + 6.0, rr.w - 40.0, 12.0, p.text_secondary, true);
                            let hot = center_hover == Some(CenterHit::ClearGroup(*i));
                            if hot { fill_rounded_rect(&mut pm, rr.x + rr.w - 24.0, rr.y + 3.0, 20.0, 20.0, 10.0, p.control_fill); }
                            fonts.draw_text_centered(&mut pm, "×", rr.x + rr.w - 14.0, rr.y + 4.0, 14.0, if hot { p.text } else { p.text_secondary }, true);
                        }
                        CenterItem::Card(i, t) => {
                            let hover = match center_hover { Some(CenterHit::Card(j, part)) if j == *i => Some(part), _ => None };
                            Self::draw_toast(&mut pm, fonts, icons, t, rr, hover, &Self::posted_label(t), false);
                        }
                    }
                }
            }

            Result::<_, ()>::Ok(damage)
        });
    }

    // ───────────────────────── App switcher (Alt+Tab) ─────────────────────────

    fn switcher_geometry(&self, count: usize) -> (f32, R) {
        let cell = ((self.screen_w as f32 - 108.0) / count.max(1) as f32).clamp(40.0,96.0);
        let w = (28.0 + cell*count as f32).ceil();
        let h = (14.0 + cell + 34.0).ceil();
        (cell,R::new(((self.screen_w as f32-w)*0.5).round(),((self.screen_h as f32-h)*0.5).round(),w,h))
    }
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
            pointer_anchor: None,
        });
        self.switcher_dirty = true;
    }

    pub fn switcher_step(&mut self, delta: i32) {
        if let Some(sw) = self.switcher.as_mut() {
            let n = sw.items.len() as i32;
            if n == 0 { return; }
            sw.selected = ((sw.selected as i32 + delta).rem_euclid(n)) as usize;
            sw.visible = true;
            self.switcher_dirty = true;
        }
    }

    pub fn switcher_hit(&self,x:f64,y:f64)->Option<usize>{
        let sw=self.switcher.as_ref().filter(|s|s.visible)?;
        let n=sw.items.len();if n==0{return None;}
        let (cell,rect)=self.switcher_geometry(n);
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

        let pad = 14.0;
        let (cell,rect) = self.switcher_geometry(sw.items.len());
        let icon = cell - 18.0;
        let (w,h) = (rect.w,rect.h);
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

    fn switch_items(n: u64) -> Vec<SwitcherItem> {
        (0..n).map(|id| SwitcherItem {key:AppKey::Window(id),window_id:id,label:format!("Fixture {id}"),icon:AppIcon {kind:crate::apps::IconKind::GenericApp,name:None}}).collect()
    }
    #[test]
    fn switcher_hit_uses_current_layout_and_first_frame_has_real_pixels() {
        let mut shell=MacShell::new(1440,900);let mut fonts=FontSystem::new();
        shell.open_switcher(switch_items(3),false);shell.switcher.as_mut().unwrap().visible=true;
        let (cell,r)=shell.switcher_geometry(3);
        shell.switcher_rect=R::new(1.0,2.0,1.0,1.0);
        assert_eq!(shell.switcher_hit((r.x+14.0+cell*1.5) as f64,(r.y+14.0+cell*0.5) as f64),Some(1));
        shell.ensure_switcher_rendered(&mut fonts);
        assert_eq!(shell.switcher_rect.w,r.w);
        let expected=(to_phys(r.w as i32)*to_phys(r.h as i32)*4) as usize;
        shell.switcher_buffer.render().draw(|pixels| {
            assert_eq!(pixels.len(),expected);assert!(pixels.iter().any(|v|*v!=0));
            Ok::<_,()>(Vec::new())
        }).unwrap();
    }
    #[test]
    fn keyboard_selection_ignores_pointer_jitter_but_allows_deliberate_hover() {
        let mut shell=MacShell::new(1440,900);shell.open_switcher(switch_items(3),false);
        let sw=shell.switcher.as_mut().unwrap();sw.pointer_anchor=Some((500.0,400.0));
        assert!(!sw.allow_pointer(500.0,400.0));assert!(!sw.allow_pointer(502.0,402.0));assert_eq!(sw.selected,1);
        assert!(sw.allow_pointer(507.0,400.0));assert!(sw.allow_pointer(501.0,400.0));
        sw.pointer_anchor=Some((501.0,400.0));assert!(!sw.allow_pointer(502.0,400.0));
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

    #[test]
    fn huge_dock_scales_to_fit_screen_in_resting_and_magnified_states() {
        let mut fonts = FontSystem::new();
        let mut shell = MacShell::new(1440, 900);
        let mut items: Vec<_> = (0..30).map(|i| item(DockTarget::App(i))).collect();
        items.extend((0..5).map(|i| item(DockTarget::Minimized(i))));
        items.push(item(DockTarget::Trash));
        shell.set_dock_items(items);
        shell.ensure_dock_rendered(&mut fonts);

        assert!(shell.dock_fit_scale() < 1.0, "36 items must shrink to fit 1440px width");
        assert!(shell.dock_bar.x >= 5.9 && shell.dock_bar.x + shell.dock_bar.w <= 1434.1);
        assert_eq!(shell.dock_slots.len(), 36);
        let first = shell.dock_slots.first().unwrap().clone();
        let last = shell.dock_slots.last().unwrap().clone();
        assert!(first.x >= shell.dock_bar.x && last.x + last.size <= shell.dock_bar.x + shell.dock_bar.w + 0.1);
        let (ox, oy) = shell.dock_origin();
        assert_eq!(
            shell.dock_target_at((ox + first.x + first.size * 0.5) as f64, (oy + first.y + first.size * 0.5) as f64).map(|(t, _)| t),
            Some(DockTarget::App(0))
        );
        assert_eq!(
            shell.dock_target_at((ox + last.x + last.size * 0.5) as f64, (oy + last.y + last.size * 0.5) as f64).map(|(t, _)| t),
            Some(DockTarget::Trash)
        );

        for hover_idx in [0usize, 18, 35] {
            let cx = shell.dock_slots[hover_idx].x + shell.dock_slots[hover_idx].size * 0.5;
            shell.dock_cursor_x = Some(cx);
            shell.dock_dirty = true;
            shell.ensure_dock_rendered(&mut fonts);
            assert!(
                shell.dock_bar.x >= 5.9 && shell.dock_bar.x + shell.dock_bar.w <= 1434.1,
                "magnified huge Dock must stay within screen bounds at hover_idx={hover_idx}: {:?}",
                shell.dock_bar
            );
            let far_idx = (hover_idx + 18) % 36;
            assert!(
                shell.dock_slots[hover_idx].size > shell.dock_slots[far_idx].size,
                "hovered icon in huge Dock must still magnify relative to unhovered icons"
            );
        }
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

    fn incoming(id: u32, actions: &[(&str, &str)]) -> crate::notifications::Incoming {
        crate::notifications::Incoming {
            id, replaces: false, app_name: "Test".into(), app_icon: String::new(), desktop_entry: String::new(),
            image: None, summary: format!("N{id}"), body: "Тело".into(),
            actions: actions.iter().map(|(k, l)| (k.to_string(), l.to_string())).collect(),
            timeout: Some(crate::notifications::DEFAULT_TIMEOUT), critical: false, resident: false, transient: false, sound: None,
        }
    }

    #[test]
    fn notification_banners_stack_replace_evict_and_report_user_actions() {
        use crate::notifications::Reason;
        let registry = AppRegistry::new();
        let mut shell = MacShell::new(1000, 700);
        shell.toasts.clear();
        for id in 1..=3 { shell.show_notification(incoming(id, &[]), &registry); }
        assert_eq!(shell.toasts.iter().map(|t| t.id).collect::<Vec<_>>(), [3, 2, 1]);
        let rects = shell.toast_rects();
        assert!(rects[1].y >= rects[0].y + rects[0].h, "stacked without overlap");
        // Replacing keeps the place and emits nothing
        let mut again = incoming(2, &[]); again.summary = "new".into(); again.replaces = true;
        shell.show_notification(again, &registry);
        assert_eq!(shell.toasts[1].title, "new");
        assert!(shell.notification_events.is_empty());
        // A fourth pushes the oldest into the center
        shell.show_notification(incoming(4, &[("default", ""), ("reply", "Ответить")]), &registry);
        assert!(shell.notification_events.is_empty());
        assert_eq!(shell.history[0].id, 1);
        // Action button, then the default click on the body
        let r = shell.toast_rects()[0];
        assert!(r.h > TOAST_H);
        let (_, b) = MacShell::toast_button_rects(&shell.toasts[0], r)[0];
        shell.handle_pointer_click((b.x + 4.0) as f64, (b.y + 4.0) as f64, &registry);
        assert_eq!(shell.notification_events, [NotificationEvent::Invoked(4, "reply".into()), NotificationEvent::Closed(4, Reason::Dismissed)]);
        shell.notification_events.clear();
        // Hover shows the close button; it dismisses without an action
        let r = shell.toast_rects()[0];
        shell.update_pointer_hover((r.x + 100.0) as f64, (r.y + 20.0) as f64);
        let c = MacShell::toast_close_rect(r);
        shell.update_pointer_hover((c.x + 10.0) as f64, (c.y + 10.0) as f64);
        shell.handle_pointer_click((c.x + 10.0) as f64, (c.y + 10.0) as f64, &registry);
        assert_eq!(shell.notification_events, [NotificationEvent::Closed(3, Reason::Dismissed)]);
        shell.notification_events.clear();
        // Client close, unknown ids and own messages are silent
        assert!(shell.close_notification(2));
        assert!(!shell.close_notification(2) && !shell.close_notification(0));
        assert_eq!(shell.notification_events, [NotificationEvent::Closed(2, Reason::Closed)]);
    }

    #[test]
    fn notification_center_keeps_history_groups_and_reports_closes() {
        use crate::notifications::Reason;
        let registry = AppRegistry::new();
        let mut shell = MacShell::new(1000, 700);
        shell.toasts.clear();
        shell.show_notification(incoming(1, &[("default", "")]), &registry);
        let mut other = incoming(2, &[]); other.app_name = "Other".into();
        shell.show_notification(other, &registry);
        let mut gone = incoming(3, &[]); gone.transient = true;
        shell.show_notification(gone, &registry);
        // Expired banners move to the center silently; a transient one closes
        for t in &mut shell.toasts { t.created_at -= Duration::from_secs(60); }
        shell.tick();
        assert!(shell.toasts.is_empty());
        assert_eq!(shell.history.iter().map(|t| t.id).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(shell.notification_events, [NotificationEvent::Closed(3, Reason::Expired)]);
        shell.notification_events.clear();
        // While the center is open, new ones go straight into it
        shell.toggle_center();
        assert!(shell.center_open && shell.has_visible_overlay());
        shell.show_notification(incoming(4, &[]), &registry);
        assert!(shell.toasts.is_empty() && shell.history[0].id == 4);
        assert_eq!(shell.center_rows(), [CenterRow::Group(0), CenterRow::Card(0), CenterRow::Card(2), CenterRow::Group(1), CenterRow::Card(1)]);
        // Default click on a card invokes, removes and closes the center
        let (_, rows, _) = shell.center_layout();
        let (r, _) = rows.iter().find(|(_, row)| *row == CenterRow::Card(2)).copied().unwrap();
        shell.update_pointer_hover((r.x + 150.0) as f64, (r.y + 40.0) as f64);
        shell.handle_pointer_click((r.x + 150.0) as f64, (r.y + 40.0) as f64, &registry);
        assert_eq!(shell.notification_events, [NotificationEvent::Invoked(1, "default".into()), NotificationEvent::Closed(1, Reason::Dismissed)]);
        assert!(!shell.center_open);
        shell.notification_events.clear();
        // Clear a group, then client close of the rest
        shell.toggle_center();
        let (_, rows, _) = shell.center_layout();
        let (g, _) = rows.iter().find(|(_, row)| *row == CenterRow::Group(1)).copied().unwrap();
        shell.handle_pointer_click((g.x + g.w - 14.0) as f64, (g.y + 12.0) as f64, &registry);
        assert_eq!(shell.notification_events, [NotificationEvent::Closed(2, Reason::Dismissed)]);
        assert!(shell.close_notification(4) && shell.history.is_empty());
        // A click outside closes it; history is bounded
        shell.handle_pointer_click(20.0, 400.0, &registry);
        assert!(!shell.center_open);
        shell.notification_events.clear();
        shell.dnd_enabled = true;
        for id in 10..10 + MAX_HISTORY as u32 + 2 { shell.show_notification(incoming(id, &[]), &registry); }
        assert!(shell.toasts.is_empty(), "do not disturb sends everything to the center");
        assert_eq!(shell.history.len(), MAX_HISTORY);
        assert_eq!(shell.notification_events, [NotificationEvent::Closed(10, Reason::Expired), NotificationEvent::Closed(11, Reason::Expired)]);
    }

    #[test]
    fn per_app_delivery_remembers_senders_and_routes_their_notifications() {
        use crate::notifications::{Delivery, Reason};
        let registry = AppRegistry::new();
        let mut shell = MacShell::new(1000, 700);
        shell.toasts.clear();
        let mut tg = incoming(1, &[]); tg.app_name = "Telegram".into(); tg.desktop_entry = "org.telegram.desktop".into();
        shell.show_notification(tg.clone(), &registry);
        assert!(shell.notify_apps_changed);
        assert_eq!(shell.notify_apps.len(), 1);
        assert_eq!(shell.notify_apps[0].key, "org.telegram.desktop");
        assert_eq!(shell.toasts[0].id, 1);
        shell.notify_apps_changed = false;
        // Same sender again: nothing new to save
        tg.id = 2;
        shell.show_notification(tg.clone(), &registry);
        assert!(!shell.notify_apps_changed);
        // Center only: no banner, but kept
        shell.notify_apps[0].delivery = Delivery::CenterOnly;
        tg.id = 3;
        shell.show_notification(tg.clone(), &registry);
        assert!(shell.toasts.iter().all(|t| t.id != 3) && shell.history[0].id == 3);
        // Critical still gets a banner
        tg.id = 4; tg.critical = true;
        shell.show_notification(tg.clone(), &registry);
        assert_eq!(shell.toasts[0].id, 4);
        // Off: dropped, an existing one with that id disappears, the client is told
        shell.notify_apps[0].delivery = Delivery::Off;
        tg.id = 2; tg.critical = false;
        shell.show_notification(tg, &registry);
        assert!(shell.toasts.iter().chain(&shell.history).all(|t| t.id != 2));
        assert_eq!(shell.notification_events, [NotificationEvent::Closed(2, Reason::Dismissed)]);
        // Senders are bounded
        for i in 0..100 { let mut n = incoming(100 + i, &[]); n.app_name = format!("App{i}"); shell.show_notification(n, &registry); }
        assert_eq!(shell.notify_apps.len(), crate::notifications::MAX_APPS);
        assert_eq!(Delivery::Off.next(), Delivery::Banners);
        // Sounds: a new banner only, never for updates, quiet routes or with sounds off
        let mut shell = MacShell::new(1000, 700);
        shell.toasts.clear();
        let chime = |id, replaces| { let mut n = incoming(id, &[]); n.sound = Some("/s.oga".into()); n.replaces = replaces; n };
        shell.show_notification(chime(1, false), &registry);
        shell.show_notification(chime(1, true), &registry);
        shell.dnd_enabled = true;
        shell.show_notification(chime(2, false), &registry);
        shell.dnd_enabled = false;
        shell.notify_sounds = false;
        shell.show_notification(chime(3, false), &registry);
        assert_eq!(shell.notification_events, [NotificationEvent::Sound("/s.oga".into())]);
    }

    #[test]
    fn do_not_disturb_schedule_wraps_midnight_and_can_be_ended_early() {
        assert!(in_window(23 * 60, 22 * 60, 7 * 60) && in_window(6 * 60 + 59, 22 * 60, 7 * 60));
        assert!(!in_window(7 * 60, 22 * 60, 7 * 60) && !in_window(12 * 60, 22 * 60, 7 * 60));
        assert!(in_window(13 * 60, 13 * 60, 14 * 60) && !in_window(14 * 60, 13 * 60, 14 * 60));
        let mut shell = MacShell::new(1000, 700);
        shell.dnd_schedule = Some((22 * 60, 7 * 60));
        assert!(!shell.update_dnd_schedule(12 * 60) && !shell.dnd_active());
        assert!(shell.update_dnd_schedule(22 * 60) && shell.dnd_active());
        // Off by hand: stays off for the rest of this window only
        shell.toggle_dnd();
        assert!(!shell.dnd_active() && !shell.dnd_enabled);
        shell.update_dnd_schedule(3 * 60);
        assert!(!shell.dnd_active());
        assert!(shell.update_dnd_schedule(7 * 60));
        shell.update_dnd_schedule(22 * 60);
        assert!(shell.dnd_active(), "the next window applies again");
        // Manual on outlives the window; removing the schedule ends a scheduled state
        shell.toggle_dnd(); shell.toggle_dnd();
        assert!(shell.dnd_enabled);
        shell.update_dnd_schedule(8 * 60);
        assert!(shell.dnd_active());
        shell.dnd_enabled = false;
        shell.update_dnd_schedule(23 * 60);
        shell.dnd_schedule = None;
        assert!(shell.update_dnd_schedule(23 * 60) && !shell.dnd_active());
        assert_eq!(dnd_window_label((22 * 60, 7 * 60)), "22:00 – 7:00");
    }

    #[test]
    fn menu_bar_follows_the_wallpaper_and_tray_icons_stay_visible() {
        let mut glass = tiny_skia::Pixmap::new(40, 30).unwrap();
        glass.fill(tiny_skia::Color::from_rgba8(20, 30, 60, 255));
        assert!(bar_luma(&glass, 900) < 0.2, "a dark wallpaper is dark");
        glass.fill(tiny_skia::Color::from_rgba8(240, 235, 250, 255));
        assert!(bar_luma(&glass, 900) > 0.8);
        // A bright strip below the bar does not count
        let mut mixed = tiny_skia::Pixmap::new(40, 30).unwrap();
        mixed.fill(tiny_skia::Color::from_rgba8(15, 15, 30, 255));
        mixed.fill_rect(tiny_skia::Rect::from_xywh(0.0, 3.0, 40.0, 27.0).unwrap(), &tiny_skia::Paint { shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::WHITE), ..Default::default() }, tiny_skia::Transform::identity(), None);
        assert!(bar_luma(&mixed, 900) < 0.2);

        let dark = BarColors::for_background(true);
        let light = BarColors::for_background(false);
        assert!(dark.text.red() > 0.9 && light.text.red() < 0.1);
        // A white icon on a light bar gets a chip and keeps its colour; on a dark bar not
        let mut white = tiny_skia::Pixmap::new(8, 8).unwrap();
        white.fill(tiny_skia::Color::WHITE);
        assert!(tray_icon_needs_chip(&white, false) && !tray_icon_needs_chip(&white, true));
        let mut yellow = tiny_skia::Pixmap::new(8, 8).unwrap();
        yellow.fill(tiny_skia::Color::from_rgba8(250, 220, 40, 255));
        assert!(tray_icon_needs_chip(&yellow, false));
        assert!(!tray_icon_needs_chip(&yellow, true));
        let empty = tiny_skia::Pixmap::new(8, 8).unwrap();
        assert!(!tray_icon_needs_chip(&empty, false));
    }

    /// Visual check: the menu bar over every wallpaper, light and dark, with typical tray
    /// icons, hovered language badge and the open input menu. Writes one PNG to
    /// FEATHER_MENUBAR_SHEET (run with --ignored).
    #[test]
    #[ignore = "writes a contact sheet: FEATHER_MENUBAR_SHEET=/path.png cargo test --release menubar_contact_sheet -- --ignored"]
    fn menubar_contact_sheet() {
        let Some(out) = std::env::var_os("FEATHER_MENUBAR_SHEET") else { return };
        let (lw, lh, scale) = (1440, 900, 2.0f32);
        let (pw, ph) = ((lw as f32 * scale) as i32, (lh as f32 * scale) as i32);
        let crop_w = 1300 * scale as i32;
        let (bar_h, menu_h) = ((TOP_BAR_HEIGHT as f32 * scale) as i32, (150.0 * scale) as i32);
        let icon = |f: &dyn Fn(u32, u32) -> [u8; 4]| {
            let mut p = tiny_skia::Pixmap::new(40, 40).unwrap();
            for (i, px) in p.data_mut().chunks_exact_mut(4).enumerate() {
                let c = f((i % 40) as u32, (i / 40) as u32);
                let a = c[3] as u16;
                px.copy_from_slice(&[(c[0] as u16 * a / 255) as u8, (c[1] as u16 * a / 255) as u8, (c[2] as u16 * a / 255) as u8, c[3]]);
            }
            Some(Arc::new(p))
        };
        let ring = |rgb: [u8; 3]| move |x: u32, y: u32| {
            let d = (((x as f32 - 19.5).powi(2) + (y as f32 - 19.5).powi(2)).sqrt() - 13.0).abs();
            if d < 3.5 { [rgb[0], rgb[1], rgb[2], 255] } else { [0, 0, 0, 0] }
        };
        let disc = |rgb: [u8; 3]| move |x: u32, y: u32| {
            if ((x as f32 - 19.5).powi(2) + (y as f32 - 19.5).powi(2)).sqrt() < 17.0 { [rgb[0], rgb[1], rgb[2], 255] } else { [0, 0, 0, 0] }
        };
        let items: Vec<crate::tray::Item> = [
            ("white", icon(&ring([255, 255, 255]))),
            ("black", icon(&ring([10, 10, 10]))),
            ("grey", icon(&ring([140, 140, 140]))),
            ("telegram", icon(&disc([40, 160, 230]))),
            ("yellow", icon(&disc([250, 215, 40]))),
            ("navy", icon(&disc([20, 30, 80]))),
        ].into_iter().map(|(k, pixels)| crate::tray::Item { key: k.into(), title: k.into(), icon_name: String::new(), pixels, menu_only: false, attention: false }).collect();
        let mut sources: Vec<(String, Vec<u8>)> = (0..3).map(|i| {
            let img = image::load_from_memory(crate::wallpaper::bundled_bytes(i)).unwrap()
                .resize_to_fill(pw as u32, ph as u32, image::imageops::FilterType::Triangle).to_rgba8();
            (crate::wallpaper::NAMES[i].to_string(), img.into_raw())
        }).collect();
        for theme in [WallpaperTheme::TahoeDay, WallpaperTheme::TahoeNight, WallpaperTheme::SequoiaMidnight, WallpaperTheme::FeatherCyan] {
            let mut px = vec![0; (pw * ph * 4) as usize];
            crate::theme::render_wallpaper(&mut px, pw, ph, theme);
            sources.push((theme.name().to_string(), px));
        }
        let row_h = bar_h * 3 + menu_h + 16;
        let mut sheet = image::RgbaImage::new(crop_w as u32 * 2 + 20, row_h as u32 * sources.len() as u32);
        for (row, (_, wall)) in sources.iter().enumerate() {
            for (col, look) in [Appearance::Light, Appearance::Dark].into_iter().enumerate() {
                crate::theme::with_render_settings(scale, look, || {
                    let mut fonts = FontSystem::new();
                    let registry = AppRegistry::new();
                    let mut shell = MacShell::new(lw, lh);
                    shell.toasts.clear();
                    let glass = crate::wallpaper::glass(wall, (pw, ph), ((pw / 8).max(8), (ph / 8).max(8)), if look == Appearance::Dark { 0.8 } else { 1.0 });
                    shell.install_asset(crate::raster::Asset::Wallpaper { pixels: Vec::new(), size: (pw, ph), image: None, glass });
                    shell.set_tray_items(items.clone());
                    let mut frame = |shell: &mut MacShell, fonts: &mut FontSystem, top: i32| {
                        shell.topbar_dirty = true;
                        shell.ensure_topbar_rendered(fonts, "Файлы", "EN");
                        let mut bar = Vec::new();
                        let _ = shell.topbar_buffer.render().draw(|d| { bar.extend_from_slice(d); Ok::<_, ()>(Vec::new()) });
                        let mut over: Option<(Vec<u8>, Rectangle<i32, Buffer>)> = None;
                        if shell.has_visible_overlay() {
                            shell.overlay_dirty = true;
                            shell.ensure_overlay_rendered(fonts, &registry, "EN");
                            let mut o = Vec::new();
                            let _ = shell.overlay_buffer.render().draw(|d| { o.extend_from_slice(d); Ok::<_, ()>(Vec::new()) });
                            over = Some((o, shell.overlay_geometry));
                        }
                        let h = if over.is_some() { bar_h + menu_h } else { bar_h };
                        let x0 = pw - crop_w;
                        for y in 0..h {
                            for x in 0..crop_w {
                                let sx = x0 + x;
                                let i = ((y * pw + sx) * 4) as usize;
                                let mut c = [wall[i] as f32, wall[i + 1] as f32, wall[i + 2] as f32];
                                let mut blend = |p: &[u8]| {
                                    let a = p[3] as f32 / 255.0;
                                    for k in 0..3 { c[k] = p[k] as f32 + c[k] * (1.0 - a); }
                                };
                                if let Some((o, g)) = &over {
                                    let (ox, oy) = (sx - g.loc.x, y - g.loc.y);
                                    if ox >= 0 && oy >= 0 && ox < g.size.w && oy < g.size.h {
                                        let j = ((oy * g.size.w + ox) * 4) as usize;
                                        // Overlay is above the bar: bar first, then overlay
                                        if y < bar_h { let b = ((y * pw + sx) * 4) as usize; blend(&bar[b..b + 4]); }
                                        blend(&o[j..j + 4]);
                                        sheet.put_pixel((col as i32 * (crop_w + 20) + x) as u32, (row as i32 * row_h + top + y) as u32, image::Rgba([c[0] as u8, c[1] as u8, c[2] as u8, 255]));
                                        continue;
                                    }
                                }
                                if y < bar_h { let b = ((y * pw + sx) * 4) as usize; blend(&bar[b..b + 4]); }
                                sheet.put_pixel((col as i32 * (crop_w + 20) + x) as u32, (row as i32 * row_h + top + y) as u32, image::Rgba([c[0] as u8, c[1] as u8, c[2] as u8, 255]));
                            }
                        }
                    };
                    frame(&mut shell, &mut fonts, 0);
                    shell.status_hover = Some(TopbarItem::Layout);
                    frame(&mut shell, &mut fonts, bar_h);
                    shell.status_hover = None;
                    shell.open_menu = Some(TopMenu::Input);
                    shell.menu_hovered = Some(0);
                    frame(&mut shell, &mut fonts, bar_h * 2 + 4);
                });
            }
        }
        sheet.save(out).unwrap();
    }

    #[test]
    fn hidden_dock_takes_no_clicks_and_its_buffer_follows_the_icon_size() {
        let mut shell = MacShell::new(1440, 900);
        shell.set_dock_items(vec![DockItem { target: DockTarget::Trash, label: "Корзина".into(), icon: AppIcon { kind: crate::apps::IconKind::TrashEmpty, name: None }, windows: 0, pinned: false }]);
        let mut fonts = FontSystem::new();
        shell.ensure_dock_rendered(&mut fonts);
        let (x, y) = (720.0, 900.0 - 30.0);
        assert!(shell.dock_contains(x, y), "shown: the bar is hit");
        shell.dock_hidden = true;
        assert!(!shell.dock_contains(x, y) && shell.dock_target_at(x, y).is_none(), "hidden: clicks pass through");
        // Sizes are pure functions of the icon size (the global is not touched in tests)
        assert_eq!(crate::theme::reserved_for(50.0, false), 74, "the old fixed value");
        assert!(crate::theme::reserved_for(64.0, false) > 74);
        assert_eq!(crate::theme::reserved_for(64.0, true), 4, "maximized windows reach almost the bottom");
    }

    /// Visual check of the Dock on each edge (hovered icon with its label). Changes the
    /// global edge, so it runs only with FEATHER_DOCK_SHEET=/path.png and --ignored.
    #[test]
    #[ignore = "FEATHER_DOCK_SHEET=/path.png cargo test --release dock_edge_sheet -- --ignored"]
    fn dock_edge_sheet() {
        let Some(out) = std::env::var_os("FEATHER_DOCK_SHEET") else { return };
        use crate::theme::DockEdge;
        let mut sheet = image::RgbaImage::from_pixel(1800, 1000, image::Rgba([90, 110, 160, 255]));
        let mut x0 = 0;
        for edge in [DockEdge::Bottom, DockEdge::Left, DockEdge::Right] {
            crate::theme::set_dock_edge(edge);
            crate::theme::with_render_settings(1.0, Appearance::Light, || {
                let mut fonts = FontSystem::new();
                let mut shell = MacShell::new(1440, 900);
                let items = (0..6).map(|i| DockItem { target: DockTarget::App(i), label: format!("Программа {i}"), icon: AppIcon { kind: crate::apps::IconKind::GenericApp, name: None }, windows: (i % 3) as u8, pinned: true }).chain(std::iter::once(DockItem { target: DockTarget::Trash, label: "Корзина".into(), icon: AppIcon { kind: crate::apps::IconKind::TrashEmpty, name: None }, windows: 0, pinned: false })).collect();
                shell.set_dock_items(items);
                shell.ensure_dock_rendered(&mut fonts);
                let (x, y, size) = shell.dock_slot_global(DockTarget::App(2)).unwrap();
                shell.update_pointer_hover((x + size * 0.5) as f64, (y + size * 0.5) as f64);
                shell.ensure_dock_rendered(&mut fonts);
                assert_eq!(shell.dock_target_at((x + size * 0.5) as f64, (y + size * 0.5) as f64).map(|t| t.0), Some(DockTarget::App(2)), "{edge:?}: hit test follows the drawing");
                let (bw, bh) = shell.dock_buffer_size();
                let mut px = Vec::new();
                let _ = shell.dock_buffer.render().draw(|d| { px.extend_from_slice(d); Ok::<_, ()>(Vec::new()) });
                let (bw, bh) = (bw as u32, bh as u32);
                for yy in 0..bh.min(1000) {
                    for xx in 0..bw.min(1800 - x0) {
                        let i = ((yy * bw + xx) * 4) as usize;
                        let a = px[i + 3] as f32 / 255.0;
                        let base = sheet.get_pixel(x0 + xx, yy).0;
                        let c = |k: usize| (px[i + k] as f32 + base[k] as f32 * (1.0 - a)) as u8;
                        sheet.put_pixel(x0 + xx, yy, image::Rgba([c(0), c(1), c(2), 255]));
                    }
                }
                x0 += if edge.vertical() { bw + 40 } else { 0 };
                if !edge.vertical() { sheet = image::imageops::crop_imm(&sheet, 0, 0, 1800, 1000).to_image(); }
            });
            if edge == DockEdge::Bottom {
                // Bottom Dock is wide and short: keep only its centre, then place the side ones
                let strip = image::imageops::crop_imm(&sheet, 300, 0, 900, 150).to_image();
                sheet = image::RgbaImage::from_pixel(1800, 1000, image::Rgba([90, 110, 160, 255]));
                image::imageops::overlay(&mut sheet, &strip, 860, 820);
            }
        }
        crate::theme::set_dock_edge(DockEdge::Bottom);
        sheet.save(out).unwrap();
    }

    #[test]
    fn notification_expiry_waits_for_hover_and_do_not_disturb_hides_non_critical() {
        let registry = AppRegistry::new();
        let mut shell = MacShell::new(1000, 700);
        shell.toasts.clear();
        shell.show_notification(incoming(1, &[]), &registry);
        let mut sticky = incoming(2, &[]); sticky.timeout = None;
        shell.show_notification(sticky, &registry);
        for t in &mut shell.toasts { t.created_at -= Duration::from_secs(60); }
        let r = shell.toast_rects()[1];
        shell.update_pointer_hover((r.x + 100.0) as f64, (r.y + 20.0) as f64);
        shell.tick();
        assert_eq!(shell.toasts.len(), 2, "hovered banner waits");
        shell.update_pointer_hover(10.0, 400.0);
        assert!(shell.toasts[1].created_at.elapsed() < Duration::from_secs(1), "full time again after leave");
        shell.toasts[1].created_at -= Duration::from_secs(60);
        shell.tick();
        assert_eq!(shell.toasts.iter().map(|t| t.id).collect::<Vec<_>>(), [2]);
        assert_eq!(shell.history[0].id, 1);
        assert!(shell.notification_events.is_empty());
        shell.dnd_enabled = true;
        shell.show_notification(incoming(3, &[]), &registry);
        let mut urgent = incoming(4, &[]); urgent.critical = true; urgent.timeout = None;
        shell.show_notification(urgent, &registry);
        assert_eq!(shell.toasts.iter().map(|t| t.id).collect::<Vec<_>>(), [4, 2]);
        assert_eq!(shell.history[0].id, 3);
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
                    shell.cc.audio=crate::controls::Audio {default:"test".into(),sinks:vec![crate::controls::Sink {name:"test".into(),channels:vec![65536],..Default::default()}],..Default::default()};
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
                    shell.toasts.clear();
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
