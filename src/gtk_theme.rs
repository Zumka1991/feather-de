//! «Пёрышко» look for GTK programs: a GTK 3/4 theme on top of the built-in Adwaita
//! (macOS traffic lights, flat title bars, rounded windows, blue accent), light and dark.
//! GTK 3 apps, Firefox and Chromium in GTK mode use it for their own title bars;
//! libadwaita apps accept no themes, so the same rules also go to the user stylesheet.
//!
//! Everything it changes is backed up once (theme, colour scheme, button layout, the
//! settings.ini keys, ~/.config/gtk-4.0/gtk.css) and restored when it is turned off.
//! All file and gsettings work runs on a short-lived thread, never on the event loop.
use std::path::{Path, PathBuf};

const COLORS_LIGHT: &str = include_str!("../assets/gtk/colors-light.css");
const COLORS_DARK: &str = include_str!("../assets/gtk/colors-dark.css");
const GTK3: &str = include_str!("../assets/gtk/gtk3.css");
const GTK4: &str = include_str!("../assets/gtk/gtk4.css");
/// First line of files we own: never back them up as the user's
const MARK: &str = "/* Feather DE: managed, restored when turned off */";
/// KDE's module draws its own GTK 3 title buttons over any theme
const KDE_DECORATIONS: &str = "window-decorations-gtk-module";
pub const LAYOUT: &str = "close,minimize,maximize:";

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The setting as shown in Системные настройки (the page is painted on a worker)
pub fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Where everything lives; tests point it at a temporary directory and skip gsettings
#[derive(Clone)]
struct Dirs {
    config: PathBuf,
    data: PathBuf,
    backup: PathBuf,
    gsettings: bool,
}

impl Dirs {
    fn user() -> Self {
        Self { config: config_dir(), data: data_dir(), backup: backup_dir(), gsettings: true }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}
fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}
fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"))
}
fn backup_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/state")).join("feather-de/gtk-backup")
}

pub fn theme_name(dark: bool) -> &'static str {
    if dark { "Feather-Dark" } else { "Feather" }
}

/// One stylesheet of the theme
pub fn css(gtk4: bool, dark: bool) -> String {
    let base = match (gtk4, dark) {
        (false, false) => "resource:///org/gtk/libgtk/theme/Adwaita/gtk-contained.css",
        (false, true) => "resource:///org/gtk/libgtk/theme/Adwaita/gtk-contained-dark.css",
        (true, false) => "resource:///org/gtk/libgtk/theme/Default/Default-light.css",
        (true, true) => "resource:///org/gtk/libgtk/theme/Default/Default-dark.css",
    };
    format!("{MARK}\n@import url(\"{base}\");\n{}\n{}", if dark { COLORS_DARK } else { COLORS_LIGHT }, if gtk4 { GTK4 } else { GTK3 })
}

/// The libadwaita user stylesheet: only our rules, no base theme
pub fn user_css(dark: bool) -> String {
    format!("{MARK}\n{}\n{}", if dark { COLORS_DARK } else { COLORS_LIGHT }, GTK4)
}

