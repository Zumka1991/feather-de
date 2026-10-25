//! Preferences and XKB preparation. Disk access / XKB includes run on a worker.
use crate::{
    theme::{Appearance, WallpaperTheme},
    FeatherCompositor,
};
use smithay::{
    input::keyboard::xkb,
    reexports::calloop::{channel, LoopHandle},
};
use std::{
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
};
#[derive(Clone, Copy, Debug)]
pub struct InputSource {
    pub id: &'static str,
    pub label: &'static str,
    pub badge: &'static str,
    pub short: &'static str,
}
pub const SOURCES: [InputSource; 12] = [
    InputSource {
        id: "us",
        label: "ABC — США",
        badge: "A",
        short: "EN",
    },
    InputSource {
        id: "ru",
        label: "Русская",
        badge: "РУ",
        short: "RU",
    },
    InputSource {
        id: "gb",
        label: "Английская — Великобритания",
        badge: "GB",
        short: "GB",
    },
    InputSource {
        id: "de",
        label: "Немецкая",
        badge: "DE",
        short: "DE",
    },
    InputSource {
        id: "fr",
        label: "Французская",
        badge: "FR",
        short: "FR",
    },
    InputSource {
        id: "es",
        label: "Испанская",
        badge: "ES",
        short: "ES",
    },
    InputSource {
        id: "it",
        label: "Итальянская",
        badge: "IT",
        short: "IT",
    },
    InputSource {
        id: "pt",
        label: "Португальская",
        badge: "PT",
        short: "PT",
    },
    InputSource {
        id: "ua",
        label: "Украинская",
        badge: "УК",
        short: "UA",
    },
    InputSource {
        id: "by",
        label: "Белорусская",
        badge: "БЕ",
        short: "BY",
    },
    InputSource {
        id: "pl",
        label: "Польская",
        badge: "PL",
        short: "PL",
    },
    InputSource {
        id: "tr",
        label: "Турецкая",
        badge: "TR",
        short: "TR",
    },
];
pub fn source(id: &str) -> Option<InputSource> {
    SOURCES.iter().find(|s| s.id == id).copied()
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shortcut {
    #[default]
    AltShift,
    CtrlSpace,
    CtrlShift,
}
impl Shortcut {
    pub const ALL: [Self; 3] = [Self::AltShift, Self::CtrlSpace, Self::CtrlShift];
    pub fn option(self) -> &'static str {
        match self {
            Self::AltShift => "grp:alt_shift_toggle",
            Self::CtrlSpace => "grp:ctrl_space_toggle",
            Self::CtrlShift => "grp:ctrl_shift_toggle",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::AltShift => "Alt + Shift",
            Self::CtrlSpace => "Ctrl + Space",
            Self::CtrlShift => "Ctrl + Shift",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.option() == s)
    }
}
/// What Caps Lock does (an XKB option compiled with the layouts)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Caps {
    #[default]
    Normal,
    Ctrl,
    Escape,
    /// Switches the input source, like macOS «Caps Lock переключает языки»
    Layout,
    Off,
}
impl Caps {
    pub const ALL: [Self; 5] = [Self::Normal, Self::Ctrl, Self::Escape, Self::Layout, Self::Off];
    pub fn option(self) -> Option<&'static str> {
        match self {
            Self::Normal => None,
            Self::Ctrl => Some("caps:ctrl_modifier"),
            Self::Escape => Some("caps:escape"),
            Self::Layout => Some("grp:caps_toggle"),
            Self::Off => Some("caps:none"),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Caps Lock",
            Self::Ctrl => "Control",
            Self::Escape => "Escape",
            Self::Layout => "Переключение языка",
            Self::Off => "Нет действия",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Ctrl => "ctrl",
            Self::Escape => "escape",
            Self::Layout => "layout",
            Self::Off => "off",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == s)
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|c| *c == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// Offered key repeat delays (ms) and rates (keys per second), short to long / slow to fast
pub const REPEAT_DELAYS: [u32; 5] = [1000, 600, 400, 250, 150];
pub const REPEAT_RATES: [u32; 5] = [10, 20, 35, 50, 70];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyboard {
    pub layouts: Vec<String>,
    pub shortcut: Shortcut,
    pub caps: Caps,
    /// Milliseconds before a held key repeats
    pub repeat_delay: u32,
    /// Repeats per second
    pub repeat_rate: u32,
}
impl Default for Keyboard {
    fn default() -> Self {
        Self {
            layouts: vec!["us".into(), "ru".into()],
            shortcut: Shortcut::default(),
            caps: Caps::default(),
            repeat_delay: 250,
            repeat_rate: 35,
        }
    }
}
impl Keyboard {
    fn normalized(mut self) -> Self {
        let mut layouts = Vec::new();
        for id in self.layouts {
            if source(&id).is_some() && !layouts.contains(&id) && layouts.len() < 4 {
                layouts.push(id);
            }
        }
        if layouts.is_empty() {
            layouts = Self::default().layouts;
        }
        self.layouts = layouts;
        self
    }
    /// XKB options: the layout shortcut and the Caps Lock behaviour
    pub fn options(&self) -> String {
        std::iter::once(self.shortcut.option()).chain(self.caps.option()).collect::<Vec<_>>().join(",")
    }
    pub fn sources(&self) -> Vec<InputSource> {
        self.layouts.iter().filter_map(|s| source(s)).collect()
    }
    pub fn compile(&self) -> Result<String, String> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(
            &ctx,
            "evdev",
            "pc105",
            &self.layouts.join(","),
            "",
            Some(self.options()),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .map(|k| k.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1))
        .ok_or_else(|| "Не удалось подготовить раскладки клавиатуры".into())
    }
}
#[derive(Clone, Debug)]
pub struct Display {
    pub name: String,
    pub mode: crate::system_settings::Mode,
}
/// Mouse, touchpad and cursor (libinput values; the cursor also goes to new clients).
#[derive(Clone, Debug, PartialEq)]
pub struct Pointer {
    /// None: XCURSOR_THEME of the session, else breeze_cursors
    pub cursor_theme: Option<String>,
    /// Logical cursor size
    pub cursor_size: u32,
    /// libinput acceleration speed, -1…1
    pub speed: f64,
    pub natural_mouse: bool,
    pub natural_touchpad: bool,
    pub tap_to_click: bool,
    pub left_handed: bool,
}
impl Default for Pointer {
    fn default() -> Self {
        Self {
            cursor_theme: None,
            cursor_size: 24,
            speed: 0.0,
            natural_mouse: false,
            natural_touchpad: true,
            tap_to_click: true,
            left_handed: false,
        }
    }
}
pub const CURSOR_SIZES: [u32; 4] = [24, 32, 48, 64];
/// A theme name is one directory name under an icons directory.
pub fn valid_theme(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name != "." && name != ".." && !name.contains(['/', '\0', '\n'])
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// Open «Об этом компьютере» at login
    pub show_about: bool,
    pub power: crate::power::Preferences,
    pub pointer: Pointer,
    pub appearance: Appearance,
    pub accent_color: crate::theme::AccentColor,
    pub highlight_color: Option<crate::theme::AccentColor>,
    pub text_size: crate::theme::TextSize,
    pub high_contrast: bool,
    pub reduce_transparency: bool,
    pub sticky_keys: bool,
    pub language: crate::theme::Lang,
    pub region: crate::theme::Region,
    pub use_24h_time: bool,
    pub first_weekday_monday: bool,
    pub metric_units: bool,
    pub screenshot_format: crate::screenshot::Format,
    pub screenshot_dir: Option<PathBuf>,
    pub wallpaper: WallpaperTheme,
    pub wallpaper_image: Option<crate::wallpaper::ImageSource>,
    pub default_browser: Option<String>,
    pub default_terminal: Option<String>,
    pub scale: Option<f64>,
    pub dock_zoom: bool,
    pub dock_locked: bool,
    pub dock_size: f32,
    pub dock_autohide: bool,
    pub dock_edge: crate::theme::DockEdge,
    pub gtk_theme: bool,
    pub do_not_disturb: bool,
    pub dnd_schedule: Option<(u16, u16)>,
    pub notify_apps: Vec<crate::notifications::AppPolicy>,
    pub notify_sounds: bool,
    pub sounds: crate::sounds::Preferences,
    pub hot_corners: [crate::hot_corners::HotAction; 4],
    pub night: crate::night::Preferences,
    pub dock: Option<Vec<String>>,
    pub animations: bool,
    pub keyboard: Keyboard,
    pub keybindings: crate::shortcuts::Bindings,
    pub display: Option<Display>,
    pub monitor_layout: Vec<crate::monitors::Placement>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            show_about: true,
            power: Default::default(),
            pointer: Pointer::default(),
            appearance: Appearance::Light,
            accent_color: crate::theme::AccentColor::Blue,
            highlight_color: None,
            text_size: crate::theme::TextSize::Default,
            high_contrast: false,
            reduce_transparency: false,
            sticky_keys: false,
            language: crate::theme::Lang::Ru,
            region: crate::theme::Region::Ru,
            use_24h_time: true,
            first_weekday_monday: true,
            metric_units: true,
            screenshot_format: crate::screenshot::Format::Png,
            screenshot_dir: None,
            wallpaper: WallpaperTheme::for_appearance(Appearance::Light),
            wallpaper_image: None,
            default_browser: None,
            default_terminal: None,
            scale: None,
            dock_zoom: true,
            dock_locked: false,
            dock_size: 50.0,
            dock_autohide: false,
            dock_edge: Default::default(),
            gtk_theme: true,
            do_not_disturb: false,
            dnd_schedule: None,
            notify_apps: Vec::new(),
            notify_sounds: true,
            sounds: Default::default(),
            hot_corners: Default::default(),
            night: Default::default(),
            dock: None,
            animations: true,
            keyboard: Keyboard::default(),
            keybindings: Default::default(),
            display: None,
            monitor_layout: Vec::new(),
        }
    }
}
fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("feather-de/settings.conf"))
}
impl Settings {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map_or_else(Self::default, |t| Self::parse(&t))
    }
    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "idle_lock_minutes" | "idle_blank_minutes" | "idle_suspend_minutes" => {
                    if let Ok(minutes)=v.parse::<u32>() {if minutes<=1440 {
                        match k.trim() {"idle_lock_minutes"=>s.power.lock=minutes,"idle_blank_minutes"=>s.power.blank=minutes,_=>s.power.suspend=minutes}
                    }}
                }

                "appearance" => match v {
                    "dark" => s.appearance = Appearance::Dark,
                    "light" => s.appearance = Appearance::Light,
                    _ => {}
                },
                "accent_color" => {
                    if let Some(c) = crate::theme::AccentColor::parse(v) {
                        s.accent_color = c;
                    }
                }
                "highlight_color" => match v {
                    "accent" | "default" | "" => s.highlight_color = None,
                    other => {
                        if let Some(c) = crate::theme::AccentColor::parse(other) {
                            s.highlight_color = Some(c);
                        }
                    }
                },
                "text_size" => {
                    if let Some(ts) = crate::theme::TextSize::parse(v) {
                        s.text_size = ts;
                    }
                }
                "high_contrast" => {
                    if let Ok(b) = v.parse() {
                        s.high_contrast = b;
                    }
                }
                "reduce_transparency" => {
                    if let Ok(b) = v.parse() {
                        s.reduce_transparency = b;
                    }
                }
                "sticky_keys" => {
                    if let Ok(b) = v.parse() {
                        s.sticky_keys = b;
                    }
                }
                "language" | "lang" => {
                    if let Some(l) = crate::theme::Lang::parse(v) {
                        s.language = l;
                    }
                }
                "region" => {
                    if let Some(r) = crate::theme::Region::parse(v) {
                        s.region = r;
                    }
                }
                "use_24h_time" => {
                    if let Ok(b) = v.parse() {
                        s.use_24h_time = b;
                    }
                }
                "first_weekday_monday" => {
                    if let Ok(b) = v.parse() {
                        s.first_weekday_monday = b;
                    }
                }
                "metric_units" => {
                    if let Ok(b) = v.parse() {
                        s.metric_units = b;
                    }
                }
                "screenshot_format" => {
                    if let Some(fmt) = crate::screenshot::Format::parse(v) {
                        s.screenshot_format = fmt;
                    }
                }
                "screenshot_dir" => s.screenshot_dir = crate::screenshot::parse_dir_id(v),
                "wallpaper" => {
                    if let Some(t) = WallpaperTheme::from_id(v) {
                        s.wallpaper = t;
                    }
                }
                "wallpaper_image" => s.wallpaper_image = crate::wallpaper::ImageSource::parse(v),
                "show_about" => {
                    if let Ok(b) = v.parse() {
                        s.show_about = b;
                    }
                }
                "cursor_theme" if valid_theme(v) => s.pointer.cursor_theme = Some(v.to_string()),
                "cursor_size" => {
                    if let Some(v) = v.parse().ok().filter(|v| CURSOR_SIZES.contains(v)) {
                        s.pointer.cursor_size = v;
                    }
                }
                "pointer_speed" => {
                    if let Some(v) = v.parse::<f64>().ok().filter(|v| v.is_finite()) {
                        s.pointer.speed = v.clamp(-1.0, 1.0);
                    }
                }
                "natural_mouse" | "natural_touchpad" | "tap_to_click" | "left_handed" => {
                    if let Ok(b) = v.parse::<bool>() {
                        match k.trim() {
                            "natural_mouse" => s.pointer.natural_mouse = b,
                            "natural_touchpad" => s.pointer.natural_touchpad = b,
                            "tap_to_click" => s.pointer.tap_to_click = b,
                            _ => s.pointer.left_handed = b,
                        }
                    }
                }
                "default_browser" => s.default_browser = Some(v.to_string()),
                "default_terminal" => s.default_terminal = Some(v.to_string()),
                "scale" => {
                    s.scale = v
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite())
                        .map(|v| v.clamp(1.0, 2.5))
                }
                "dock_zoom" => {
                    if let Ok(v) = v.parse() {
                        s.dock_zoom = v;
                    }
                }
                "dock_locked" => {
                    if let Ok(v) = v.parse() {
                        s.dock_locked = v;
                    }
                }
                "notify_app" => {
                    let mut f = v.splitn(3, '|');
                    if let (Some(key), Some(label), Some(Some(delivery))) = (f.next(), f.next(), f.next().map(crate::notifications::Delivery::parse)) {
                        let key = crate::notifications::clean_field(key);
                        if !key.is_empty() && s.notify_apps.len() < crate::notifications::MAX_APPS && !s.notify_apps.iter().any(|a| a.key == key) {
                            s.notify_apps.push(crate::notifications::AppPolicy { key, label: crate::notifications::clean_field(label), delivery });
                        }
                    }
                }
                "dnd_schedule" => {
                    let parsed = v.split_once('-').and_then(|(a, b)| Some((a.parse::<u16>().ok()?, b.parse::<u16>().ok()?)));
                    s.dnd_schedule = parsed.filter(|(a, b)| *a < 1440 && *b < 1440 && a != b);
                }
                "night_manual" => {
                    if let Ok(v) = v.parse() {
                        s.night.manual = v;
                    }
                }
                "night_schedule" => {
                    let parsed = v.split_once('-').and_then(|(a, b)| Some((a.parse::<u16>().ok()?, b.parse::<u16>().ok()?)));
                    s.night.schedule = parsed.filter(|(a, b)| *a < 1440 && *b < 1440 && a != b);
                }
                "night_kelvin" => {
                    if let Some(k) = v.parse::<u32>().ok().filter(|k| (1900..=6500).contains(k)) {
                        s.night.kelvin = k;
                    }
                }
                "hot_corners" => {
                    let parsed: Vec<_> = v.split(',').map(crate::hot_corners::HotAction::parse).collect();
                    if let [Some(a), Some(b), Some(c), Some(d)] = parsed[..] {
                        s.hot_corners = [a, b, c, d];
                    }
                }
                "alert_sound" => {if let Some(tone)=crate::sounds::Tone::parse(v) {s.sounds.tone=tone;}}
                "alert_volume" => {if let Some(volume)=v.parse::<u8>().ok().filter(|v|*v<=100) {s.sounds.volume=volume;}}
                "alert_sounds" | "interface_sounds" | "volume_feedback" => {
                    if let Ok(on)=v.parse() {match k.trim() {
                        "alert_sounds"=>s.sounds.alerts=on,"interface_sounds"=>s.sounds.interface=on,_=>s.sounds.volume_feedback=on,
                    }}
                }
                "notify_sounds" => {
                    if let Ok(v) = v.parse() {
                        s.notify_sounds = v;
                    }
                }
                "dock_size" => {
                    if let Some(v) = v.parse::<f32>().ok().filter(|v| crate::theme::DOCK_SIZES.contains(v)) {
                        s.dock_size = v;
                    }
                }
                "gtk_theme" => {
                    if let Ok(v) = v.parse() {
                        s.gtk_theme = v;
                    }
                }
                "dock_edge" => {
                    if let Some(e) = crate::theme::DockEdge::parse(v) {
                        s.dock_edge = e;
                    }
                }
                "dock_autohide" => {
                    if let Ok(v) = v.parse() {
                        s.dock_autohide = v;
                    }
                }
                "do_not_disturb" => {
                    if let Ok(v) = v.parse() {
                        s.do_not_disturb = v;
                    }
                }
                "animations" => {
                    if let Ok(v) = v.parse() {
                        s.animations = v;
                    }
                }
                "dock" => {
                    s.dock = Some(
                        v.split(',')
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                            .map(str::to_string)
                            .collect(),
                    )
                }
                "keyboard_layouts" => {
                    s.keyboard.layouts = v.split(',').map(str::trim).map(str::to_string).collect()
                }
                "keyboard_shortcut" => {
                    if let Some(v) = Shortcut::parse(v) {
                        s.keyboard.shortcut = v;
                    }
                }
                "keybinding" => s.keybindings.parse_line(v),
                "keyboard_caps" => {
                    if let Some(v) = Caps::parse(v) {
                        s.keyboard.caps = v;
                    }
                }
                "keyboard_repeat_delay" => {
                    if let Some(v) = v.parse().ok().filter(|v| REPEAT_DELAYS.contains(v)) {
                        s.keyboard.repeat_delay = v;
                    }
                }
                "keyboard_repeat_rate" => {
                    if let Some(v) = v.parse().ok().filter(|v| REPEAT_RATES.contains(v)) {
                        s.keyboard.repeat_rate = v;
                    }
                }
                "monitor_layout" => {
                    if let Ok(serde_json::Value::Array(rows))=serde_json::from_str(v) {
                        s.monitor_layout=rows.into_iter().take(16).filter_map(|v|{
                            let name=v.get("name")?.as_str()?.to_owned();let x=i32::try_from(v.get("x")?.as_i64()?).ok()?;let y=i32::try_from(v.get("y")?.as_i64()?).ok()?;
                            if name.is_empty()||name.len()>128||name.chars().any(char::is_control)||x.unsigned_abs()>32768||y.unsigned_abs()>32768{return None;}
                            Some(crate::monitors::Placement{name,x,y,primary:v.get("primary").and_then(|v|v.as_bool()).unwrap_or(false)})
                        }).collect();
                    }
                }
                "display" => {
                    let fields: Vec<_> = v.split(';').collect();
                    if fields.len() == 4
                        && fields[0]
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    {
                        if let (Ok(width), Ok(height), Ok(refresh)) = (
                            fields[1].parse::<i32>(),
                            fields[2].parse::<i32>(),
                            fields[3].parse::<i32>(),
                        ) {
                            if (320..=16384).contains(&width)
                                && (200..=16384).contains(&height)
                                && (1000..=1000000).contains(&refresh)
                            {
                                s.display = Some(Display {
                                    name: fields[0].into(),
                                    mode: crate::system_settings::Mode {
                                        width,
                                        height,
                                        refresh,
                                    },
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        s.keyboard = s.keyboard.normalized();
        s.keybindings.normalize();
        s
    }
    fn text(&self) -> String {
        let mut text=format!("appearance={}\naccent_color={}\nhighlight_color={}\ntext_size={}\nhigh_contrast={}\nreduce_transparency={}\nsticky_keys={}\nlanguage={}\nregion={}\nuse_24h_time={}\nfirst_weekday_monday={}\nmetric_units={}\nwallpaper={}\n{}dock_zoom={}\n{}animations={}\nkeyboard_layouts={}\nkeyboard_shortcut={}\n{}",
            if self.appearance==Appearance::Dark{"dark"}else{"light"},
            self.accent_color.id(),
            self.highlight_color.map_or("accent", |c| c.id()),
            self.text_size.id(),
            self.high_contrast,
            self.reduce_transparency,
            self.sticky_keys,
            self.language.id(),
            self.region.id(),
            self.use_24h_time,
            self.first_weekday_monday,
            self.metric_units,
            self.wallpaper.id(),self.scale.map(|v|format!("scale={v}\n")).unwrap_or_default(),
            self.dock_zoom,self.dock.as_ref().map(|d|format!("dock={}\n",d.join(","))).unwrap_or_default(),self.animations,self.keyboard.layouts.join(","),self.keyboard.shortcut.option(),
            self.display.as_ref().map(|d|format!("display={};{};{};{}\n",d.name,d.mode.width,d.mode.height,d.mode.refresh)).unwrap_or_default());
        text.push_str(&format!("idle_lock_minutes={}\nidle_blank_minutes={}\nidle_suspend_minutes={}\n",self.power.lock,self.power.blank,self.power.suspend));
        text.push_str(&format!("keyboard_caps={}\nkeyboard_repeat_delay={}\nkeyboard_repeat_rate={}\n", self.keyboard.caps.id(), self.keyboard.repeat_delay, self.keyboard.repeat_rate));
        for line in self.keybindings.lines() { text.push_str(&line); }
        text.push_str(&format!("show_about={}\n", self.show_about));
        text.push_str(&format!("dock_locked={}\n", self.dock_locked));
        text.push_str(&format!("do_not_disturb={}\n", self.do_not_disturb));
        text.push_str(&format!("dock_size={}\ndock_autohide={}\ndock_edge={}\ngtk_theme={}\n", self.dock_size, self.dock_autohide, self.dock_edge.id(), self.gtk_theme));
        text.push_str(&format!("notify_sounds={}\n", self.notify_sounds));
        text.push_str(&format!("alert_sounds={}\nalert_sound={}\nalert_volume={}\ninterface_sounds={}\nvolume_feedback={}\n",
            self.sounds.alerts,self.sounds.tone.id(),self.sounds.volume,self.sounds.interface,self.sounds.volume_feedback));
        text.push_str(&format!("hot_corners={}\n", self.hot_corners.map(|a| a.id()).join(",")));
        text.push_str(&format!("night_manual={}\nnight_kelvin={}\n", self.night.manual, self.night.kelvin));
        if let Some((from, to)) = self.night.schedule {
            text.push_str(&format!("night_schedule={from}-{to}\n"));
        }
        if let Some((from, to)) = self.dnd_schedule {
            text.push_str(&format!("dnd_schedule={from}-{to}\n"));
        }
        for a in &self.notify_apps {
            text.push_str(&format!("notify_app={}|{}|{}\n", a.key, a.label, a.delivery.id()));
        }
        let pt = &self.pointer;
        if let Some(theme) = pt.cursor_theme.as_deref().filter(|t| valid_theme(t)) {
            text.push_str(&format!("cursor_theme={theme}\n"));
        }
        text.push_str(&format!(
            "cursor_size={}\npointer_speed={:.2}\nnatural_mouse={}\nnatural_touchpad={}\ntap_to_click={}\nleft_handed={}\n",
            pt.cursor_size, pt.speed, pt.natural_mouse, pt.natural_touchpad, pt.tap_to_click, pt.left_handed
        ));
        if let Some(image) = &self.wallpaper_image {
            text.push_str(&format!("wallpaper_image={}\n", image.id()));
        }
        text.push_str(&format!("screenshot_format={}\n", self.screenshot_format.id()));
        if let Some(dir) = &self.screenshot_dir {
            text.push_str(&format!("screenshot_dir={}\n", crate::screenshot::encode_dir_id(dir)));
        }
        for (key, value) in [
            ("default_browser", &self.default_browser),
            ("default_terminal", &self.default_terminal),
        ] {
            if let Some(value) = value {
                if !value.chars().any(char::is_control) {
                    text.push_str(&format!("{key}={value}\n"));
                }
            }
        }
        if !self.monitor_layout.is_empty(){
            let rows:Vec<_>=self.monitor_layout.iter().take(16).map(|m|serde_json::json!({"name":m.name,"x":m.x,"y":m.y,"primary":m.primary})).collect();
            text.push_str(&format!("monitor_layout={}\n",serde_json::Value::Array(rows)));
        }
        text
    }
    fn write(&self, p: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, self.text())?;
        std::fs::rename(tmp, p)
    }
}
enum Update {
    Keyboard {
        generation: u64,
        config: Keyboard,
        keymap: Result<String, String>,
    },
    SaveError(String),
}
type PendingKey = Option<(u64, Keyboard)>;
pub struct Worker {
    latest: Arc<Mutex<Option<Settings>>>,
    key: Arc<Mutex<PendingKey>>,
    wake: mpsc::SyncSender<()>,
}
impl Worker {
    pub fn new(
        handle: &LoopHandle<'static, FeatherCompositor>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let latest: Arc<Mutex<Option<Settings>>> = Arc::new(Mutex::new(None));
        let key: Arc<Mutex<PendingKey>> = Arc::new(Mutex::new(None));
        let (wake, rx) = mpsc::sync_channel(1);
        let (updates, source) = channel::sync_channel(2);
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                match event {
                    Update::Keyboard {
                        generation,
                        config,
                        keymap,
                    } => state.install_keyboard(generation, config, keymap),
                    Update::SaveError(e) => {
                        state.system_settings.error = Some(e);
                        state.mark_builtin_windows_dirty();
                        state.needs_redraw = true;
                    }
                }
            }
        })?;
        let save = latest.clone();
        let prepare = key.clone();
        let location = path();
        std::thread::Builder::new()
            .name("feather-prefs".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                while rx.recv().is_ok() {
                    let request = prepare.lock().unwrap().take();
                    if let Some((generation, config)) = request {
                        let keymap = config.compile();
                        if updates
                            .send(Update::Keyboard {
                                generation,
                                config,
                                keymap,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    let snapshot = save.lock().unwrap().take();
                    if let (Some(s), Some(p)) = (snapshot, location.as_ref()) {
                        if let Err(e) = s.write(p) {
                            let _ = updates.send(Update::SaveError(format!(
                                "Не удалось сохранить настройки: {e}"
                            )));
                        }
                    }
                }
            })?;
        Ok(Self { latest, key, wake })
    }
    pub fn save(&self, s: Settings) {
        *self.latest.lock().unwrap() = Some(s);
        let _ = self.wake.try_send(());
    }
    pub fn keyboard(&self, generation: u64, config: Keyboard) {
        *self.key.lock().unwrap() = Some((generation, config));
        let _ = self.wake.try_send(());
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn idle_preferences_default_to_off_and_reject_invalid_intervals() {
        use super::Settings;
        assert_eq!(Settings::parse("").power, crate::power::Preferences::default());
        let saved = Settings::parse("idle_lock_minutes=5\nidle_blank_minutes=10\nidle_suspend_minutes=30\n");
        assert_eq!(Settings::parse(&saved.text()).power, crate::power::Preferences {lock:5,blank:10,suspend:30});
        let invalid = Settings::parse("idle_lock_minutes=-1\nidle_blank_minutes=NaN\nidle_suspend_minutes=1441\n");
        assert_eq!(invalid.power, crate::power::Preferences::default());
    }
    use super::*;
    #[test]
    fn system_sound_preferences_round_trip_and_validate() {
        let mut s=Settings::parse("");assert_eq!(s.sounds,crate::sounds::Preferences::default());
        s.sounds=crate::sounds::Preferences {alerts:false,tone:crate::sounds::Tone::Drop,volume:75,interface:false,volume_feedback:true};
        assert_eq!(Settings::parse(&s.text()).sounds,s.sounds);
        assert_eq!(Settings::parse("alert_volume=101\nalert_volume=-1\nalert_volume=NaN\nalert_sound=../x\nalert_sounds=yes\ninterface_sounds=no\nvolume_feedback=1").sounds,crate::sounds::Preferences::default());
        assert_eq!(Settings::parse("alert_volume=0").sounds.volume,0);
    }
    #[test]
    fn notification_preferences_round_trip_and_reject_bad_lines() {
        use crate::notifications::{AppPolicy, Delivery};
        let mut s = Settings::default();
        assert!(!s.do_not_disturb && s.notify_apps.is_empty());
        s.do_not_disturb = true;
        assert!(s.notify_sounds);
        s.notify_sounds = false;
        s.notify_apps = vec![
            AppPolicy { key: "org.telegram.desktop".into(), label: "Telegram".into(), delivery: Delivery::CenterOnly },
            AppPolicy { key: "firefox".into(), label: "Firefox".into(), delivery: Delivery::Off },
        ];
        let restored = Settings::parse(&s.text());
        assert!(restored.do_not_disturb && !restored.notify_sounds);
        assert_eq!(restored.dnd_schedule, None);
        use crate::hot_corners::HotAction as H;
        assert_eq!(restored.hot_corners, [H::None; 4]);
        s.hot_corners = [H::Launchpad, H::None, H::Lock, H::Notifications];
        assert_eq!(Settings::parse(&s.text()).hot_corners, s.hot_corners);
        assert_eq!(Settings::parse("hot_corners=lock,none").hot_corners, [H::None; 4], "incomplete");
        s.night = crate::night::Preferences { manual: true, schedule: Some((1320, 420)), kelvin: 3400 };
        assert_eq!(Settings::parse(&s.text()).night, s.night);
        let bad = Settings::parse("night_kelvin=100\nnight_schedule=5-5\nnight_manual=yes");
        assert_eq!(bad.night, crate::night::Preferences::default());
        assert_eq!(Settings::parse("hot_corners=lock,none,boom,none").hot_corners, [H::None; 4], "unknown action");
        s.dnd_schedule = Some((1320, 420));
        assert_eq!(Settings::parse(&s.text()).dnd_schedule, Some((1320, 420)));
        for bad in ["dnd_schedule=1440-1", "dnd_schedule=60-60", "dnd_schedule=x", "dnd_schedule=10"] {
            assert_eq!(Settings::parse(bad).dnd_schedule, None, "{bad}");
        }
        assert_eq!(restored.notify_apps, s.notify_apps);
        let bad = Settings::parse("notify_app=x|X|loud\nnotify_app=|Y|off\nnotify_app=a|A|off\nnotify_app=a|B|banners\nnotify_app=only\n");
        assert_eq!(bad.notify_apps, [AppPolicy { key: "a".into(), label: "A".into(), delivery: Delivery::Off }]);
    }

    #[test]
    fn monitor_layout_round_trips_and_rejects_unbounded_or_control_coordinates(){
        let mut s=Settings::default();s.monitor_layout=vec![crate::monitors::Placement{name:"DP-1".into(),x:-1920,y:120,primary:true}];
        assert_eq!(Settings::parse(&s.text()).monitor_layout,s.monitor_layout);
        assert!(Settings::parse(r#"monitor_layout=[{"name":"DP-1","x":9999999999,"y":0}]"#).monitor_layout.is_empty());
        assert!(Settings::parse(r#"monitor_layout=[{"name":"DP-1","x":-32769,"y":0}]"#).monitor_layout.is_empty());
    }
    #[test]
    fn custom_shortcuts_survive_the_settings_file() {
        use crate::shortcuts::{Action as A, Chord, CTRL, SUPER};
        let mut s = Settings::default();
        s.keybindings.set(A::Search, Some(Chord {mods:CTRL|SUPER,key:112})).unwrap();
        s.keybindings.set(A::Files, None).unwrap();
        let back = Settings::parse(&s.text());
        assert_eq!(back.keybindings,s.keybindings);
        assert_eq!(Settings::parse("keybinding=files;999;123" ).keybindings,Default::default());
    }

    #[test]
    fn key_repeat_and_caps_lock_round_trip_and_compile() {
        let mut s = Settings::default();
        assert_eq!((s.keyboard.repeat_delay, s.keyboard.repeat_rate, s.keyboard.caps), (250, 35, Caps::Normal));
        assert_eq!(s.keyboard.options(), "grp:alt_shift_toggle");
        s.keyboard.caps = Caps::Escape;
        s.keyboard.repeat_delay = 400;
        s.keyboard.repeat_rate = 50;
        let back = Settings::parse(&s.text());
        assert_eq!(back.keyboard, s.keyboard);
        assert_eq!(back.keyboard.options(), "grp:alt_shift_toggle,caps:escape");
        for caps in Caps::ALL {
            let mut k = Keyboard::default();
            k.caps = caps;
            assert!(k.compile().is_ok(), "{caps:?} compiles");
        }
        let bad = Settings::parse("keyboard_repeat_delay=3\nkeyboard_repeat_rate=9999\nkeyboard_caps=boom");
        assert_eq!((bad.keyboard.repeat_delay, bad.keyboard.repeat_rate, bad.keyboard.caps), (250, 35, Caps::Normal));
        assert_eq!(Caps::Off.next(), Caps::Normal);
        s.dock_size = 64.0;
        s.dock_autohide = true;
        let back = Settings::parse(&s.text());
        assert!(back.dock_size == 64.0 && back.dock_autohide);
        s.dock_edge = crate::theme::DockEdge::Left;
        assert_eq!(Settings::parse(&s.text()).dock_edge, crate::theme::DockEdge::Left);
        assert_eq!(Settings::parse("dock_edge=top").dock_edge, crate::theme::DockEdge::Bottom);
        assert_eq!(Settings::parse("dock_size=13").dock_size, 50.0);
    }

    #[test]
    fn dock_pins_and_lock_survive_settings_round_trip() {
        let old = Settings::parse("dock=konsole,firefox\n");
        assert!(!old.dock_locked);
        assert_eq!(old.dock.as_deref().unwrap(), ["konsole", "firefox"]);
        let mut settings = old;
        settings.dock_locked = true;
        let restored = Settings::parse(&settings.text());
        assert!(restored.dock_locked);
        assert_eq!(restored.dock, settings.dock);
        settings.dock_locked = false;
        settings.dock = Some(Vec::new());
        let restored = Settings::parse(&settings.text());
        assert!(!restored.dock_locked);
        assert_eq!(restored.dock, Some(Vec::new()));
        assert!(!Settings::parse("dock_locked=invalid").dock_locked);
    }

    #[test]
    fn pointer_settings_round_trip_and_reject_bad_values() {
        let mut settings = Settings::default();
        settings.pointer = Pointer {
            cursor_theme: Some("Breeze_Light".into()),
            cursor_size: 48,
            speed: -0.4,
            natural_mouse: true,
            natural_touchpad: false,
            tap_to_click: false,
            left_handed: true,
        };
        assert_eq!(Settings::parse(&settings.text()).pointer, settings.pointer);
        let bad = Settings::parse("cursor_theme=../../etc\ncursor_size=13\npointer_speed=NaN\nleft_handed=maybe\n");
        assert_eq!(bad.pointer, Pointer::default());
        assert_eq!(Settings::parse("pointer_speed=7").pointer.speed, 1.0);
    }
    #[test]
    fn preferences_roundtrip_and_invalid_layouts_never_remove_all_input_sources() {
        let mut s=Settings::parse("keyboard_layouts=ru,ru,bogus,de,fr,us,es\nkeyboard_shortcut=grp:ctrl_space_toggle\nanimations=false\nscale=NaN\n");
        assert_eq!(s.keyboard.layouts, ["ru", "de", "fr", "us"]);
        assert_eq!(s.scale, None);
        assert!(!s.animations);
        s.scale = Some(1.25);
        let r = Settings::parse(&s.text());
        assert_eq!(r.keyboard, s.keyboard);
        assert_eq!(r.scale, s.scale);
        assert!(!r.animations);
        assert_eq!(
            Settings::parse("keyboard_layouts=,,bogus").keyboard,
            Keyboard::default()
        );
    }
    #[test]
    fn appearance_accent_highlight_and_text_size_round_trip() {
        use crate::theme::{AccentColor, TextSize};
        let mut s = Settings::default();
        assert_eq!(
            (s.accent_color, s.highlight_color, s.text_size),
            (AccentColor::Blue, None, TextSize::Default)
        );
        for accent in AccentColor::ALL {
            for ts in TextSize::ALL {
                s.accent_color = accent;
                s.highlight_color = Some(AccentColor::Pink);
                s.text_size = ts;
                let back = Settings::parse(&s.text());
                assert_eq!(back.accent_color, accent);
                assert_eq!(back.highlight_color, Some(AccentColor::Pink));
                assert_eq!(back.text_size, ts);
            }
        }
        s.highlight_color = None;
        s.high_contrast = true;
        s.reduce_transparency = true;
        s.sticky_keys = true;
        let back = Settings::parse(&s.text());
        assert_eq!(back.highlight_color, None);
        assert!(back.high_contrast && back.reduce_transparency && back.sticky_keys);
        let bad = Settings::parse(
            "accent_color=neon\nhighlight_color=bogus\ntext_size=huge\nhigh_contrast=maybe\nreduce_transparency=1\nsticky_keys=yes\n",
        );
        assert_eq!(
            (bad.accent_color, bad.highlight_color, bad.text_size, bad.high_contrast, bad.reduce_transparency, bad.sticky_keys),
            (AccentColor::Blue, None, TextSize::Default, false, false, false)
        );
    }
    #[test]
    fn each_offered_input_source_and_shortcut_compiles() {
        for source in SOURCES {
            for shortcut in Shortcut::ALL {
                assert!(
                    Keyboard {
                        layouts: vec!["us".into(), source.id.into()],
                        shortcut,
                        ..Default::default()
                    }
                    .compile()
                    .is_ok(),
                    "{} {shortcut:?}",
                    source.id
                );
            }
        }
    }
}

impl crate::FeatherCompositor {
    /// Mouse/touchpad settings for one libinput device (System Settings → Мышь и трекпад).
    pub fn configure_input_device(&self, device: &mut smithay::reexports::input::Device) {
        let p = &self.pointer_config;
        let touchpad = device.config_tap_finger_count() > 0;
        if touchpad {
            let _ = device.config_tap_set_enabled(p.tap_to_click);
        }
        if device.config_scroll_has_natural_scroll() {
            let _ = device.config_scroll_set_natural_scroll_enabled(if touchpad { p.natural_touchpad } else { p.natural_mouse });
        }
        if device.config_accel_is_available() {
            let _ = device.config_accel_set_speed(p.speed);
        }
        if device.config_left_handed_is_available() {
            let _ = device.config_left_handed_set(p.left_handed);
        }
    }

    /// After a change: every device, the compositor cursor and new clients' cursor.
    pub fn apply_pointer_config(&mut self) {
        let mut devices = std::mem::take(&mut self.input_devices);
        for device in &mut devices {
            self.configure_input_device(device);
        }
        self.input_devices = devices;
        let p = self.pointer_config.clone();
        self.cursor.configure(p.cursor_theme.as_deref(), p.cursor_size);
        crate::apps::set_cursor_env(crate::cursor::theme_name(p.cursor_theme.as_deref()), p.cursor_size);
        if !self.backend.is_drm() {
            // Inside the labwc kiosk session labwc owns libinput and the cursor
            crate::labwc::apply(p, self.scale_factor);
        }
        self.needs_redraw = true;
    }
}
