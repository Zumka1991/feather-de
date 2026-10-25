use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use tiny_skia::{Color, PixmapMut};

use crate::text::FontSystem;
use crate::theme::{
    fill_circle, fill_path_gradient, fill_rounded_rect, fill_squircle_gradient, rgba,
    stroke_line, stroke_squircle,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconKind {
    Finder,
    Terminal,
    Browser,
    Antigravity,
    Vpn,
    Notes,
    Preview,
    ActivityMonitor,
    Settings,
    Launchpad,
    GenericApp,
    TrashEmpty,
    TrashFull,
}

#[derive(Debug, Clone)]
pub enum AppAction {
    /// Launches an external command inside our Wayland compositor session
    Exec(String),
    /// Opens a built-in native Feather DE system window (< 0.5 ms, zero external deps)
    BuiltinWindow(BuiltinWindowKind),
    /// Hot-reloads the Shell UI & config without dropping any Wayland client connections
    HotReloadShell,
    /// Shows the full-screen app grid (Launchpad)
    OpenLaunchpad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinWindowKind {
    AboutFeather,
    ActivityMonitor,
    QuickNotes,
    SystemSettings,
}

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    /// Procedural icon used when no themed icon is found
    pub icon: IconKind,
    /// Freedesktop icon name (or absolute path) from the .desktop file
    pub icon_name: Option<String>,
    /// StartupWMClass: what the app reports as its Wayland app_id
    pub wm_class: Option<String>,
    /// Other ids this app is known by (desktop file stems, reverse-DNS ids)
    pub aliases: Vec<String>,
    pub action: AppAction,
    pub pinned_in_dock: bool,
    /// Always draw the procedural icon: a .desktop theme icon must not replace it
    pub own_icon: bool,
}

impl AppEntry {
    fn new(id: &str, name: &'static str, subtitle: &'static str, icon: IconKind, action: AppAction, pinned: bool) -> Self {
        let loc_name = feather_ui::i18n::tr_static(name);
        let loc_sub = feather_ui::i18n::tr_static(subtitle);
        let mut aliases = Vec::new();
        if loc_name != name {
            aliases.push(name.to_string());
        }
        Self {
            id: id.to_string(),
            name: loc_name.to_string(),
            subtitle: loc_sub.to_string(),
            icon,
            icon_name: None,
            wm_class: None,
            aliases,
            action,
            pinned_in_dock: pinned,
            own_icon: false,
        }
    }

    fn with_own_icon(mut self) -> Self {
        self.own_icon = true;
        self
    }

    fn with_aliases(mut self, aliases: &[&str]) -> Self {
        for a in aliases {
            if !self.aliases.iter().any(|x| x == a) {
                self.aliases.push((*a).to_string());
            }
        }
        self
    }

    fn with_icon(mut self, icon: &str) -> Self {
        self.icon_name = Some(icon.to_string());
        self
    }

    pub fn is_builtin(&self) -> bool {
        !matches!(self.action, AppAction::Exec(_))
    }

    /// Basename of the executable ("konsole" for "/usr/bin/konsole --foo")
    fn exec_basename(&self) -> Option<String> {
        match &self.action {
            AppAction::Exec(cmd) => cmd
                .split_whitespace()
                .find(|t| !t.contains('='))
                .map(|t| t.trim_matches(|c| c == '"' || c == '\'').rsplit('/').next().unwrap_or(t).to_lowercase()),
            _ => None,
        }
    }
}

/// One of Feather's own programs next to our executable, quoted for `sh -c`
fn own_program(name: &str) -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|path| path.is_file())
        .map(|path| format!("'{}'", path.display().to_string().replace('\'', "'\\''")))
}

/// Feather's file manager, else Dolphin
pub fn files_command() -> String {
    own_program("feather-files").unwrap_or_else(|| "dolphin".into())
}

/// Feather's terminal, else Konsole
pub fn terminal_command() -> String {
    own_program("feather-terminal").unwrap_or_else(|| "konsole".into())
}

/// Feather's Notes app when built next to the compositor binary
pub fn notes_command() -> Option<String> {
    own_program("feather-notes")
}

/// Feather's Preview app (images & PDF) when built next to the compositor binary
pub fn preview_command() -> Option<String> {
    own_program("feather-preview")
}

pub struct AppRegistry {
    pub apps: Vec<AppEntry>,
}

