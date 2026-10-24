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
    Keyboard,
    Pointer,
    Displays,
    Desktop,
    Defaults,
    DateTime,
    Power,
    Notifications,
}
impl Page {
    pub const ALL: [Self; 9] = [
        Self::Appearance,
        Self::Keyboard,
        Self::Pointer,
        Self::Displays,
        Self::Desktop,
        Self::Defaults,
        Self::DateTime,
        Self::Power,
        Self::Notifications,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Оформление",
            Self::Keyboard => "Клавиатура",
            Self::Pointer => "Мышь и трекпад",
            Self::Displays => "Дисплеи",
            Self::Desktop => "Рабочий стол и Dock",
            Self::Defaults => "Приложения",
            Self::DateTime => "Дата и время",
            Self::Power => "Экран блокировки и энергосбережение",
            Self::Notifications => "Уведомления",
        }
    }
    fn caption(self) -> &'static str {
        match self {
            Self::Appearance => "Выберите внешний вид и поведение интерфейса.",
            Self::Keyboard => "Источники ввода и переключение языка.",
            Self::Pointer => "Курсор, скорость, прокрутка и нажатие.",
            Self::Displays => "Размер интерфейса, разрешение и частота экрана.",
            Self::Desktop => "Настройте рабочее пространство под себя.",
            Self::Defaults => "Выберите приложения для повседневных задач.",
            Self::DateTime => "Синхронизация часов и часовой пояс.",
            Self::Power => "Пароль, выключение экрана и переход в сон.",
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
    NotifySounds,
    /// Next delivery mode of the program at this index
    NotifyDelivery(usize),
    Appearance(Appearance),
    Animations,
    DockZoom,
    Wallpaper,
    WallpaperImage(Option<crate::wallpaper::ImageSource>),
    ChooseWallpaper,
    AppPicker(bool),
    DefaultApp { browser: bool, id: String },
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
    /// Pointer over the window (content coordinates), for hover highlights
    pub pointer: Option<(f32, f32)>,
    pub cursor_picker: bool,
    pub zone_picker: bool,
    pub zone_query: feather_ui::input::TextInput,
    pub time_editing: bool,
    pub time_input: feather_ui::input::TextInput,
    pub wallpaper_busy: bool,
    pub page: Page,
    pub adding: bool,
    pub keyboard_busy: bool,
    pub keyboard_generation: u64,
    pub error: Option<String>,
    pub selected_mode: Option<usize>,
    pub confirming: bool,
    pub queued_mode: Option<(usize, bool)>,
    pub scroll: f32,
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
    pub modes: &'a [Mode],
    pub current: Mode,
    pub direct: bool,
    pub wallpaper: WallpaperTheme,
    pub wallpaper_image: Option<&'a crate::wallpaper::ImageSource>,
    pub dnd: bool,
    pub notify_sounds: bool,
    pub notify_apps: &'a [crate::notifications::AppPolicy],
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
    pub modes: Vec<Mode>,
    pub current: Mode,
    pub direct: bool,
    pub wallpaper: WallpaperTheme,
    pub dnd: bool,
    pub notify_sounds: bool,
    pub notify_apps: Vec<crate::notifications::AppPolicy>,
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
            modes: c.modes.to_vec(),
            current: c.current,
            direct: c.direct,
            wallpaper: c.wallpaper,
            dnd: c.dnd,
            notify_sounds: c.notify_sounds,
            notify_apps: c.notify_apps.to_vec(),
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
            modes: &self.modes,
            current: self.current,
            direct: self.direct,
            wallpaper: self.wallpaper,
            wallpaper_image: self.wallpaper_image.as_ref(),
            dnd: self.dnd,
            notify_sounds: self.notify_sounds,
            notify_apps: &self.notify_apps,
        }
    }
}
/// Scrolled content above this line (content coordinates) is hidden under the toolbar.
const TOOLBAR_CLIP: f32 = 12.0;
/// Sidebar groups, like the sections of macOS System Settings.
const SIDEBAR: [&[Page]; 3] = [
    &[Page::Appearance, Page::Desktop, Page::Displays],
    &[Page::Keyboard, Page::Pointer],
    &[Page::Notifications, Page::Defaults, Page::DateTime, Page::Power],
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
    Push(bool),
    Popup,
    Mini,
    Source { badge: &'static str, selected: bool },
    Preview(bool, bool),
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
        self.out.push(Control {
            rect,
            label: label.into(),
            detail: String::new(),
            value: String::new(),
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
        Page::Keyboard => (rgba(160, 165, 175, 255), rgba(112, 117, 128, 255)),
        Page::Pointer => (rgba(120, 128, 140, 255), rgba(74, 80, 92, 255)),
        Page::Displays => (rgba(70, 175, 255, 255), rgba(20, 115, 235, 255)),
        Page::Desktop => (rgba(80, 200, 245, 255), rgba(20, 140, 220, 255)),
        Page::Defaults => (rgba(150, 120, 255, 255), rgba(100, 70, 225, 255)),
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
impl Panel {
    pub fn editing(&self) -> bool {
        self.page == Page::DateTime && (self.time_editing || self.zone_picker)
    }
    pub fn edit_key(&mut self, sym: u32, ch: Option<char>, ctrl: bool, shift: bool) -> Option<Action> {
        use smithay::input::keyboard::keysyms;
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
        let mut y = 24.0;
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
            y += 12.0;
        }
        // Right-side control of a row, vertically centred.
        let right = |r: R, w: f32, h: f32| R::new(r.x + r.w - 14.0 - w, r.y + (r.h - h) / 2.0, w, h);
        let kb = !self.keyboard_busy;
        match self.page {
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
                l.row(
                    52.0,
                    "Анимации интерфейса",
                    "Появление окон, меню и запуск приложений".into(),
                    Style::Toggle(c.animations),
                    Some(Action::Animations),
                );
                l.end();
                l.footer("Если анимации выключены, окна и панели появляются мгновенно.");
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
                    let r = l.row(48.0, "Разрешение и частота", String::new(), Style::Row, None);
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
                    let r = l.row(48.0, "Разрешение и частота", String::new(), Style::Row, None);
                    let _ = r;
                    l.out.last_mut().unwrap().value = "Задаётся окном".into();
                    l.end();
                    l.footer("Режимы экрана доступны при самостоятельном входе в Пёрышко.");
                }
            }
            Page::Defaults => {
                let state = &c.system;
                l.hero(Page::Defaults);
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
                l.end();
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
        }
        let mut controls = l.out;
        for control in controls.iter_mut().filter(|c| !c.fixed) {
            control.rect.y -= self.scroll;
        }
        controls
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
            0.5,
            if dark {
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
            stroke_line(pm, ox + side, oy + TOOLBAR_CLIP, ox + w, oy + TOOLBAR_CLIP, 0.5, p.separator);
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
        let hover_fill = if dark { rgba(255, 255, 255, 18) } else { rgba(0, 0, 0, 12) };
        let group_fill = if dark {
            rgba(255, 255, 255, 13)
        } else {
            rgba(0, 0, 0, 10)
        };
        let push_fill = if dark {
            rgba(255, 255, 255, 30)
        } else {
            rgba(0, 0, 0, 16)
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
            stroke_line(pm, r.x + 14.0, r.y, r.x + r.w - 14.0, r.y, 0.5, p.separator);
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
                draw_page_icon(pm, page, r.x + 14.0, r.y + 14.0, 44.0);
                fonts.draw_text_clipped(pm, &control.label, r.x + 72.0, r.y + 17.0, r.w - 86.0, 15.0, p.text, true);
                fonts.draw_text_clipped(pm, &control.detail, r.x + 72.0, r.y + 40.0, r.w - 86.0, 12.0, p.text_secondary, false);
            }
            Style::Display => {
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 12.0, group_fill);
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
            }
            Style::Row => {
                let reserve = if control.value.is_empty() {
                    0.0
                } else {
                    fonts.measure_text(&control.value, 13.0, false).min(r.w * 0.55) + 12.0
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
                    rgba(255, 255, 255, 40)
                } else {
                    rgba(0, 0, 0, 30)
                };
                let track = if on { p.accent } else { off };
                fill_rounded_rect(pm, tx, ty, tw, th, th / 2.0, if disabled { with_alpha(track, 0.45) } else { track });
                let kx = if on { tx + tw - 12.0 } else { tx + 12.0 };
                fill_circle(pm, kx, ty + 12.5, 10.0, rgba(0, 0, 0, 22));
                fill_circle(pm, kx, ty + 12.0, 10.0, rgba(255, 255, 255, 255));
            }
            Style::Track => {
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, if dark { rgba(255, 255, 255, 22) } else { rgba(0, 0, 0, 14) });
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
            Style::Slider(value) => {
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
                fonts.draw_text(pm, "Медленно", tx, r.y + 25.0, 11.0, p.text_secondary, false);
                fonts.draw_text_right(pm, "Быстро", tx + tw, r.y + 25.0, 11.0, p.text_secondary, false);
            }
            Style::Push(primary) => {
                let fill = if primary { p.accent } else { push_fill };
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, if disabled { with_alpha(fill, 0.5) } else { fill });
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
                fill_rounded_rect(pm, t.x + t.w / 2.0 - 16.0, t.y + t.h - 7.0, 32.0, 4.0, 2.0, rgba(255, 255, 255, 150));
                fonts.draw_text_clipped_centered(pm, &control.label, r.x + r.w / 2.0, r.y + r.h - 16.0, r.w, 11.0, if selected { p.text } else { p.text_secondary }, selected);
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
                let input = if matches!(control.action, Some(Action::ZoneSearchFocus)) { &self.zone_query } else { &self.time_input };
                if input.is_empty() && !active {
                    fonts.draw_text_clipped(pm, &control.label, r.x + 9.0, r.y + (r.h - 13.0) / 2.0 - 1.0, r.w - 18.0, 13.0, p.text_tertiary, false);
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
    pub fn settings_text_click(&mut self, action: &Action, x: f32, y: f32, w: f32, h: f32) {
        if !matches!(action, Action::EditTime | Action::ZoneSearchFocus) { return; }
        let Some(rect) = self.system_settings.text_rect(x, y, w, h, &self.settings_context()) else { return; };
        let input = if matches!(action, Action::ZoneSearchFocus) { &mut self.system_settings.zone_query } else { &mut self.system_settings.time_input };
        if matches!(action, Action::EditTime) && input.is_empty() {
            input.set(&self.system_preferences.time.local_input);
        }
        input.click(&mut self.fonts, x - rect.x - 9.0, rect.w - 18.0, 13.0, false, self.keyboard.modifier_state().shift);
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
            modes: &self.display_modes,
            current: Mode {
                width: self.phys_w,
                height: self.phys_h,
                refresh: self.refresh_mhz,
            },
            direct: self.backend.is_drm(),
            wallpaper: self.shell.theme,
            wallpaper_image: self.shell.wallpaper_image.as_ref(),
            dnd: self.shell.dnd_enabled,
            notify_sounds: self.shell.notify_sounds,
            notify_apps: &self.shell.notify_apps,
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
            Action::LockNow=>self.lock_session(),
            Action::DoNotDisturb => {
                self.shell.dnd_enabled = !self.shell.dnd_enabled;
                self.shell.topbar_dirty = true;
                self.shell.overlay_dirty = true;
                self.save_settings();
            }
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
                self.system_settings.apps_picker = None;
                self.system_settings.cursor_picker = false;
                self.system_settings.zone_picker = false;
                self.system_settings.time_editing = false;
                self.system_settings.page = page;
                self.system_settings.adding = false;
                self.system_settings.scroll = 0.0;
                self.open_or_focus_builtin(crate::apps::BuiltinWindowKind::SystemSettings);
                if matches!(page, Page::Defaults | Page::DateTime | Page::Pointer) {
                    self.request_system_preference(crate::system_preferences::Command::Refresh);
                }
            }
            Action::AppPicker(browser) => {
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
                    && self.wallpaper_picker.as_ref().is_some_and(|p| p.open())
                {
                    self.system_settings.wallpaper_busy = true;
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
        self.shell.invalidate_all();
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
                let result = d
                    .output
                    .use_mode::<_, crate::render::FeatherElement>(
                        mode,
                        &mut d.renderer,
                        &smithay::backend::drm::output::DrmOutputRenderElements::default(),
                    )
                    .map_err(|e| format!("Не удалось применить режим: {e:?}"));
                if result.is_ok() {
                    d.size = (target.width, target.height);
                    d.refresh = target.refresh;
                    d.output.with_compositor(|c| {
                        c.set_output_mode_source(smithay::output::OutputModeSource::Static {
                            size: d.size.into(),
                            scale: 1.0.into(),
                            transform: smithay::utils::Transform::Normal,
                        })
                    });
                    d.output.reset_buffers();
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
        self.phys_w = target.width;
        self.phys_h = target.height;
        self.refresh_mhz = target.refresh;
        let scale = if confirm {
            self.scale_factor
                .min((target.width as f64 / 640.0).min(target.height as f64 / 480.0))
                .max(1.0)
        } else {
            self.pending_display.map_or(self.scale_factor, |p| p.2)
        };
        self.apply_hidpi_scale(scale);
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
    let ink = if appearance() == Appearance::Dark {
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
            modes: &[mode],
            current: mode,
            direct: false,
            wallpaper: WallpaperTheme::for_appearance(Appearance::Light),
            wallpaper_image: None,
            dnd: false,
            notify_sounds: true,
            notify_apps: &apps,
        };
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
                            Panel { page: Page::DateTime, zone_picker: true, ..Default::default() },
                            // Hover: sidebar «Дисплеи» and, below, nothing else lit
                            Panel { page: Page::Desktop, pointer: Some((100.0, 100.0)), ..Default::default() },
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
            modes: &[mode],
            current: mode,
            direct: false,
            wallpaper: WallpaperTheme::for_appearance(Appearance::Light),
            wallpaper_image: None,
            dnd: false,
            notify_sounds: true,
            notify_apps: &[],
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
