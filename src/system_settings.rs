//! System Settings: one layout describes painting and hit testing.
use crate::{
    settings::{Keyboard, Shortcut, SOURCES},
    shell::R,
    text::FontSystem,
    theme::*,
};
use tiny_skia::PixmapMut;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Appearance,
    Accessibility,
    Language,
    Network,
    Bluetooth,
    Keyboard,
    Pointer,
    Displays,
    Desktop,
    Defaults,
    Privacy,
    DateTime,
    Power,
    Notifications,
    Sound,
}
impl Page {
    pub const ALL: [Self; 15] = [
        Self::Appearance,
        Self::Accessibility,
        Self::Language,
        Self::Network,
        Self::Bluetooth,
        Self::Keyboard,
        Self::Pointer,
        Self::Displays,
        Self::Desktop,
        Self::Defaults,
        Self::Privacy,
        Self::DateTime,
        Self::Power,
        Self::Notifications,
        Self::Sound,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Оформление",
            Self::Accessibility => "Универсальный доступ",
            Self::Language => "Язык и регион",
            Self::Network => "Сеть",
            Self::Bluetooth => "Bluetooth",
            Self::Keyboard => "Клавиатура",
            Self::Pointer => "Мышь и трекпад",
            Self::Displays => "Дисплеи",
            Self::Desktop => "Рабочий стол и Dock",
            Self::Defaults => "Приложения",
            Self::Privacy => "Конфиденциальность",
            Self::DateTime => "Дата и время",
            Self::Power => "Экран блокировки и энергосбережение",
            Self::Sound => "Звук",
            Self::Notifications => "Уведомления",
        }
    }
    fn caption(self) -> &'static str {
        match self {
            Self::Appearance => "Выберите внешний вид и поведение интерфейса.",
            Self::Accessibility => "Крупный текст, контраст, прозрачность и залипание клавиш.",
            Self::Language => "Язык интерфейса, региональные форматы даты, времени и единиц.",
            Self::Network => "Wi‑Fi, проводная сеть, VPN и сохранённые профили.",
            Self::Bluetooth => "Беспроводные наушники, клавиатуры, мыши и другие устройства.",
            Self::Keyboard => "Источники ввода и переключение языка.",
            Self::Pointer => "Курсор, скорость, прокрутка и нажатие.",
            Self::Displays => "Размер интерфейса, разрешение и частота экрана.",
            Self::Desktop => "Настройте рабочее пространство под себя.",
            Self::Defaults => "Приложения по умолчанию и автозапуск при входе.",
            Self::Privacy => "Доступ программ к записи экрана и снимкам через системный портал.",
            Self::DateTime => "Синхронизация часов и часовой пояс.",
            Self::Power => "Пароль, выключение экрана и переход в сон.",
            Self::Sound => "Системные сигналы, устройства вывода и микрофоны.",
            Self::Notifications => "Баннеры, центр уведомлений и «Не беспокоить».",
        }
    }
}
#[derive(Clone, Debug)]
pub enum Action {
    Page(Page),
    LockNow,
    IdleTime(u8),
    DoNotDisturb,
    DndSchedule,
    DndWindow,
    NotifySounds,
    AlertSounds,
    AlertTone,
    AlertVolume(u8),
    PreviewAlert,
    InterfaceSounds,
    VolumeFeedback,
    Audio(crate::controls::Control),
    Network(crate::controls::Control),
    Bluetooth(crate::controls::Control),
    WifiPassword(crate::controls::network::Network),
    WifiPasswordFocus,
    WifiPasswordSubmit,
    WifiPasswordCancel,
    OpenConnectionEditor,
    OpenBluetoothManager,
    /// Next action of this corner (0 top-left … 3 bottom-right)
    HotCorner(usize),
    NightNow,
    NightSchedule,
    NightWindow,
    NightWarmth,
    /// Next delivery mode of the program at this index
    NotifyDelivery(usize),
    Appearance(Appearance),
    AccentColor(crate::theme::AccentColor),
    HighlightColor(Option<crate::theme::AccentColor>),
    TextSize(crate::theme::TextSize),
    HighContrast,
    ReduceTransparency,
    StickyKeys,
    Language(crate::theme::Lang),
    RegionStep(i32),
    Use24HourTime,
    FirstWeekdayMonday(bool),
    MetricUnits(bool),
    Animations,
    DockZoom,
    DockSize(u32),
    GtkTheme,
    DockEdge(crate::theme::DockEdge),
    DockAutohide,
    Wallpaper,
    WallpaperImage(Option<crate::wallpaper::ImageSource>),
    ChooseWallpaper,
    ScreenshotFormat(crate::screenshot::Format),
    ChooseScreenshotDir,
    ResetScreenshotDir,
    ScreenCapturePolicy(crate::file_portal::ScreenCapturePolicy),
    ScreenCaptureCursor,
    ScreenCapturePicker(bool),
    PrivacySearchFocus,
    PrivacySearchSubmit,
    AddScreenCaptureApp(String),
    ToggleScreenCaptureApp(String),
    RemoveScreenCaptureApp(String),
    ClearScreenCaptureApps,
    AppPicker(bool),
    DefaultApp { browser: bool, id: String },
    AutostartPicker(bool),
    AutostartSearchFocus,
    AutostartSearchSubmit,
    AddAutostart(String),
    ToggleAutostart { id: String, enabled: bool },
    RemoveAutostart(String),
    RefreshSystem,
    Ntp,
    ZonePicker,
    Timezone(String),
    ZoneSearchSubmit,
    ZoneSearchFocus,
    EditTime,
    ApplyTime,
    AddPicker(bool),
    AddSource(String),
    RemoveSource(usize),
    SelectSource(usize),
    Shortcut(Shortcut),
    CapsLock,
    EditShortcut(crate::shortcuts::Action),
    ResetShortcuts,
    RepeatDelay(u32),
    RepeatRate(u32),
    CursorPicker,
    CursorTheme(Option<String>),
    CursorSize(u32),
    /// Set from where the slider was clicked (-1…1)
    PointerSpeed(f64),
    NaturalMouse,
    NaturalTouchpad,
    TapToClick,
    LeftHanded,
    Scale(f64),
    ModeStep(i32),
    PrimaryMonitor(String),
    PositionMonitor(String,u8),
    ApplyMode,
    KeepMode,
    RevertMode,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: i32,
    pub height: i32,
    pub refresh: i32,
}
impl Mode {
    pub fn label(self) -> String {
        format!(
            "{} × {} · {:.2} Гц",
            self.width,
            self.height,
            self.refresh as f64 / 1000.0
        )
    }
}
#[derive(Default, Clone)]
pub struct Panel {
    pub app_count: usize,
    pub apps_picker: Option<bool>,
    pub autostart_picker: bool,
    pub autostart_query: feather_ui::input::TextInput,
    pub privacy_picker: bool,
    pub privacy_query: feather_ui::input::TextInput,
    /// Pointer over the window (content coordinates), for hover highlights
    pub pointer: Option<(f32, f32)>,
    pub cursor_picker: bool,
    pub zone_picker: bool,
    pub zone_query: feather_ui::input::TextInput,
    pub time_editing: bool,
    pub time_input: feather_ui::input::TextInput,
    pub wifi_target: Option<crate::controls::network::Network>,
    pub wifi_password: feather_ui::input::TextInput,
    pub wallpaper_busy: bool,
    pub screenshot_dir_busy: bool,
    pub page: Page,
    pub adding: bool,
    pub keyboard_busy: bool,
    pub keybindings: crate::shortcuts::Bindings,
    pub shortcut_capture: Option<crate::shortcuts::Action>,
    pub keyboard_generation: u64,
    pub error: Option<String>,
    pub selected_mode: Option<usize>,
    pub confirming: bool,
    pub queued_mode: Option<(usize, bool)>,
    pub scroll: f32,
    pub audio_drag: Option<(R, crate::controls::Control)>,
    pub audio_sent_volume: Option<f32>,
}
pub struct Context<'a> {
    pub system: crate::system_preferences::State,
    pub pointer: crate::settings::Pointer,
    pub power: crate::power::Preferences,
    pub power_ready: bool,
    /// Mouse settings reach the hardware (DRM or our labwc host), not a nested window
    pub pointer_managed: bool,
    pub keyboard: &'a Keyboard,
    pub active: usize,
    pub animations: bool,
    pub dock_zoom: bool,
    pub scale: f64,
    pub display_name: &'a str,
    pub monitors: Vec<crate::monitors::Info>,
    pub modes: &'a [Mode],
    pub current: Mode,
    pub direct: bool,
    pub wallpaper: WallpaperTheme,
    pub wallpaper_image: Option<&'a crate::wallpaper::ImageSource>,
    pub dnd: bool,
    pub dnd_schedule: Option<(u16, u16)>,
    pub notify_sounds: bool,
    pub sounds: crate::sounds::Preferences,
    pub audio: crate::controls::Audio,
    pub wifi: crate::controls::network::Wifi,
    pub bluetooth: crate::controls::bluetooth::Bluetooth,
    pub hot_corners: [crate::hot_corners::HotAction; 4],
    pub night: crate::night::Preferences,
    pub night_active: bool,
    pub notify_apps: &'a [crate::notifications::AppPolicy],
    pub screen_capture_policy: crate::file_portal::ScreenCapturePolicy,
    pub screen_capture_cursor: bool,
    pub screen_capture_apps: &'a [crate::file_portal::ScreenCaptureApp],
    pub sticky_keys: bool,
}