impl AppRegistry {
    pub fn new() -> Self {
        use AppAction::*;
        let mut apps = vec![
            AppEntry::new(
                "builtin.launchpad",
                "Приложения",
                "Все приложения сеткой (Super+A)",
                IconKind::Launchpad,
                OpenLaunchpad,
                true,
            ),
            // Id stays "dolphin" so saved Dock pins keep working; our own Files app is used
            // when it sits next to the compositor binary
            AppEntry::new("dolphin", "Файлы", "Файловый менеджер", IconKind::Finder, Exec(files_command()), true)
                .with_aliases(&["org.kde.dolphin", "feather-files"])
                .with_own_icon(),
            AppEntry::new("firefox", "Firefox", "Веб-браузер", IconKind::Browser, Exec("firefox".into()), true)
                .with_aliases(&["org.mozilla.firefox", "firefox-esr"])
                .with_icon("firefox"),
            // Id stays "konsole" for saved Dock pins; Feather's own Terminal when installed
            AppEntry::new("konsole", "Терминал", "Терминал", IconKind::Terminal, Exec(terminal_command()), true)
                .with_aliases(&["org.kde.konsole", "feather-terminal"]),
            AppEntry::new(
                "antigravity",
                "Antigravity",
                "Разработка Пёрышко DE на ходу",
                IconKind::Antigravity,
                Exec("/home/zumka/.local/bin/antigravity".into()),
                true,
            ),
            AppEntry::new("mousevpn", "MouseVPN", "VPN-клиент", IconKind::Vpn, Exec("mousevpn-linux-gui".into()), true),
            // Id stays "builtin.notes" for saved Dock pins; Feather's Notes app when installed
            match own_program("feather-notes") {
                Some(cmd) => AppEntry::new("builtin.notes", "Заметки", "Заметки в стиле macOS", IconKind::Notes, Exec(cmd), true)
                    .with_aliases(&["feather-notes"]),
                None => AppEntry::new("builtin.notes", "Заметки", "Быстрые заметки (RU / EN)", IconKind::Notes, BuiltinWindow(BuiltinWindowKind::QuickNotes), true),
            },
            AppEntry::new(
                "builtin.preview",
                "Просмотр",
                "Изображения и документы PDF",
                IconKind::Preview,
                Exec(preview_command().unwrap_or_else(|| "feather-preview".into())),
                true,
            )
            .with_aliases(&["feather-preview"])
            .with_own_icon(),
            AppEntry::new(
                "builtin.monitor",
                "Мониторинг системы",
                "Память, окна Wayland и рендер",
                IconKind::ActivityMonitor,
                BuiltinWindow(BuiltinWindowKind::ActivityMonitor),
                true,
            ),
            AppEntry::new(
                "builtin.settings",
                "Системные настройки",
                "Оформление, Dock, язык и масштаб",
                IconKind::Settings,
                BuiltinWindow(BuiltinWindowKind::SystemSettings),
                true,
            ),
            AppEntry::new(
                "builtin.about",
                "Об этом компьютере",
                "Пёрышко DE: версия и горячие клавиши",
                IconKind::GenericApp,
                BuiltinWindow(BuiltinWindowKind::AboutFeather),
                false,
            ),
            AppEntry::new(
                "builtin.reload",
                "Перезагрузить оболочку",
                "Обновить интерфейс без закрытия окон (Super+Shift+R)",
                IconKind::Settings,
                HotReloadShell,
                false,
            ),
        ];

        if let Ok(home) = std::env::var("HOME") {
            Self::scan_desktop_dir(&Path::new(&home).join(".local/share/applications"), &mut apps);
        }
        Self::scan_desktop_dir(Path::new("/usr/share/applications"), &mut apps);

        Self { apps }
    }