/// Value of `key` in the [Settings] group of a GTK settings.ini
pub fn ini_get(text: &str, key: &str) -> Option<String> {
    let mut group = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            group = t == "[Settings]";
        } else if group {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == key {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// Sets (Some) or removes (None) keys of the [Settings] group, keeping everything else
pub fn ini_set(text: &str, keys: &[(&str, Option<&str>)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    let mut group = false;
    let mut seen_group = false;
    let flush = |out: &mut Vec<String>, done: &mut Vec<String>| {
        for (k, v) in keys {
            if let Some(v) = v.filter(|_| !done.iter().any(|d| d == k)) {
                out.push(format!("{k}={v}"));
                done.push(k.to_string());
            }
        }
    };
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if group {
                flush(&mut out, &mut done);
            }
            group = t == "[Settings]";
            seen_group |= group;
            out.push(line.to_string());
            continue;
        }
        if group {
            if let Some((k, _)) = t.split_once('=') {
                if let Some((key, value)) = keys.iter().find(|(key, _)| *key == k.trim()) {
                    if let Some(v) = value {
                        out.push(format!("{key}={v}"));
                    }
                    done.push(key.to_string());
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if !seen_group {
        out.push("[Settings]".into());
        group = true;
    }
    if group {
        flush(&mut out, &mut done);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// gtk-modules without KDE's decoration module (None when nothing is left)
pub fn without_kde_decorations(modules: &str) -> Option<String> {
    let kept: Vec<_> = modules.split(':').filter(|m| !m.is_empty() && *m != KDE_DECORATIONS).collect();
    (!kept.is_empty()).then(|| kept.join(":"))
}

fn write(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("feather-tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let out = std::process::Command::new("gsettings").args(["get", schema, key]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_string())
}
fn gsettings_set(schema: &str, key: &str, value: &str) {
    let _ = std::process::Command::new("gsettings").args(["set", schema, key, value]).status();
}

const SETTINGS_KEYS: [&str; 4] = ["gtk-theme-name", "gtk-decoration-layout", "gtk-application-prefer-dark-theme", "gtk-modules"];

/// Saves what we are about to change, once (a later apply keeps the first backup)
fn backup(d: &Dirs) {
    let dir = d.backup.clone();
    if dir.join("values.conf").exists() {
        return;
    }
    let mut text = String::new();
    for (schema, key) in [("org.gnome.desktop.interface", "gtk-theme"), ("org.gnome.desktop.interface", "color-scheme"), ("org.gnome.desktop.wm.preferences", "button-layout")] {
        if let Some(v) = d.gsettings.then(|| gsettings_get(schema, key)).flatten() {
            text.push_str(&format!("gsettings\t{schema}\t{key}\t{v}\n"));
        }
    }
    for gtk in ["gtk-3.0", "gtk-4.0"] {
        let ini = std::fs::read_to_string(d.config.join(gtk).join("settings.ini")).unwrap_or_default();
        for key in SETTINGS_KEYS {
            // An absent key is recorded too, so restoring removes ours
            let v = ini_get(&ini, key).unwrap_or_else(|| "\u{0}".into());
            text.push_str(&format!("ini\t{gtk}\t{key}\t{v}\n"));
        }
    }
    let css = d.config.join("gtk-4.0/gtk.css");
    if let Ok(old) = std::fs::read_to_string(&css) {
        if !old.starts_with(MARK) {
            write(&dir.join("gtk-4.0.css"), &old);
        }
    }
    write(&dir.join("values.conf"), &text);
}

fn install_theme(d: &Dirs) {
    for dark in [false, true] {
        let root = d.data.join("themes").join(theme_name(dark));
        write(&root.join("index.theme"), &format!("[Desktop Entry]\nType=X-GNOME-Metatheme\nName={}\nComment=Пёрышко (Feather DE), macOS Tahoe style\n\n[X-GNOME-Metatheme]\nGtkTheme={}\n", theme_name(dark), theme_name(dark)));
        for (gtk, four) in [("gtk-3.0", false), ("gtk-4.0", true)] {
            write(&root.join(gtk).join("gtk.css"), &css(four, dark));
            // GTK uses gtk-dark.css when an app prefers the dark variant
            write(&root.join(gtk).join("gtk-dark.css"), &css(four, true));
        }
    }
}

/// Turns the theme on for the current appearance (also used when it changes)
pub fn apply(dark: bool) {
    let _ = std::thread::Builder::new().name("feather-gtk-theme".into()).spawn(move || apply_in(&Dirs::user(), dark));
}

fn apply_in(d: &Dirs, dark: bool) {
    backup(d);
    install_theme(d);
    let name = theme_name(dark);
    let prefer = if dark { "true" } else { "false" };
    for gtk in ["gtk-3.0", "gtk-4.0"] {
        let path = d.config.join(gtk).join("settings.ini");
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let modules = ini_get(&old, "gtk-modules").map(|m| without_kde_decorations(&m));
        let mut keys: Vec<(&str, Option<&str>)> = vec![("gtk-theme-name", Some(name)), ("gtk-decoration-layout", Some(LAYOUT)), ("gtk-application-prefer-dark-theme", Some(prefer))];
        if let Some(m) = &modules {
            keys.push(("gtk-modules", m.as_deref()));
        }
        write(&path, &ini_set(&old, &keys));
    }
    write(&d.config.join("gtk-4.0/gtk.css"), &user_css(dark));
    if d.gsettings {
        gsettings_set("org.gnome.desktop.interface", "gtk-theme", name);
        gsettings_set("org.gnome.desktop.interface", "color-scheme", if dark { "prefer-dark" } else { "default" });
        gsettings_set("org.gnome.desktop.wm.preferences", "button-layout", LAYOUT);
    }
}

/// Puts back what was there before the first apply
pub fn restore() {
    let _ = std::thread::Builder::new().name("feather-gtk-theme".into()).spawn(|| restore_in(&Dirs::user()));
}

fn restore_in(d: &Dirs) {
    {
        let dir = d.backup.clone();
        let Ok(text) = std::fs::read_to_string(dir.join("values.conf")) else { return };
        let mut ini: std::collections::HashMap<&str, Vec<(String, Option<String>)>> = Default::default();
        for line in text.lines() {
            let f: Vec<_> = line.splitn(4, '\t').collect();
            match f.as_slice() {
                ["gsettings", schema, key, value] => if d.gsettings { gsettings_set(schema, key, value) },
                ["ini", gtk, key, value] => {
                    let gtk: &str = if *gtk == "gtk-4.0" { "gtk-4.0" } else { "gtk-3.0" };
                    ini.entry(gtk).or_default().push((key.to_string(), (*value != "\u{0}").then(|| value.to_string())));
                }
                _ => {}
            }
        }
        for (gtk, keys) in ini {
            let path = d.config.join(gtk).join("settings.ini");
            let old = std::fs::read_to_string(&path).unwrap_or_default();
            let keys: Vec<(&str, Option<&str>)> = keys.iter().map(|(k, v)| (k.as_str(), v.as_deref())).collect();
            write(&path, &ini_set(&old, &keys));
        }
        let css = d.config.join("gtk-4.0/gtk.css");
        match std::fs::read_to_string(dir.join("gtk-4.0.css")) {
            Ok(old) => write(&css, &old),
            Err(_) => {
                if std::fs::read_to_string(&css).is_ok_and(|t| t.starts_with(MARK)) {
                    let _ = std::fs::remove_file(&css);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_ini_keys_change_without_touching_the_rest() {
        let text = "[Settings]\ngtk-theme-name=Breeze\ngtk-font-name=Inter, 10\ngtk-modules=window-decorations-gtk-module:colorreload-gtk-module\n\n[Other]\ngtk-theme-name=keep\n";
        assert_eq!(ini_get(text, "gtk-theme-name").as_deref(), Some("Breeze"));
        let out = ini_set(text, &[("gtk-theme-name", Some("Feather")), ("gtk-decoration-layout", Some(LAYOUT)), ("gtk-modules", Some("colorreload-gtk-module"))]);
        assert_eq!(ini_get(&out, "gtk-theme-name").as_deref(), Some("Feather"));
        assert_eq!(ini_get(&out, "gtk-decoration-layout").as_deref(), Some(LAYOUT), "added to [Settings]");
        assert!(out.contains("gtk-font-name=Inter, 10") && out.contains("[Other]\ngtk-theme-name=keep"), "{out}");
        let back = ini_set(&out, &[("gtk-theme-name", Some("Breeze")), ("gtk-decoration-layout", None)]);
        assert_eq!(ini_get(&back, "gtk-decoration-layout"), None, "a key that was absent goes away");
        assert_eq!(ini_get(&ini_set("", &[("gtk-theme-name", Some("Feather"))]), "gtk-theme-name").as_deref(), Some("Feather"));
        assert_eq!(without_kde_decorations("window-decorations-gtk-module:colorreload-gtk-module").as_deref(), Some("colorreload-gtk-module"));
        assert_eq!(without_kde_decorations("window-decorations-gtk-module"), None);
    }

    #[test]
    fn apply_and_restore_round_trip_on_files() {
        let root = std::env::temp_dir().join(format!("feather-gtk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let d = Dirs { config: root.join("config"), data: root.join("data"), backup: root.join("backup"), gsettings: false };
        let ini3 = "[Settings]\ngtk-theme-name=Breeze\ngtk-font-name=Inter, 10\ngtk-modules=window-decorations-gtk-module:colorreload-gtk-module\n";
        write(&d.config.join("gtk-3.0/settings.ini"), ini3);
        write(&d.config.join("gtk-4.0/gtk.css"), "@import 'colors.css';\n");
        apply_in(&d, true);
        let now = std::fs::read_to_string(d.config.join("gtk-3.0/settings.ini")).unwrap();
        assert_eq!(ini_get(&now, "gtk-theme-name").as_deref(), Some("Feather-Dark"));
        assert_eq!(ini_get(&now, "gtk-modules").as_deref(), Some("colorreload-gtk-module"), "KDE's title buttons are off");
        assert_eq!(ini_get(&now, "gtk-decoration-layout").as_deref(), Some(LAYOUT));
        assert!(d.data.join("themes/Feather-Dark/gtk-3.0/gtk.css").exists() && d.data.join("themes/Feather/gtk-4.0/gtk-dark.css").exists());
        assert!(std::fs::read_to_string(d.config.join("gtk-4.0/gtk.css")).unwrap().starts_with(MARK));
        // Switching to light keeps the first backup
        apply_in(&d, false);
        assert_eq!(ini_get(&std::fs::read_to_string(d.config.join("gtk-3.0/settings.ini")).unwrap(), "gtk-theme-name").as_deref(), Some("Feather"));
        restore_in(&d);
        let back = std::fs::read_to_string(d.config.join("gtk-3.0/settings.ini")).unwrap();
        assert_eq!(ini_get(&back, "gtk-theme-name").as_deref(), Some("Breeze"));
        assert_eq!(ini_get(&back, "gtk-modules").as_deref(), Some("window-decorations-gtk-module:colorreload-gtk-module"));
        assert_eq!(ini_get(&back, "gtk-decoration-layout"), None, "a key that was absent goes away");
        assert_eq!(std::fs::read_to_string(d.config.join("gtk-4.0/gtk.css")).unwrap(), "@import 'colors.css';\n");
        assert!(!d.backup.exists());
        // gtk-4.0/settings.ini did not exist: restoring leaves no theme keys behind
        let four = std::fs::read_to_string(d.config.join("gtk-4.0/settings.ini")).unwrap_or_default();
        assert_eq!(ini_get(&four, "gtk-theme-name"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stylesheets_import_the_right_base_and_are_marked() {
        assert!(css(false, false).contains("Adwaita/gtk-contained.css"));
        assert!(css(false, true).contains("gtk-contained-dark.css") && css(false, true).contains("#1e1e20"));
        assert!(css(true, false).contains("Default-light.css"));
        assert!(user_css(true).starts_with(MARK) && !user_css(true).contains("@import"));
    }
}