pub struct FrameJob {
    pub system: crate::system_preferences::State,
    pub pointer: crate::settings::Pointer,
    pub power: crate::power::Preferences,
    pub power_ready: bool,
    pub pointer_managed: bool,
    pub wallpaper_image: Option<crate::wallpaper::ImageSource>,
    pub shadow: bool,
    pub ticket: std::sync::Weak<()>,
    pub panel: Panel,
    pub keyboard: Keyboard,
    pub active: usize,
    pub animations: bool,
    pub dock_zoom: bool,
    pub scale: f64,
    pub display_name: String,
    pub monitors: Vec<crate::monitors::Info>,
    pub modes: Vec<Mode>,
    pub current: Mode,
    pub direct: bool,
    pub wallpaper: WallpaperTheme,
    pub dnd: bool,
    pub dnd_schedule: Option<(u16, u16)>,
    pub notify_sounds: bool,
    pub sounds: crate::sounds::Preferences,
    pub audio: crate::controls::Audio,
    pub wifi: crate::controls::network::Wifi,
    pub bluetooth: crate::controls::bluetooth::Bluetooth,
    pub hot_corners: [crate::hot_corners::HotAction; 4],
    pub night: crate::night::Preferences,
    pub night_active: bool,
    pub notify_apps: Vec<crate::notifications::AppPolicy>,
    pub screen_capture_policy: crate::file_portal::ScreenCapturePolicy,
    pub screen_capture_cursor: bool,
    pub screen_capture_apps: Vec<crate::file_portal::ScreenCaptureApp>,
    pub sticky_keys: bool,
    pub width: i32,
    pub height: i32,
    pub focused: bool,
    pub group_hovered: bool,
    pub traffic: Option<crate::window::TrafficButton>,
}
impl FrameJob {
    pub fn new(
        ticket: std::sync::Weak<()>,
        panel: &Panel,
        c: &Context<'_>,
        win: &crate::window::ManagedWindow,
    ) -> Self {
        Self {
            system: c.system.clone(),
            pointer: c.pointer.clone(),
            power: c.power,power_ready:c.power_ready,
            pointer_managed: c.pointer_managed,
            wallpaper_image: c.wallpaper_image.cloned(),
            shadow: win.settings_needs_shadow(),
            ticket,
            panel: panel.clone(),
            keyboard: c.keyboard.clone(),
            active: c.active,
            animations: c.animations,
            dock_zoom: c.dock_zoom,
            scale: c.scale,
            display_name: c.display_name.into(),
            monitors: c.monitors.clone(),
            modes: c.modes.to_vec(),
            current: c.current,
            direct: c.direct,
            wallpaper: c.wallpaper,
            dnd: c.dnd,
            dnd_schedule: c.dnd_schedule,
            notify_sounds: c.notify_sounds,
            sounds: c.sounds,
            audio: c.audio.clone(),
            wifi: c.wifi.clone(),
            bluetooth: c.bluetooth.clone(),
            hot_corners: c.hot_corners,
            night: c.night,
            night_active: c.night_active,
            notify_apps: c.notify_apps.to_vec(),
            screen_capture_policy: c.screen_capture_policy,
            screen_capture_cursor: c.screen_capture_cursor,
            screen_capture_apps: c.screen_capture_apps.to_vec(),
            sticky_keys: c.sticky_keys,
            width: win.width,
            height: win.height,
            focused: win.activated,
            group_hovered: win.traffic_group_hovered,
            traffic: win.hovered_traffic,
        }
    }
    pub fn context(&self) -> Context<'_> {
        Context {
            system: self.system.clone(),
            pointer: self.pointer.clone(),
            power:self.power,power_ready:self.power_ready,
            pointer_managed: self.pointer_managed,
            keyboard: &self.keyboard,
            active: self.active,
            animations: self.animations,
            dock_zoom: self.dock_zoom,
            scale: self.scale,
            display_name: &self.display_name,
            monitors: self.monitors.clone(),
            modes: &self.modes,
            current: self.current,
            direct: self.direct,
            wallpaper: self.wallpaper,
            wallpaper_image: self.wallpaper_image.as_ref(),
            dnd: self.dnd,
            dnd_schedule: self.dnd_schedule,
            notify_sounds: self.notify_sounds,
            sounds: self.sounds,
            audio: self.audio.clone(),
            wifi: self.wifi.clone(),
            bluetooth: self.bluetooth.clone(),
            hot_corners: self.hot_corners,
            night: self.night,
            night_active: self.night_active,
            notify_apps: &self.notify_apps,
            screen_capture_policy: self.screen_capture_policy,
            screen_capture_cursor: self.screen_capture_cursor,
            screen_capture_apps: &self.screen_capture_apps,
            sticky_keys: self.sticky_keys,
        }
    }
}
/// Scrolled content above this line (content coordinates) is hidden under the toolbar.
const TOOLBAR_CLIP: f32 = 12.0;
/// Sidebar groups, like the sections of macOS System Settings.
const SIDEBAR: [&[Page]; 3] = [
    &[Page::Appearance, Page::Accessibility, Page::Desktop, Page::Displays],
    &[Page::Network, Page::Bluetooth, Page::Keyboard, Page::Pointer, Page::Sound],
    &[Page::Notifications, Page::Privacy, Page::Defaults, Page::Language, Page::DateTime, Page::Power],
];
fn side_width(w: f32) -> f32 {
    if w >= 720.0 {
        220.0
    } else {
        184.0
    }
}
#[derive(Clone, Copy)]
enum Style {
    Sidebar(Page, bool),
    Hero(Page),
    Display,
    Header,
    Footer,
    Group,
    /// Grouped-form row: label and detail on the left; `value` right-aligned.
    Row,
    Toggle(bool),
    Track,
    Segment(bool),
    /// Value 0…1; the click position decides the new value
    Slider(f32),
    Volume(f32),
    Push(bool),
    Popup,
    Mini,
    Source { badge: &'static str, selected: bool },
    Preview(bool, bool),
    ColorSwatch { color: Option<crate::theme::AccentColor>, selected: bool },
    Wallpaper(usize, bool),
    Gradient(bool),
    TextField(bool),
    /// Click-away layer of a sheet (dimmed) or of a popup menu (invisible).
    Scrim(bool),
    Sheet,
    Menu,
    MenuItem(bool),
    Banner(bool),
}
struct Control {
    rect: R,
    label: String,
    detail: String,
    value: String,
    right_reserve: f32,
    action: Option<Action>,
    style: Style,
    /// Not scrolled with the page: sidebar and overlays.
    fixed: bool,
    divider: bool,
}
/// Builds grouped forms top to bottom; groups are sized once their rows are known.
struct Layout {
    x: f32,
    cw: f32,
    y: f32,
    out: Vec<Control>,
    group: Option<usize>,
    first: bool,
}
impl Layout {
    fn push(&mut self, rect: R, label: &str, style: Style, action: Option<Action>) -> &mut Control {
        if self.group.is_some()
            && matches!(
                style,
                Style::Push(_)
                    | Style::Popup
                    | Style::Track
                    | Style::Mini
                    | Style::TextField(_)
                    | Style::ColorSwatch { .. }
            )
        {
            if let Some(row) = self
                .out
                .iter_mut()
                .rev()
                .find(|c| matches!(c.style, Style::Row))
            {
                if rect.y >= row.rect.y
                    && rect.y < row.rect.y + row.rect.h
                    && rect.x > row.rect.x + 40.0
                {
                    let reserve = (row.rect.x + row.rect.w - rect.x + 12.0).max(0.0);
                    row.right_reserve = row.right_reserve.max(reserve);
                }
            }
        }
        self.out.push(Control {
            rect,
            label: label.into(),
            detail: String::new(),
            value: String::new(),
            right_reserve: 0.0,
            action,
            style,
            fixed: false,
            divider: false,
        });
        self.out.last_mut().unwrap()
    }
    fn hero(&mut self, page: Page) {
        let r = R::new(self.x, self.y, self.cw, 72.0);
        self.push(r, page.title(), Style::Hero(page), None).detail = page.caption().into();
        self.y += 72.0 + 20.0;
    }
    fn header(&mut self, text: &str) {
        let r = R::new(self.x + 8.0, self.y, self.cw - 16.0, 18.0);
        self.push(r, text, Style::Header, None);
        self.y += 24.0;
    }
    fn begin(&mut self) {
        let r = R::new(self.x, self.y, self.cw, 0.0);
        self.push(r, "", Style::Group, None);
        self.group = Some(self.out.len() - 1);
        self.first = true;
    }
    /// A row of the open group; extra controls are placed inside the returned rect.
    fn row(&mut self, h: f32, label: &str, detail: String, style: Style, action: Option<Action>) -> R {
        let r = R::new(self.x, self.y, self.cw, h);
        let divider = !std::mem::replace(&mut self.first, false);
        let c = self.push(r, label, style, action);
        c.detail = detail;
        c.divider = divider;
        self.y += h;
        r
    }
    fn end(&mut self) {
        if let Some(i) = self.group.take() {
            let g = &mut self.out[i];
            g.rect.h = self.y - g.rect.y;
        }
        self.y += 8.0;
    }
    fn footer(&mut self, text: &str) {
        let lines = if text.chars().count() as f32 * 6.0 > self.cw - 16.0 {
            2.0
        } else {
            1.0
        };
        let r = R::new(self.x + 8.0, self.y, self.cw - 16.0, lines * 15.0);
        self.push(r, text, Style::Footer, None);
        self.y += lines * 15.0 + 12.0;
    }
    fn fixed(&mut self, rect: R, label: &str, style: Style, action: Option<Action>) -> &mut Control {
        let c = self.push(rect, label, style, action);
        c.fixed = true;
        c
    }
}
/// "breeze_cursors" → "Breeze", "Breeze_Light" → "Breeze Light".
fn theme_label(dir: &str) -> String {
    let name = dir.trim_end_matches("_cursors").trim_end_matches("-cursors").replace(['_', '-'], " ");
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

fn page_color(page: Page) -> (tiny_skia::Color, tiny_skia::Color) {
    match page {
        Page::Appearance => (rgba(80, 80, 88, 255), rgba(28, 28, 32, 255)),
        Page::Accessibility => (rgba(35, 145, 255, 255), rgba(10, 88, 215, 255)),
        Page::Language => (rgba(65, 185, 255, 255), rgba(20, 118, 232, 255)),
        Page::Network => (rgba(55, 165, 255, 255), rgba(15, 105, 225, 255)),
        Page::Bluetooth => (rgba(45, 145, 255, 255), rgba(10, 85, 215, 255)),
        Page::Keyboard => (rgba(160, 165, 175, 255), rgba(112, 117, 128, 255)),
        Page::Pointer => (rgba(120, 128, 140, 255), rgba(74, 80, 92, 255)),
        Page::Displays => (rgba(70, 175, 255, 255), rgba(20, 115, 235, 255)),
        Page::Desktop => (rgba(80, 200, 245, 255), rgba(20, 140, 220, 255)),
        Page::Defaults => (rgba(150, 120, 255, 255), rgba(100, 70, 225, 255)),
        Page::Privacy => (rgba(48, 176, 199, 255), rgba(16, 114, 210, 255)),
        Page::Sound => (rgba(255, 130, 150, 255), rgba(220, 60, 100, 255)),
        Page::Power => (rgba(90,165,220,255),rgba(45,95,175,255)),
        Page::Notifications => (rgba(255, 92, 86, 255), rgba(236, 52, 62, 255)),
        Page::DateTime => (rgba(255, 105, 105, 255), rgba(225, 55, 70, 255)),
    }
}
/// macOS-like settings glyph: a rounded tile with a white symbol, any size.
fn draw_page_icon(pm: &mut PixmapMut, page: Page, x: f32, y: f32, size: f32) {
    let (top, bottom) = page_color(page);
    if let Some(path) = rounded_rect_path(x, y, size, size, size * 0.24) {
        fill_path_gradient(pm, &path, y, y + size, top, bottom);
    }
    let k = size / 20.0;
    let (ix, iy) = (x, y);
    let pt = |dx: f32, dy: f32| (ix + dx * k, iy + dy * k);
    let white = rgba(255, 255, 255, 255);
    match page {
        Page::Appearance => {
            let (cx, cy) = pt(10.0, 10.0);
            stroke_circle(pm, cx, cy, 5.5 * k, 1.3 * k, white);
            // Right half filled: the classic light/dark appearance symbol.
            let mut half = tiny_skia::PathBuilder::new();
            half.move_to(cx, cy - 5.5 * k);
            half.cubic_to(cx + 7.33 * k, cy - 5.5 * k, cx + 7.33 * k, cy + 5.5 * k, cx, cy + 5.5 * k);
            half.close();
            if let Some(half) = half.finish() {
                fill_path_gradient(pm, &half, cy - 6.0 * k, cy + 6.0 * k, white, white);
            }
        }
        Page::Accessibility => {
            let (cx, cy) = pt(10.0, 10.0);
            stroke_circle(pm, cx, cy, 6.2 * k, 1.15 * k, white);
            let (hx, hy) = pt(10.0, 6.3);
            fill_circle(pm, hx, hy, 1.15 * k, white);
            let (alx, aly) = pt(6.3, 8.7);
            let (arx, ary) = pt(13.7, 8.7);
            stroke_line(pm, alx, aly, arx, ary, 1.2 * k, white);
            let (bx0, by0) = pt(10.0, 8.7);
            let (bx1, by1) = pt(10.0, 11.4);
            stroke_line(pm, bx0, by0, bx1, by1, 1.25 * k, white);
            let (llx, lly) = pt(8.0, 14.4);
            let (lrx, lry) = pt(12.0, 14.4);
            stroke_line(pm, bx1, by1, llx, lly, 1.2 * k, white);
            stroke_line(pm, bx1, by1, lrx, lry, 1.2 * k, white);
        }
        Page::Language => {
            let (cx, cy) = pt(10.0, 10.0);
            stroke_circle(pm, cx, cy, 6.2 * k, 1.2 * k, white);
            stroke_line(pm, cx - 5.4 * k, cy - 2.3 * k, cx + 5.4 * k, cy - 2.3 * k, 0.95 * k, white);
            stroke_line(pm, cx - 5.4 * k, cy + 2.3 * k, cx + 5.4 * k, cy + 2.3 * k, 0.95 * k, white);
            let mut b = tiny_skia::PathBuilder::new();
            b.move_to(cx, cy - 6.0 * k);
            b.quad_to(cx - 3.6 * k, cy, cx, cy + 6.0 * k);
            b.move_to(cx, cy - 6.0 * k);
            b.quad_to(cx + 3.6 * k, cy, cx, cy + 6.0 * k);
            if let Some(path) = b.finish() {
                let s = crate::theme::ui_scale();
                let mut paint = tiny_skia::Paint::default();
                paint.set_color(white);
                paint.anti_alias = true;
                let stroke = tiny_skia::Stroke { width: 1.0 * k, ..Default::default() };
                pm.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::from_scale(s, s), None);
            }
        }
        Page::Network => {
            let (cx, cy) = pt(10.0, 10.0);
            stroke_circle(pm, cx, cy, 6.2 * k, 1.2 * k, white);
            stroke_line(pm, cx - 6.0 * k, cy, cx + 6.0 * k, cy, 1.0 * k, white);
            stroke_line(pm, cx, cy - 6.0 * k, cx, cy + 6.0 * k, 1.0 * k, white);
            let mut b = tiny_skia::PathBuilder::new();
            b.move_to(cx, cy - 6.0 * k);
            b.quad_to(cx - 3.6 * k, cy, cx, cy + 6.0 * k);
            b.move_to(cx, cy - 6.0 * k);
            b.quad_to(cx + 3.6 * k, cy, cx, cy + 6.0 * k);
            if let Some(path) = b.finish() {
                let s = crate::theme::ui_scale();
                let mut paint = tiny_skia::Paint::default();
                paint.set_color(white);
                paint.anti_alias = true;
                let stroke = tiny_skia::Stroke { width: 1.0 * k, ..Default::default() };
                pm.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::from_scale(s, s), None);
            }
        }
        Page::Bluetooth => {
            let (cx, cy) = pt(10.0, 10.0);
            stroke_line(pm, cx, cy - 5.8 * k, cx, cy + 5.8 * k, 1.3 * k, white);
            stroke_line(pm, cx, cy - 5.8 * k, cx + 3.8 * k, cy - 2.6 * k, 1.25 * k, white);
            stroke_line(pm, cx + 3.8 * k, cy - 2.6 * k, cx - 3.6 * k, cy + 2.8 * k, 1.25 * k, white);
            stroke_line(pm, cx, cy + 5.8 * k, cx + 3.8 * k, cy + 2.6 * k, 1.25 * k, white);
            stroke_line(pm, cx + 3.8 * k, cy + 2.6 * k, cx - 3.6 * k, cy - 2.8 * k, 1.25 * k, white);
        }
        Page::Keyboard => {
            let (x0, y0) = pt(3.0, 6.0);
            stroke_rounded_rect(pm, x0, y0, 14.0 * k, 9.0 * k, 2.0 * k, 1.1 * k, white);
            for row in 0..2 {
                for col in 0..4 {
                    let (cx, cy) = pt(5.75 + col as f32 * 2.85, 8.4 + row as f32 * 2.6);
                    fill_circle(pm, cx, cy, 0.65 * k, white);
                }
            }
            let (a, b) = pt(7.5, 13.4);
            stroke_line(pm, a, b, a + 5.0 * k, b, 1.0 * k, white);
        }
        Page::Pointer => {
            // Arrow cursor
            let mut b = tiny_skia::PathBuilder::new();
            let pts = [(6.5, 3.5), (6.5, 15.0), (9.3, 12.4), (11.3, 16.6), (13.0, 15.8), (11.0, 11.7), (14.8, 11.7)];
            let (sx, sy) = pt(pts[0].0, pts[0].1);
            b.move_to(sx, sy);
            for &(px, py) in &pts[1..] {
                let (x, y) = pt(px, py);
                b.line_to(x, y);
            }
            b.close();
            if let Some(path) = b.finish() {
                fill_path_gradient(pm, &path, y, y + size, white, white);
            }
        }
        Page::Displays => {
            let (x0, y0) = pt(3.0, 4.0);
            stroke_rounded_rect(pm, x0, y0, 14.0 * k, 10.0 * k, 2.0 * k, 1.2 * k, white);
            let (a, b) = pt(7.0, 16.0);
            stroke_line(pm, a, b, a + 6.0 * k, b, 1.2 * k, white);
        }
        Page::Sound => {
            let (mx, my) = pt(7.0, 3.0);
            stroke_rounded_rect(pm, mx, my, 6.0*k, 9.0*k, 3.0*k, 1.4*k, white);
            let (cx, cy) = pt(10.0, 10.0);
            stroke_circle(pm, cx, cy, 5.0*k, 1.2*k, white);
            // Cover the upper half of the outer ring with the tile gradient.
            if let Some(path) = rounded_rect_path(x+3.5*k, y+3.0*k, 13.0*k, 6.0*k, 0.0) {
                fill_path_gradient(pm, &path, y, y+size, top, bottom);
            }
            stroke_rounded_rect(pm, mx, my, 6.0*k, 9.0*k, 3.0*k, 1.4*k, white);
            stroke_line(pm, cx, cy+5.0*k, cx, cy+8.0*k, 1.3*k, white);
            stroke_line(pm, cx-3.0*k, cy+8.0*k, cx+3.0*k, cy+8.0*k, 1.3*k, white);
        }
        Page::Power => {
            let (x0,y0)=pt(4.0,9.0);
            stroke_rounded_rect(pm,x0,y0,12.0*k,9.0*k,2.0*k,1.3*k,white);
            let (cx,cy)=pt(10.0,7.5);stroke_circle(pm,cx,cy,3.5*k,1.3*k,white);
        }
        Page::Notifications => {
            // Bell: dome, rim and clapper
            let mut b = tiny_skia::PathBuilder::new();
            let (l, t) = pt(5.0, 14.0);
            b.move_to(l, t);
            let (c1x, c1y) = pt(6.4, 12.4);
            let (sx, sy) = pt(6.0, 8.5);
            b.quad_to(c1x, c1y, sx, sy);
            let (k1x, k1y) = pt(6.0, 4.4);
            let (tx, ty) = pt(10.0, 4.4);
            b.quad_to(k1x, k1y, tx, ty);
            let (k2x, k2y) = pt(14.0, 4.4);
            let (ex, ey) = pt(14.0, 8.5);
            b.quad_to(k2x, k2y, ex, ey);
            let (c2x, c2y) = pt(13.6, 12.4);
            let (rx, ry) = pt(15.0, 14.0);
            b.quad_to(c2x, c2y, rx, ry);
            b.close();
            if let Some(path) = b.finish() {
                fill_path_gradient(pm, &path, y, y + size, white, white);
            }
            let (cx, cy) = pt(10.0, 15.6);
            fill_circle(pm, cx, cy, 1.5 * k, white);
        }
        Page::DateTime => {
            let (cx, cy) = pt(10.0, 10.0);
            stroke_circle(pm, cx, cy, 6.0 * k, 1.2 * k, white);
            stroke_line(pm, cx, cy - 4.5 * k, cx, cy, 1.2 * k, white);
            stroke_line(pm, cx, cy, cx + 3.5 * k, cy + 2.0 * k, 1.2 * k, white);
        }
        Page::Desktop => {
            let (x0, y0) = pt(3.0, 4.5);
            stroke_rounded_rect(pm, x0, y0, 14.0 * k, 11.0 * k, 2.0 * k, 1.1 * k, white);
            let (dx, dy) = pt(6.0, 12.0);
            fill_rounded_rect(pm, dx, dy, 8.0 * k, 1.8 * k, 0.9 * k, white);
        }
        Page::Defaults => {
            for row in 0..2 {
                for col in 0..2 {
                    let (cx, cy) = pt(4.0 + col as f32 * 7.0, 4.0 + row as f32 * 7.0);
                    fill_rounded_rect(pm, cx, cy, 5.0 * k, 5.0 * k, 1.3 * k, white);
                }
            }
        }
        Page::Privacy => {
            let mut b = tiny_skia::PathBuilder::new();
            let (tx, ty) = pt(10.0, 3.8);
            let (rx, ry) = pt(15.2, 5.8);
            let (rmx, rmy) = pt(15.0, 10.6);
            let (bx, by) = pt(10.0, 16.4);
            let (lmx, lmy) = pt(5.0, 10.6);
            let (lx, ly) = pt(4.8, 5.8);
            b.move_to(tx, ty);
            b.line_to(rx, ry);
            b.quad_to(rmx, rmy, bx, by);
            b.quad_to(lmx, lmy, lx, ly);
            b.close();
            if let Some(path) = b.finish() {
                let s = crate::theme::ui_scale();
                let mut paint = tiny_skia::Paint::default();
                paint.set_color(white);
                paint.anti_alias = true;
                let stroke = tiny_skia::Stroke { width: 1.25 * k, ..Default::default() };
                pm.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::from_scale(s, s), None);
            }
            let (p1x, p1y) = pt(7.8, 10.2);
            let (p2x, p2y) = pt(9.5, 11.9);
            let (p3x, p3y) = pt(12.5, 8.5);
            stroke_line(pm, p1x, p1y, p2x, p2y, 1.3 * k, white);
            stroke_line(pm, p2x, p2y, p3x, p3y, 1.3 * k, white);
        }
    }
}
/// Cheap layered shadow for sheets and menus: no blur pass, a few translucent rings.
fn soft_shadow(pm: &mut PixmapMut, r: R, radius: f32) {
    for i in 1..=4 {
        let e = i as f32 * 2.0;
        fill_rounded_rect(
            pm,
            r.x - e,
            r.y - e + 4.0,
            r.w + 2.0 * e,
            r.h + 2.0 * e,
            radius + e,
            rgba(0, 0, 0, 9),
        );
    }
}
fn draw_chevrons(pm: &mut PixmapMut, cx: f32, cy: f32, color: tiny_skia::Color) {
    stroke_line(pm, cx - 3.0, cy - 2.0, cx, cy - 5.0, 1.2, color);
    stroke_line(pm, cx, cy - 5.0, cx + 3.0, cy - 2.0, 1.2, color);
    stroke_line(pm, cx - 3.0, cy + 2.0, cx, cy + 5.0, 1.2, color);
    stroke_line(pm, cx, cy + 5.0, cx + 3.0, cy + 2.0, 1.2, color);
}
fn zone_matches(zone: &str, query: &str) -> bool {
    if zone.to_lowercase().contains(query) || zone.replace('_', " ").to_lowercase().contains(query)
    {
        return true;
    }
    let alias = match zone {
        "Europe/Moscow" => "москва московское",
        "Asia/Barnaul" => "барнаул алтай",
        "Asia/Novosibirsk" => "новосибирск",
        "Asia/Yekaterinburg" => "екатеринбург",
        "Asia/Krasnoyarsk" => "красноярск",
        "Asia/Irkutsk" => "иркутск",
        "Asia/Vladivostok" => "владивосток",
        "Europe/Kaliningrad" => "калининград",
        "Europe/Berlin" => "берлин",
        "UTC" | "Etc/UTC" => "всемирное время utc",
        _ => "",
    };
    alias.contains(query)
}
fn app_matches(app: &crate::system_preferences::DesktopApp, query: &str) -> bool {
    query.is_empty()
        || app.name.to_lowercase().contains(query)
        || app.id.to_lowercase().contains(query)
        || app.exec.to_lowercase().contains(query)
}
impl Panel {
    pub fn editing(&self) -> bool {
        (self.page == Page::DateTime && (self.time_editing || self.zone_picker))
            || (self.page == Page::Network && self.wifi_target.is_some())
            || (self.page == Page::Defaults && self.autostart_picker)
            || (self.page == Page::Privacy && self.privacy_picker)
    }
    pub fn edit_key(&mut self, sym: u32, ch: Option<char>, ctrl: bool, shift: bool) -> Option<Action> {
        use smithay::input::keyboard::keysyms;
        if self.page == Page::Network && self.wifi_target.is_some() {
            if sym == keysyms::KEY_Escape {
                self.wifi_target = None;
                self.wifi_password.clear();
                return None;
            }
            if matches!(sym, keysyms::KEY_Return | keysyms::KEY_KP_Enter) {
                return (!self.wifi_password.is_empty()).then_some(Action::WifiPasswordSubmit);
            }
            let utf8 = ch.filter(|c| !c.is_control()).map(|c| c.to_string());
            self.wifi_password = std::mem::take(&mut self.wifi_password).with_byte_limit(64);
            self.wifi_password.key(sym, utf8.as_deref(), ctrl, shift);
            return None;
        }
        if self.page == Page::Defaults && self.autostart_picker {
            if sym == keysyms::KEY_Escape {
                self.autostart_picker = false;
                self.autostart_query.clear();
                return None;
            }
            if matches!(sym, keysyms::KEY_Return | keysyms::KEY_KP_Enter) {
                return Some(Action::AutostartSearchSubmit);
            }
            let utf8 = ch.filter(|c| !c.is_control()).map(|c| c.to_string());
            self.autostart_query = std::mem::take(&mut self.autostart_query).with_limit(64);
            if self.autostart_query.key(sym, utf8.as_deref(), ctrl, shift)
                == feather_ui::input::Edit::Changed
            {
                self.scroll = 0.0;
            }
            return None;
        }
        if self.page == Page::Privacy && self.privacy_picker {
            if sym == keysyms::KEY_Escape {
                self.privacy_picker = false;
                self.privacy_query.clear();
                return None;
            }
            if matches!(sym, keysyms::KEY_Return | keysyms::KEY_KP_Enter) {
                return Some(Action::PrivacySearchSubmit);
            }
            let utf8 = ch.filter(|c| !c.is_control()).map(|c| c.to_string());
            self.privacy_query = std::mem::take(&mut self.privacy_query).with_limit(64);
            if self.privacy_query.key(sym, utf8.as_deref(), ctrl, shift)
                == feather_ui::input::Edit::Changed
            {
                self.scroll = 0.0;
            }
            return None;
        }
        if sym == keysyms::KEY_Escape {
            self.time_editing = false;
            self.zone_picker = false;
            return None;
        }
        if matches!(sym, keysyms::KEY_Return | keysyms::KEY_KP_Enter) {
            self.time_editing = false;
            return Some(if self.zone_picker {
                Action::ZoneSearchSubmit
            } else {
                Action::ApplyTime
            });
        }
        let query = self.zone_picker;
        let text = if query {
            &mut self.zone_query
        } else {
            &mut self.time_input
        };
        let utf8 = ch.filter(|c| !c.is_control() && (query || c.is_ascii_digit() || matches!(c, '-' | ':' | ' '))).map(|c| c.to_string());
        *text = std::mem::take(text).with_limit(if query { 64 } else { 16 });
        let edit = text.key(sym, utf8.as_deref(), ctrl, shift);
        if query && edit == feather_ui::input::Edit::Changed {
            self.scroll = 0.0;
        }
        None
    }
    fn controls(&self, w: f32, h: f32, c: &Context<'_>) -> Vec<Control> {
        let side = side_width(w);
        let x = side + 20.0;
        let cw = (w - x - 20.0).max(200.0);
        let mut l = Layout {
            x,
            cw,
            y: 20.0,
            out: Vec::new(),
            group: None,
            first: true,
        };
        let mut y = 18.0;
        for group in SIDEBAR {
            for &page in group {
                l.fixed(
                    R::new(15.0, y, side - 30.0, 30.0),
                    if page==Page::Power {"Блокировка и питание"}else{page.title()},
                    Style::Sidebar(page, self.page == page),
                    Some(Action::Page(page)),
                );
                y += 32.0;
            }
            y += 8.0;
        }
        // Right-side control of a row, vertically centred.
        let right = |r: R, w: f32, h: f32| R::new(r.x + r.w - 14.0 - w, r.y + (r.h - h) / 2.0, w, h);
        let kb = !self.keyboard_busy;
        match self.page {
            Page::Network => {
                use crate::controls::{network::Security, Control as Net};
                l.hero(Page::Network);
                if let Some(error) = &c.wifi.error {
                    l.footer(error);
                }
                l.header("Wi‑Fi");
                if !c.wifi.available {
                    l.footer("Адаптер Wi‑Fi или служба NetworkManager недоступны.");
                } else {
                    let status = if !c.wifi.hardware {
                        "Аппаратно заблокирован".into()
                    } else if !c.wifi.enabled {
                        "Выключен".into()
                    } else if c.wifi.connecting {
                        "Подключение…".into()
                    } else if let Some(active) = &c.wifi.connected {
                        format!("Подключено к «{active}»")
                    } else {
                        "Включён · не подключено".into()
                    };
                    l.begin();
                    l.row(
                        52.0,
                        "Wi‑Fi",
                        status,
                        Style::Toggle(c.wifi.enabled),
                        c.wifi.hardware.then_some(Action::Network(Net::Wifi(!c.wifi.enabled))),
                    );
                    if c.wifi.enabled && c.wifi.hardware {
                        l.row(
                            48.0,
                            "Поиск сетей",
                            "Обновить список доступных точек доступа".into(),
                            Style::Row,
                            Some(Action::Network(Net::Scan)),
                        );
                        for n in &c.wifi.networks {
                            let state = if n.active {
                                "Подключено"
                            } else if n.saved.is_some() {
                                "Сохранена"
                            } else {
                                "Доступна"
                            };
                            let detail = format!("{state} · {} · {} %", n.security.label(), n.strength);
                            let action = if n.active {
                                Some(Action::Network(Net::Disconnect(n.device.clone())))
                            } else if n.saved.is_some() || matches!(n.security, Security::Open | Security::Owe) {
                                Some(Action::Network(Net::Connect {
                                    network: n.clone(),
                                    password: None,
                                }))
                            } else if n.security.password() {
                                Some(Action::WifiPassword(n.clone()))
                            } else {
                                Some(Action::OpenConnectionEditor)
                            };
                            l.row(52.0, &n.label, detail, Style::Row, action);
                            l.out.last_mut().unwrap().value = if n.active {
                                "Отключить".into()
                            } else {
                                "Подключить".into()
                            };
                        }
                    }
                    l.end();
                    if c.wifi.enabled && c.wifi.hardware && c.wifi.networks.is_empty() {
                        l.footer("Доступные сети Wi‑Fi не найдены.");
                    }
                }
                l.header("Сохранённые сети Wi‑Fi");
                if c.wifi.saved_wifi.is_empty() {
                    l.footer("Сохранённых профилей Wi‑Fi пока нет.");
                } else {
                    l.begin();
                    for s in &c.wifi.saved_wifi {
                        let state = if s.active {
                            "Подключено"
                        } else if s.in_range {
                            "В зоне действия"
                        } else {
                            "Вне зоны действия"
                        };
                        let auto = if s.autoconnect { "Автоподключение" } else { "Вручную" };
                        let detail = format!("{state} · {} · {auto}", s.security.label());
                        let r = l.row(54.0, &s.label, detail, Style::Row, None);
                        let forget = right(r, 76.0, 26.0);
                        let auto_btn = R::new(forget.x - 6.0 - 86.0, forget.y, 86.0, 26.0);
                        l.push(
                            auto_btn,
                            if s.autoconnect { "Авто" } else { "Вручную" },
                            Style::Push(s.autoconnect),
                            Some(Action::Network(Net::SetAutoconnect {
                                profile: s.profile.clone(),
                                autoconnect: !s.autoconnect,
                            })),
                        );
                        l.push(
                            forget,
                            "Забыть",
                            Style::Push(false),
                            Some(Action::Network(Net::ForgetConnection(s.profile.clone()))),
                        );
                    }
                    l.end();
                    l.footer("«Авто» разрешает подключаться к сети автоматически. «Забыть» удаляет профиль и пароль из NetworkManager.");
                }
                l.header("Ethernet");
                if c.wifi.ethernets.is_empty() {
                    l.footer("Проводные сетевые адаптеры не найдены.");
                } else {
                    l.begin();
                    for e in &c.wifi.ethernets {
                        let status = if e.active {
                            "Подключено"
                        } else if e.connecting {
                            "Подключение…"
                        } else if e.carrier {
                            "Кабель подключён"
                        } else {
                            "Кабель не подключён"
                        };
                        let detail = if e.speed > 0 {
                            format!("{} · {status} · {} Мбит/с", e.interface, e.speed)
                        } else {
                            format!("{} · {status}", e.interface)
                        };
                        let action = if e.active || e.connecting {
                            Some(Action::Network(Net::Disconnect(e.device.clone())))
                        } else if e.carrier {
                            Some(Action::Network(Net::ConnectEthernet {
                                device: e.device.clone(),
                                profile: e.profile.clone(),
                            }))
                        } else {
                            None
                        };
                        l.row(52.0, &e.label, detail, Style::Row, action);
                        l.out.last_mut().unwrap().value = if e.active || e.connecting {
                            "Отключить".into()
                        } else if e.carrier {
                            "Подключить".into()
                        } else {
                            "Нет сигнала".into()
                        };
                    }
                    l.end();
                }
                l.header("VPN");
                if c.wifi.vpns.is_empty() {
                    l.footer("Профили VPN и WireGuard не настроены.");
                } else {
                    l.begin();
                    for v in &c.wifi.vpns {
                        let status = if v.active {
                            "Подключено"
                        } else if v.connecting {
                            "Подключение…"
                        } else {
                            "Отключено"
                        };
                        let detail = format!("{} · {status}", v.kind);
                        let on = v.active || v.connecting;
                        let action = if on {
                            v.active_path
                                .as_ref()
                                .map(|p| Action::Network(Net::DisconnectVpn(p.clone())))
                        } else {
                            Some(Action::Network(Net::ConnectVpn(v.profile.clone())))
                        };
                        l.row(52.0, &v.label, detail, Style::Toggle(on), action);
                    }
                    l.end();
                }
                l.begin();
                l.row(
                    48.0,
                    "Обновить состояние сети",
                    String::new(),
                    Style::Row,
                    Some(Action::Network(Net::RefreshNetwork)),
                );
                l.row(
                    52.0,
                    "Расширенные настройки сети…",
                    "Сертификаты 802.1X, маршруты и импорт профилей VPN".into(),
                    Style::Row,
                    Some(Action::OpenConnectionEditor),
                );
                l.end();
                if let Some(net) = &self.wifi_target {
                    let sw = (w - 120.0).min(420.0);
                    let sheet = R::new((w - sw) / 2.0, 48.0, sw, 146.0);
                    l.fixed(
                        R::new(0.0, -100.0, w, h + 200.0),
                        "",
                        Style::Scrim(true),
                        Some(Action::WifiPasswordCancel),
                    );
                    l.fixed(sheet, &format!("Пароль для «{}»", net.label), Style::Sheet, None).detail =
                        net.security.label().into();
                    l.fixed(
                        R::new(sheet.x + 20.0, sheet.y + 52.0, sw - 40.0, 28.0),
                        "Введите пароль сети Wi‑Fi",
                        Style::TextField(true),
                        Some(Action::WifiPasswordFocus),
                    );
                    l.fixed(
                        R::new(sheet.x + sw - 220.0, sheet.y + sheet.h - 42.0, 96.0, 28.0),
                        "Отмена",
                        Style::Push(false),
                        Some(Action::WifiPasswordCancel),
                    );
                    l.fixed(
                        R::new(sheet.x + sw - 116.0, sheet.y + sheet.h - 42.0, 96.0, 28.0),
                        "Подключить",
                        Style::Push(true),
                        (!self.wifi_password.is_empty()).then_some(Action::WifiPasswordSubmit),
                    );
                }
            }
            Page::Bluetooth => {
                use crate::controls::Control as Bt;
                l.hero(Page::Bluetooth);
                if let Some(error) = &c.bluetooth.error {
                    l.footer(error);
                }
                if !c.bluetooth.available {
                    l.footer("Адаптер Bluetooth или служба BlueZ недоступны.");
                } else {
                    let connected_names: Vec<&str> = c
                        .bluetooth
                        .devices
                        .iter()
                        .filter(|d| d.connected)
                        .map(|d| d.name.as_str())
                        .collect();
                    let status = if !c.bluetooth.powered {
                        "Выключен".into()
                    } else if c.bluetooth.connecting.is_some() {
                        "Подключение…".into()
                    } else if connected_names.len() == 1 {
                        format!("Подключено: «{}»", connected_names[0])
                    } else if !connected_names.is_empty() {
                        format!("Подключено устройств: {}", connected_names.len())
                    } else if c.bluetooth.discovering {
                        "Включён · идёт поиск устройств…".into()
                    } else {
                        "Включён · доступен для подключения".into()
                    };
                    l.begin();
                    l.row(
                        52.0,
                        "Bluetooth",
                        status,
                        Style::Toggle(c.bluetooth.powered),
                        Some(Action::Bluetooth(Bt::Bluetooth(!c.bluetooth.powered))),
                    );
                    if c.bluetooth.powered {
                        l.row(
                            48.0,
                            if c.bluetooth.discovering {
                                "Остановить поиск устройств"
                            } else {
                                "Поиск устройств"
                            },
                            if c.bluetooth.discovering {
                                "Сканирование эфира активно".into()
                            } else {
                                "Найти наушники, мыши, клавиатуры и другие аксессуары".into()
                            },
                            Style::Row,
                            Some(Action::Bluetooth(Bt::ScanBluetooth)),
                        );
                    }
                    l.end();

                    if c.bluetooth.powered {
                        let my_devices: Vec<_> = c
                            .bluetooth
                            .devices
                            .iter()
                            .filter(|d| d.paired || d.connected)
                            .collect();
                        l.header("Мои устройства");
                        if my_devices.is_empty() {
                            l.footer("Сопряжённых устройств Bluetooth пока нет.");
                        } else {
                            l.begin();
                            for d in my_devices {
                                let state = if d.connected {
                                    "Подключено"
                                } else if c.bluetooth.connecting.as_deref() == Some(d.path.as_str()) {
                                    "Подключение…"
                                } else {
                                    "Сопряжено"
                                };
                                let mut detail = format!("{} · {state}", d.kind_label());
                                if let Some(b) = d.battery {
                                    detail.push_str(&format!(" · Заряд {b} %"));
                                }
                                let r = l.row(56.0, &d.name, detail, Style::Row, None);
                                let forget = right(r, 76.0, 26.0);
                                let toggle = R::new(forget.x - 6.0 - 106.0, forget.y, 106.0, 26.0);
                                l.push(
                                    toggle,
                                    if d.connected { "Отключить" } else { "Подключить" },
                                    Style::Push(!d.connected),
                                    Some(Action::Bluetooth(if d.connected {
                                        Bt::DisconnectBluetooth(d.path.clone())
                                    } else {
                                        Bt::ConnectBluetooth(d.path.clone())
                                    })),
                                );
                                l.push(
                                    forget,
                                    "Забыть",
                                    Style::Push(false),
                                    Some(Action::Bluetooth(Bt::ForgetBluetooth(d.path.clone()))),
                                );
                            }
                            l.end();
                        }

                        let other_devices: Vec<_> = c
                            .bluetooth
                            .devices
                            .iter()
                            .filter(|d| !d.paired && !d.connected)
                            .collect();
                        l.header("Другие устройства");
                        if other_devices.is_empty() {
                            l.footer(if c.bluetooth.discovering {
                                "Переведите аксессуар в режим сопряжения — он появится здесь."
                            } else {
                                "Нажмите «Поиск устройств», чтобы обнаружить новые аксессуары рядом."
                            });
                        } else {
                            l.begin();
                            for d in other_devices {
                                let busy = c.bluetooth.connecting.as_deref() == Some(d.path.as_str());
                                let detail = if busy {
                                    format!("{} · Сопряжение и подключение…", d.kind_label())
                                } else {
                                    format!("{} · Доступно для подключения", d.kind_label())
                                };
                                l.row(
                                    52.0,
                                    &d.name,
                                    detail,
                                    Style::Row,
                                    Some(Action::Bluetooth(Bt::ConnectBluetooth(d.path.clone()))),
                                );
                                l.out.last_mut().unwrap().value = if busy {
                                    "Подключение…".into()
                                } else {
                                    "Подключить".into()
                                };
                            }
                            l.end();
                        }
                    }
                }
                l.begin();
                l.row(
                    48.0,
                    "Обновить состояние Bluetooth",
                    String::new(),
                    Style::Row,
                    Some(Action::Bluetooth(Bt::RefreshBluetooth)),
                );
                l.row(
                    52.0,
                    "Расширенный диспетчер Bluetooth…",
                    "Профили кодеков A2DP, передача файлов OBEX и диагностика".into(),
                    Style::Row,
                    Some(Action::OpenBluetoothManager),
                );
                l.end();
            }
            Page::Sound => {
                use crate::controls::Control as Audio;
                l.hero(Page::Sound);
                l.header("Сигналы и звуки интерфейса");
                l.begin();
                l.row(52.0,"Звуки предупреждений","Системный сигнал и запросы программ".into(),Style::Toggle(c.sounds.alerts),Some(Action::AlertSounds));
                l.row(52.0,"Сигнал предупреждения",c.sounds.tone.label().into(),Style::Row,Some(Action::AlertTone));
                let r=l.row(82.0,"Громкость сигналов",format!("{} % от громкости устройства",c.sounds.volume),Style::Row,None);
                for (i,volume) in [0,25,50,75,100].into_iter().enumerate() {
                    let width=(r.w-28.0)/5.0;
                    l.push(R::new(r.x+14.0+i as f32*width,r.y+48.0,width,26.0),&format!("{volume} %"),Style::Segment(c.sounds.volume==volume),Some(Action::AlertVolume(volume)));
                }
                l.row(44.0,"Прослушать сигнал",String::new(),Style::Row,Some(Action::PreviewAlert));
                l.row(52.0,"Звуки интерфейса","Короткий звук после снимка экрана".into(),Style::Toggle(c.sounds.interface),Some(Action::InterfaceSounds));
                l.row(52.0,"Звук при изменении громкости","Отклик клавиш громкости, кроме выключенного звука".into(),Style::Toggle(c.sounds.volume_feedback),Some(Action::VolumeFeedback));
                l.row(52.0,"Звук уведомлений","Короткий сигнал при появлении баннера".into(),Style::Toggle(c.notify_sounds),Some(Action::NotifySounds));
                l.end();
                l.footer("«Не беспокоить» отключает автоматические сигналы. Прослушивание доступно при выключенных предупреждениях; уведомления настраиваются отдельно.");
                for input in [true, false] {
                    l.header(if input { "Микрофон" } else { "Вывод звука" });
                    let audio = &c.audio;
                    let (devices, default, current, error) = if input {
                        (&audio.sources, &audio.default_source, audio.current_source(), &audio.input_error)
                    } else { (&audio.sinks, &audio.default, audio.current(), &audio.error) };
                    if let Some(error) = error { l.footer(error); }
                    if devices.is_empty() {
                        l.footer(if input { "Микрофоны не найдены. Подключите устройство записи." } else { "Устройства вывода не найдены." });
                    } else {
                        l.begin();
                        for device in devices {
                            let selected = device.name == *default;
                            let detail = match (selected, device.unplugged) {
                                (true, true) => "Выбрано · разъём не подключён",
                                (false, true) => "Разъём не подключён",
                                (true, false) => "Выбрано",
                                (false, false) => "Нажмите, чтобы выбрать",
                            };
                            l.row(52.0, &device.label, detail.into(), Style::Row,
                                Some(Action::Audio(if input { Audio::Input(device.name.clone()) } else { Audio::Output(device.name.clone()) })));
                        }
                        l.end();
                        if let Some(device) = current {
                            l.begin();
                            let r = l.row(94.0, if input { "Уровень записи" } else { "Громкость" },
                                format!("{} %", (self.audio_volume(device, input)*100.0).round() as u32), Style::Row, None);
                            l.push(R::new(r.x+14.0, r.y+55.0, r.w-28.0, 26.0), "", Style::Volume(self.audio_volume(device, input).clamp(0.0, 1.0)),
                                Some(Action::Audio(if input { Audio::InputVolume { source: device.name.clone(), value: 0.0 } }
                                    else { Audio::Volume { sink: device.name.clone(), value: 0.0 } })));
                            l.row(48.0, if input { "Выключить микрофон" } else { "Выключить звук" }, String::new(), Style::Toggle(device.muted),
                                Some(Action::Audio(if input { Audio::InputMute { source: device.name.clone(), muted: !device.muted } }
                                    else { Audio::Mute { sink: device.name.clone(), muted: !device.muted } })));
                            l.end();
                        } else { l.footer("Устройство по умолчанию недоступно. Выберите устройство из списка."); }
                    }
                }
                l.footer("Уровень записи задаёт усиление микрофона. Изменение уровня сохраняет баланс каналов.");
                l.begin();
                l.row(48.0, "Обновить устройства", String::new(), Style::Row, Some(Action::Audio(Audio::RefreshAudio)));
                l.end();
            }
            Page::Power => {
                l.hero(Page::Power);
                l.begin();
                l.row(52.0,"Заблокировать сейчас","Super + L · пароль учётной записи".into(),Style::Row,Some(Action::LockNow));
                l.end();
                l.header("При отсутствии активности");
                l.begin();
                for (index,title,value) in [(0,"Блокировать экран",c.power.lock),(1,"Выключать экран",c.power.blank),(2,"Переводить компьютер в сон",c.power.suspend)] {
                    l.row(58.0,title,crate::power::Preferences::label(value),Style::Row,Some(Action::IdleTime(index)));
                }
                l.end();
                l.footer("Нажмите на строку, чтобы выбрать время. Выключение экрана и сон всегда требуют пароль при возвращении.");
                l.footer(if c.direct && c.power_ready {"После пробуждения экран блокируется автоматически. Видео в видимом окне может отложить действия по простою."} else if c.direct {"Служба питания недоступна. Блокировка работает; автоматический сон пока недоступен."} else {"В оконном режиме гаснет только окно Пёрышка. Сон компьютера доступен в прямой сессии DRM."});
            }
            Page::Notifications => {
                l.hero(Page::Notifications);
                l.begin();
                l.row(52.0, "Не беспокоить", "Всё уходит в центр уведомлений, баннер только у срочных".into(), Style::Toggle(c.dnd), Some(Action::DoNotDisturb));
                l.row(52.0, "По расписанию", "Каждый день в выбранные часы".into(), Style::Toggle(c.dnd_schedule.is_some()), Some(Action::DndSchedule));
                if let Some(window) = c.dnd_schedule {
                    l.row(52.0, "Время", crate::shell::dnd_window_label(window), Style::Row, Some(Action::DndWindow));
                }
                l.row(52.0, "Звук уведомлений", "Короткий сигнал при появлении баннера".into(), Style::Toggle(c.notify_sounds), Some(Action::NotifySounds));
                l.end();
                l.header("Уведомления программ");
                if c.notify_apps.is_empty() {
                    l.footer("Программы появятся здесь после первого уведомления.");
                } else {
                    l.begin();
                    for (i, a) in c.notify_apps.iter().enumerate() {
                        l.row(52.0, &a.label, a.delivery.label().into(), Style::Row, Some(Action::NotifyDelivery(i)));
                    }
                    l.end();
                    l.footer("Нажмите на строку, чтобы сменить способ показа. Выключенные уведомления не сохраняются.");
                }
            }
            Page::Appearance => {
                l.hero(Page::Appearance);
                l.begin();
                let r = l.row(112.0, "Оформление", String::new(), Style::Row, None);
                for (i, a) in [Appearance::Light, Appearance::Dark].into_iter().enumerate() {
                    l.push(
                        R::new(r.x + r.w - 14.0 - (2 - i) as f32 * 102.0 + 10.0, r.y + 14.0, 92.0, 84.0),
                        a.name(),
                        Style::Preview(a == Appearance::Dark, appearance() == a),
                        Some(Action::Appearance(a)),
                    );
                }
                let current_accent = crate::theme::accent_color();
                let r = l.row(54.0, "Цвет акцента", current_accent.label().into(), Style::Row, None);
                let bar = right(r, 8.0 * 27.0 - 3.0, 24.0);
                for (i, color) in crate::theme::AccentColor::ALL.into_iter().enumerate() {
                    l.push(
                        R::new(bar.x + i as f32 * 27.0, bar.y, 24.0, 24.0),
                        color.label(),
                        Style::ColorSwatch {
                            color: Some(color),
                            selected: current_accent == color,
                        },
                        Some(Action::AccentColor(color)),
                    );
                }
                let current_hl = crate::theme::highlight_choice();
                let r = l.row(
                    54.0,
                    "Цвет выделения",
                    current_hl.map_or("Как цвет акцента", |c| c.label()).into(),
                    Style::Row,
                    None,
                );
                let bar = right(r, 9.0 * 26.0 - 2.0, 24.0);
                for (i, choice) in std::iter::once(None)
                    .chain(crate::theme::AccentColor::ALL.into_iter().map(Some))
                    .enumerate()
                {
                    l.push(
                        R::new(bar.x + i as f32 * 26.0, bar.y, 24.0, 24.0),
                        choice.map_or("Как цвет акцента", |c| c.label()),
                        Style::ColorSwatch {
                            color: choice,
                            selected: current_hl == choice,
                        },
                        Some(Action::HighlightColor(choice)),
                    );
                }
                let current_ts = crate::theme::text_size();
                let r = l.row(
                    54.0,
                    "Размер текста интерфейса",
                    format!("{} ({} %)", current_ts.label(), current_ts.percent()),
                    Style::Row,
                    None,
                );
                let track = right(r, 4.0 * 72.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, ts) in crate::theme::TextSize::ALL.into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * 72.0, track.y, 72.0, 28.0),
                        ts.short_label(),
                        Style::Segment(current_ts == ts),
                        Some(Action::TextSize(ts)),
                    );
                }
                l.row(
                    52.0,
                    "Анимации интерфейса",
                    "Появление окон, меню и запуск приложений".into(),
                    Style::Toggle(c.animations),
                    Some(Action::Animations),
                );
                l.row(
                    52.0,
                    "Программы GTK в стиле Пёрышка",
                    "Светофоры и заголовки в Firefox и программах GTK 3/4".into(),
                    Style::Toggle(crate::gtk_theme::enabled()),
                    Some(Action::GtkTheme),
                );
                l.end();
                l.footer("Цвета и размер текста сразу применяются к системным панелям, Файлам и Заметкам.");
            }
            Page::Accessibility => {
                l.hero(Page::Accessibility);
                l.header("Зрение");
                l.begin();
                let current_ts = crate::theme::text_size();
                let r = l.row(
                    54.0,
                    "Размер текста интерфейса",
                    format!("{} ({} %)", current_ts.label(), current_ts.percent()),
                    Style::Row,
                    None,
                );
                let track = right(r, 4.0 * 72.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, ts) in crate::theme::TextSize::ALL.into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * 72.0, track.y, 72.0, 28.0),
                        ts.short_label(),
                        Style::Segment(current_ts == ts),
                        Some(Action::TextSize(ts)),
                    );
                }
                l.row(
                    52.0,
                    "Увеличить контраст",
                    "Чёткие границы карточек, панелей, разделителей и подписей".into(),
                    Style::Toggle(crate::theme::high_contrast()),
                    Some(Action::HighContrast),
                );
                l.row(
                    52.0,
                    "Уменьшить прозрачность",
                    "Сплошной непрозрачный фон меню, Dock, Центра управления и окон".into(),
                    Style::Toggle(crate::theme::reduce_transparency()),
                    Some(Action::ReduceTransparency),
                );
                l.end();
                l.footer("Применяется сразу к панелям, окнам, Файлам и Заметкам.");
                l.header("Клавиатура и ввод");
                l.begin();
                l.row(
                    52.0,
                    "Залипание клавиш",
                    "Shift, Ctrl, Alt и Super удерживаются для следующей клавиши".into(),
                    Style::Toggle(c.sticky_keys),
                    Some(Action::StickyKeys),
                );
                l.end();
                l.footer("Позволяет нажимать сочетания клавиш последовательно. Повторное нажатие модификатора снимает фиксацию.");
            }
            Page::Language => {
                l.hero(Page::Language);
                l.header("Язык интерфейса");
                l.begin();
                let current_lang = crate::theme::lang();
                let r = l.row(
                    82.0,
                    "Основной язык",
                    current_lang.full_label().into(),
                    Style::Row,
                    None,
                );
                let track = R::new(r.x + 14.0, r.y + 46.0, r.w - 28.0, 28.0);
                l.push(track, "", Style::Track, None);
                let sw = track.w / crate::theme::Lang::ALL.len() as f32;
                for (i, lang) in crate::theme::Lang::ALL.into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * sw, track.y, sw, track.h),
                        lang.native_name(),
                        Style::Segment(current_lang == lang),
                        Some(Action::Language(lang)),
                    );
                }
                l.end();
                l.footer("Применяется сразу к строке меню, Dock, Центру управления, Настройкам, Файлам и Заметкам.");
                l.header("Регион и форматы");
                l.begin();
                let current_region = crate::theme::region();
                let r = l.row(48.0, "Регион", String::new(), Style::Row, None);
                l.push(R::new(r.x, r.y, r.w - 64.0, r.h), "", Style::Row, None).value =
                    current_region.label().into();
                for (i, label, dir) in [(0, "‹", -1), (1, "›", 1)] {
                    l.push(
                        R::new(r.x + r.w - 70.0 + i as f32 * 28.0, r.y + 12.0, 26.0, 24.0),
                        label,
                        Style::Mini,
                        Some(Action::RegionStep(dir)),
                    );
                }
                let use_24h = crate::theme::use_24h_time();
                l.row(
                    48.0,
                    "24‑часовой формат времени",
                    "Часы в строке меню и на экране блокировки".into(),
                    Style::Toggle(use_24h),
                    Some(Action::Use24HourTime),
                );
                let monday = crate::theme::first_weekday_monday();
                let r = l.row(48.0, "Первый день недели", String::new(), Style::Row, None);
                let track = right(r, 2.0 * 116.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, (label, is_monday)) in [("Понедельник", true), ("Воскресенье", false)].into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * 116.0, track.y, 116.0, 28.0),
                        label,
                        Style::Segment(monday == is_monday),
                        Some(Action::FirstWeekdayMonday(is_monday)),
                    );
                }
                let metric = crate::theme::metric_units();
                let r = l.row(48.0, "Единицы измерения", String::new(), Style::Row, None);
                let track = right(r, 2.0 * 116.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, (label, is_metric)) in [("Метрические", true), ("США (дюймы, °F)", false)].into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * 116.0, track.y, 116.0, 28.0),
                        label,
                        Style::Segment(metric == is_metric),
                        Some(Action::MetricUnits(is_metric)),
                    );
                }
                l.row(
                    48.0,
                    "Пример форматирования",
                    "Числа, валюта, дата и температура".into(),
                    Style::Row,
                    None,
                );
                l.out.last_mut().unwrap().value =
                    current_region.format_preview(current_lang, use_24h, monday, metric);
                l.end();
                l.footer("Язык и региональные стандарты также передаются программам, запускаемым в сессии Пёрышка.");
            }
            Page::Keyboard => {
                l.hero(Page::Keyboard);
                l.header("Источники ввода");
                l.begin();
                for (i, s) in c.keyboard.sources().into_iter().enumerate() {
                    l.row(
                        38.0,
                        s.label,
                        String::new(),
                        Style::Source {
                            badge: s.badge,
                            selected: i == c.active,
                        },
                        kb.then_some(Action::SelectSource(i)),
                    );
                }
                let r = l.row(32.0, "", String::new(), Style::Row, None);
                l.out.last_mut().unwrap().value = format!("{} из 4", c.keyboard.layouts.len());
                l.push(
                    R::new(r.x + 8.0, r.y + 4.0, 26.0, 24.0),
                    "+",
                    Style::Mini,
                    (kb && c.keyboard.layouts.len() < 4).then_some(Action::AddPicker(true)),
                );
                l.push(
                    R::new(r.x + 36.0, r.y + 4.0, 26.0, 24.0),
                    "−",
                    Style::Mini,
                    (kb && c.keyboard.layouts.len() > 1).then_some(Action::RemoveSource(c.active)),
                );
                l.end();
                l.footer("Выбранный источник становится активным; «−» удаляет выбранный.");
                l.header("Сочетания клавиш");
                l.begin();
                let seg = 98.0;
                let wide = cw - 28.0 - 3.0 * seg >= 150.0;
                let r = l.row(
                    if wide { 48.0 } else { 84.0 },
                    "Переключение раскладки",
                    String::new(),
                    Style::Row,
                    None,
                );
                let track = if wide {
                    right(r, 3.0 * seg, 28.0)
                } else {
                    R::new(r.x + 14.0, r.y + 44.0, r.w - 28.0, 28.0)
                };
                l.push(track, "", Style::Track, None);
                let sw = track.w / 3.0;
                for (i, shortcut) in Shortcut::ALL.into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * sw, track.y, sw, track.h),
                        shortcut.label(),
                        Style::Segment(c.keyboard.shortcut == shortcut),
                        kb.then_some(Action::Shortcut(shortcut)),
                    );
                }
                l.end();
                l.footer("Сменить язык можно также в меню источников ввода в строке меню.");
                l.header("Клавиши");
                l.begin();
                for (title, values, current, labels, make) in [
                    ("Задержка перед повтором", crate::settings::REPEAT_DELAYS, c.keyboard.repeat_delay, ["Долго", "", "", "", "Коротко"], Action::RepeatDelay as fn(u32) -> Action),
                    ("Скорость повтора", crate::settings::REPEAT_RATES, c.keyboard.repeat_rate, ["Медленно", "", "", "", "Быстро"], Action::RepeatRate as fn(u32) -> Action),
                ] {
                    let r = l.row(84.0, title, format!("{}  ·  {}", labels[0], labels[4]), Style::Row, None);
                    let track = R::new(r.x + 14.0, r.y + 46.0, r.w - 28.0, 28.0);
                    l.push(track, "", Style::Track, None);
                    let sw = track.w / values.len() as f32;
                    for (i, v) in values.into_iter().enumerate() {
                        let label = if title.starts_with("Задержка") { format!("{v} мс") } else { format!("{v}/с") };
                        l.push(R::new(track.x + i as f32 * sw, track.y, sw, track.h), &label, Style::Segment(current == v), Some(make(v)));
                    }
                }
                l.row(52.0, "Клавиша Caps Lock", c.keyboard.caps.label().into(), Style::Row, kb.then_some(Action::CapsLock));
                l.end();
                l.footer("Автоповтор применяется сразу во всех программах. Нажмите на строку Caps Lock, чтобы выбрать её действие.");
                l.header("Сочетания действий");
                l.begin();
                for action in crate::shortcuts::Action::ALL.iter().copied() {
                    let value = if self.shortcut_capture == Some(action) { "Нажмите сочетание…".into() }
                        else { self.keybindings.chord(action).map_or("Выключено".into(), |c| c.label()) };
                    l.row(48.0, action.label(), value, Style::Row, Some(Action::EditShortcut(action)));
                }
                l.row(48.0, "Вернуть стандартные сочетания", String::new(), Style::Row, Some(Action::ResetShortcuts));
                l.end();
                l.footer("Нажмите на действие, затем сочетание с Ctrl, Alt или Super. Esc — отмена; Backspace — выключить. Super+L, Alt+Tab и системные клавиши сохраняются.");
            }
            Page::Pointer => {
                let p = &c.pointer;
                l.hero(Page::Pointer);
                if !c.pointer_managed {
                    let r = R::new(x, l.y, cw, 58.0);
                    l.push(r, "Мышью сейчас управляет внешний композитор", Style::Banner(false), None).detail =
                        "Пёрышко запущено в окне. Настройки сохранятся и применятся при входе в сессию Пёрышка.".into();
                    l.y += 58.0 + 16.0;
                }
                l.header("Курсор");
                l.begin();
                let r = l.row(48.0, "Тема", String::new(), Style::Row, None);
                let anchor = right(r, (cw * 0.5).min(240.0), 26.0);
                let theme = theme_label(&crate::cursor::theme_name(p.cursor_theme.as_deref()));
                l.push(anchor, &theme, Style::Popup, Some(Action::CursorPicker));
                let r = l.row(48.0, "Размер", String::new(), Style::Row, None);
                let track = right(r, 4.0 * 64.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, size) in crate::settings::CURSOR_SIZES.into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * 64.0, track.y, 64.0, 28.0),
                        &size.to_string(),
                        Style::Segment(p.cursor_size == size),
                        Some(Action::CursorSize(size)),
                    );
                }
                l.end();
                l.footer("Тема и размер применяются к курсору Пёрышка и к программам, открытым после изменения.");
                l.header("Мышь и трекпад");
                l.begin();
                let r = l.row(86.0, "Скорость перемещения", String::new(), Style::Row, None);
                l.push(
                    R::new(r.x + 14.0, r.y + 40.0, r.w - 28.0, 34.0),
                    "",
                    Style::Slider(((p.speed + 1.0) / 2.0) as f32),
                    Some(Action::PointerSpeed(p.speed)),
                );
                for (label, detail, on, action) in [
                    ("Естественная прокрутка мыши", "Содержимое следует за колесом, как на трекпаде", p.natural_mouse, Action::NaturalMouse),
                    ("Естественная прокрутка трекпада", "Содержимое следует за движением пальцев", p.natural_touchpad, Action::NaturalTouchpad),
                    ("Касание вместо нажатия", "Лёгкое касание трекпада — щелчок", p.tap_to_click, Action::TapToClick),
                    ("Основная кнопка справа", "Для левой руки", p.left_handed, Action::LeftHanded),
                ] {
                    l.row(52.0, label, detail.into(), Style::Toggle(on), Some(action));
                }
                l.end();

                if self.cursor_picker {
                    let themes = c.system.cursor_themes.clone();
                    let mut items: Vec<(String, Option<String>, bool)> = vec![(
                        "Как в системе".into(),
                        None,
                        p.cursor_theme.is_none(),
                    )];
                    items.extend(themes.iter().map(|t| (theme_label(t), Some(t.clone()), p.cursor_theme.as_deref() == Some(t.as_str()))));
                    let mh = items.len().min(10) as f32 * 26.0 + 10.0;
                    let mut my = anchor.y + anchor.h + 4.0 - self.scroll;
                    if my + mh > h - 8.0 {
                        my = (anchor.y - self.scroll - 4.0 - mh).max(TOOLBAR_CLIP);
                    }
                    l.fixed(R::new(0.0, -100.0, w, h + 200.0), "", Style::Scrim(false), Some(Action::CursorPicker));
                    let menu = R::new(anchor.x, my, anchor.w, mh);
                    l.fixed(menu, "", Style::Menu, None);
                    for (i, (label, value, checked)) in items.into_iter().take(10).enumerate() {
                        l.fixed(
                            R::new(menu.x + 4.0, my + 5.0 + i as f32 * 26.0, menu.w - 8.0, 26.0),
                            &label,
                            Style::MenuItem(checked),
                            Some(Action::CursorTheme(value)),
                        );
                    }
                }
            }
            Page::Displays => {
                let r = R::new(x, l.y, cw, 168.0);
                let d = l.push(r, c.display_name, Style::Display, None);
                d.detail = c.current.label();
                l.y += 168.0 + 20.0;
                if c.monitors.len()>1 {
                    l.begin();
                    for m in &c.monitors {
                        let detail=format!("{} × {} · ({}, {}){}",m.rect.w,m.rect.h,m.rect.x,m.rect.y,if m.primary {" · Основной"}else{""});
                        let r=l.row(if m.primary{58.0}else{96.0},&m.name,detail,Style::Row,None);
                        if !m.primary {
                            l.push(R::new(r.x+r.w-116.0,r.y+10.0,105.0,25.0),"Основной",Style::Push(false),(!self.confirming).then(||Action::PrimaryMonitor(m.name.clone())));
                            for (i,label) in ["Слева","Справа","Сверху","Снизу"].iter().enumerate(){
                                l.push(R::new(r.x+14.0+i as f32*76.0,r.y+59.0,70.0,25.0),label,Style::Mini,(!self.confirming).then(||Action::PositionMonitor(m.name.clone(),i as u8)));
                            }
                        }
                    }
                    l.end();l.y+=12.0;
                    l.footer("Меню и Dock — на основном экране. Масштаб общий для всех дисплеев.");
                }

                l.begin();
                let r = l.row(
                    94.0,
                    "Масштаб интерфейса",
                    "Крупнее  ·  Больше места".into(),
                    Style::Row,
                    None,
                );
                let track = R::new(r.x + 14.0, r.y + 54.0, r.w - 28.0, 28.0);
                l.push(track, "", Style::Track, None);
                let scales = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5];
                let sw = track.w / scales.len() as f32;
                for (i, scale) in scales.into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * sw, track.y, sw, track.h),
                        &format!("{}%", (scale * 100.0) as i32),
                        Style::Segment((c.scale - scale).abs() < 0.01),
                        (!self.confirming).then_some(Action::Scale(scale)),
                    );
                }
                let selected = self
                    .selected_mode
                    .and_then(|i| c.modes.get(i))
                    .copied()
                    .unwrap_or(c.current);
                if c.direct {
                    let r = l.row(48.0, "Режим основного дисплея", String::new(), Style::Row, None);
                    // Value sits left of the steppers; the row keeps its full-width divider.
                    l.push(R::new(r.x, r.y, r.w - 64.0, r.h), "", Style::Row, None).value =
                        selected.label();
                    for (i, label, dir) in [(0, "‹", -1), (1, "›", 1)] {
                        l.push(
                            R::new(r.x + r.w - 70.0 + i as f32 * 28.0, r.y + 12.0, 26.0, 24.0),
                            label,
                            Style::Mini,
                            (!self.confirming && c.modes.len() > 1).then_some(Action::ModeStep(dir)),
                        );
                    }
                    l.end();
                    let r = R::new(x + cw - 170.0, l.y, 170.0, 28.0);
                    l.push(
                        r,
                        "Применить режим",
                        Style::Push(false),
                        (!self.confirming && selected != c.current).then_some(Action::ApplyMode),
                    );
                    l.y += 40.0;
                } else {
                    let r = l.row(48.0, "Режим основного дисплея", String::new(), Style::Row, None);
                    let _ = r;
                    l.out.last_mut().unwrap().value = "Задаётся окном".into();
                    l.end();
                    l.footer("Режимы экрана доступны при самостоятельном входе в Пёрышко.");
                }
                l.header("Ночной режим");
                l.begin();
                l.row(52.0, "Включить сейчас", "Тёплые цвета вечером легче для глаз".into(), Style::Toggle(c.night_active), Some(Action::NightNow));
                l.row(52.0, "По расписанию", "Каждый день в выбранные часы".into(), Style::Toggle(c.night.schedule.is_some()), Some(Action::NightSchedule));
                if let Some(w) = c.night.schedule {
                    l.row(52.0, "Время", crate::shell::dnd_window_label(w), Style::Row, Some(Action::NightWindow));
                }
                l.row(52.0, "Цветовая температура", crate::night::warmth_label(c.night.kelvin), Style::Row, Some(Action::NightWarmth));
                l.end();
                l.footer(if c.direct {"Цвета меняются плавно за несколько секунд. Нажмите на строку, чтобы выбрать время или теплоту."} else {"Ночной режим меняет цвета монитора в прямой сессии Пёрышка; в окне настройки только сохраняются."});
            }
            Page::Defaults => {
                let state = &c.system;
                l.hero(Page::Defaults);
                l.header("Приложения по умолчанию");
                l.begin();
                let mut anchors = [R::new(0.0, 0.0, 0.0, 0.0); 2];
                for (i, (label, value)) in [
                    ("Веб-браузер", state.app_name(state.browser.as_deref())),
                    ("Терминал", state.app_name(Some(&state.terminal))),
                ]
                .into_iter()
                .enumerate()
                {
                    let r = l.row(48.0, label, String::new(), Style::Row, None);
                    let pw = (cw * 0.5).min(240.0);
                    anchors[i] = right(r, pw, 26.0);
                    l.push(
                        anchors[i],
                        &value,
                        Style::Popup,
                        (!state.busy).then_some(Action::AppPicker(i == 0)),
                    );
                }
                l.end();
                l.footer("Браузер открывает ссылки и HTML-файлы. Терминал запускается по Super+T и Super+Enter.");

                l.header("Автозапуск при входе");
                l.begin();
                if state.autostart.is_empty() {
                    l.row(
                        52.0,
                        "Нет программ в автозапуске",
                        "Добавьте приложения, которые должны открываться при входе в систему".into(),
                        Style::Row,
                        None,
                    );
                } else {
                    for item in state.autostart.iter() {
                        let status = if item.enabled {
                            "Запускается при входе"
                        } else {
                            "Отключено"
                        };
                        let r = l.row(
                            52.0,
                            &item.name,
                            format!("{status} · {}", item.exec),
                            Style::Row,
                            None,
                        );
                        let toggle_rect = R::new(
                            r.x + r.w - 14.0 - 80.0 - 6.0 - 68.0,
                            r.y + (r.h - 26.0) / 2.0,
                            68.0,
                            26.0,
                        );
                        l.push(
                            toggle_rect,
                            if item.enabled { "Вкл." } else { "Выкл." },
                            Style::Push(item.enabled),
                            (!state.busy).then_some(Action::ToggleAutostart {
                                id: item.id.clone(),
                                enabled: !item.enabled,
                            }),
                        );
                        l.push(
                            right(r, 80.0, 26.0),
                            "Удалить",
                            Style::Push(false),
                            (!state.busy).then_some(Action::RemoveAutostart(item.id.clone())),
                        );
                    }
                }
                let add_row = l.row(
                    48.0,
                    "Добавить в автозапуск",
                    "Установленные программы и записи ~/.config/autostart".into(),
                    Style::Row,
                    None,
                );
                l.push(
                    right(add_row, 118.0, 26.0),
                    "Добавить…",
                    Style::Push(false),
                    (!state.busy).then_some(Action::AutostartPicker(true)),
                );
                l.end();
                l.footer("Записи сохраняются в стандарте XDG Autostart (~/.config/autostart/*.desktop) и запускаются при входе в сессию Пёрышка.");

                if let Some(browser) = self.apps_picker {
                    let anchor = anchors[usize::from(!browser)];
                    let apps: Vec<_> = state
                        .apps
                        .iter()
                        .filter(|a| if browser { a.browser } else { a.terminal })
                        .collect();
                    let mh = apps.len().max(1) as f32 * 26.0 + 10.0;
                    let mut my = anchor.y + anchor.h + 4.0 - self.scroll;
                    if my + mh > h - 8.0 {
                        my = (anchor.y - self.scroll - 4.0 - mh).max(TOOLBAR_CLIP);
                    }
                    l.fixed(R::new(0.0, -100.0, w, h + 200.0), "", Style::Scrim(false), Some(Action::AppPicker(browser)));
                    let menu = R::new(anchor.x, my, anchor.w, mh);
                    l.fixed(menu, "", Style::Menu, None);
                    if apps.is_empty() {
                        l.fixed(R::new(menu.x + 4.0, my + 5.0, menu.w - 8.0, 26.0), "Не найдено", Style::MenuItem(false), None);
                    }
                    for (i, app) in apps.into_iter().enumerate() {
                        let selected = if browser {
                            state.browser.as_ref() == Some(&app.id)
                        } else {
                            state.terminal == app.id
                        };
                        l.fixed(
                            R::new(menu.x + 4.0, my + 5.0 + i as f32 * 26.0, menu.w - 8.0, 26.0),
                            &app.name,
                            Style::MenuItem(selected),
                            (!state.busy).then_some(Action::DefaultApp {
                                browser,
                                id: app.id.clone(),
                            }),
                        );
                    }
                }
                if self.autostart_picker {
                    let sw = (w - 120.0).min(440.0);
                    let sheet = R::new((w - sw) / 2.0, 0.0, sw, 120.0 + 8.0 * 30.0);
                    l.fixed(
                        R::new(0.0, -100.0, w, h + 200.0),
                        "",
                        Style::Scrim(true),
                        Some(Action::AutostartPicker(false)),
                    );
                    l.fixed(sheet, "Добавить в автозапуск", Style::Sheet, None).detail =
                        "Выберите установленное приложение".into();
                    l.fixed(
                        R::new(sheet.x + 20.0, sheet.y + 52.0, sw - 40.0, 28.0),
                        self.autostart_query.text(),
                        Style::TextField(true),
                        Some(Action::AutostartSearchFocus),
                    );
                    let query = self.autostart_query.to_lowercase();
                    let matches: Vec<_> = state
                        .apps
                        .iter()
                        .filter(|a| app_matches(a, &query))
                        .take(8)
                        .collect();
                    if matches.is_empty() {
                        l.fixed(
                            R::new(sheet.x + 16.0, sheet.y + 88.0, sw - 32.0, 28.0),
                            "Приложения не найдены",
                            Style::MenuItem(false),
                            None,
                        );
                    }
                    for (i, app) in matches.into_iter().enumerate() {
                        let already = state
                            .autostart
                            .iter()
                            .any(|item| item.id == app.id && item.enabled);
                        l.fixed(
                            R::new(sheet.x + 16.0, sheet.y + 88.0 + i as f32 * 30.0, sw - 32.0, 28.0),
                            &app.name,
                            Style::MenuItem(already),
                            (!state.busy).then_some(Action::AddAutostart(app.id.clone())),
                        );
                    }
                    l.fixed(
                        R::new(sheet.x + sw - 116.0, sheet.y + sheet.h - 42.0, 96.0, 28.0),
                        "Отмена",
                        Style::Push(false),
                        Some(Action::AutostartPicker(false)),
                    );
                }
            }
            Page::Privacy => {
                let state = &c.system;
                l.hero(Page::Privacy);
                l.header("Запись экрана и снимки (портал)");
                l.begin();
                let r = l.row(
                    52.0,
                    "Новые запросы доступа",
                    "ScreenCast, Screenshot и Access в xdg-desktop-portal".into(),
                    Style::Row,
                    None,
                );
                let track = right(r, 3.0 * 92.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, policy) in crate::file_portal::ScreenCapturePolicy::ALL.into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * 92.0, track.y, 92.0, track.h),
                        policy.label(),
                        Style::Segment(c.screen_capture_policy == policy),
                        Some(Action::ScreenCapturePolicy(policy)),
                    );
                }
                l.row(
                    52.0,
                    "Показывать курсор в записи",
                    "Режим указателя по умолчанию для сеансов ScreenCast".into(),
                    Style::Toggle(c.screen_capture_cursor),
                    Some(Action::ScreenCaptureCursor),
                );
                l.end();
                l.footer("Портал Пёрышка (org.freedesktop.impl.portal.desktop.feather) проверяет права программ перед захватом экрана и синхронизирует таблицу разрешений.");

                l.header("Доступ программ к экрану");
                l.begin();
                if c.screen_capture_apps.is_empty() {
                    l.row(
                        52.0,
                        "Список программ пуст",
                        "Программы появятся здесь при обращении к порталу или после добавления вручную".into(),
                        Style::Row,
                        None,
                    );
                } else {
                    for app in c.screen_capture_apps {
                        let status = if app.allowed {
                            "Разрешено"
                        } else {
                            "Запрещено"
                        };
                        let r = l.row(
                            52.0,
                            &app.label,
                            format!("{status} · {}", app.id),
                            Style::Row,
                            None,
                        );
                        let toggle_rect = R::new(
                            r.x + r.w - 14.0 - 80.0 - 6.0 - 68.0,
                            r.y + (r.h - 26.0) / 2.0,
                            68.0,
                            26.0,
                        );
                        l.push(
                            toggle_rect,
                            if app.allowed { "Вкл." } else { "Выкл." },
                            Style::Push(app.allowed),
                            Some(Action::ToggleScreenCaptureApp(app.id.clone())),
                        );
                        l.push(
                            right(r, 80.0, 26.0),
                            "Удалить",
                            Style::Push(false),
                            Some(Action::RemoveScreenCaptureApp(app.id.clone())),
                        );
                    }
                }
                let add_row = l.row(
                    48.0,
                    "Добавить программу",
                    "Выдать или ограничить доступ к записи экрана заранее".into(),
                    Style::Row,
                    None,
                );
                let add_rect = right(add_row, 118.0, 26.0);
                if !c.screen_capture_apps.is_empty() {
                    let clear_rect = R::new(add_rect.x - 6.0 - 88.0, add_rect.y, 88.0, 26.0);
                    l.push(
                        clear_rect,
                        "Очистить",
                        Style::Push(false),
                        Some(Action::ClearScreenCaptureApps),
                    );
                }
                l.push(
                    add_rect,
                    "Добавить…",
                    Style::Push(false),
                    Some(Action::ScreenCapturePicker(true)),
                );
                l.end();
                l.footer("При политике «По списку» неизвестные программы добавляются при первом запросе; вы можете заранее разрешить или заблокировать любую программу.");

                if self.privacy_picker {
                    let sw = (w - 120.0).min(440.0);
                    let sheet = R::new((w - sw) / 2.0, 0.0, sw, 120.0 + 8.0 * 30.0);
                    l.fixed(
                        R::new(0.0, -100.0, w, h + 200.0),
                        "",
                        Style::Scrim(true),
                        Some(Action::ScreenCapturePicker(false)),
                    );
                    l.fixed(sheet, "Добавить программу", Style::Sheet, None).detail =
                        "Разрешить доступ к записи экрана".into();
                    l.fixed(
                        R::new(sheet.x + 20.0, sheet.y + 52.0, sw - 40.0, 28.0),
                        self.privacy_query.text(),
                        Style::TextField(true),
                        Some(Action::PrivacySearchFocus),
                    );
                    let query = self.privacy_query.to_lowercase();
                    let matches: Vec<_> = state
                        .apps
                        .iter()
                        .filter(|a| app_matches(a, &query))
                        .take(8)
                        .collect();
                    if matches.is_empty() {
                        l.fixed(
                            R::new(sheet.x + 16.0, sheet.y + 88.0, sw - 32.0, 28.0),
                            "Приложения не найдены",
                            Style::MenuItem(false),
                            None,
                        );
                    }
                    for (i, app) in matches.into_iter().enumerate() {
                        let already = c.screen_capture_apps.iter().any(|item| {
                            crate::file_portal::same_app_id(&item.id, &app.id) && item.allowed
                        });
                        l.fixed(
                            R::new(sheet.x + 16.0, sheet.y + 88.0 + i as f32 * 30.0, sw - 32.0, 28.0),
                            &app.name,
                            Style::MenuItem(already),
                            Some(Action::AddScreenCaptureApp(app.id.clone())),
                        );
                    }
                    l.fixed(
                        R::new(sheet.x + sw - 116.0, sheet.y + sheet.h - 42.0, 96.0, 28.0),
                        "Отмена",
                        Style::Push(false),
                        Some(Action::ScreenCapturePicker(false)),
                    );
                }
            }
            Page::DateTime => {
                let state = &c.system;
                let time = &state.time;
                let enabled = time.available && !state.busy;
                l.hero(Page::DateTime);
                l.begin();
                l.row(
                    52.0,
                    "Устанавливать время автоматически",
                    if !time.available {
                        "Служба времени systemd-timedated недоступна".into()
                    } else if time.synchronized {
                        "Часы синхронизированы с сервером времени".into()
                    } else {
                        "Синхронизация с сервером времени".into()
                    },
                    Style::Toggle(time.ntp),
                    (enabled && (time.can_ntp || time.ntp)).then_some(Action::Ntp),
                );
                let r = l.row(48.0, "Дата и время", String::new(), Style::Row, None);
                if time.ntp || !time.available {
                    l.out.last_mut().unwrap().value =
                        chrono::Local::now().format("%d.%m.%Y, %H:%M").to_string();
                } else {
                    let set = right(r, 110.0, 26.0);
                    l.push(
                        set,
                        "Установить",
                        Style::Push(false),
                        (enabled && !self.time_input.is_empty()).then_some(Action::ApplyTime),
                    );
                    l.push(
                        R::new(set.x - 186.0, set.y, 176.0, 26.0),
                        if self.time_input.is_empty() {
                            "ГГГГ-ММ-ДД ЧЧ:ММ"
                        } else {
                            &self.time_input
                        },
                        Style::TextField(self.time_editing),
                        enabled.then_some(Action::EditTime),
                    );
                }
                let r = l.row(48.0, "Часовой пояс", String::new(), Style::Row, None);
                l.push(
                    right(r, (cw * 0.5).min(240.0), 26.0),
                    &if time.zone.is_empty() {
                        "Не определён".into()
                    } else {
                        time.zone.replace('_', " ")
                    },
                    Style::Popup,
                    enabled.then_some(Action::ZonePicker),
                );
                l.end();
                let fy = l.y;
                l.footer(if time.ntp {
                    "Для ручной установки отключите синхронизацию."
                } else {
                    "Изменение может потребовать пароль администратора."
                });
                l.out.last_mut().unwrap().rect.w -= 110.0;
                l.push(
                    R::new(x + cw - 100.0, fy - 2.0, 100.0, 26.0),
                    "Обновить",
                    Style::Push(false),
                    (!state.busy).then_some(Action::RefreshSystem),
                );
                if self.zone_picker {
                    let sw = (w - 120.0).min(440.0);
                    let sheet = R::new((w - sw) / 2.0, 0.0, sw, 120.0 + 8.0 * 30.0);
                    l.fixed(R::new(0.0, -100.0, w, h + 200.0), "", Style::Scrim(true), Some(Action::ZonePicker));
                    l.fixed(sheet, "Часовой пояс", Style::Sheet, None).detail =
                        "Начните вводить город или регион".into();
                    l.fixed(
                        R::new(sheet.x + 20.0, sheet.y + 52.0, sw - 40.0, 28.0),
                        self.zone_query.text(),
                        Style::TextField(true),
                        Some(Action::ZoneSearchFocus),
                    );
                    let query = self.zone_query.to_lowercase();
                    for (i, zone) in state
                        .zones
                        .iter()
                        .filter(|z| zone_matches(z, &query))
                        .take(8)
                        .enumerate()
                    {
                        l.fixed(
                            R::new(sheet.x + 16.0, sheet.y + 88.0 + i as f32 * 30.0, sw - 32.0, 28.0),
                            &zone.replace('_', " "),
                            Style::MenuItem(time.zone == *zone),
                            enabled.then_some(Action::Timezone(zone.clone())),
                        );
                    }
                    l.fixed(
                        R::new(sheet.x + sw - 116.0, sheet.y + sheet.h - 42.0, 96.0, 28.0),
                        "Отмена",
                        Style::Push(false),
                        Some(Action::ZonePicker),
                    );
                }
            }
            Page::Desktop => {
                l.hero(Page::Desktop);
                l.header("Обои");
                l.begin();
                let r = l.row(
                    52.0,
                    "Текущие обои",
                    c.wallpaper_image
                        .map_or_else(|| c.wallpaper.name().to_string(), |s| s.name()),
                    Style::Row,
                    None,
                );
                l.push(
                    right(r, 150.0, 26.0),
                    if self.wallpaper_busy {
                        "Выбор…"
                    } else {
                        "Выбрать файл…"
                    },
                    Style::Push(false),
                    (!self.wallpaper_busy).then_some(Action::ChooseWallpaper),
                );
                let tw = (cw - 28.0 - 3.0 * 12.0) / 4.0;
                let th = (tw * 0.6).round() + 22.0;
                let r = l.row(th + 28.0, "", String::new(), Style::Row, None);
                for i in 0..3 {
                    l.push(
                        R::new(r.x + 14.0 + i as f32 * (tw + 12.0), r.y + 14.0, tw, th),
                        crate::wallpaper::NAMES[i],
                        Style::Wallpaper(
                            i,
                            c.wallpaper_image == Some(&crate::wallpaper::ImageSource::Bundled(i)),
                        ),
                        Some(Action::WallpaperImage(Some(crate::wallpaper::ImageSource::Bundled(i)))),
                    );
                }
                l.push(
                    R::new(r.x + 14.0 + 3.0 * (tw + 12.0), r.y + 14.0, tw, th),
                    c.wallpaper.name(),
                    Style::Gradient(c.wallpaper_image.is_none()),
                    Some(Action::Wallpaper),
                );
                l.end();
                l.footer("Свои изображения PNG, JPEG и WebP заполняют экран без искажения. Повторный выбор градиента переключает его вариант.");
                l.header("Dock");
                l.begin();
                l.row(
                    52.0,
                    "Увеличение значков",
                    "Значки увеличиваются при наведении".into(),
                    Style::Toggle(c.dock_zoom),
                    Some(Action::DockZoom),
                );
                let r = l.row(48.0, "Размер значков", String::new(), Style::Row, None);
                let track = right(r, 3.0 * 96.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, (size, label)) in crate::theme::DOCK_SIZES.iter().zip(["Маленькие", "Средние", "Крупные"]).enumerate() {
                    l.push(R::new(track.x + i as f32 * 96.0, track.y, 96.0, track.h), label, Style::Segment(crate::theme::dock_icon() == *size), Some(Action::DockSize(*size as u32)));
                }
                let r = l.row(48.0, "Положение на экране", String::new(), Style::Row, None);
                let track = right(r, 3.0 * 96.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, edge) in crate::theme::DockEdge::ALL.into_iter().enumerate() {
                    l.push(R::new(track.x + i as f32 * 96.0, track.y, 96.0, track.h), edge.label(), Style::Segment(crate::theme::dock_edge() == edge), Some(Action::DockEdge(edge)));
                }
                l.row(52.0, "Автоматически скрывать Dock", "Появляется, когда указатель у нижнего края".into(), Style::Toggle(crate::theme::dock_autohide()), Some(Action::DockAutohide));
                l.end();
                l.header("Снимки экрана");
                l.begin();
                let custom_dir = crate::screenshot::custom_dir();
                let r = l.row(
                    52.0,
                    "Папка сохранения",
                    crate::screenshot::dir_display_label(custom_dir.as_deref()),
                    Style::Row,
                    None,
                );
                let choose_rect = right(r, 146.0, 26.0);
                if custom_dir.is_some() {
                    let reset_rect = R::new(choose_rect.x - 6.0 - 88.0, choose_rect.y, 88.0, 26.0);
                    l.push(
                        reset_rect,
                        "Сбросить",
                        Style::Push(false),
                        (!self.screenshot_dir_busy).then_some(Action::ResetScreenshotDir),
                    );
                }
                l.push(
                    choose_rect,
                    if self.screenshot_dir_busy {
                        "Выбор…"
                    } else {
                        "Выбрать папку…"
                    },
                    Style::Push(false),
                    (!self.screenshot_dir_busy && !self.wallpaper_busy)
                        .then_some(Action::ChooseScreenshotDir),
                );
                let current_fmt = crate::screenshot::format();
                let r = l.row(
                    52.0,
                    "Формат файлов",
                    format!("Расширение .{}" , current_fmt.extension()),
                    Style::Row,
                    None,
                );
                let track = right(r, 3.0 * 88.0, 28.0);
                l.push(track, "", Style::Track, None);
                for (i, fmt) in crate::screenshot::Format::ALL.into_iter().enumerate() {
                    l.push(
                        R::new(track.x + i as f32 * 88.0, track.y, 88.0, track.h),
                        fmt.label(),
                        Style::Segment(current_fmt == fmt),
                        Some(Action::ScreenshotFormat(fmt)),
                    );
                }
                l.end();
                l.footer("Print — весь экран, Shift+Print — область или окно, Alt+Print — активное окно. Снимки также копируются в буфер обмена.");
                l.header("Активные углы");
                l.begin();
                for (i, name) in crate::hot_corners::CORNER_NAMES.iter().enumerate() {
                    l.row(52.0, name, c.hot_corners[i].label().into(), Style::Row, Some(Action::HotCorner(i)));
                }
                l.end();
                l.footer("Подведите указатель в угол экрана, чтобы выполнить действие. Нажмите на строку, чтобы выбрать его.");
            }
        }
        if self.page == Page::Keyboard && self.adding {
            let sw = (w - 120.0).min(500.0);
            let rows = SOURCES.len().div_ceil(2) as f32;
            let sheet = R::new((w - sw) / 2.0, 0.0, sw, 112.0 + rows * 44.0);
            l.fixed(R::new(0.0, -100.0, w, h + 200.0), "", Style::Scrim(true), Some(Action::AddPicker(false)));
            l.fixed(sheet, "Добавить источник ввода", Style::Sheet, None).detail =
                format!("Выбрано {} из 4", c.keyboard.layouts.len());
            let cell = (sw - 50.0) / 2.0;
            for (i, s) in SOURCES.into_iter().enumerate() {
                let exists = c.keyboard.layouts.iter().any(|l| l == s.id);
                l.fixed(
                    R::new(
                        sheet.x + 20.0 + (i % 2) as f32 * (cell + 10.0),
                        sheet.y + 56.0 + (i / 2) as f32 * 44.0,
                        cell,
                        38.0,
                    ),
                    s.label,
                    Style::Source {
                        badge: s.badge,
                        selected: exists,
                    },
                    (!exists && c.keyboard.layouts.len() < 4 && kb).then(|| Action::AddSource(s.id.into())),
                );
            }
            l.fixed(
                R::new(sheet.x + sw - 116.0, sheet.y + sheet.h - 44.0, 96.0, 28.0),
                "Готово",
                Style::Push(true),
                Some(Action::AddPicker(false)),
            );
        }
        if self.confirming {
            let r = R::new(x, h - 64.0, cw, 52.0);
            l.fixed(r, "Сохранить новый режим?", Style::Banner(false), None).detail =
                "Без подтверждения прежний режим вернётся через 15 секунд.".into();
            l.fixed(
                R::new(r.x + r.w - 214.0, r.y + 12.0, 96.0, 28.0),
                "Вернуть",
                Style::Push(false),
                Some(Action::RevertMode),
            );
            l.fixed(
                R::new(r.x + r.w - 110.0, r.y + 12.0, 96.0, 28.0),
                "Сохранить",
                Style::Push(true),
                Some(Action::KeepMode),
            );
        } else if let Some(e) = &self.error {
            l.fixed(R::new(x, h - 52.0, cw, 40.0), e, Style::Banner(true), None);
        } else if self.shortcut_capture.is_some() && self.page == Page::Keyboard {
            l.fixed(R::new(x, h - 64.0, cw, 52.0), "Нажмите новое сочетание", Style::Banner(false), None).detail =
                "Esc — отмена; Backspace — выключить.".into();
        }
        let mut controls = l.out;
        for control in controls.iter_mut().filter(|c| !c.fixed) {
            control.rect.y -= self.scroll;
        }
        controls
    }
    fn audio_volume(&self, device: &crate::controls::Sink, input: bool) -> f32 {
        use crate::controls::Control;
        match &self.audio_drag {
            Some((_, Control::InputVolume { source, value })) if input && *source == device.name => *value,
            Some((_, Control::Volume { sink, value })) if !input && *sink == device.name => *value,
            _ => device.volume(),
        }
    }
    pub fn audio_motion(&mut self, x: f32) -> Option<Action> {
        use crate::controls::Control;
        let (rect, command) = self.audio_drag.as_mut()?;
        let value = ((x - rect.x - 10.0) / (rect.w - 20.0)).clamp(0.0, 1.0);
        let current = match command { Control::InputVolume { value, .. } | Control::Volume { value, .. } => value, _ => return None };
        let sent = *self.audio_sent_volume.get_or_insert(*current);
        *current = value;
        if (sent - value).abs() < 0.02 { return None; }
        self.audio_sent_volume = Some(value);
        Some(Action::Audio(command.clone()))
    }
    pub fn audio_release(&mut self) -> Option<Action> {
        self.audio_sent_volume = None;
        self.audio_drag.take().map(|(_, command)| Action::Audio(command))
    }
    /// The control a click at the pointer would reach, if it does anything.
    fn hovered_index(&self, controls: &[Control], h: f32) -> Option<usize> {
        let (x, y) = self.pointer?;
        let i = controls
            .iter()
            .rposition(|i| i.rect.contains(x, y) && (i.fixed || (TOOLBAR_CLIP..h).contains(&y)))?;
        (controls[i].action.is_some() && !matches!(controls[i].style, Style::Scrim(_))).then_some(i)
    }

    /// Hover identity: style kind plus position, compared before and after a move.
    pub fn hover_key(&self, w: f32, h: f32, c: &Context<'_>) -> Option<(i32, i32)> {
        let controls = self.controls(w, h, c);
        self.hovered_index(&controls, h).map(|i| (controls[i].rect.x as i32, controls[i].rect.y as i32))
    }

    pub fn hit(&self, x: f32, y: f32, w: f32, h: f32, c: &Context<'_>) -> Option<Action> {
        let control = self
            .controls(w, h, c)
            .into_iter()
            .rev()
            .find(|i| i.rect.contains(x, y) && (i.fixed || (TOOLBAR_CLIP..h).contains(&y)))?;
        match (control.style, control.action) {
            (Style::Slider(_), Some(Action::PointerSpeed(_))) => {
                let t = ((x - control.rect.x - 10.0) / (control.rect.w - 20.0)).clamp(0.0, 1.0);
                // Snap to tenths, like the tick marks.
                Some(Action::PointerSpeed(((t as f64 * 2.0 - 1.0) * 10.0).round() / 10.0))
            }
            (Style::Volume(_), Some(Action::Audio(command))) => {
                let value = ((x-control.rect.x-10.0)/(control.rect.w-20.0)).clamp(0.0, 1.0);
                Some(Action::Audio(match command {
                    crate::controls::Control::InputVolume { source, .. } => crate::controls::Control::InputVolume { source, value },
                    crate::controls::Control::Volume { sink, .. } => crate::controls::Control::Volume { sink, value },
                    other => other,
                }))
            }
            (_, action) => action,
        }
    }
    fn text_rect(&self, x: f32, y: f32, w: f32, h: f32, c: &Context<'_>) -> Option<R> {
        self.controls(w, h, c).into_iter().rev().find(|i| i.rect.contains(x, y) && matches!(i.style, Style::TextField(_))).map(|i| i.rect)
    }
    pub fn debug_point(
        &self,
        w: f32,
        h: f32,
        c: &Context<'_>,
        predicate: impl Fn(&Action) -> bool,
    ) -> Option<(f32, f32)> {
        // Topmost first, as a real click: a sheet's Done button before its scrim.
        self.controls(w, h, c)
            .into_iter()
            .rev()
            .filter(|i| !matches!(i.style, Style::Scrim(_)))
            .find(|i| i.action.as_ref().is_some_and(&predicate))
            .map(|i| (i.rect.x + i.rect.w / 2.0, i.rect.y + i.rect.h / 2.0))
    }
    pub fn scroll_by(&mut self, delta: f64, w: f32, h: f32, c: &Context<'_>) -> bool {
        let bottom = self
            .controls(w, h, c)
            .iter()
            .filter(|i| !i.fixed)
            .map(|i| i.rect.y + i.rect.h + self.scroll)
            .fold(0.0, f32::max);
        let old = self.scroll;
        self.scroll = (old + delta as f32 * 3.0).clamp(0.0, (bottom - h + 20.0).max(0.0));
        self.scroll != old
    }
    /// `origin` is the content origin below a `top`-high toolbar strip, which this panel
    /// also paints (unified toolbar): the sidebar runs the full window height.
    pub fn paint(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        origin: (f32, f32),
        size: (f32, f32),
        top: f32,
        c: &Context<'_>,
    ) {
        let p = palette();
        let dark = appearance() == Appearance::Dark;
        let hc = crate::theme::high_contrast();
        let (ox, oy) = origin;
        let (w, h) = size;
        let side = side_width(w);
        let sidebar = if dark {
            rgba(46, 46, 50, 255)
        } else {
            rgba(240, 240, 244, 255)
        };
        let sb = R::new(ox + 7.0, oy - top + 7.0, side - 14.0, h + top - 14.0);
        fill_rounded_rect(pm, sb.x, sb.y, sb.w, sb.h, 10.0, sidebar);
        stroke_rounded_rect(
            pm,
            sb.x + 0.25,
            sb.y + 0.25,
            sb.w - 0.5,
            sb.h - 0.5,
            10.0,
            if hc { 1.0 } else { 0.5 },
            if hc {
                p.separator
            } else if dark {
                rgba(255, 255, 255, 22)
            } else {
                rgba(0, 0, 0, 18)
            },
        );
        if self.page != Page::Desktop {
            crate::wallpaper::release_thumbs();
        }
        let controls = self.controls(w, h, c);
        let hovered = self.hovered_index(&controls, h);
        for (i, control) in controls.iter().enumerate().filter(|(_, i)| !i.fixed) {
            let r = control.rect;
            if r.y + r.h > TOOLBAR_CLIP - 8.0 && r.y < h {
                self.paint_control(pm, fonts, control, (ox, oy), c, hovered == Some(i));
            }
        }
        // Unified toolbar: scrolled content slides under it.
        fill_rounded_rect(pm, ox + side, oy - top + 12.0, w - side, top + TOOLBAR_CLIP - 12.0, 0.0, p.window_bg);
        fill_rounded_rect(pm, ox + side, oy - top, w - side, 24.0, crate::theme::WINDOW_CORNER_RADIUS, p.window_bg);
        if self.scroll > 0.0 {
            stroke_line(pm, ox + side, oy + TOOLBAR_CLIP, ox + w, oy + TOOLBAR_CLIP, if hc { 1.0 } else { 0.5 }, p.separator);
        }
        fonts.draw_text(pm, self.page.title(), ox + side + 20.0, oy - top + 11.0, 15.0, p.text, true);
        if self.keyboard_busy || c.system.busy {
            fonts.draw_text_right(pm, "Применение…", ox + w - 20.0, oy - top + 13.0, 12.0, p.text_secondary, false);
        }
        for (i, control) in controls.iter().enumerate().filter(|(_, i)| i.fixed) {
            if let Style::Scrim(dim) = control.style {
                if dim {
                    fill_rounded_rect(pm, ox, oy - top, w, h + top, crate::theme::WINDOW_CORNER_RADIUS, rgba(0, 0, 0, if dark { 90 } else { 40 }));
                }
                continue;
            }
            self.paint_control(pm, fonts, control, (ox, oy), c, hovered == Some(i));
        }
    }
    fn paint_control(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        control: &Control,
        (ox, oy): (f32, f32),
        c: &Context<'_>,
        hover: bool,
    ) {
        let p = palette();
        let dark = appearance() == Appearance::Dark;
        let hc = crate::theme::high_contrast();
        let hover_fill = if dark { rgba(255, 255, 255, if hc { 28 } else { 18 }) } else { rgba(0, 0, 0, if hc { 20 } else { 12 }) };
        let group_fill = if dark {
            rgba(255, 255, 255, if hc { 24 } else { 13 })
        } else {
            rgba(0, 0, 0, if hc { 18 } else { 10 })
        };
        let push_fill = if dark {
            rgba(255, 255, 255, if hc { 52 } else { 30 })
        } else {
            rgba(0, 0, 0, if hc { 32 } else { 16 })
        };
        let r = control.rect;
        let r = R::new(r.x + ox, r.y + oy, r.w, r.h);
        let interactive = !matches!(
            control.style,
            Style::Row | Style::Group | Style::Header | Style::Footer | Style::Hero(_) | Style::Display | Style::Sheet | Style::Menu | Style::Banner(_) | Style::Track | Style::Scrim(_)
        );
        let disabled = interactive && control.action.is_none();
        let fg = if disabled { p.text_tertiary } else { p.text };
        if control.divider {
            stroke_line(pm, r.x + 14.0, r.y, r.x + r.w - 14.0, r.y, if hc { 1.0 } else { 0.5 }, p.separator);
        }
        // Label/detail block of a form row.
        let row_text = |pm: &mut PixmapMut, fonts: &mut FontSystem, reserve: f32| {
            if control.label.is_empty() {
                return;
            }
            let (ly, dy) = if r.h > 60.0 {
                (r.y + 13.0, r.y + 32.0)
            } else if control.detail.is_empty() {
                (r.y + (r.h - 13.0) / 2.0 - 1.0, 0.0)
            } else {
                (r.y + r.h / 2.0 - 15.0, r.y + r.h / 2.0 + 2.0)
            };
            fonts.draw_text_clipped(pm, &control.label, r.x + 14.0, ly, r.w - 28.0 - reserve, 13.0, fg, false);
            if !control.detail.is_empty() {
                fonts.draw_text_clipped(pm, &control.detail, r.x + 14.0, dy, r.w - 28.0 - reserve, 11.0, p.text_secondary, false);
            }
        };
        match control.style {
            Style::Sidebar(page, selected) => {
                if selected {
                    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 8.0, p.accent);
                } else if hover {
                    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 8.0, hover_fill);
                }
                draw_page_icon(pm, page, r.x + 6.0, r.y + 5.0, 20.0);
                fonts.draw_text_clipped(
                    pm,
                    &control.label,
                    r.x + 34.0,
                    r.y + 8.0,
                    r.w - 40.0,
                    13.0,
                    if selected { p.on_accent } else { p.text },
                    false,
                );
            }
            Style::Hero(page) => {
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 12.0, group_fill);
                if hc {
                    stroke_rounded_rect(pm, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, 12.0, 1.0, p.separator);
                }
                draw_page_icon(pm, page, r.x + 14.0, r.y + 14.0, 44.0);
                fonts.draw_text_clipped(pm, &control.label, r.x + 72.0, r.y + 17.0, r.w - 86.0, 15.0, p.text, true);
                fonts.draw_text_clipped(pm, &control.detail, r.x + 72.0, r.y + 40.0, r.w - 86.0, 12.0, p.text_secondary, false);
            }
            Style::Display => {
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 12.0, group_fill);
                if hc {
                    stroke_rounded_rect(pm, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, 12.0, 1.0, p.separator);
                }
                let (mw, mh) = (150.0, 88.0);
                let mx = r.x + (r.w - mw) / 2.0;
                let my = r.y + 16.0;
                fill_rounded_rect(pm, mx, my, mw, mh, 7.0, if dark { rgba(20, 20, 22, 255) } else { rgba(40, 40, 44, 255) });
                let (top, bottom) = match c.wallpaper {
                    WallpaperTheme::TahoeNight | WallpaperTheme::SequoiaMidnight => (rgba(48, 57, 96, 255), rgba(78, 51, 88, 255)),
                    _ => (rgba(150, 205, 245, 255), rgba(240, 195, 185, 255)),
                };
                if let Some(path) = rounded_rect_path(mx + 4.0, my + 4.0, mw - 8.0, mh - 8.0, 4.0) {
                    fill_path_gradient(pm, &path, my, my + mh, top, bottom);
                }
                fill_rounded_rect(pm, mx + mw / 2.0 - 4.0, my + mh, 8.0, 10.0, 0.0, p.text_tertiary);
                fill_rounded_rect(pm, mx + mw / 2.0 - 26.0, my + mh + 9.0, 52.0, 4.0, 2.0, p.text_tertiary);
                fonts.draw_text_clipped_centered(pm, &control.label, r.x + r.w / 2.0, r.y + 128.0, r.w - 28.0, 13.0, p.text, true);
                fonts.draw_text_clipped_centered(pm, &control.detail, r.x + r.w / 2.0, r.y + 147.0, r.w - 28.0, 11.0, p.text_secondary, false);
            }
            Style::Header => {
                fonts.draw_text_clipped(pm, &control.label, r.x, r.y, r.w, 13.0, p.text, true);
            }
            Style::Footer => {
                for (i, line) in fonts.wrap_two_lines(&control.label, r.w, 11.0, false).iter().enumerate() {
                    fonts.draw_text(pm, line, r.x, r.y + i as f32 * 15.0, 11.0, p.text_secondary, false);
                }
            }
            Style::Group => {
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 12.0, group_fill);
                if hc {
                    stroke_rounded_rect(pm, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, 12.0, 1.0, p.separator);
                }
            }
            Style::Row => {
                let reserve = if control.value.is_empty() {
                    control.right_reserve
                } else {
                    (fonts.measure_text(&control.value, 13.0, false).min(r.w * 0.55) + 12.0)
                        .max(control.right_reserve)
                };
                row_text(pm, fonts, reserve);
                if !control.value.is_empty() {
                    let value = fonts.ellipsize(&control.value, r.w * 0.55, 13.0, false);
                    fonts.draw_text_right(pm, &value, r.x + r.w - 14.0, r.y + (r.h - 13.0) / 2.0 - 1.0, 13.0, p.text_secondary, false);
                }
            }
            Style::Toggle(on) => {
                if hover && !disabled {
                    fill_rounded_rect(pm, r.x + 4.0, r.y + 3.0, r.w - 8.0, r.h - 6.0, 9.0, hover_fill);
                }
                row_text(pm, fonts, 60.0);
                let (tw, th) = (40.0, 24.0);
                let tx = r.x + r.w - 14.0 - tw;
                let ty = r.y + (r.h - th) / 2.0;
                let off = if dark {
                    rgba(255, 255, 255, if hc { 72 } else { 40 })
                } else {
                    rgba(0, 0, 0, if hc { 56 } else { 30 })
                };
                let track = if on { p.accent } else { off };
                fill_rounded_rect(pm, tx, ty, tw, th, th / 2.0, if disabled { with_alpha(track, 0.45) } else { track });
                if hc && !on {
                    stroke_rounded_rect(pm, tx + 0.5, ty + 0.5, tw - 1.0, th - 1.0, th / 2.0, 1.0, p.separator);
                }
                let kx = if on { tx + tw - 12.0 } else { tx + 12.0 };
                fill_circle(pm, kx, ty + 12.5, 10.0, rgba(0, 0, 0, 22));
                fill_circle(pm, kx, ty + 12.0, 10.0, rgba(255, 255, 255, 255));
            }
            Style::Track => {
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, if dark { rgba(255, 255, 255, if hc { 42 } else { 22 }) } else { rgba(0, 0, 0, if hc { 28 } else { 14 }) });
            }
            Style::Segment(selected) => {
                if hover && !selected && !disabled {
                    fill_rounded_rect(pm, r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 4.0, (r.h - 4.0) / 2.0, hover_fill);
                }
                if selected {
                    fill_rounded_rect(pm, r.x + 2.0, r.y + 2.5, r.w - 4.0, r.h - 4.0, (r.h - 4.0) / 2.0, rgba(0, 0, 0, 18));
                    fill_rounded_rect(
                        pm,
                        r.x + 2.0,
                        r.y + 2.0,
                        r.w - 4.0,
                        r.h - 4.0,
                        (r.h - 4.0) / 2.0,
                        if dark { rgba(110, 110, 116, 255) } else { rgba(255, 255, 255, 255) },
                    );
                }
                fonts.draw_text_clipped_centered(pm, &control.label, r.x + r.w / 2.0, r.y + (r.h - 12.0) / 2.0 - 1.0, r.w - 8.0, 12.0, fg, selected);
            }
            Style::Slider(value) | Style::Volume(value) => {
                let (tx, tw, cy) = (r.x + 10.0, r.w - 20.0, r.y + 10.0);
                let rail = if dark { rgba(255, 255, 255, 40) } else { rgba(0, 0, 0, 24) };
                for i in 0..=10 {
                    let x = tx + tw * i as f32 / 10.0;
                    fill_rounded_rect(pm, x - 0.75, cy + 7.0, 1.5, 4.0, 0.75, rail);
                }
                fill_rounded_rect(pm, tx, cy - 2.0, tw, 4.0, 2.0, rail);
                fill_rounded_rect(pm, tx, cy - 2.0, tw * value, 4.0, 2.0, p.accent);
                let kx = tx + tw * value;
                fill_circle(pm, kx, cy + 0.5, 10.0, rgba(0, 0, 0, 30));
                fill_circle(pm, kx, cy, 10.0, rgba(255, 255, 255, 255));
                stroke_circle(pm, kx, cy, 10.0, 0.5, rgba(0, 0, 0, 40));
                if matches!(control.style, Style::Slider(_)) {
                fonts.draw_text(pm, "Медленно", tx, r.y + 25.0, 11.0, p.text_secondary, false);
                fonts.draw_text_right(pm, "Быстро", tx + tw, r.y + 25.0, 11.0, p.text_secondary, false);
                }
            }
            Style::Push(primary) => {
                let fill = if primary { p.accent } else { push_fill };
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, if disabled { with_alpha(fill, 0.5) } else { fill });
                if hc && !primary {
                    stroke_rounded_rect(pm, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, r.h / 2.0, 1.0, p.separator);
                }
                if hover && !disabled {
                    let over = if primary { rgba(255, 255, 255, 36) } else { hover_fill };
                    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, over);
                }
                fonts.draw_text_clipped_centered(
                    pm,
                    &control.label,
                    r.x + r.w / 2.0,
                    r.y + (r.h - 13.0) / 2.0 - 1.0,
                    r.w - 16.0,
                    13.0,
                    if primary { p.on_accent } else { fg },
                    false,
                );
            }
            Style::Popup => {
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, push_fill);
                if hc {
                    stroke_rounded_rect(pm, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, r.h / 2.0, 1.0, p.separator);
                }
                if hover && !disabled {
                    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, hover_fill);
                }
                fonts.draw_text_clipped(pm, &control.label, r.x + 12.0, r.y + (r.h - 13.0) / 2.0 - 1.0, r.w - 36.0, 13.0, fg, false);
                draw_chevrons(pm, r.x + r.w - 16.0, r.y + r.h / 2.0, if disabled { p.text_tertiary } else { p.text_secondary });
            }
            Style::Mini => {
                if hover && !disabled {
                    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 6.0, hover_fill);
                }
                fonts.draw_text_centered(pm, &control.label, r.x + r.w / 2.0, r.y + (r.h - 16.0) / 2.0 - 1.0, 16.0, fg, false);
            }
            Style::Source { badge, selected } => {
                if hover && !selected && !disabled {
                    fill_rounded_rect(pm, r.x + 6.0, r.y + 3.0, r.w - 12.0, r.h - 6.0, 8.0, hover_fill);
                }
                if selected {
                    fill_rounded_rect(pm, r.x + 6.0, r.y + 3.0, r.w - 12.0, r.h - 6.0, 8.0, with_alpha(p.accent, if dark { 0.28 } else { 0.13 }));
                }
                draw_input_badge(pm, fonts, badge, r.x + 16.0, r.y + (r.h - 18.0) * 0.5, 24.0, 18.0, if disabled && !selected { p.text_tertiary } else { p.text });
                fonts.draw_text_clipped(pm, &control.label, r.x + 50.0, r.y + (r.h - 13.0) * 0.5 - 1.0, r.w - 84.0, 13.0, if disabled && !selected { p.text_tertiary } else { p.text }, false);
                if selected {
                    fonts.draw_text_right(pm, "✓", r.x + r.w - 16.0, r.y + (r.h - 13.0) * 0.5 - 1.0, 13.0, p.accent, true);
                }
            }
            Style::Preview(dark_tile, selected) => {
                let t = R::new(r.x, r.y, r.w, r.h - 22.0);
                if hover && !selected {
                    stroke_rounded_rect(pm, t.x - 3.0, t.y - 3.0, t.w + 6.0, t.h + 6.0, 10.0, 2.5, if dark { rgba(255, 255, 255, 70) } else { rgba(0, 0, 0, 45) });
                }
                if selected {
                    stroke_rounded_rect(pm, t.x - 3.0, t.y - 3.0, t.w + 6.0, t.h + 6.0, 10.0, 2.5, p.accent);
                }
                let (top, bottom) = if dark_tile {
                    (rgba(48, 57, 96, 255), rgba(78, 51, 88, 255))
                } else {
                    (rgba(160, 212, 247, 255), rgba(246, 207, 191, 255))
                };
                if let Some(path) = rounded_rect_path(t.x, t.y, t.w, t.h, 7.0) {
                    fill_path_gradient(pm, &path, t.y, t.y + t.h, top, bottom);
                }
                let (win, bar) = if dark_tile {
                    (rgba(44, 45, 50, 255), rgba(62, 63, 68, 255))
                } else {
                    (rgba(255, 255, 255, 245), rgba(236, 236, 238, 255))
                };
                fill_rounded_rect(pm, t.x + 14.0, t.y + 12.0, t.w - 28.0, t.h - 22.0, 4.0, win);
                fill_rounded_rect(pm, t.x + 14.0, t.y + 12.0, t.w - 28.0, 8.0, 4.0, bar);
                for (i, color) in [rgba(255, 95, 86, 255), rgba(255, 189, 46, 255), rgba(39, 201, 63, 255)].into_iter().enumerate() {
                    fill_circle(pm, t.x + 19.0 + i as f32 * 5.0, t.y + 16.0, 1.5, color);
                }
                fill_rounded_rect(pm, t.x + 20.0, t.y + 24.0, 20.0, 4.5, 2.25, p.accent);
                fill_rounded_rect(pm, t.x + t.w / 2.0 - 16.0, t.y + t.h - 7.0, 32.0, 4.0, 2.0, rgba(255, 255, 255, 150));
                fonts.draw_text_clipped_centered(pm, &control.label, r.x + r.w / 2.0, r.y + r.h - 16.0, r.w, 11.0, if selected { p.text } else { p.text_secondary }, selected);
            }
            Style::ColorSwatch { color, selected } => {
                let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                if hover && !selected {
                    stroke_circle(
                        pm,
                        cx,
                        cy,
                        11.0,
                        1.5,
                        if dark { rgba(255, 255, 255, 80) } else { rgba(0, 0, 0, 50) },
                    );
                }
                if let Some(c) = color {
                    let (cr, cg, cb) = c.rgb(appearance());
                    fill_circle(pm, cx, cy, 9.0, rgba(cr, cg, cb, 255));
                    stroke_circle(
                        pm,
                        cx,
                        cy,
                        9.0,
                        0.75,
                        if dark { rgba(255, 255, 255, 45) } else { rgba(0, 0, 0, 35) },
                    );
                } else {
                    let (cr, cg, cb) = crate::theme::accent_color().rgb(appearance());
                    let fill = rgba(cr, cg, cb, 255);
                    stroke_circle(pm, cx, cy, 8.2, 2.0, fill);
                    fill_circle(pm, cx, cy, 5.2, fill);
                }
                if selected {
                    fill_circle(pm, cx, cy, 3.6, rgba(255, 255, 255, 255));
                    stroke_circle(pm, cx, cy, 3.6, 0.5, rgba(0, 0, 0, 45));
                }
            }
            Style::Wallpaper(index, selected) => {
                let t = R::new(r.x, r.y, r.w, r.h - 22.0);
                if hover && !selected {
                    stroke_rounded_rect(pm, t.x - 3.0, t.y - 3.0, t.w + 6.0, t.h + 6.0, 10.0, 2.5, if dark { rgba(255, 255, 255, 70) } else { rgba(0, 0, 0, 45) });
                }
                if selected {
                    stroke_rounded_rect(pm, t.x - 3.0, t.y - 3.0, t.w + 6.0, t.h + 6.0, 10.0, 2.5, p.accent);
                }
                crate::wallpaper::paint_thumb_rounded(pm, index, t, 7.0);
                fonts.draw_text_clipped_centered(pm, &control.label, r.x + r.w / 2.0, r.y + r.h - 16.0, r.w, 11.0, if selected { p.text } else { p.text_secondary }, selected);
            }
            Style::Gradient(selected) => {
                let t = R::new(r.x, r.y, r.w, r.h - 22.0);
                if hover && !selected {
                    stroke_rounded_rect(pm, t.x - 3.0, t.y - 3.0, t.w + 6.0, t.h + 6.0, 10.0, 2.5, if dark { rgba(255, 255, 255, 70) } else { rgba(0, 0, 0, 45) });
                }
                if selected {
                    stroke_rounded_rect(pm, t.x - 3.0, t.y - 3.0, t.w + 6.0, t.h + 6.0, 10.0, 2.5, p.accent);
                }
                let (top, bottom) = match c.wallpaper {
                    WallpaperTheme::TahoeDay => (rgba(185, 225, 249, 255), rgba(246, 207, 191, 255)),
                    WallpaperTheme::TahoeNight => (rgba(48, 57, 96, 255), rgba(78, 51, 88, 255)),
                    WallpaperTheme::SequoiaMidnight => (rgba(22, 32, 72, 255), rgba(70, 40, 92, 255)),
                    WallpaperTheme::FeatherCyan => (rgba(0, 196, 220, 255), rgba(32, 62, 140, 255)),
                };
                if let Some(path) = rounded_rect_path(t.x, t.y, t.w, t.h, 7.0) {
                    fill_path_gradient(pm, &path, t.y, t.y + t.h, top, bottom);
                }
                fonts.draw_text_clipped_centered(pm, &control.label, r.x + r.w / 2.0, r.y + r.h - 16.0, r.w, 11.0, if selected { p.text } else { p.text_secondary }, selected);
            }
            Style::TextField(active) => {
                if active {
                    stroke_rounded_rect(pm, r.x - 1.5, r.y - 1.5, r.w + 3.0, r.h + 3.0, 8.5, 3.0, with_alpha(p.accent, 0.45));
                }
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 7.0, if dark { rgba(255, 255, 255, 18) } else { rgba(255, 255, 255, 255) });
                stroke_rounded_rect(pm, r.x, r.y, r.w, r.h, 7.0, 0.5, if dark { rgba(255, 255, 255, 40) } else { rgba(0, 0, 0, 45) });
                let masked = matches!(control.action, Some(Action::WifiPasswordFocus));
                let input = if matches!(control.action, Some(Action::ZoneSearchFocus)) {
                    &self.zone_query
                } else if matches!(control.action, Some(Action::AutostartSearchFocus)) {
                    &self.autostart_query
                } else if matches!(control.action, Some(Action::PrivacySearchFocus)) {
                    &self.privacy_query
                } else if masked {
                    &self.wifi_password
                } else {
                    &self.time_input
                };
                if input.is_empty() && !active {
                    fonts.draw_text_clipped(pm, &control.label, r.x + 9.0, r.y + (r.h - 13.0) / 2.0 - 1.0, r.w - 18.0, 13.0, p.text_tertiary, false);
                } else if masked {
                    input.draw_masked(pm, fonts, r.x + 9.0, r.y + (r.h - 13.0) / 2.0 - 1.0, r.w - 18.0, 13.0, fg, p.accent);
                } else {
                    input.draw(pm, fonts, r.x + 9.0, r.y + (r.h - 13.0) / 2.0 - 1.0, r.w - 18.0, 13.0, false, fg, active, p.accent);
                }
            }
            Style::Scrim(_) => {}
            Style::Sheet => {
                soft_shadow(pm, r, 14.0);
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 14.0, p.window_bg);
                stroke_rounded_rect(pm, r.x, r.y, r.w, r.h, 14.0, 0.5, p.window_outline);
                fonts.draw_text_clipped(pm, &control.label, r.x + 20.0, r.y + 16.0, r.w - 40.0, 15.0, p.text, true);
                fonts.draw_text_right(pm, &control.detail, r.x + r.w - 20.0, r.y + 19.0, 11.0, p.text_secondary, false);
            }
            Style::Menu => {
                soft_shadow(pm, r, 9.0);
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 9.0, if dark { rgba(52, 52, 56, 255) } else { rgba(250, 250, 252, 255) });
                stroke_rounded_rect(pm, r.x, r.y, r.w, r.h, 9.0, 0.5, p.window_outline);
            }
            Style::MenuItem(checked) => {
                let lit = hover && !disabled;
                if lit {
                    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 6.0, p.accent);
                }
                if checked {
                    fonts.draw_text(pm, "✓", r.x + 9.0, r.y + (r.h - 13.0) / 2.0 - 1.0, 13.0, if lit { p.on_accent } else { p.accent }, true);
                }
                fonts.draw_text_clipped(pm, &control.label, r.x + 28.0, r.y + (r.h - 13.0) / 2.0 - 1.0, r.w - 36.0, 13.0, if lit { p.on_accent } else { fg }, false);
            }
            Style::Banner(error) => {
                soft_shadow(pm, r, 12.0);
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 12.0, if dark { rgba(56, 56, 60, 255) } else { rgba(250, 250, 252, 255) });
                stroke_rounded_rect(pm, r.x, r.y, r.w, r.h, 12.0, 0.5, p.window_outline);
                if error {
                    fonts.draw_text_clipped(pm, &control.label, r.x + 14.0, r.y + (r.h - 12.0) / 2.0 - 1.0, r.w - 28.0, 12.0, rgba(220, 55, 55, 255), false);
                } else {
                    // Fixed banners carry two buttons on the right
                    let tw = if control.fixed { r.w - 250.0 } else { r.w - 28.0 };
                    fonts.draw_text_clipped(pm, &control.label, r.x + 14.0, r.y + 10.0, tw, 13.0, p.text, true);
                    fonts.draw_text_clipped(pm, &control.detail, r.x + 14.0, r.y + 32.0, tw, 11.0, p.text_secondary, false);
                }
            }
        }
    }
}