    fn scan_desktop_dir(dir: &Path, out: &mut Vec<AppEntry>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let active_lang = feather_ui::scale::lang();
        let name_keys = active_lang.desktop_name_keys();
        let comment_keys = active_lang.desktop_comment_keys();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let mut name = None;
            let mut name_loc = None;
            let mut comment = None;
            let mut comment_loc = None;
            let mut exec = None;
            let mut icon_name = None;
            let mut wm_class = None;
            let mut no_display = false;
            let mut in_desktop_entry = false;

            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_desktop_entry = line == "[Desktop Entry]";
                    continue;
                }
                if !in_desktop_entry {
                    continue;
                }
                if let Some(v) = name_keys.iter().find_map(|k| line.strip_prefix(k)) {
                    name_loc = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("Name=") {
                    name.get_or_insert_with(|| v.trim().to_string());
                } else if let Some(v) = comment_keys.iter().find_map(|k| line.strip_prefix(k)) {
                    comment_loc = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("Comment=") {
                    comment.get_or_insert_with(|| v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("Exec=") {
                    let cleaned = v
                        .split_whitespace()
                        .filter(|part| !part.starts_with('%'))
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !cleaned.is_empty() {
                        exec = Some(cleaned);
                    }
                } else if let Some(v) = line.strip_prefix("Icon=") {
                    icon_name = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("StartupWMClass=") {
                    wm_class = Some(v.trim().to_string());
                } else if line == "NoDisplay=true" || line == "Hidden=true" {
                    no_display = true;
                }
            }

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("app")
                .to_string();
            let Some(cmd) = exec else { continue };
            let candidate = AppEntry {
                id: stem.clone(),
                name: name_loc.or(name).unwrap_or_else(|| stem.clone()),
                subtitle: comment_loc.or(comment).unwrap_or_else(|| cmd.clone()),
                icon: IconKind::GenericApp,
                icon_name,
                wm_class,
                aliases: Vec::new(),
                action: AppAction::Exec(cmd),
                pinned_in_dock: false,
                own_icon: false,
            };

            // Same program as a pinned/known entry: enrich it (icon, WM class) instead of
            // adding a duplicate
            let exe = candidate.exec_basename();
            if let Some(existing) = out.iter_mut().find(|a| {
                a.id.eq_ignore_ascii_case(&stem)
                    || a.aliases.iter().any(|x| x.eq_ignore_ascii_case(&stem))
                    || (exe.is_some() && a.exec_basename() == exe)
            }) {
                if existing.icon_name.is_none() && !existing.own_icon {
                    existing.icon_name = candidate.icon_name.clone();
                }
                if existing.wm_class.is_none() {
                    existing.wm_class = candidate.wm_class.clone();
                }
                if !existing.aliases.iter().any(|x| x.eq_ignore_ascii_case(&stem)) {
                    existing.aliases.push(stem);
                }
                continue;
            }
            if no_display {
                // Still useful to match windows (e.g. URL handlers), but never listed
                continue;
            }
            out.push(candidate);
        }
    }

    /// The app a Wayland window belongs to, from its app_id
    pub fn app_for_app_id(&self, app_id: &str) -> Option<usize> {
        if app_id.is_empty() {
            return None;
        }
        let a = app_id.to_lowercase();
        let tail = a.rsplit('.').next().unwrap_or(&a).to_string();
        let matches = |e: &AppEntry, key: &str| {
            e.id.eq_ignore_ascii_case(key)
                || e.aliases.iter().any(|x| x.eq_ignore_ascii_case(key))
                || e.wm_class.as_deref().is_some_and(|w| w.eq_ignore_ascii_case(key))
                || e.exec_basename().as_deref() == Some(key)
        };
        self.apps
            .iter()
            .position(|e| matches(e, &a))
            .or_else(|| self.apps.iter().position(|e| matches(e, &tail)))
    }

    /// Default Dock contents (used until the user rearranges the Dock)
    /// Feather's own apps, first in Launchpad in this order.
    pub const CORE: [&'static str; 5] = ["dolphin", "konsole", "builtin.notes", "builtin.settings", "builtin.monitor"];
    /// Always in the Dock, like Finder on macOS.
    pub const PERMANENT: &'static str = "dolphin";

    pub fn default_pins(&self) -> Vec<String> {
        self.apps
            .iter()
            .filter(|a| a.pinned_in_dock)
            .map(|a| a.id.clone())
            .collect()
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.apps.iter().position(|a| a.id == id)
    }

    /// Apps shown in Launchpad: everything launchable, alphabetically, optionally filtered
    pub fn launchpad_apps(&self, query: &str) -> Vec<usize> {
        let q = query.trim().to_lowercase();
        let mut list: Vec<usize> = self
            .apps
            .iter()
            .enumerate()
            .filter(|(_, a)| matches!(a.action, AppAction::Exec(_) | AppAction::BuiltinWindow(_)))
            .filter(|(_, a)| {
                q.is_empty()
                    || a.name.to_lowercase().contains(&q)
                    || a.id.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        let rank = |i: usize| Self::CORE.iter().position(|c| *c == self.apps[i].id).unwrap_or(Self::CORE.len());
        list.sort_by_cached_key(|&i| (rank(i), self.apps[i].name.to_lowercase()));
        list
    }

    pub fn search(&self, query: &str) -> Vec<&AppEntry> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.apps.iter().take(7).collect();
        }
        let mut matches: Vec<(i32, &AppEntry)> = self
            .apps
            .iter()
            .filter_map(|app| {
                let name_l = app.name.to_lowercase();
                let sub_l = app.subtitle.to_lowercase();
                let id_l = app.id.to_lowercase();
                if name_l.starts_with(&q) || id_l.starts_with(&q) {
                    Some((100, app))
                } else if name_l.contains(&q) || id_l.contains(&q) {
                    Some((70, app))
                } else if sub_l.contains(&q) {
                    Some((40, app))
                } else {
                    None
                }
            })
            .collect();
        matches.sort_by(|a, b| b.0.cmp(&a.0));
        matches.into_iter().map(|(_, a)| a).take(7).collect()
    }
}

/// Cursor theme/size handed to programs started from now on (XCURSOR_THEME/XCURSOR_SIZE).
static CURSOR_ENV: std::sync::Mutex<Option<(String, u32)>> = std::sync::Mutex::new(None);

pub fn set_cursor_env(theme: String, size: u32) {
    *CURSOR_ENV.lock().unwrap() = Some((theme, size));
}

/// Spawns an external command connected to our compositor's Wayland socket
pub fn spawn_wayland_command(cmd_line: &str, wayland_socket: &str) -> bool {
    let trimmed = cmd_line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let current_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin".into());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/zumka".into());
    let enriched_path = format!("{home}/.cargo/bin:{home}/.local/bin:/usr/local/bin:{current_path}");

    let mut command = Command::new("sh");
    if let Some((theme, size)) = CURSOR_ENV.lock().unwrap().clone() {
        command.env("XCURSOR_THEME", theme).env("XCURSOR_SIZE", size.to_string());
    }
    // X programs go to our XWayland, never to a host X server of a nested run
    match crate::xwayland::display() {
        Some(display) => { command.env("DISPLAY", display); }
        None if std::env::var_os("FEATHER_XWAYLAND").is_some() => { command.env_remove("DISPLAY"); }
        None => {}
    }
    let region_locale = feather_ui::scale::region().locale_code();
    command
        .arg("-c")
        .arg(trimmed)
        .env("PATH", enriched_path)
        .env("WAYLAND_DISPLAY", wayland_socket)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "FeatherDE")
        .env("LANGUAGE", feather_ui::scale::lang().id())
        .env("LC_TIME", region_locale)
        .env("LC_MEASUREMENT", region_locale)
        .env("QT_QPA_PLATFORM", "wayland;xcb")
        .env("GDK_BACKEND", "wayland,x11")
        .env("GTK_USE_PORTAL", "1")
        .env("PLASMA_INTEGRATION_USE_PORTAL", "1")
        .env("QT_QPA_PLATFORMTHEME", std::env::var("QT_QPA_PLATFORMTHEME").unwrap_or_else(|_|"xdgdesktopportal".into()))
        .env("MOZ_ENABLE_WAYLAND", "1")
        .env("ELECTRON_OZONE_PLATFORM_HINT", "auto")
        .env("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|mut child| {
            // Reap the child when it exits; otherwise every closed app stays a zombie
            // until the compositor itself exits
            let _ = std::thread::Builder::new()
                .name("feather-reaper".into())
                .stack_size(32 * 1024)
                .spawn(move || {
                    let _ = child.wait();
                });
        })
        .is_ok()
}

