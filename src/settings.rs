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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyboard {
    pub layouts: Vec<String>,
    pub shortcut: Shortcut,
}
impl Default for Keyboard {
    fn default() -> Self {
        Self {
            layouts: vec!["us".into(), "ru".into()],
            shortcut: Shortcut::default(),
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
            Some(self.shortcut.option().into()),
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
    pub wallpaper: WallpaperTheme,
    pub wallpaper_image: Option<crate::wallpaper::ImageSource>,
    pub default_browser: Option<String>,
    pub default_terminal: Option<String>,
    pub scale: Option<f64>,
    pub dock_zoom: bool,
    pub dock_locked: bool,
    pub do_not_disturb: bool,
    pub dock: Option<Vec<String>>,
    pub animations: bool,
    pub keyboard: Keyboard,
    pub display: Option<Display>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            show_about: true,
            power: Default::default(),
            pointer: Pointer::default(),
            appearance: Appearance::Light,
            wallpaper: WallpaperTheme::for_appearance(Appearance::Light),
            wallpaper_image: None,
            default_browser: None,
            default_terminal: None,
            scale: None,
            dock_zoom: true,
            dock_locked: false,
            do_not_disturb: false,
            dock: None,
            animations: true,
            keyboard: Keyboard::default(),
            display: None,
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
        s
    }
    fn text(&self) -> String {
        let mut text=format!("appearance={}\nwallpaper={}\n{}dock_zoom={}\n{}animations={}\nkeyboard_layouts={}\nkeyboard_shortcut={}\n{}",
            if self.appearance==Appearance::Dark{"dark"}else{"light"},self.wallpaper.id(),self.scale.map(|v|format!("scale={v}\n")).unwrap_or_default(),
            self.dock_zoom,self.dock.as_ref().map(|d|format!("dock={}\n",d.join(","))).unwrap_or_default(),self.animations,self.keyboard.layouts.join(","),self.keyboard.shortcut.option(),
            self.display.as_ref().map(|d|format!("display={};{};{};{}\n",d.name,d.mode.width,d.mode.height,d.mode.refresh)).unwrap_or_default());
        text.push_str(&format!("idle_lock_minutes={}\nidle_blank_minutes={}\nidle_suspend_minutes={}\n",self.power.lock,self.power.blank,self.power.suspend));
        text.push_str(&format!("show_about={}\n", self.show_about));
        text.push_str(&format!("dock_locked={}\n", self.dock_locked));
        text.push_str(&format!("do_not_disturb={}\n", self.do_not_disturb));
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
    fn each_offered_input_source_and_shortcut_compiles() {
        for source in SOURCES {
            for shortcut in Shortcut::ALL {
                assert!(
                    Keyboard {
                        layouts: vec!["us".into(), source.id.into()],
                        shortcut
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