impl crate::FeatherCompositor {
    pub fn settings_control_press(&mut self, action: &Action, x: f32, y: f32, w: f32, h: f32) {
        if matches!(action, Action::Audio(crate::controls::Control::Volume { .. } | crate::controls::Control::InputVolume { .. })) {
            if let Some(control) = self.system_settings.controls(w, h, &self.settings_context()).into_iter().rev()
                .find(|c| matches!(c.style, Style::Volume(_)) && c.rect.contains(x, y)) {
                if let Some(win) = self.windows.iter().find(|w| w.app_id == "builtin.settings") {
                    let r = control.rect;
                    if let Action::Audio(command) = action {
                        self.system_settings.audio_sent_volume = None;
                        self.system_settings.audio_drag = Some((R::new(r.x+win.x as f32, r.y+(win.y+win.titlebar_h()) as f32, r.w, r.h), command.clone()));
                    }
                }
            }
        }
        if !matches!(action, Action::EditTime | Action::ZoneSearchFocus | Action::AutostartSearchFocus | Action::PrivacySearchFocus | Action::WifiPasswordFocus) { return; }
        let Some(rect) = self.system_settings.text_rect(x, y, w, h, &self.settings_context()) else { return; };
        let masked = matches!(action, Action::WifiPasswordFocus);
        let input = if matches!(action, Action::ZoneSearchFocus) {
            &mut self.system_settings.zone_query
        } else if matches!(action, Action::AutostartSearchFocus) {
            &mut self.system_settings.autostart_query
        } else if matches!(action, Action::PrivacySearchFocus) {
            &mut self.system_settings.privacy_query
        } else if masked {
            &mut self.system_settings.wifi_password
        } else {
            &mut self.system_settings.time_input
        };
        if matches!(action, Action::EditTime) && input.is_empty() {
            input.set(&self.system_preferences.time.local_input);
        }
        let extend = self.keyboard.modifier_state().shift;
        if masked {
            input.click_masked(&mut self.fonts, x - rect.x - 9.0, rect.w - 18.0, 13.0, extend);
        } else {
            input.click(&mut self.fonts, x - rect.x - 9.0, rect.w - 18.0, 13.0, false, extend);
        }
    }
    pub fn settings_context(&self) -> Context<'_> {
        Context {
            system: self.system_preferences.clone(),
            pointer: self.pointer_config.clone(),
            power:self.power.preferences,power_ready:self.power.ready,
            pointer_managed: self.pointer_managed,
            keyboard: &self.keyboard_config,
            active: self.active_layout_index,
            animations: self.animations.enabled(),
            dock_zoom: self.shell.dock_zoom_enabled,
            scale: self.scale_factor,
            display_name: &self.display_name,
            monitors: self.monitors.iter().map(|m|m.info.clone()).collect(),
            modes: &self.display_modes,
            current: Mode {
                width: self.phys_w,
                height: self.phys_h,
                refresh: self.refresh_mhz,
            },
            direct: self.backend.is_drm(),
            wallpaper: self.shell.theme,
            wallpaper_image: self.shell.wallpaper_image.as_ref(),
            dnd: self.shell.dnd_active(),
            dnd_schedule: self.shell.dnd_schedule,
            notify_sounds: self.shell.notify_sounds,
            sounds: self.sound_preferences,
            audio: self.shell.cc.audio.clone(),
            wifi: self.shell.cc.wifi.clone(),
            bluetooth: self.shell.cc.bluetooth.clone(),
            hot_corners: self.hot_corners.actions,
            night: self.night.prefs,
            night_active: self.night.active(),
            notify_apps: &self.shell.notify_apps,
            screen_capture_policy: self.screen_capture_policy,
            screen_capture_cursor: self.screen_capture_cursor,
            screen_capture_apps: &self.screen_capture_apps,
            sticky_keys: self.sticky_keys,
        }
    }

    pub fn refresh_input_source(&mut self, index: usize) {
        let sources = self.keyboard_config.sources();
        let index = index.min(sources.len().saturating_sub(1));
        self.active_layout_index = index;
        self.active_layout_name = sources[index].short.into();
        self.shell
            .set_input_sources(sources, index, self.keyboard_config.shortcut.label());
        self.mark_builtin_windows_dirty();
        self.needs_redraw = true;
    }
    pub fn select_input_source(&mut self, index: usize) {
        self.cancel_key_repeat();
        if index >= self.keyboard_config.layouts.len() {
            return;
        }
        let keyboard = self.keyboard.clone();
        keyboard.with_xkb_state(self, |mut state| {
            state.set_layout(smithay::input::keyboard::Layout(index as u32))
        });
        self.refresh_input_source(index);
    }
    fn request_keyboard(&mut self, config: Keyboard) {
        self.cancel_key_repeat();
        if self.system_settings.keyboard_busy {
            return;
        }
        if let Some(worker) = &self.preferences {
            self.system_settings.keyboard_generation =
                self.system_settings.keyboard_generation.wrapping_add(1);
            self.system_settings.keyboard_busy = true;
            self.system_settings.error = None;
            worker.keyboard(self.system_settings.keyboard_generation, config);
        }
    }
    /// The room kept for the Dock changed: maximized windows take the new height
    pub fn refit_maximized_windows(&mut self) {
        let (sw, sh) = (self.shell.screen_w, self.shell.screen_h);
        for w in self.windows.iter_mut().filter(|w| w.maximized && !w.fullscreen) {
            let (ax, _, aw, ah) = crate::theme::maximized_area(sw, sh);
            w.x = ax;
            w.resize_client(aw, ah - w.titlebar_h());
        }
        self.dock_model_dirty = true;
        self.needs_redraw = true;
    }

    /// Programs learn the new repeat delay/rate at once (wl_keyboard.repeat_info)
    pub fn apply_key_repeat(&mut self) {
        self.cancel_key_repeat();
        let (rate, delay) = (self.keyboard_config.repeat_rate as i32, self.keyboard_config.repeat_delay as i32);
        let keyboard = self.keyboard.clone();
        keyboard.change_repeat_info(rate, delay);
        self.save_settings();
    }

    pub fn install_keyboard(
        &mut self,
        generation: u64,
        config: Keyboard,
        keymap: Result<String, String>,
    ) {
        if generation != self.system_settings.keyboard_generation {
            return;
        }
        let previous = self
            .keyboard_config
            .layouts
            .get(self.active_layout_index)
            .cloned();
        let keyboard = self.keyboard.clone();
        let result = keymap.and_then(|keymap| {
            keyboard
                .set_keymap_from_string(self, keymap)
                .map_err(|e| format!("Не удалось применить раскладки: {e}"))
        });
        self.system_settings.keyboard_busy = false;
        match result {
            Ok(()) => {
                let index = previous
                    .and_then(|id| config.layouts.iter().position(|s| *s == id))
                    .unwrap_or(0);
                let mut config = config;
                config.repeat_rate = self.keyboard_config.repeat_rate;
                config.repeat_delay = self.keyboard_config.repeat_delay;
                self.keyboard_config = config;
                self.select_input_source(index);
                self.save_settings();
            }
            Err(e) => self.system_settings.error = Some(e),
        }
        self.mark_builtin_windows_dirty();
        self.needs_redraw = true;
    }
    pub fn apply_system_setting(&mut self, action: Action) {
        self.system_settings.error = None;
        match action {
            Action::Audio(command) => {
                if let Err(error) = self.controls.as_ref().ok_or("Служба звука недоступна").and_then(|c| c.send(command)) {
                    self.system_settings.error = Some(error.into());
                }
            }
            Action::Network(command) => {
                self.shell.cc.wifi.error = None;
                match &command {
                    crate::controls::Control::Connect { .. } => self.shell.cc.wifi.connecting = true,
                    crate::controls::Control::Disconnect(_) => self.shell.cc.wifi.connecting = false,
                    _ => {}
                }
                if let Err(error) = self.controls.as_ref().ok_or("Служба сети недоступна").and_then(|c| c.send(command)) {
                    self.system_settings.error = Some(error.into());
                }
            }
            Action::Bluetooth(command) => {
                self.shell.cc.bluetooth.error = None;
                match &command {
                    crate::controls::Control::ConnectBluetooth(path) => {
                        self.shell.cc.bluetooth.connecting = Some(path.clone());
                    }
                    crate::controls::Control::DisconnectBluetooth(_)
                    | crate::controls::Control::ForgetBluetooth(_)
                    | crate::controls::Control::Bluetooth(false) => {
                        self.shell.cc.bluetooth.connecting = None;
                    }
                    _ => {}
                }
                if let Err(error) = self.controls.as_ref().ok_or("Служба Bluetooth недоступна").and_then(|c| c.send(command)) {
                    self.system_settings.error = Some(error.into());
                }
            }
            Action::WifiPassword(net) => {
                self.cancel_key_repeat();
                self.system_settings.wifi_password = feather_ui::input::TextInput::default().with_byte_limit(64);
                self.system_settings.wifi_target = Some(net);
            }
            Action::WifiPasswordFocus => {}
            Action::WifiPasswordCancel => {
                self.cancel_key_repeat();
                self.system_settings.wifi_target = None;
                self.system_settings.wifi_password.clear();
            }
            Action::WifiPasswordSubmit => {
                self.cancel_key_repeat();
                if let Some(network) = self.system_settings.wifi_target.take() {
                    let password: crate::controls::Secret = self.system_settings.wifi_password.take_text().into();
                    if !password.as_str().is_empty() {
                        self.shell.cc.wifi.error = None;
                        self.shell.cc.wifi.connecting = true;
                        if let Err(error) = self.controls.as_ref().ok_or("Служба сети недоступна").and_then(|c| {
                            c.send(crate::controls::Control::Connect {
                                network,
                                password: Some(password),
                            })
                        }) {
                            self.system_settings.error = Some(error.into());
                        }
                    }
                }
            }
            Action::OpenConnectionEditor => {
                self.execute_shell_action(crate::shell::ShellClickAction::LaunchApp(
                    crate::apps::AppAction::Exec("nm-connection-editor".into()),
                    "Настройки сети".into(),
                ));
            }
            Action::OpenBluetoothManager => {
                self.execute_shell_action(crate::shell::ShellClickAction::LaunchApp(
                    crate::apps::AppAction::Exec("blueman-manager".into()),
                    "Bluetooth".into(),
                ));
            }
            Action::LockNow=>self.lock_session(),
            Action::DoNotDisturb => {
                self.shell.toggle_dnd();
                self.save_settings();
            }
            Action::DndSchedule | Action::DndWindow => {
                let windows = crate::shell::DND_WINDOWS;
                self.shell.dnd_schedule = match (action, self.shell.dnd_schedule) {
                    (Action::DndSchedule, Some(_)) => None,
                    (Action::DndSchedule, None) => Some(windows[0]),
                    (_, current) => {
                        let i = current.and_then(|w| windows.iter().position(|o| *o == w)).map_or(0, |i| (i + 1) % windows.len());
                        Some(windows[i])
                    }
                };
                use chrono::Timelike;
                let now = chrono::Local::now();
                self.shell.update_dnd_schedule((now.hour() * 60 + now.minute()) as u16);
                self.save_settings();
            }
            Action::NightNow => {
                self.night.toggle();
                self.night.jump();
                self.apply_night_shift();
                self.save_settings();
            }
            Action::NightSchedule | Action::NightWindow => {
                let windows = crate::night::WINDOWS;
                self.night.prefs.schedule = match (action, self.night.prefs.schedule) {
                    (Action::NightSchedule, Some(_)) => None,
                    (Action::NightSchedule, None) => Some(windows[0]),
                    (_, current) => Some(windows[current.and_then(|w| windows.iter().position(|o| *o == w)).map_or(0, |i| (i + 1) % windows.len())]),
                };
                self.night_tick();
                self.night.jump();
                self.apply_night_shift();
                self.save_settings();
            }
            Action::NightWarmth => {
                let w = crate::night::WARMTH;
                let i = w.iter().position(|(k, _)| *k == self.night.prefs.kelvin).map_or(0, |i| (i + 1) % w.len());
                self.night.prefs.kelvin = w[i].0;
                self.night.jump();
                self.apply_night_shift();
                self.save_settings();
            }
            Action::GtkTheme => {
                self.gtk_theme = !self.gtk_theme;
                crate::gtk_theme::set_enabled(self.gtk_theme);
                if self.own_session {
                    if self.gtk_theme {
                        crate::gtk_theme::apply(crate::theme::appearance() == crate::theme::Appearance::Dark);
                    } else {
                        crate::gtk_theme::restore();
                    }
                } else {
                    self.system_settings.error = Some("Тема GTK меняется только в собственной сессии Пёрышка".into());
                }
                self.save_settings();
            }
            Action::DockSize(size) => {
                crate::theme::set_dock_icon(size as f32);
                self.shell.resize_dock();
                self.refit_maximized_windows();
                self.save_settings();
            }
            Action::DockEdge(edge) => {
                crate::theme::set_dock_edge(edge);
                self.shell.resize_dock();
                self.refit_maximized_windows();
                self.save_settings();
            }
            Action::DockAutohide => {
                let on = !crate::theme::dock_autohide();
                crate::theme::set_dock_autohide(on);
                self.shell.dock_hidden = on;
                self.animations.set_dock_hidden(on);
                self.shell.desktop.invalidate();
                self.refit_maximized_windows();
                self.save_settings();
            }
            Action::HotCorner(i) => {
                if let Some(a) = self.hot_corners.actions.get_mut(i) {
                    *a = a.next();
                    self.save_settings();
                }
            }
            Action::AlertSounds | Action::InterfaceSounds | Action::VolumeFeedback => {
                match action {
                    Action::AlertSounds=>self.sound_preferences.alerts=!self.sound_preferences.alerts,
                    Action::InterfaceSounds=>self.sound_preferences.interface=!self.sound_preferences.interface,
                    _=>self.sound_preferences.volume_feedback=!self.sound_preferences.volume_feedback,
                }
                self.save_settings();
            }
            Action::AlertTone => {
                self.sound_preferences.tone=self.sound_preferences.tone.next();
                self.save_settings();
                self.play_effect(crate::sounds::Effect::Alert(self.sound_preferences.tone),true);
            }
            Action::AlertVolume(value) => {
                self.sound_preferences.volume=value.min(100);self.save_settings();
                self.play_effect(crate::sounds::Effect::Alert(self.sound_preferences.tone),true);
            }
            Action::PreviewAlert => {self.play_effect(crate::sounds::Effect::Alert(self.sound_preferences.tone),true);}
            Action::NotifySounds => {
                self.shell.notify_sounds = !self.shell.notify_sounds;
                self.save_settings();
            }
            Action::NotifyDelivery(index) => {
                if let Some(a) = self.shell.notify_apps.get_mut(index) {
                    a.delivery = a.delivery.next();
                    self.save_settings();
                }
            }
            Action::IdleTime(index)=>{
                let p=&mut self.power.preferences;
                let value=match index {0=>&mut p.lock,1=>&mut p.blank,_=>&mut p.suspend};
                *value=crate::power::Preferences::next(*value);self.activity();self.rearm_idle();self.save_settings();
            }
            Action::Page(page) => {
                self.cancel_key_repeat();
                self.system_settings.shortcut_capture = None;
                self.system_settings.apps_picker = None;
                self.system_settings.autostart_picker = false;
                self.system_settings.autostart_query.clear();
                self.system_settings.privacy_picker = false;
                self.system_settings.privacy_query.clear();
                self.system_settings.cursor_picker = false;
                self.system_settings.zone_picker = false;
                self.system_settings.time_editing = false;
                self.system_settings.wifi_target = None;
                self.system_settings.wifi_password.clear();
                self.system_settings.audio_drag = None;
                self.system_settings.page = page;
                self.system_settings.adding = false;
                self.system_settings.scroll = 0.0;
                self.open_or_focus_builtin(crate::apps::BuiltinWindowKind::SystemSettings);
                if page == Page::Sound {
                    if let Some(controls) = &self.controls { let _ = controls.send(crate::controls::Control::RefreshAudio); }
                }
                if page == Page::Network {
                    if let Some(controls) = &self.controls { let _ = controls.send(crate::controls::Control::RefreshNetwork); }
                }
                if page == Page::Bluetooth {
                    if let Some(controls) = &self.controls { let _ = controls.send(crate::controls::Control::RefreshBluetooth); }
                }
                if matches!(page, Page::Defaults | Page::Privacy | Page::DateTime | Page::Pointer) {
                    self.request_system_preference(crate::system_preferences::Command::Refresh);
                }
            }
            Action::ScreenCapturePolicy(policy) => {
                if self.screen_capture_policy != policy {
                    self.screen_capture_policy = policy;
                    self.save_settings();
                }
            }
            Action::ScreenCaptureCursor => {
                self.screen_capture_cursor = !self.screen_capture_cursor;
                self.save_settings();
            }
            Action::ScreenCapturePicker(open) => {
                self.system_settings.privacy_picker = open;
                self.system_settings.privacy_query.clear();
                self.system_settings.scroll = 0.0;
            }
            Action::PrivacySearchFocus => {}
            Action::PrivacySearchSubmit => {
                let query = self.system_settings.privacy_query.to_lowercase();
                if let Some(id) = self
                    .system_preferences
                    .apps
                    .iter()
                    .find(|a| app_matches(a, &query))
                    .map(|a| a.id.clone())
                {
                    self.apply_system_setting(Action::AddScreenCaptureApp(id));
                }
            }
            Action::AddScreenCaptureApp(id) => {
                let norm = crate::file_portal::normalize_app_key(&id);
                if !norm.is_empty() {
                    let label = self
                        .system_preferences
                        .apps
                        .iter()
                        .find(|a| crate::file_portal::same_app_id(&a.id, &norm))
                        .map(|a| crate::file_portal::clean_app_field(&a.name))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| norm.clone());
                    if let Some(existing) = self
                        .screen_capture_apps
                        .iter_mut()
                        .find(|a| crate::file_portal::same_app_id(&a.id, &norm))
                    {
                        existing.allowed = true;
                    } else if self.screen_capture_apps.len() < crate::file_portal::MAX_CAPTURE_APPS {
                        self.screen_capture_apps.push(crate::file_portal::ScreenCaptureApp {
                            id: norm.clone(),
                            label,
                            allowed: true,
                        });
                    }
                    if let Some(w) = &self.system_worker {
                        let _ = w.send(crate::system_preferences::Command::SyncPortalPermission {
                            id: norm,
                            allowed: Some(true),
                        });
                    }
                    self.save_settings();
                }
                self.system_settings.privacy_picker = false;
                self.system_settings.privacy_query.clear();
                self.system_settings.scroll = 0.0;
            }
            Action::ToggleScreenCaptureApp(id) => {
                if let Some(app) = self
                    .screen_capture_apps
                    .iter_mut()
                    .find(|a| crate::file_portal::same_app_id(&a.id, &id))
                {
                    app.allowed = !app.allowed;
                    let allowed = app.allowed;
                    let norm = app.id.clone();
                    if let Some(w) = &self.system_worker {
                        let _ = w.send(crate::system_preferences::Command::SyncPortalPermission {
                            id: norm,
                            allowed: Some(allowed),
                        });
                    }
                    self.save_settings();
                }
            }
            Action::RemoveScreenCaptureApp(id) => {
                let before = self.screen_capture_apps.len();
                self.screen_capture_apps
                    .retain(|a| !crate::file_portal::same_app_id(&a.id, &id));
                if self.screen_capture_apps.len() != before {
                    if let Some(w) = &self.system_worker {
                        let _ = w.send(crate::system_preferences::Command::SyncPortalPermission {
                            id: crate::file_portal::normalize_app_key(&id),
                            allowed: None,
                        });
                    }
                    self.save_settings();
                }
            }
            Action::ClearScreenCaptureApps => {
                if !self.screen_capture_apps.is_empty() {
                    let removed: Vec<String> =
                        self.screen_capture_apps.drain(..).map(|a| a.id).collect();
                    if let Some(w) = &self.system_worker {
                        for id in removed {
                            let _ = w.send(
                                crate::system_preferences::Command::SyncPortalPermission {
                                    id,
                                    allowed: None,
                                },
                            );
                        }
                    }
                    self.save_settings();
                }
            }
            Action::AppPicker(browser) => {
                self.system_settings.autostart_picker = false;
                self.system_settings.apps_picker =
                    if self.system_settings.apps_picker == Some(browser) {
                        None
                    } else {
                        Some(browser)
                    };
                self.system_settings.scroll = 0.0;
                self.system_settings.app_count = self
                    .system_preferences
                    .apps
                    .iter()
                    .filter(|a| if browser { a.browser } else { a.terminal })
                    .count();
            }
            Action::DefaultApp { browser, id } => {
                self.request_system_preference(crate::system_preferences::Command::DefaultApp {
                    browser,
                    id,
                });
                self.system_settings.apps_picker = None;
                self.system_settings.scroll = 0.0;
            }
            Action::AutostartPicker(open) => {
                self.system_settings.apps_picker = None;
                self.system_settings.autostart_picker = open;
                self.system_settings.autostart_query.clear();
                self.system_settings.scroll = 0.0;
            }
            Action::AutostartSearchFocus => {}
            Action::AutostartSearchSubmit => {
                let query = self.system_settings.autostart_query.to_lowercase();
                if let Some(id) = self
                    .system_preferences
                    .apps
                    .iter()
                    .find(|a| app_matches(a, &query))
                    .map(|a| a.id.clone())
                {
                    self.apply_system_setting(Action::AddAutostart(id));
                }
            }
            Action::AddAutostart(id) => {
                self.request_system_preference(crate::system_preferences::Command::AddAutostart(
                    id,
                ));
                self.system_settings.autostart_picker = false;
                self.system_settings.autostart_query.clear();
                self.system_settings.scroll = 0.0;
            }
            Action::ToggleAutostart { id, enabled } => {
                self.request_system_preference(
                    crate::system_preferences::Command::ToggleAutostart { id, enabled },
                );
            }
            Action::RemoveAutostart(id) => {
                self.request_system_preference(
                    crate::system_preferences::Command::RemoveAutostart(id),
                );
            }
            Action::RefreshSystem => {
                self.request_system_preference(crate::system_preferences::Command::Refresh)
            }
            Action::Ntp => self.request_system_preference(crate::system_preferences::Command::Ntp(
                !self.system_preferences.time.ntp,
            )),
            Action::ZonePicker => {
                self.system_settings.zone_picker = !self.system_settings.zone_picker;
                self.system_settings.zone_query.clear();
                self.system_settings.scroll = 0.0;
                self.system_settings.time_editing = false;
            }
            Action::ZoneSearchFocus => {}
            Action::Timezone(zone) => {
                self.request_system_preference(crate::system_preferences::Command::Timezone(zone));
                self.system_settings.zone_picker = false;
                self.system_settings.scroll = 0.0;
            }
            Action::ZoneSearchSubmit => {
                let query = self.system_settings.zone_query.to_lowercase();
                if let Some(zone) = self
                    .system_preferences
                    .zones
                    .iter()
                    .find(|z| zone_matches(z, &query))
                    .cloned()
                {
                    self.apply_system_setting(Action::Timezone(zone));
                }
            }
            Action::EditTime => {
                if self.system_settings.time_input.is_empty() {
                    self.system_settings.time_input =
                        self.system_preferences.time.local_input.clone().into();
                }
                self.system_settings.time_editing = true;
            }
            Action::ApplyTime => {
                self.system_settings.time_editing = false;
                self.request_system_preference(crate::system_preferences::Command::SetTime(
                    self.system_settings.time_input.text().to_owned(),
                ));
            }
            Action::Appearance(a) => {
                if appearance() != a {
                    self.toggle_appearance();
                }
            }
            Action::AccentColor(color) => {
                if crate::theme::accent_color() != color {
                    crate::theme::set_accent_color(color);
                    self.shell.invalidate_all();
                    self.mark_builtin_windows_dirty();
                    if self.own_session && self.gtk_theme {
                        crate::gtk_theme::apply(
                            crate::theme::appearance() == crate::theme::Appearance::Dark,
                        );
                    }
                    self.save_settings();
                }
            }
            Action::HighlightColor(choice) => {
                if crate::theme::highlight_choice() != choice {
                    crate::theme::set_highlight_choice(choice);
                    self.shell.desktop.invalidate();
                    self.mark_builtin_windows_dirty();
                    self.save_settings();
                }
            }
            Action::TextSize(size) => {
                if crate::theme::text_size() != size {
                    crate::theme::set_text_size(size);
                    self.shell.invalidate_all();
                    self.mark_builtin_windows_dirty();
                    self.lock.repaint();
                    self.save_settings();
                }
            }
            Action::HighContrast => {
                crate::theme::set_high_contrast(!crate::theme::high_contrast());
                self.shell.invalidate_all();
                self.mark_builtin_windows_dirty();
                self.lock.repaint();
                self.save_settings();
            }
            Action::ReduceTransparency => {
                crate::theme::set_reduce_transparency(!crate::theme::reduce_transparency());
                self.shell.invalidate_all();
                self.mark_builtin_windows_dirty();
                self.lock.repaint();
                self.save_settings();
            }
            Action::StickyKeys => {
                self.sticky_keys = !self.sticky_keys;
                if !self.sticky_keys {
                    let time = self.start_time.elapsed().as_millis() as u32;
                    self.release_latched_modifiers(time);
                }
                self.save_settings();
            }
            Action::Language(lang) => {
                if crate::theme::lang() != lang {
                    crate::theme::set_lang(lang);
                    self.app_registry = crate::apps::AppRegistry::new();
                    self.shell.last_clock_str = crate::shell::MacShell::format_ru_clock();
                    self.dock_model_dirty = true;
                    self.shell.invalidate_all();
                    self.mark_builtin_windows_dirty();
                    self.lock.repaint();
                    self.save_settings();
                }
            }
            Action::RegionStep(delta) => {
                let next = crate::theme::region().step(delta);
                crate::theme::set_region(next);
                self.shell.last_clock_str = crate::shell::MacShell::format_ru_clock();
                self.shell.topbar_dirty = true;
                self.lock.repaint();
                self.save_settings();
            }
            Action::Use24HourTime => {
                crate::theme::set_use_24h_time(!crate::theme::use_24h_time());
                self.shell.last_clock_str = crate::shell::MacShell::format_ru_clock();
                self.shell.topbar_dirty = true;
                self.lock.repaint();
                self.save_settings();
            }
            Action::FirstWeekdayMonday(monday) => {
                if crate::theme::first_weekday_monday() != monday {
                    crate::theme::set_first_weekday_monday(monday);
                    self.save_settings();
                }
            }
            Action::MetricUnits(metric) => {
                if crate::theme::metric_units() != metric {
                    crate::theme::set_metric_units(metric);
                    self.save_settings();
                }
            }
            Action::Animations => {
                let enabled = !self.animations.enabled();
                self.animations
                    .set_enabled(enabled, &self.windows, &self.shell);
                self.shell.pending_bounce = None;
                if self.shell.bounce.take().is_some() {
                    self.shell.dock_dirty = true;
                }
                self.save_settings();
            }
            Action::DockZoom => {
                self.execute_shell_action(crate::shell::ShellClickAction::ToggleDockZoom)
            }
            Action::Wallpaper => {
                self.execute_shell_action(crate::shell::ShellClickAction::CycleTheme)
            }
            Action::WallpaperImage(image) => self.set_wallpaper_image(image),
            Action::ChooseWallpaper => {
                if !self.system_settings.wallpaper_busy
                    && !self.system_settings.screenshot_dir_busy
                    && self.wallpaper_picker.as_ref().is_some_and(|p| p.open())
                {
                    self.system_settings.wallpaper_busy = true;
                }
            }
            Action::ScreenshotFormat(fmt) => {
                if crate::screenshot::format() != fmt {
                    crate::screenshot::set_format(fmt);
                    self.save_settings();
                }
            }
            Action::ChooseScreenshotDir => {
                if !self.system_settings.screenshot_dir_busy
                    && !self.system_settings.wallpaper_busy
                    && self
                        .wallpaper_picker
                        .as_ref()
                        .is_some_and(|p| p.open_screenshot_dir(crate::screenshot::custom_dir()))
                {
                    self.system_settings.screenshot_dir_busy = true;
                }
            }
            Action::ResetScreenshotDir => {
                if crate::screenshot::custom_dir().is_some() {
                    crate::screenshot::set_custom_dir(None);
                    self.save_settings();
                }
            }
            Action::AddPicker(open) => {
                self.system_settings.adding = open;
                self.system_settings.scroll = 0.0;
            }
            Action::SelectSource(index) => self.select_input_source(index),
            Action::AddSource(id) => {
                let mut config = self.keyboard_config.clone();
                if crate::settings::source(&id).is_some()
                    && config.layouts.len() < 4
                    && !config.layouts.contains(&id)
                {
                    config.layouts.push(id);
                    self.request_keyboard(config);
                }
            }
            Action::RemoveSource(index) => {
                let mut config = self.keyboard_config.clone();
                if config.layouts.len() > 1 && index < config.layouts.len() {
                    config.layouts.remove(index);
                    self.request_keyboard(config);
                }
            }
            Action::Shortcut(shortcut) => {
                let mut config = self.keyboard_config.clone();
                config.shortcut = shortcut;
                self.request_keyboard(config);
            }
            Action::EditShortcut(action) => {
                self.cancel_key_repeat();
                self.system_settings.shortcut_capture = Some(action);
            }
            Action::ResetShortcuts => {
                self.keybindings.reset();
                self.system_settings.keybindings = self.keybindings.clone();
                self.system_settings.shortcut_capture = None;
                self.save_settings();
            }
            Action::CapsLock => {
                let mut config = self.keyboard_config.clone();
                config.caps = config.caps.next();
                self.request_keyboard(config);
            }
            Action::RepeatDelay(delay) => {
                self.keyboard_config.repeat_delay = delay;
                self.apply_key_repeat();
            }
            Action::RepeatRate(rate) => {
                self.keyboard_config.repeat_rate = rate;
                self.apply_key_repeat();
            }
            Action::CursorPicker => {
                self.system_settings.cursor_picker = !self.system_settings.cursor_picker;
            }
            Action::CursorTheme(theme) => {
                self.system_settings.cursor_picker = false;
                if theme.as_deref().is_none_or(crate::settings::valid_theme) {
                    self.pointer_config.cursor_theme = theme;
                    self.apply_pointer_config();
                    self.save_settings();
                }
            }
            Action::CursorSize(size) => {
                if crate::settings::CURSOR_SIZES.contains(&size) {
                    self.pointer_config.cursor_size = size;
                    self.apply_pointer_config();
                    self.save_settings();
                }
            }
            Action::PointerSpeed(speed) => {
                if speed.is_finite() {
                    self.pointer_config.speed = speed.clamp(-1.0, 1.0);
                    self.apply_pointer_config();
                    self.save_settings();
                }
            }
            Action::NaturalMouse | Action::NaturalTouchpad | Action::TapToClick | Action::LeftHanded => {
                let p = &mut self.pointer_config;
                let flag = match action {
                    Action::NaturalMouse => &mut p.natural_mouse,
                    Action::NaturalTouchpad => &mut p.natural_touchpad,
                    Action::TapToClick => &mut p.tap_to_click,
                    _ => &mut p.left_handed,
                };
                *flag = !*flag;
                self.apply_pointer_config();
                self.save_settings();
            }
            Action::Scale(scale) => {
                if !scale.is_finite() || !(1.0..=2.5).contains(&scale) {
                    return;
                }
                if self.phys_w as f64 / scale < 640.0 || self.phys_h as f64 / scale < 480.0 {
                    self.system_settings.error =
                        Some("На экране недостаточно места для такого масштаба".into());
                } else {
                    self.explicit_scale = Some(scale);
                    self.apply_hidpi_scale(scale);
                    self.save_settings();
                }
            }
            Action::PrimaryMonitor(name)=>self.select_primary_monitor(&name),
            Action::PositionMonitor(name,side)=>self.position_monitor(&name,side),
            Action::ModeStep(delta) => {
                if !self.display_modes.is_empty() && !self.system_settings.confirming {
                    let current = self.system_settings.selected_mode.unwrap_or_else(|| {
                        self.display_modes
                            .iter()
                            .position(|m| *m == self.settings_context().current)
                            .unwrap_or(0)
                    });
                    self.system_settings.selected_mode = Some(
                        (current as i32 + delta).rem_euclid(self.display_modes.len() as i32)
                            as usize,
                    );
                }
            }
            Action::ApplyMode => {
                if let Some(i) = self.system_settings.selected_mode {
                    if self.backend.is_drm() && self.pending_display.is_none() {
                        self.system_settings.queued_mode = Some((i, true));
                    }
                }
            }
            Action::KeepMode => {
                if self.pending_display.take().is_some() {
                    self.system_settings.queued_mode = None;
                    self.system_settings.confirming = false;
                    self.explicit_scale = Some(self.scale_factor);
                    self.display_preference = Some(crate::settings::Display {
                        name: self.display_name.clone(),
                        mode: self.settings_context().current,
                    });
                    self.save_settings();
                }
            }
            Action::RevertMode => {
                if let Some((i, _, _)) = self.pending_display {
                    self.system_settings.queued_mode = Some((i, false));
                }
            }
        }
        if self.system_settings.error.is_some() {self.play_effect(crate::sounds::Effect::Alert(self.sound_preferences.tone),false);}
        self.mark_settings_dirty();
        self.needs_redraw = true;
    }
    /// Pointer moved: repaint only when the control under it changes.
    pub fn settings_pointer_moved(&mut self, x: f64, y: f64) {
        let Some(win) = self.windows.iter().rev().find(|w| w.is_visible() && w.app_id == "builtin.settings") else {
            return;
        };
        let inside = self.windows.iter().rev().find(|w| w.is_visible() && w.contains_point(x, y)).is_some_and(|w| w.id == win.id);
        let (w, h) = (win.width as f32, win.height as f32);
        let local = inside.then(|| (x as f32 - win.x as f32, y as f32 - (win.y + win.titlebar_h()) as f32));
        if local == self.system_settings.pointer {
            return;
        }
        let before = self.system_settings.hover_key(w, h, &self.settings_context());
        self.system_settings.pointer = local;
        if self.system_settings.hover_key(w, h, &self.settings_context()) != before {
            self.mark_settings_dirty();
            self.needs_redraw = true;
        }
    }

    pub fn mark_settings_dirty(&mut self) {
        for w in &mut self.windows {
            if w.app_id == "builtin.settings" {
                w.dirty = true;
            }
        }
    }
    pub fn request_system_preference(&mut self, command: crate::system_preferences::Command) {
        if self.system_preferences.busy {
            return;
        }
        if self.system_worker.as_ref().is_some_and(|w| w.send(command)) {
            self.system_preferences.busy = true;
            self.system_preferences.error = None;
            self.system_settings.error = None;
        }
    }
    pub fn set_wallpaper_image(&mut self, image: Option<crate::wallpaper::ImageSource>) {
        self.shell.wallpaper_image = image;
        self.shell.invalidate_wallpaper();
        self.mark_builtin_windows_dirty();
        self.needs_redraw = true;
    }
    pub fn refresh_display_choices(&mut self) {
        if let crate::backend::Backend::Drm(d) = &self.backend {
            self.display_name = d.connector_name.clone();
            self.display_modes = d
                .modes
                .iter()
                .copied()
                .map(crate::drm_backend::mode_info)
                .filter(|m| m.width >= 640 && m.height >= 480)
                .collect();
            self.display_modes.sort_by_key(|m| {
                (
                    std::cmp::Reverse(m.width),
                    std::cmp::Reverse(m.height),
                    std::cmp::Reverse(m.refresh),
                )
            });
            self.display_modes.dedup();
        } else {
            self.display_name = "Оконный дисплей".into();
            self.display_modes = vec![self.settings_context().current];
        }
    }
    /// Mode changes wait for the previous frame. KMS never waits for a vblank here.
    pub fn flush_display_mode(&mut self) {
        if self.frame_pending {
            return;
        }
        if !matches!(&self.backend,crate::backend::Backend::Drm(d)if d.active) {
            return;
        }
        let Some((index, confirm)) = self.system_settings.queued_mode.take() else {
            return;
        };
        let Some(target) = self.display_modes.get(index).copied() else {
            return;
        };
        let previous = self
            .display_modes
            .iter()
            .position(|m| *m == self.settings_context().current)
            .unwrap_or(0);
        let previous_scale = self.scale_factor;
        let result = if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if let Some(mode) = d
                .modes
                .iter()
                .copied()
                .find(|m| crate::drm_backend::mode_info(*m) == target)
            {
                let Some(head)=d.heads.iter_mut().find(|h|h.name==d.connector_name)else{return};
                let result = head.output
                    .use_mode::<_, crate::render::OutputElement>(
                        mode,
                        &mut d.renderer,
                        &smithay::backend::drm::output::DrmOutputRenderElements::default(),
                    )
                    .map_err(|e| format!("Не удалось применить режим: {e:?}"));
                if result.is_ok() {
                    d.size = (target.width, target.height);
                    d.refresh = target.refresh;
                    head.size=d.size;head.refresh=d.refresh;
                    head.output.with_compositor(|c| {
                        c.set_output_mode_source(smithay::output::OutputModeSource::Static {
                            size: d.size.into(),
                            scale: 1.0.into(),
                            transform: smithay::utils::Transform::Normal,
                        })
                    });
                    head.output.reset_buffers();head.pending=false;head.protected=false;
                }
                result
            } else {
                Err("Режим больше недоступен".into())
            }
        } else {
            return;
        };
        if let Err(e) = result {
            self.system_settings.error = Some(e);
            self.mark_builtin_windows_dirty();
            self.needs_redraw = true;
            return;
        }
        if let Some(m)=self.monitors.iter_mut().find(|m|m.info.primary){m.info.mode=target;}
        self.phys_w = target.width;
        self.phys_h = target.height;
        self.refresh_mhz = target.refresh;
        // A modeset may bring back the identity gamma table
        self.apply_night_shift();
        let scale = if confirm {
            self.scale_factor
                .min((target.width as f64 / 640.0).min(target.height as f64 / 480.0))
                .max(1.0)
        } else {
            self.pending_display.map_or(self.scale_factor, |p| p.2)
        };
        let pending=self.pending_display;
        self.apply_hidpi_scale(scale);
        self.pending_display=pending;
        self.system_settings.selected_mode = Some(index);
        if confirm {
            self.display_generation = self.display_generation.wrapping_add(1);
            let generation = self.display_generation;
            self.pending_display = Some((previous, generation, previous_scale));
            self.system_settings.confirming = true;
            let _ = self.loop_handle.insert_source(
                smithay::reexports::calloop::timer::Timer::from_duration(
                    std::time::Duration::from_secs(15),
                ),
                move |_, _, state| {
                    if state.pending_display == Some((previous, generation, previous_scale)) {
                        state.system_settings.queued_mode = Some((previous, false));
                        state.needs_redraw = true;
                    }
                    smithay::reexports::calloop::timer::TimeoutAction::Drop
                },
            );
        } else {
            self.pending_display = None;
            self.system_settings.confirming = false;
        }
        self.mark_builtin_windows_dirty();
        self.needs_redraw = true;
    }
}