/// External polkit agent, used when Feather's own agent cannot register.
pub fn spawn_polkit_fallback(wayland_socket: &str) {
    let polkit_cmd = "killall -q lxqt-policykit-agent polkit-kde-authentication-agent-1 2>/dev/null; \
        sleep 0.15; \
        if command -v lxqt-policykit-agent >/dev/null 2>&1; then \
            exec lxqt-policykit-agent; \
        elif [ -x /usr/lib/polkit-kde-authentication-agent-1 ]; then \
            exec /usr/lib/polkit-kde-authentication-agent-1; \
        fi";
    spawn_wayland_command(polkit_cmd, wayland_socket);
}

/// Starts essential background session daemons (Polkit Authentication Agent for VPN/pkexec, D-Bus env)
pub fn spawn_session_services(wayland_socket: &str, is_standalone: bool) {
    if is_standalone {
        crate::file_portal::session_start(wayland_socket.to_owned());


    } else {
        // Even in nested mode, ensure a Polkit agent is running if none is active
        let polkit_fallback = "pgrep -u \"$(id -u)\" -f 'lxqt-policykit-agent|polkit-kde-authentication-agent-1' >/dev/null 2>&1 || \
            if command -v lxqt-policykit-agent >/dev/null 2>&1; then \
                exec lxqt-policykit-agent; \
            elif [ -x /usr/lib/polkit-kde-authentication-agent-1 ]; then \
                exec /usr/lib/polkit-kde-authentication-agent-1; \
            fi";
        spawn_wayland_command(polkit_fallback, wayland_socket);
    }
}

/// Evaluates simple math expressions in Spotlight (e.g. "1920 * 1080", "256 / 8 + 14")
pub fn eval_spotlight_math(expr: &str) -> Option<String> {
    let s = expr.trim().replace(',', ".");
    if s.is_empty()
        || !s.chars().any(|c| matches!(c, '+' | '-' | '*' | '/'))
        || !s
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '*' | '/' | '.' | ' ' | '(' | ')'))
    {
        return None;
    }

    // Simple recursive descent parser for +, -, *, /, ()
    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
    }
    impl<'a> Parser<'a> {
        fn skip_ws(&mut self) {
            while self.pos < self.bytes.len() && self.bytes[self.pos] == b' ' {
                self.pos += 1;
            }
        }
        fn parse_expr(&mut self) -> Option<f64> {
            let mut val = self.parse_term()?;
            loop {
                self.skip_ws();
                if self.pos >= self.bytes.len() {
                    break;
                }
                match self.bytes[self.pos] {
                    b'+' => {
                        self.pos += 1;
                        val += self.parse_term()?;
                    }
                    b'-' => {
                        self.pos += 1;
                        val -= self.parse_term()?;
                    }
                    _ => break,
                }
            }
            Some(val)
        }
        fn parse_term(&mut self) -> Option<f64> {
            let mut val = self.parse_factor()?;
            loop {
                self.skip_ws();
                if self.pos >= self.bytes.len() {
                    break;
                }
                match self.bytes[self.pos] {
                    b'*' => {
                        self.pos += 1;
                        val *= self.parse_factor()?;
                    }
                    b'/' => {
                        self.pos += 1;
                        let rhs = self.parse_factor()?;
                        if rhs.abs() < 1e-12 {
                            return None;
                        }
                        val /= rhs;
                    }
                    _ => break,
                }
            }
            Some(val)
        }
        fn parse_factor(&mut self) -> Option<f64> {
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                return None;
            }
            if self.bytes[self.pos] == b'(' {
                self.pos += 1;
                let v = self.parse_expr()?;
                self.skip_ws();
                if self.pos >= self.bytes.len() || self.bytes[self.pos] != b')' {
                    return None;
                }
                self.pos += 1;
                return Some(v);
            }
            let start = self.pos;
            if self.bytes[self.pos] == b'-' {
                self.pos += 1;
            }
            while self.pos < self.bytes.len()
                && (self.bytes[self.pos].is_ascii_digit() || self.bytes[self.pos] == b'.')
            {
                self.pos += 1;
            }
            if self.pos == start {
                return None;
            }
            std::str::from_utf8(&self.bytes[start..self.pos])
                .ok()?
                .parse::<f64>()
                .ok()
        }
    }

    let mut p = Parser {
        bytes: s.as_bytes(),
        pos: 0,
    };
    let res = p.parse_expr()?;
    p.skip_ws();
    if p.pos != p.bytes.len() || !res.is_finite() {
        return None;
    }
    if (res - res.round()).abs() < 1e-9 {
        Some(format!("{}", res.round() as i64))
    } else {
        Some(format!("{:.4}", res).trim_end_matches('0').trim_end_matches('.').to_string())
    }
}

