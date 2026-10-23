use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use tiny_skia::{Color, PixmapMut};

use crate::text::FontSystem;
use crate::theme::{
    fill_circle, fill_rounded_rect, fill_squircle_gradient, rgba, stroke_line, stroke_squircle,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconKind {
    Finder,
    Terminal,
    Browser,
    Antigravity,
    Vpn,
    Notes,
    ActivityMonitor,
    Settings,
    Launchpad,
    GenericApp,
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
    fn new(id: &str, name: &str, subtitle: &str, icon: IconKind, action: AppAction, pinned: bool) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            subtitle: subtitle.to_string(),
            icon,
            icon_name: None,
            wm_class: None,
            aliases: Vec::new(),
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
        self.aliases = aliases.iter().map(|a| a.to_string()).collect();
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
            AppEntry::new("builtin.notes", "Заметки", "Быстрые заметки (RU / EN)", IconKind::Notes, BuiltinWindow(BuiltinWindowKind::QuickNotes), true),
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
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let mut name = None;
            let mut name_ru = None;
            let mut comment = None;
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
                if let Some(v) = line.strip_prefix("Name[ru]=") {
                    name_ru = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("Name=") {
                    name.get_or_insert_with(|| v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("Comment[ru]=") {
                    comment = Some(v.trim().to_string());
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
                name: name_ru.or(name).unwrap_or_else(|| stem.clone()),
                subtitle: comment.unwrap_or_else(|| cmd.clone()),
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
        list.sort_by_cached_key(|&i| self.apps[i].name.to_lowercase());
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

/// Spawns an external command connected to our compositor's Wayland socket
pub fn spawn_wayland_command(cmd_line: &str, wayland_socket: &str) -> bool {
    let trimmed = cmd_line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let current_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin".into());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/zumka".into());
    let enriched_path = format!("{home}/.cargo/bin:{home}/.local/bin:/usr/local/bin:{current_path}");

    Command::new("sh")
        .arg("-c")
        .arg(trimmed)
        .env("PATH", enriched_path)
        .env("WAYLAND_DISPLAY", wayland_socket)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "FeatherDE")
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

/// Starts essential background session daemons (Polkit Authentication Agent for VPN/pkexec, D-Bus env)
pub fn spawn_session_services(wayland_socket: &str, is_standalone: bool) {
    if is_standalone {
        crate::file_portal::session_start(wayland_socket.to_owned());

        let polkit_cmd = "killall -q lxqt-policykit-agent polkit-kde-authentication-agent-1 2>/dev/null; \
            sleep 0.15; \
            if command -v lxqt-policykit-agent >/dev/null 2>&1; then \
                exec lxqt-policykit-agent; \
            elif [ -x /usr/lib/polkit-kde-authentication-agent-1 ]; then \
                exec /usr/lib/polkit-kde-authentication-agent-1; \
            fi";
        spawn_wayland_command(polkit_cmd, wayland_socket);
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
        IconKind::Settings => {
            tile(pixmap, rgba(170, 172, 180, 255), rgba(110, 112, 120, 255));
            let (cx, cy) = (x + size * 0.5, y + size * 0.5);
            for k in 0..12 {
                let a = k as f32 / 12.0 * std::f32::consts::TAU;
                stroke_line(
                    pixmap,
                    cx + a.cos() * size * 0.22,
                    cy + a.sin() * size * 0.22,
                    cx + a.cos() * size * 0.34,
                    cy + a.sin() * size * 0.34,
                    size * 0.06,
                    rgba(60, 62, 70, 255),
                );
            }
            fill_circle(pixmap, cx, cy, size * 0.25, rgba(60, 62, 70, 255));
            fill_circle(pixmap, cx, cy, size * 0.11, rgba(200, 202, 210, 255));
        }
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
    }

    // Subtle rim, as on macOS icons
    stroke_squircle(pixmap, x + 0.25, y + 0.25, size - 0.5, 0.5, rgba(0, 0, 0, 30));
}
