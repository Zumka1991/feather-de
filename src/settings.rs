//! Persistent user preferences (~/.config/feather-de/settings.conf, simple key=value)

use std::path::PathBuf;

use crate::theme::{Appearance, WallpaperTheme};

pub struct Settings {
    pub appearance: Appearance,
    pub wallpaper: WallpaperTheme,
    /// Explicit HiDPI scale chosen by the user (None = automatic)
    pub scale: Option<f64>,
    pub dock_zoom: bool,
    /// Pinned Dock apps (desktop/app ids) in order; None = defaults
    pub dock: Option<Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        // Light appearance by default, like a fresh macOS install
        Self {
            appearance: Appearance::Light,
            wallpaper: WallpaperTheme::for_appearance(Appearance::Light),
            scale: None,
            dock_zoom: true,
            dock: None,
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("feather-de").join("settings.conf"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return s;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match (k.trim(), v.trim()) {
                ("appearance", "dark") => s.appearance = Appearance::Dark,
                ("appearance", "light") => s.appearance = Appearance::Light,
                ("wallpaper", id) => {
                    if let Some(t) = WallpaperTheme::from_id(id) {
                        s.wallpaper = t;
                    }
                }
                ("scale", v) => s.scale = v.parse::<f64>().ok().map(|v| v.clamp(1.0, 2.5)),
                ("dock_zoom", v) => s.dock_zoom = v == "true",
                ("dock", v) => {
                    s.dock = Some(v.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                }
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = format!(
            "appearance={}\nwallpaper={}\n{}dock_zoom={}\n{}",
            if self.appearance == Appearance::Dark { "dark" } else { "light" },
            self.wallpaper.id(),
            self.scale.map(|v| format!("scale={v}\n")).unwrap_or_default(),
            self.dock_zoom,
            self.dock.as_ref().map(|d| format!("dock={}\n", d.join(","))).unwrap_or_default(),
        );
        if let Err(err) = std::fs::write(&p, text) {
            tracing::warn!("Не удалось сохранить настройки: {err}");
        }
    }
}