/// Draws a procedural macOS-style squircle icon (for built-in apps and as a fallback)
#[allow(clippy::too_many_arguments)]
/// Closed gear outline: `teeth` trapezoid teeth between radii `inner` and `outer`.
fn gear_path(cx: f32, cy: f32, inner: f32, outer: f32, teeth: usize) -> Option<tiny_skia::Path> {
    let mut b = tiny_skia::PathBuilder::new();
    let step = std::f32::consts::TAU / teeth as f32;
    for i in 0..teeth {
        let a = i as f32 * step - std::f32::consts::FRAC_PI_2;
        // root → flank up → tip → flank down (tips narrower than roots)
        let pts = [(a, inner), (a + step * 0.16, outer), (a + step * 0.44, outer), (a + step * 0.60, inner)];
        for (k, (ang, r)) in pts.into_iter().enumerate() {
            let (px, py) = (cx + ang.cos() * r, cy + ang.sin() * r);
            if i == 0 && k == 0 {
                b.move_to(px, py);
            } else {
                b.line_to(px, py);
            }
        }
        // Root arc to the next tooth
        let next = a + step;
        let mid = a + step * 0.80;
        b.quad_to(cx + mid.cos() * inner * 1.02, cy + mid.sin() * inner * 1.02, cx + next.cos() * inner, cy + next.sin() * inner);
    }
    b.close();
    b.finish()
}

/// System Settings: brushed-metal tile, graphite gear with a recessed ring and an inner gear.
fn draw_settings_icon(pm: &mut PixmapMut, x: f32, y: f32, size: f32) {
    let k = size;
    fill_squircle_gradient(pm, x, y, size, rgba(236, 237, 241, 255), rgba(168, 171, 180, 255));
    let (cx, cy) = (x + k * 0.5, y + k * 0.5);
    let circle = |r: f32| tiny_skia::PathBuilder::from_circle(cx, cy, k * r);
    let grad = |pm: &mut PixmapMut, path: Option<tiny_skia::Path>, r: f32, top: Color, bottom: Color| {
        if let Some(path) = path {
            fill_path_gradient(pm, &path, cy - k * r, cy + k * r, top, bottom);
        }
    };
    // Soft shadow of the big gear on the tile
    for (d, a) in [(0.012, 40u8), (0.024, 22)] {
        if let Some(path) = gear_path(cx, cy + k * d, k * 0.335, k * 0.425, 18) {
            fill_path_gradient(pm, &path, y, y + k, rgba(30, 32, 40, a), rgba(30, 32, 40, a));
        }
    }
    grad(pm, gear_path(cx, cy, k * 0.335, k * 0.425, 18), 0.43, rgba(118, 121, 130, 255), rgba(52, 54, 62, 255));
    // Bevel: light top rim of the gear body
    if let Some(path) = circle(0.33) {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(rgba(255, 255, 255, 60));
        paint.anti_alias = true;
        pm.stroke_path(&path, &paint, &tiny_skia::Stroke { width: k * 0.012, ..Default::default() }, crate::theme::scale_transform(), None);
    }
    // Recessed ring
    grad(pm, circle(0.29), 0.29, rgba(150, 153, 162, 255), rgba(222, 224, 230, 255));
    grad(pm, circle(0.255), 0.255, rgba(205, 208, 215, 255), rgba(150, 153, 161, 255));
    // Inner gear
    grad(pm, gear_path(cx, cy, k * 0.15, k * 0.205, 10), 0.21, rgba(104, 107, 116, 255), rgba(46, 48, 56, 255));
    // Hub
    grad(pm, circle(0.105), 0.105, rgba(246, 247, 250, 255), rgba(170, 173, 182, 255));
    grad(pm, circle(0.045), 0.045, rgba(70, 72, 80, 255), rgba(120, 123, 132, 255));
}