/// A macOS-like source glyph, shared by the menu bar, menu and Settings.
pub fn draw_input_badge(
    pm: &mut PixmapMut,
    fonts: &mut FontSystem,
    label: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: tiny_skia::Color,
) {
    fill_rounded_rect(pm, x, y, w, h, 3.0, color);
    // Letters knocked out of the badge: dark on a light badge, light on a dark one. Taken
    // from the badge itself (white on a dark wallpaper or a selected menu row), not the theme.
    let luma = 0.2126 * color.red() + 0.7152 * color.green() + 0.0722 * color.blue();
    let ink = if luma > 0.5 {
        rgba(24, 25, 30, 255)
    } else {
        rgba(250, 251, 253, 255)
    };
    fonts.draw_text_centered(
        pm,
        label,
        x + w / 2.0,
        y + (h - 10.0) / 2.0 - 0.5,
        10.0,
        ink,
        true,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn date_entry_search_and_ntp_guard_use_visible_actions() {
        use smithay::input::keyboard::keysyms;
        let mut panel = Panel {
            page: Page::DateTime,
            time_editing: true,
            time_input: "old".into(),
            ..Default::default()
        };
        panel.edit_key(keysyms::KEY_a, Some('a'), true, false);
        assert_eq!(panel.time_input.selected_text(), Some("old"));
        for ch in "2026-10-24 12:30".chars() {
            panel.edit_key(0, Some(ch), false, false);
        }
        assert_eq!(panel.time_input.text(), "2026-10-24 12:30");
        panel.edit_key(0, Some('X'), false, false);
        assert_eq!(panel.time_input.text(), "2026-10-24 12:30");
        assert!(matches!(
            panel.edit_key(keysyms::KEY_Return, None, false, false),
            Some(Action::ApplyTime)
        ));
        assert!(!panel.editing());
        panel.zone_picker = true;
        for ch in "Барнаул".chars() {
            panel.edit_key(0, Some(ch), false, false);
        }
        assert!(zone_matches(
            "Asia/Barnaul",
            &panel.zone_query.to_lowercase()
        ));
        assert!(!zone_matches(
            "Europe/Moscow",
            &panel.zone_query.to_lowercase()
        ));
        panel.edit_key(keysyms::KEY_Escape, None, false, false);
        assert!(!panel.editing());
    }
    #[test]
    fn all_pages_paint_at_fractional_scale_and_clicks_use_the_painted_geometry() {
        let keyboard = Keyboard::default();
        let apps: Vec<_> = ["Telegram", "Firefox"].iter().map(|n| crate::notifications::AppPolicy { key: n.to_lowercase(), label: n.to_string(), delivery: Default::default() }).collect();
        let capture_apps = [
            crate::file_portal::ScreenCaptureApp {
                id: "com.obsproject.Studio".into(),
                label: "OBS Studio".into(),
                allowed: true,
            },
            crate::file_portal::ScreenCaptureApp {
                id: "org.mozilla.firefox".into(),
                label: "Firefox".into(),
                allowed: false,
            },
        ];
        let mode = Mode {
            width: 3840,
            height: 2160,
            refresh: 59940,
        };
        let context = Context {
            system: Default::default(),
            pointer: Default::default(),
            power:Default::default(),power_ready:false,
            pointer_managed: true,
            keyboard: &keyboard,
            active: 1,
            animations: true,
            dock_zoom: true,
            scale: 2.0,
            display_name: "DP-1",
            monitors: vec![crate::monitors::Info{name:"DP-1".into(),rect:crate::monitors::Rect{x:0,y:0,w:1920,h:1080},mode,primary:true},crate::monitors::Info{name:"HDMI-A-1".into(),rect:crate::monitors::Rect{x:-1280,y:0,w:1280,h:720},mode:Mode{width:2560,height:1440,refresh:60000},primary:false}],
            modes: &[mode],
            current: mode,
            direct: false,
            wallpaper: WallpaperTheme::for_appearance(Appearance::Light),
            wallpaper_image: None,
            dnd: false,
            dnd_schedule: Some((1320, 420)),
            notify_sounds: true,
            sounds: Default::default(),
            audio: Default::default(),
            wifi: Default::default(),
            bluetooth: Default::default(),
            hot_corners: [crate::hot_corners::HotAction::Launchpad, Default::default(), Default::default(), crate::hot_corners::HotAction::Lock],
            night: crate::night::Preferences { manual: false, schedule: Some((1320, 420)), kelvin: 3400 },
            night_active: true,
            notify_apps: &apps,
            sticky_keys: false,
            screen_capture_policy: crate::file_portal::ScreenCapturePolicy::Ask,
            screen_capture_cursor: true,
            screen_capture_apps: &capture_apps,
        };
        let mut context = context;
        context.audio = crate::controls::Audio {
            default: "speakers".into(),
            sinks: vec![crate::controls::Sink { name:"speakers".into(), label:"Встроенные динамики".into(), channels:vec![32768,32768], ..Default::default() }],
            default_source: "builtin".into(),
            sources: vec![
                crate::controls::Sink {name:"builtin".into(),label:"Встроенный микрофон".into(),channels:vec![49152,24576],unplugged:true,..Default::default()},
                crate::controls::Sink {name:"usb".into(),label:"USB · Студийный микрофон".into(),channels:vec![32768],..Default::default()},
            ],
            ..Default::default()
        };
        context.wifi = crate::controls::network::Wifi {
            available: true,
            enabled: true,
            hardware: true,
            connected: Some("Feather-5G".into()),
            connecting: false,
            networks: vec![
                crate::controls::network::Network {
                    ssid: b"Feather-5G".to_vec(),
                    label: "Feather-5G".into(),
                    ap: "/ap/home".into(),
                    device: "/dev/wlan0".into(),
                    saved: Some("/settings/1".into()),
                    security: crate::controls::network::Security::Sae,
                    strength: 92,
                    active: true,
                },
                crate::controls::network::Network {
                    ssid: "Гостевая сеть".as_bytes().to_vec(),
                    label: "Гостевая сеть".into(),
                    ap: "/ap/guest".into(),
                    device: "/dev/wlan0".into(),
                    saved: None,
                    security: crate::controls::network::Security::Psk,
                    strength: 71,
                    active: false,
                },
            ],
            saved_wifi: vec![
                crate::controls::network::SavedWifi {
                    profile: "/settings/1".into(),
                    ssid: b"Feather-5G".to_vec(),
                    label: "Feather-5G".into(),
                    security: crate::controls::network::Security::Sae,
                    autoconnect: true,
                    active: true,
                    in_range: true,
                },
                crate::controls::network::SavedWifi {
                    profile: "/settings/2".into(),
                    ssid: "Дача".as_bytes().to_vec(),
                    label: "Дача".into(),
                    security: crate::controls::network::Security::Psk,
                    autoconnect: false,
                    active: false,
                    in_range: false,
                },
            ],
            ethernets: vec![crate::controls::network::Ethernet {
                device: "/dev/eth0".into(),
                interface: "enp3s0".into(),
                label: "Проводное подключение 1".into(),
                profile: Some("/settings/eth0".into()),
                carrier: true,
                speed: 1000,
                active: true,
                connecting: false,
            }],
            vpns: vec![crate::controls::network::Vpn {
                profile: "/settings/wg0".into(),
                label: "Офис · WireGuard".into(),
                kind: "WireGuard".into(),
                active_path: None,
                active: false,
                connecting: false,
            }],
            error: None,
        };
        context.bluetooth = crate::controls::bluetooth::Bluetooth {
            available: true,
            powered: true,
            discovering: true,
            adapter_name: "Feather".into(),
            connected: Some("AirPods Pro".into()),
            connecting: None,
            devices: vec![
                crate::controls::bluetooth::BtDevice {
                    path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01".into(),
                    adapter: "/org/bluez/hci0".into(),
                    address: "AA:BB:CC:DD:EE:01".into(),
                    name: "AirPods Pro".into(),
                    icon: "audio-headphones".into(),
                    paired: true,
                    trusted: true,
                    connected: true,
                    rssi: None,
                    battery: Some(85),
                },
                crate::controls::bluetooth::BtDevice {
                    path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into(),
                    adapter: "/org/bluez/hci0".into(),
                    address: "AA:BB:CC:DD:EE:02".into(),
                    name: "Keychron K2".into(),
                    icon: "input-keyboard".into(),
                    paired: true,
                    trusted: true,
                    connected: false,
                    rssi: None,
                    battery: Some(64),
                },
                crate::controls::bluetooth::BtDevice {
                    path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03".into(),
                    adapter: "/org/bluez/hci0".into(),
                    address: "AA:BB:CC:DD:EE:03".into(),
                    name: "MX Master 3S".into(),
                    icon: "input-mouse".into(),
                    paired: false,
                    trusted: false,
                    connected: false,
                    rssi: Some(-52),
                    battery: None,
                },
            ],
            error: None,
        };
        {
            use smithay::input::keyboard::keysyms;
            use crate::controls::Control as Net;
            let mut panel = Panel { page: Page::Network, ..Default::default() };
            let controls = panel.controls(800.0, 540.0, &context);
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::WifiPassword(n)) if n.label == "Гостевая сеть")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Network(Net::SetAutoconnect { profile, autoconnect: false })) if profile == "/settings/1")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Network(Net::ForgetConnection(p))) if p == "/settings/2")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Network(Net::ConnectVpn(p))) if p == "/settings/wg0")));
            panel.wifi_target = Some(context.wifi.networks[1].clone());
            assert!(panel.editing());
            assert!(panel.edit_key(keysyms::KEY_Return, None, false, false).is_none());
            for ch in "correct-horse-battery".chars() {
                panel.edit_key(0, Some(ch), false, false);
            }
            assert_eq!(panel.wifi_password.text(), "correct-horse-battery");
            assert!(matches!(
                panel.edit_key(keysyms::KEY_Return, None, false, false),
                Some(Action::WifiPasswordSubmit)
            ));
            panel.edit_key(keysyms::KEY_Escape, None, false, false);
            assert!(!panel.editing());
            assert!(panel.wifi_password.is_empty());
        }
        {
            use crate::controls::Control as Bt;
            let panel = Panel { page: Page::Bluetooth, ..Default::default() };
            let controls = panel.controls(800.0, 540.0, &context);
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Bluetooth(Bt::Bluetooth(false))))));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Bluetooth(Bt::ScanBluetooth)))));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Bluetooth(Bt::DisconnectBluetooth(p))) if p == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Bluetooth(Bt::ConnectBluetooth(p))) if p == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Bluetooth(Bt::ForgetBluetooth(p))) if p == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::Bluetooth(Bt::ConnectBluetooth(p))) if p == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03")));
        }
        {
            use crate::theme::{AccentColor, TextSize};
            let panel = Panel { page: Page::Appearance, ..Default::default() };
            let controls = panel.controls(800.0, 540.0, &context);
            for color in AccentColor::ALL {
                assert!(controls.iter().any(|c| matches!(c.action, Some(Action::AccentColor(ac)) if ac == color)));
                assert!(controls.iter().any(|c| matches!(c.action, Some(Action::HighlightColor(Some(hc))) if hc == color)));
            }
            assert!(controls.iter().any(|c| matches!(c.action, Some(Action::HighlightColor(None)))));
            for ts in TextSize::ALL {
                assert!(controls.iter().any(|c| matches!(c.action, Some(Action::TextSize(s)) if s == ts)));
            }
            let acc_panel = Panel { page: Page::Accessibility, ..Default::default() };
            let acc_controls = acc_panel.controls(800.0, 540.0, &context);
            for ts in TextSize::ALL {
                assert!(acc_controls.iter().any(|c| matches!(c.action, Some(Action::TextSize(s)) if s == ts)));
            }
            assert!(acc_controls.iter().any(|c| matches!(c.action, Some(Action::HighContrast))));
            assert!(acc_controls.iter().any(|c| matches!(c.action, Some(Action::ReduceTransparency))));
            assert!(acc_controls.iter().any(|c| matches!(c.action, Some(Action::StickyKeys))));
            let lang_panel = Panel { page: Page::Language, ..Default::default() };
            let lang_controls = lang_panel.controls(800.0, 540.0, &context);
            for l in crate::theme::Lang::ALL {
                assert!(lang_controls.iter().any(|c| matches!(c.action, Some(Action::Language(lang)) if lang == l)));
            }
            assert!(lang_controls.iter().any(|c| matches!(c.action, Some(Action::RegionStep(-1)))));
            assert!(lang_controls.iter().any(|c| matches!(c.action, Some(Action::RegionStep(1)))));
            assert!(lang_controls.iter().any(|c| matches!(c.action, Some(Action::Use24HourTime))));
            assert!(lang_controls.iter().any(|c| matches!(c.action, Some(Action::FirstWeekdayMonday(true)))));
            assert!(lang_controls.iter().any(|c| matches!(c.action, Some(Action::FirstWeekdayMonday(false)))));
            assert!(lang_controls.iter().any(|c| matches!(c.action, Some(Action::MetricUnits(true)))));
            assert!(lang_controls.iter().any(|c| matches!(c.action, Some(Action::MetricUnits(false)))));
        }
        context.system.apps = std::sync::Arc::new(vec![
            crate::system_preferences::DesktopApp {
                id: "firefox.desktop".into(),
                name: "Firefox".into(),
                exec: "firefox".into(),
                browser: true,
                terminal: false,
            },
            crate::system_preferences::DesktopApp {
                id: "feather-notes.desktop".into(),
                name: "Заметки".into(),
                exec: "feather-notes".into(),
                browser: false,
                terminal: false,
            },
        ]);
        context.system.autostart = std::sync::Arc::new(vec![
            crate::system_preferences::AutostartItem {
                id: "feather-notes.desktop".into(),
                name: "Заметки".into(),
                exec: "feather-notes".into(),
                enabled: true,
            },
            crate::system_preferences::AutostartItem {
                id: "org.telegram.desktop".into(),
                name: "Telegram".into(),
                exec: "telegram-desktop".into(),
                enabled: false,
            },
        ]);
        {
            use smithay::input::keyboard::keysyms;
            let mut panel = Panel { page: Page::Defaults, ..Default::default() };
            let controls = panel.controls(800.0, 540.0, &context);
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ToggleAutostart { id, enabled: false }) if id == "feather-notes.desktop")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ToggleAutostart { id, enabled: true }) if id == "org.telegram.desktop")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::RemoveAutostart(id)) if id == "feather-notes.desktop")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::AutostartPicker(true)))));
            panel.autostart_picker = true;
            assert!(panel.editing());
            for ch in "заметки".chars() {
                panel.edit_key(0, Some(ch), false, false);
            }
            let controls = panel.controls(800.0, 540.0, &context);
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::AddAutostart(id)) if id == "feather-notes.desktop")));
            assert!(matches!(
                panel.edit_key(keysyms::KEY_Return, None, false, false),
                Some(Action::AutostartSearchSubmit)
            ));
            panel.edit_key(keysyms::KEY_Escape, None, false, false);
            assert!(!panel.editing());
        }
        {
            use smithay::input::keyboard::keysyms;
            use crate::file_portal::ScreenCapturePolicy;
            let mut panel = Panel { page: Page::Privacy, ..Default::default() };
            let controls = panel.controls(800.0, 540.0, &context);
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ScreenCapturePolicy(ScreenCapturePolicy::Ask)))));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ScreenCapturePolicy(ScreenCapturePolicy::Allow)))));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ScreenCapturePolicy(ScreenCapturePolicy::Deny)))));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ScreenCaptureCursor))));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ToggleScreenCaptureApp(id)) if id == "com.obsproject.Studio")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ToggleScreenCaptureApp(id)) if id == "org.mozilla.firefox")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::RemoveScreenCaptureApp(id)) if id == "com.obsproject.Studio")));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ClearScreenCaptureApps))));
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::ScreenCapturePicker(true)))));
            panel.privacy_picker = true;
            assert!(panel.editing());
            for ch in "заметки".chars() {
                panel.edit_key(0, Some(ch), false, false);
            }
            let controls = panel.controls(800.0, 540.0, &context);
            assert!(controls.iter().any(|c| matches!(&c.action, Some(Action::AddScreenCaptureApp(id)) if id == "feather-notes.desktop")));
            assert!(matches!(
                panel.edit_key(keysyms::KEY_Return, None, false, false),
                Some(Action::PrivacySearchSubmit)
            ));
            panel.edit_key(keysyms::KEY_Escape, None, false, false);
            assert!(!panel.editing());
        }
        {
            use crate::controls::Control as Audio;
            let mut panel = Panel { page:Page::Sound, ..Default::default() };
            let controls = panel.controls(800.0, 540.0, &context);
            assert!(controls.iter().any(|c| c.detail.contains("разъём не подключён")));
            let volume = controls.iter().find(|c| matches!(c.action, Some(Action::Audio(Audio::InputVolume { .. })))).unwrap();
            panel.scroll=(volume.rect.y-300.0).max(0.0);
            let controls=panel.controls(800.0,540.0,&context);
            let r=controls.iter().find(|c| matches!(c.action,Some(Action::Audio(Audio::InputVolume { .. })))).unwrap().rect;
            for (fraction, expected) in [(0.0,0.0),(0.4,0.4),(1.0,1.0)] {
                let action = panel.hit(r.x+10.0+(r.w-20.0)*fraction, r.y+10.0, 800.0,540.0,&context);
                assert!(matches!(action, Some(Action::Audio(Audio::InputVolume { source, value })) if source=="builtin" && (value-expected).abs()<0.001));
            }
            panel.audio_drag = Some((r, Audio::InputVolume { source:"builtin".into(), value:0.5 }));
            assert!(panel.audio_motion(r.x+r.w+100.0).is_some());
            assert_eq!(panel.audio_volume(&context.audio.sources[0],true),1.0);
            assert!(matches!(panel.audio_release(),Some(Action::Audio(Audio::InputVolume { value:1.0, .. }))));
            assert!(panel.audio_motion(r.x).is_none());
            panel.audio_drag = Some((r, Audio::InputVolume {source:"builtin".into(),value:0.2}));
            let sent = (1..=20).filter_map(|step| panel.audio_motion(r.x+10.0+(r.w-20.0)*(0.2+step as f32*0.005))).count();
            assert!(sent>=3,"slow motion must compare against the last command, not the preceding pointer event");
            assert!(panel.audio_release().is_some());
            assert!(panel.audio_sent_volume.is_none());
            let audio = std::mem::take(&mut context.audio);
            context.audio.input_error=Some("Звуковая служба недоступна".into());
            let controls = panel.controls(800.0,540.0,&context);
            assert!(controls.iter().any(|c| c.label.contains("Микрофоны не найдены")));
            assert!(controls.iter().any(|c| c.label=="Звуковая служба недоступна"));
            assert!(!controls.iter().any(|c| matches!(c.action,Some(Action::Audio(Audio::InputVolume { .. } | Audio::InputMute { .. } | Audio::Input(_))))));
            context.audio=audio;
        }
        let mut fonts = FontSystem::new();
        for scale in [1.0, 1.25, 2.0] {
            for dark in [false, true] {
                with_render_settings(
                    scale,
                    if dark {
                        Appearance::Dark
                    } else {
                        Appearance::Light
                    },
                    || {
                        let overlays = [
                            Panel { page: Page::Keyboard, adding: true, ..Default::default() },
                            Panel { page: Page::Defaults, apps_picker: Some(true), ..Default::default() },
                            Panel { page: Page::Defaults, autostart_picker: true, ..Default::default() },
                            Panel { page: Page::Privacy, privacy_picker: true, ..Default::default() },
                            Panel { page: Page::DateTime, zone_picker: true, ..Default::default() },
                            // Hover: sidebar «Дисплеи» and, below, nothing else lit
                            Panel { page: Page::Desktop, pointer: Some((100.0, 100.0)), ..Default::default() },
                            Panel { page: Page::Sound, scroll: 440.0, ..Default::default() },
                            Panel {
                                page: Page::Network,
                                wifi_target: Some(context.wifi.networks[1].clone()),
                                wifi_password: "secret-password".into(),
                                ..Default::default()
                            },
                        ];
                        for (n, panel) in Page::ALL
                            .into_iter()
                            .map(|page| Panel { page, ..Default::default() })
                            .chain(overlays)
                            .enumerate()
                        {
                            let page = panel.page;
                            let page = if n >= Page::ALL.len() { format!("{page:?}-overlay") } else { format!("{page:?}") };
                            let mut pixels = tiny_skia::Pixmap::new(
                                (800.0 * scale) as u32,
                                (568.0 * scale) as u32,
                            )
                            .unwrap();
                            pixels.fill(palette().window_bg);
                            panel.paint(
                                &mut pixels.as_mut(),
                                &mut fonts,
                                (0.0, 28.0),
                                (800.0, 540.0),
                                28.0,
                                &context,
                            );
                            for control in panel
                                .controls(800.0, 540.0, &context)
                                .into_iter()
                                .filter(|c| c.action.is_some() && c.rect.y + c.rect.h / 2.0 < 540.0)
                                // Under a sheet or menu only the overlay itself is clickable.
                                .filter(|c| n < Page::ALL.len() || (c.fixed && !matches!(c.style, Style::Sidebar(..) | Style::Scrim(_))))
                            {
                                let r = control.rect;
                                assert!(
                                    panel
                                        .hit(
                                            r.x + r.w / 2.0,
                                            r.y + r.h / 2.0,
                                            800.0,
                                            540.0,
                                            &context
                                        )
                                        .is_some(),
                                    "{page}: {}",
                                    control.label
                                );
                            }
                            if let Some(dir) = std::env::var_os("FEATHER_SETTINGS_FIXTURES") {
                                let dir = std::path::PathBuf::from(dir);
                                std::fs::create_dir_all(&dir).unwrap();
                                pixels
                                    .save_png(dir.join(format!("{page}-{scale}-{dark}.png")))
                                    .unwrap();
                            }
                        }
                    },
                );
            }
        }
    }
    #[test]
    fn last_source_cannot_be_removed_and_clipped_controls_cannot_be_clicked() {
        let keyboard = Keyboard {
            layouts: vec!["us".into()],
            ..Default::default()
        };
        let mode = Mode {
            width: 1920,
            height: 1080,
            refresh: 60000,
        };
        let c = Context {
            system: Default::default(),
            pointer: Default::default(),
            power:Default::default(),power_ready:false,
            pointer_managed: true,
            keyboard: &keyboard,
            active: 0,
            animations: false,
            dock_zoom: false,
            scale: 1.0,
            display_name: "Window",
            monitors: Vec::new(),
            modes: &[mode],
            current: mode,
            direct: false,
            wallpaper: WallpaperTheme::for_appearance(Appearance::Light),
            wallpaper_image: None,
            dnd: false,
            dnd_schedule: Some((1320, 420)),
            notify_sounds: true,
            sounds: Default::default(),
            audio: Default::default(),
            wifi: Default::default(),
            bluetooth: Default::default(),
            hot_corners: [crate::hot_corners::HotAction::Launchpad, Default::default(), Default::default(), crate::hot_corners::HotAction::Lock],
            night: crate::night::Preferences { manual: false, schedule: Some((1320, 420)), kelvin: 3400 },
            night_active: true,
            notify_apps: &[],
            sticky_keys: false,
            screen_capture_policy: crate::file_portal::ScreenCapturePolicy::Ask,
            screen_capture_cursor: true,
            screen_capture_apps: &[],
        };
        let mut p = Panel {
            page: Page::Keyboard,
            ..Default::default()
        };
        assert!(!p
            .controls(640.0, 320.0, &c)
            .iter()
            .any(|i| matches!(i.action, Some(Action::RemoveSource(_)))));
        assert!(p.hit(235.0, 290.0, 640.0, 270.0, &c).is_none());
        assert!(p.scroll_by(30.0, 640.0, 240.0, &c));
        assert!(p.scroll > 0.0);
        assert!(!p
            .controls(800.0, 540.0, &c)
            .iter()
            .any(|i| matches!(i.action, Some(Action::ApplyMode))));
    }
}