/// The Dock trash, macOS-like: a perforated glass bin in perspective, sized to carry the
/// same visual weight as the tiles next to it; crumpled paper when full.
fn draw_trash(pm: &mut PixmapMut, x: f32, y: f32, size: f32, full: bool) {
    let k = size;
    let at = |fx: f32, fy: f32| (x + k * fx, y + k * fy);
    let oval = |cx: f32, cy: f32, rx: f32, ry: f32| {
        tiny_skia::Rect::from_xywh(x + k * (cx - rx), y + k * (cy - ry), k * rx * 2.0, k * ry * 2.0)
            .and_then(tiny_skia::PathBuilder::from_oval)
    };
    let fill = |pm: &mut PixmapMut, path: Option<tiny_skia::Path>, y0: f32, y1: f32, top: Color, bottom: Color| {
        if let Some(path) = path {
            fill_path_gradient(pm, &path, y + k * y0, y + k * y1, top, bottom);
        }
    };
    // Soft contact shadow.
    for (i, a) in [(0.0, 18u8), (0.02, 14), (0.04, 10)] {
        fill(pm, oval(0.5, 0.955, 0.30 + i, 0.035 + i * 0.5), 0.9, 1.0, rgba(0, 0, 0, a), rgba(0, 0, 0, a));
    }
    // Rim: a metallic ring around the opening. The whole ring goes behind the paper,
    // its front half again over the body, so paper sits *inside* the bin.
    let rim = |pm: &mut PixmapMut, front_only: bool| {
        let (cx, cy, rx, ry) = (x + k * 0.5, y + k * 0.17, k * 0.40, k * 0.09);
        let ring = |rx: f32, ry: f32| {
            let mut b = tiny_skia::PathBuilder::new();
            let c = 0.5523;
            b.move_to(cx - rx, cy);
            b.cubic_to(cx - rx, cy + ry * c, cx - rx * c, cy + ry, cx, cy + ry);
            b.cubic_to(cx + rx * c, cy + ry, cx + rx, cy + ry * c, cx + rx, cy);
            if !front_only {
                b.cubic_to(cx + rx, cy - ry * c, cx + rx * c, cy - ry, cx, cy - ry);
                b.cubic_to(cx - rx * c, cy - ry, cx - rx, cy - ry * c, cx - rx, cy);
            }
            b.finish()
        };
        let mut paint = tiny_skia::Paint::default();
        paint.anti_alias = true;
        if let Some(shader) = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(0.0, y + k * 0.08),
            tiny_skia::Point::from_xy(0.0, y + k * 0.26),
            vec![
                tiny_skia::GradientStop::new(0.0, rgba(250, 251, 253, 255)),
                tiny_skia::GradientStop::new(1.0, rgba(150, 158, 172, 255)),
            ],
            tiny_skia::SpreadMode::Pad,
            tiny_skia::Transform::identity(),
        ) {
            paint.shader = shader;
        }
        let t = crate::theme::scale_transform();
        if let Some(path) = ring(rx, ry) {
            pm.stroke_path(&path, &paint, &tiny_skia::Stroke { width: k * 0.035, ..Default::default() }, t, None);
        }
        paint.shader = tiny_skia::Shader::SolidColor(rgba(60, 68, 82, 110));
        if let Some(path) = ring(rx + k * 0.018, ry + k * 0.018) {
            pm.stroke_path(&path, &paint, &tiny_skia::Stroke { width: k * 0.008, ..Default::default() }, t, None);
        }
    };
    // Inside of the bin, seen through the opening.
    fill(pm, oval(0.5, 0.17, 0.39, 0.085), 0.09, 0.26, rgba(92, 98, 110, 255), rgba(150, 156, 168, 255));
    rim(pm, false);
    if full {
        let papers = [(0.33, 0.10, 0.13, 0.10, 236u8), (0.58, 0.06, 0.16, 0.115, 250), (0.45, 0.14, 0.15, 0.10, 226), (0.72, 0.12, 0.12, 0.09, 214), (0.24, 0.16, 0.09, 0.07, 220)];
        for (cx, cy, rx, ry, shade) in papers {
            fill(pm, oval(cx, cy, rx, ry), cy - ry, cy + ry, rgba(shade, shade, shade, 255), rgba(shade - 18, shade - 18, shade - 14, 255));
            if let Some(path) = oval(cx, cy, rx, ry) {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color(rgba(0, 0, 0, 38));
                paint.anti_alias = true;
                let stroke = tiny_skia::Stroke { width: k * 0.008, ..Default::default() };
                pm.stroke_path(&path, &paint, &stroke, crate::theme::scale_transform(), None);
            }
        }
        // Creases.
        let (cx, cy) = at(0.58, 0.03);
        stroke_line(pm, cx - k * 0.06, cy + k * 0.02, cx + k * 0.02, cy + k * 0.07, k * 0.009, rgba(0, 0, 0, 40));
        let (cx, cy) = at(0.40, 0.08);
        stroke_line(pm, cx - k * 0.04, cy + k * 0.04, cx + k * 0.04, cy, k * 0.009, rgba(0, 0, 0, 34));
    }
    // Body: a tapered cylinder from the rim down to a rounded base.
    let mut body = tiny_skia::PathBuilder::new();
    let (l0, r0, top) = (0.11, 0.89, 0.17);
    let (l1, r1, bottom) = (0.20, 0.80, 0.93);
    body.move_to(x + k * l0, y + k * top);
    body.line_to(x + k * r0, y + k * top);
    body.line_to(x + k * r1, y + k * (bottom - 0.02));
    body.quad_to(x + k * (r1 - 0.01), y + k * (bottom + 0.025), x + k * 0.5, y + k * (bottom + 0.03));
    body.quad_to(x + k * (l1 + 0.01), y + k * (bottom + 0.025), x + k * l1, y + k * (bottom - 0.02));
    body.close();
    let body = body.finish();
    fill(pm, body.clone(), top, bottom, rgba(246, 248, 252, 250), rgba(200, 206, 216, 252));
    // Cylinder shading: highlight left, shade right.
    let band = |pm: &mut PixmapMut, f0: f32, f1: f32, color: Color| {
        let mut b = tiny_skia::PathBuilder::new();
        let lerp = |l: f32, r: f32, f: f32| l + (r - l) * f;
        b.move_to(x + k * lerp(l0, r0, f0), y + k * top);
        b.line_to(x + k * lerp(l0, r0, f1), y + k * top);
        b.line_to(x + k * lerp(l1, r1, f1), y + k * bottom);
        b.line_to(x + k * lerp(l1, r1, f0), y + k * bottom);
        b.close();
        if let Some(path) = b.finish() {
            fill_path_gradient(pm, &path, y + k * top, y + k * bottom, color, color);
        }
    };
    band(pm, 0.08, 0.22, rgba(255, 255, 255, 120));
    band(pm, 0.80, 0.97, rgba(40, 50, 70, 26));
    // Perforated mesh following the taper.
    let rows = 10;
    let cols = 8;
    for r in 0..rows {
        let fy = top + 0.08 + (bottom - top - 0.13) * r as f32 / (rows - 1) as f32;
        let t = (fy - top) / (bottom - top);
        let (left, right) = (l0 + (l1 - l0) * t, r0 + (r1 - r0) * t);
        for c in 0..cols {
            let fx = left + (right - left) * (c as f32 + 0.75) / (cols as f32 + 0.5);
            let (hx, hy) = at(fx, fy);
            // Holes foreshorten towards the sides of the cylinder.
            let edge = ((fx - 0.5) / ((right - left) * 0.5)).abs().min(1.0);
            let hw = k * 0.034 * (1.0 - edge * 0.45);
            fill_rounded_rect(pm, hx - hw / 2.0, hy - k * 0.017, hw, k * 0.034, k * 0.012, rgba(96, 106, 124, 96));
        }
    }
    if let Some(path) = body {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(rgba(70, 78, 92, 120));
        paint.anti_alias = true;
        let stroke = tiny_skia::Stroke { width: k * 0.01, ..Default::default() };
        pm.stroke_path(&path, &paint, &stroke, crate::theme::scale_transform(), None);
    }
    rim(pm, true);

}

pub fn draw_app_icon(
    pixmap: &mut PixmapMut,
    fonts: &mut FontSystem,
    kind: IconKind,
    label: &str,
    x: f32,
    y: f32,
    size: f32,
) {
    let tile = |pm: &mut PixmapMut, top: Color, bottom: Color| {
        fill_squircle_gradient(pm, x, y, size, top, bottom);
    };

    if matches!(kind, IconKind::TrashEmpty | IconKind::TrashFull) {
        draw_trash(pixmap, x, y, size, kind == IconKind::TrashFull);
        return;
    }
    match kind {
        IconKind::Finder => {
            // Files: a light folder on a deep-blue tile, in the same depth/gradient
            // language as the other Tahoe tiles (own design, not Apple's Finder face).
            tile(pixmap, rgba(92, 184, 255, 255), rgba(14, 104, 232, 255));
            let (fx, fw) = (x + size * 0.17, size * 0.66);
            fill_rounded_rect(pixmap, fx, y + size * 0.33, fw, size * 0.47, size * 0.07, rgba(0, 40, 110, 60));
            let back = rgba(176, 218, 255, 255);
            fill_rounded_rect(pixmap, fx, y + size * 0.235, size * 0.29, size * 0.14, size * 0.045, back);
            fill_rounded_rect(pixmap, fx, y + size * 0.29, fw, size * 0.46, size * 0.065, back);
            if let Some(front) = crate::theme::rounded_rect_path(fx, y + size * 0.37, fw, size * 0.39, size * 0.065) {
                crate::theme::fill_path_gradient(pixmap, &front, y + size * 0.37, y + size * 0.76, rgba(255, 255, 255, 255), rgba(214, 235, 255, 255));
            }
            stroke_line(pixmap, fx + size * 0.05, y + size * 0.383, fx + fw - size * 0.05, y + size * 0.383, size * 0.012, rgba(255, 255, 255, 230));
        }
        IconKind::Terminal => {
            tile(pixmap, rgba(70, 72, 78, 255), rgba(28, 28, 32, 255));
            fill_rounded_rect(
                pixmap,
                x + size * 0.12,
                y + size * 0.14,
                size * 0.76,
                size * 0.70,
                size * 0.08,
                rgba(12, 12, 14, 255),
            );
            fonts.draw_text(
                pixmap,
                ">_",
                x + size * 0.20,
                y + size * 0.24,
                size * 0.30,
                Color::from_rgba8(235, 235, 235, 255),
                true,
            );
        }
        IconKind::Browser => {
            tile(pixmap, rgba(255, 255, 255, 255), rgba(232, 232, 236, 255));
            fill_circle(pixmap, x + size * 0.5, y + size * 0.5, size * 0.34, rgba(30, 140, 255, 255));
            fill_circle(pixmap, x + size * 0.5, y + size * 0.5, size * 0.29, rgba(90, 190, 255, 255));
            stroke_line(pixmap, x + size * 0.40, y + size * 0.60, x + size * 0.62, y + size * 0.38, size * 0.07, rgba(255, 59, 48, 255));
            stroke_line(pixmap, x + size * 0.40, y + size * 0.60, x + size * 0.58, y + size * 0.42, size * 0.07, Color::WHITE);
        }
        IconKind::Antigravity => {
            tile(pixmap, rgba(120, 100, 255, 255), rgba(30, 190, 240, 255));
            fonts.draw_text_centered(pixmap, "AG", x + size * 0.5, y + size * 0.28, size * 0.36, Color::WHITE, true);
        }
        IconKind::Vpn => {
            tile(pixmap, rgba(52, 211, 153, 255), rgba(5, 120, 85, 255));
            fill_circle(pixmap, x + size * 0.5, y + size * 0.5, size * 0.30, rgba(255, 255, 255, 50));
            fonts.draw_text_centered(pixmap, "VPN", x + size * 0.5, y + size * 0.34, size * 0.24, Color::WHITE, true);
        }
        IconKind::Notes => {
            tile(pixmap, rgba(255, 255, 255, 255), rgba(246, 244, 236, 255));
            fill_rounded_rect(pixmap, x + size * 0.08, y + size * 0.08, size * 0.84, size * 0.20, size * 0.06, rgba(255, 204, 0, 255));
            for i in 0..3 {
                let ly = y + size * (0.44 + i as f32 * 0.15);
                stroke_line(pixmap, x + size * 0.18, ly, x + size * 0.82, ly, size * 0.025, rgba(200, 196, 186, 255));
            }
        }
        IconKind::Preview => {
            // Preview («Просмотр»): sky-blue Tahoe tile with framed landscape photograph and magnifier loupe
            tile(pixmap, rgba(96, 192, 255, 255), rgba(24, 108, 236, 255));
            fill_rounded_rect(
                pixmap,
                x + size * 0.15,
                y + size * 0.17,
                size * 0.66,
                size * 0.56,
                size * 0.07,
                rgba(255, 255, 255, 240),
            );
            fill_rounded_rect(
                pixmap,
                x + size * 0.19,
                y + size * 0.21,
                size * 0.58,
                size * 0.48,
                size * 0.045,
                rgba(38, 134, 245, 255),
            );
            // Sun + mountain peaks inside the framed photo
            fill_circle(pixmap, x + size * 0.31, y + size * 0.33, size * 0.055, rgba(255, 224, 102, 255));
            stroke_line(pixmap, x + size * 0.22, y + size * 0.61, x + size * 0.40, y + size * 0.43, size * 0.04, rgba(255, 255, 255, 230));
            stroke_line(pixmap, x + size * 0.40, y + size * 0.43, x + size * 0.52, y + size * 0.55, size * 0.04, rgba(255, 255, 255, 230));
            stroke_line(pixmap, x + size * 0.52, y + size * 0.55, x + size * 0.64, y + size * 0.41, size * 0.04, rgba(255, 255, 255, 230));
            stroke_line(pixmap, x + size * 0.64, y + size * 0.41, x + size * 0.74, y + size * 0.61, size * 0.04, rgba(255, 255, 255, 230));
            // Inspecting loupe in bottom-right corner
            let (lx, ly) = (x + size * 0.68, y + size * 0.66);
            fill_circle(pixmap, lx, ly, size * 0.16, rgba(255, 255, 255, 250));
            fill_circle(pixmap, lx, ly, size * 0.115, rgba(165, 218, 255, 240));
            stroke_line(pixmap, lx + size * 0.10, ly + size * 0.10, lx + size * 0.19, ly + size * 0.19, size * 0.055, rgba(245, 247, 252, 255));
        }
        IconKind::ActivityMonitor => {
            tile(pixmap, rgba(58, 58, 64, 255), rgba(22, 22, 26, 255));
            let c = rgba(50, 215, 75, 255);
            let cy = y + size * 0.54;
            let w = size * 0.045;
            stroke_line(pixmap, x + size * 0.14, cy, x + size * 0.34, cy, w, c);
            stroke_line(pixmap, x + size * 0.34, cy, x + size * 0.45, y + size * 0.26, w, c);
            stroke_line(pixmap, x + size * 0.45, y + size * 0.26, x + size * 0.57, y + size * 0.74, w, c);
            stroke_line(pixmap, x + size * 0.57, y + size * 0.74, x + size * 0.68, cy, w, c);
            stroke_line(pixmap, x + size * 0.68, cy, x + size * 0.86, cy, w, c);
        }
        IconKind::Settings => draw_settings_icon(pixmap, x, y, size),
        IconKind::Launchpad => {
            tile(pixmap, rgba(245, 245, 247, 255), rgba(222, 222, 228, 255));
            let colors = [
                rgba(255, 69, 58, 255),
                rgba(255, 159, 10, 255),
                rgba(255, 214, 10, 255),
                rgba(48, 209, 88, 255),
                rgba(100, 210, 255, 255),
                rgba(10, 132, 255, 255),
                rgba(94, 92, 230, 255),
                rgba(191, 90, 242, 255),
                rgba(255, 55, 95, 255),
            ];
            let cell = size * 0.18;
            let gap = size * 0.07;
            let x0 = x + (size - cell * 3.0 - gap * 2.0) * 0.5;
            let y0 = y + (size - cell * 3.0 - gap * 2.0) * 0.5;
            for (k, c) in colors.iter().enumerate() {
                let (cx, cy) = ((k % 3) as f32, (k / 3) as f32);
                fill_rounded_rect(pixmap, x0 + cx * (cell + gap), y0 + cy * (cell + gap), cell, cell, cell * 0.28, *c);
            }
        }
        IconKind::GenericApp => {
            tile(pixmap, rgba(100, 150, 255, 255), rgba(40, 90, 220, 255));
            let first = label.chars().next().unwrap_or('A').to_uppercase().to_string();
            fonts.draw_text_centered(pixmap, &first, x + size * 0.5, y + size * 0.24, size * 0.44, Color::WHITE, true);
        }
        IconKind::TrashEmpty | IconKind::TrashFull => {}
    }

    // Subtle rim, as on macOS icons
    stroke_squircle(pixmap, x + 0.25, y + 0.25, size - 0.5, 0.5, rgba(0, 0, 0, 30));
}
#[cfg(test)]
mod trash_icon_preview {
    use super::*;
    #[test]
    #[ignore]
    fn preview() {
        let dir = std::env::var("FEATHER_ICON_PREVIEW").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
        let mut fonts = FontSystem::new();
        for dark in [false, true] {
            let mut pm = tiny_skia::Pixmap::new(5 * 150, 170).unwrap();
            pm.fill(if dark { rgba(60, 50, 70, 255) } else { rgba(235, 215, 210, 255) });
            let kinds = [IconKind::Terminal, IconKind::Settings, IconKind::GenericApp, IconKind::TrashEmpty, IconKind::TrashFull];
            for (i, k) in kinds.into_iter().enumerate() {
                draw_app_icon(&mut pm.as_mut(), &mut fonts, k, "Okno", 11.0 + i as f32 * 150.0, 21.0, 128.0);
            }
            pm.save_png(format!("{dir}/icons-{dark}.png")).unwrap();
        }
    }
}
