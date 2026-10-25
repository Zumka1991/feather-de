//! Default applications and timedated. All filesystem, bus and subprocess work is here.
use smithay::reexports::calloop::{channel, LoopHandle};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use zbus::Proxy;

#[derive(Clone, Debug)]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub browser: bool,
    pub terminal: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutostartItem {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub enabled: bool,
}
#[derive(Clone, Default, Debug)]
pub struct TimeState {
    pub available: bool,
    pub zone: String,
    pub ntp: bool,
    pub can_ntp: bool,
    pub synchronized: bool,
    pub local_input: String,
}
#[derive(Clone, Debug)]
pub struct State {
    pub apps: Arc<Vec<DesktopApp>>,
    pub autostart: Arc<Vec<AutostartItem>>,
    pub loaded: bool,
    pub browser: Option<String>,
    pub terminal: String,
    pub time: TimeState,
    pub zones: Arc<Vec<String>>,
    /// Installed xcursor themes (directory names), sorted
    pub cursor_themes: Arc<Vec<String>>,
    pub busy: bool,
    pub error: Option<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            apps: Arc::new(Vec::new()),
            autostart: Arc::new(Vec::new()),
            loaded: false,
            browser: None,
            terminal: "feather-terminal.desktop".into(),
            time: TimeState::default(),
            zones: Arc::new(Vec::new()),
            cursor_themes: Arc::new(Vec::new()),
            busy: false,
            error: None,
        }
    }
}
impl State {
    pub fn app_name(&self, id: Option<&str>) -> String {
        id.and_then(|id| {
            self.apps
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.name.clone())
        })
        .unwrap_or_else(|| "Не выбрано".into())
    }
    pub fn terminal_command(&self) -> String {
        self.apps
            .iter()
            .find(|a| a.id == self.terminal && a.terminal)
            .map(|a| a.exec.clone())
            .unwrap_or_else(crate::apps::terminal_command)
    }
}
#[derive(Clone, Debug)]
pub enum Command {
    Refresh,
    DefaultApp { browser: bool, id: String },
    AddAutostart(String),
    ToggleAutostart { id: String, enabled: bool },
    RemoveAutostart(String),
    Ntp(bool),
    Timezone(String),
    SetTime(String),
}
pub struct Worker(async_channel::Sender<Command>);

/// Icon themes that ship cursors: `<icons dir>/<name>/cursors`.
pub fn scan_cursor_themes() -> Vec<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let mut dirs = vec![data.join("icons"), home.join(".icons")];
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("icons")));
    let mut themes: Vec<String> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|d| d.flatten())
        .filter(|e| e.path().join("cursors").is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| crate::settings::valid_theme(n))
        .collect();
    themes.sort_by_key(|t| t.to_lowercase());
    themes.dedup();
    themes.truncate(64);
    themes
}
const SERVICE: &str = "org.freedesktop.timedate1";
const OBJECT: &str = "/org/freedesktop/timedate1";
fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
}
fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        })
}
fn valid_id(id: &str) -> bool {
    id.ends_with(".desktop")
        && !id.is_empty()
        && !id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ';' | '=' | '[' | ']'))
}

fn scan_apps() -> Vec<DesktopApp> {
    let mut dirs = vec![data_home()];
    dirs.extend(
        std::env::var_os("XDG_DATA_DIRS")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]),
    );
    let mut seen = std::collections::HashSet::new();
    let mut apps = vec![
        DesktopApp {
            id: "feather-files.desktop".into(),
            name: "Файлы".into(),
            exec: crate::apps::files_command(),
            browser: false,
            terminal: false,
        },
        DesktopApp {
            id: "feather-terminal.desktop".into(),
            name: "Терминал Feather".into(),
            exec: crate::apps::terminal_command(),
            browser: false,
            terminal: true,
        },
    ];
    seen.insert("feather-files.desktop".to_string());
    seen.insert("feather-terminal.desktop".to_string());
    if let Some(exec) = crate::apps::notes_command() {
        apps.push(DesktopApp {
            id: "feather-notes.desktop".into(),
            name: "Заметки".into(),
            exec,
            browser: false,
            terminal: false,
        });
        seen.insert("feather-notes.desktop".to_string());
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("applications")) else {
            continue;
        };
        for file in entries.flatten() {
            let path = file.path();
            let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !valid_id(id) || !seen.insert(id.to_string()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut in_entry = false;
            let mut kind = "Application";
            let mut name = None;
            let mut ru = None;
            let mut exec = None;
            let mut categories = String::new();
            let mut mime = String::new();
            let mut hidden = false;
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                    continue;
                }
                if !in_entry {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "Type" => kind = value.trim(),
                        "Name" => name = Some(value.to_string()),
                        "Name[ru]" => ru = Some(value.to_string()),
                        "Exec" => {
                            exec = Some(
                                value
                                    .split_whitespace()
                                    .filter(|p| !p.starts_with('%'))
                                    .collect::<Vec<_>>()
                                    .join(" "),
                            )
                        }
                        "Categories" => categories = value.into(),
                        "MimeType" => mime = value.into(),
                        "Hidden" | "NoDisplay" => hidden |= value == "true",
                        _ => {}
                    }
                }
            }
            if hidden || kind != "Application" {
                continue;
            }
            let browser = categories.split(';').any(|c| c == "WebBrowser")
                || mime.split(';').any(|m| m == "x-scheme-handler/http");
            let terminal = categories.split(';').any(|c| c == "TerminalEmulator");
            if let Some(exec) = exec.filter(|e| !e.is_empty()) {
                apps.push(DesktopApp {
                    id: id.into(),
                    name: ru.or(name).unwrap_or_else(|| id.to_string()),
                    exec,
                    browser,
                    terminal,
                });
            }
        }
    }
    apps.sort_by_key(|a| (a.name.to_lowercase(), a.id.clone()));
    apps
}

pub fn scan_autostart(config: &Path) -> Vec<AutostartItem> {
    let Ok(entries) = std::fs::read_dir(config.join("autostart")) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for file in entries.flatten() {
        let path = file.path();
        let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut in_entry = false;
        let mut name = None;
        let mut ru = None;
        let mut exec = None;
        let mut hidden = false;
        let mut gnome_enabled = None;
        let mut only_show = String::new();
        let mut not_show = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim();
                match key.trim() {
                    "Name" => name = Some(value.to_string()),
                    "Name[ru]" => ru = Some(value.to_string()),
                    "Exec" => {
                        exec = Some(
                            value
                                .split_whitespace()
                                .filter(|p| !p.starts_with('%'))
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                    }
                    "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
                    "X-GNOME-Autostart-enabled" => {
                        gnome_enabled = Some(!value.eq_ignore_ascii_case("false"))
                    }
                    "OnlyShowIn" => only_show = value.to_string(),
                    "NotShowIn" => not_show = value.to_string(),
                    _ => {}
                }
            }
        }
        if !not_show.is_empty()
            && not_show
                .split(';')
                .any(|d| d.trim().eq_ignore_ascii_case("FeatherDE"))
        {
            continue;
        }
        if !only_show.is_empty()
            && !only_show
                .split(';')
                .filter(|d| !d.trim().is_empty())
                .any(|d| d.trim().eq_ignore_ascii_case("FeatherDE"))
        {
            continue;
        }
        if let Some(exec) = exec.filter(|e| !e.is_empty()) {
            items.push(AutostartItem {
                id: id.to_string(),
                name: ru
                    .or(name)
                    .unwrap_or_else(|| id.trim_end_matches(".desktop").to_string()),
                exec,
                enabled: !hidden && gnome_enabled.unwrap_or(true),
            });
        }
    }
    items.sort_by_key(|i| (i.name.to_lowercase(), i.id.clone()));
    items
}

fn write_autostart(config: &Path, app: &DesktopApp) -> Result<(), String> {
    if !valid_id(&app.id) {
        return Err("Недопустимый desktop ID".into());
    }
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
    let text = format!(
        "[Desktop Entry]\nType=Application\nName={}\nExec={}\nHidden=false\nX-GNOME-Autostart-enabled=true\n",
        clean(&app.name),
        clean(&app.exec)
    );
    atomic_write(&config.join("autostart").join(&app.id), &text)
        .map_err(|e| format!("Не удалось добавить в автозапуск: {e}"))
}

fn toggle_autostart(config: &Path, id: &str, enabled: bool) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Недопустимый desktop ID".into());
    }
    let path = config.join("autostart").join(id);
    let old = std::fs::read_to_string(&path)
        .map_err(|e| format!("Не удалось прочитать элемент автозапуска: {e}"))?;
    let text = update_ini(
        &old,
        "Desktop Entry",
        &[
            ("Hidden", (!enabled).to_string()),
            ("X-GNOME-Autostart-enabled", enabled.to_string()),
        ],
    );
    atomic_write(&path, &text).map_err(|e| format!("Не удалось обновить автозапуск: {e}"))
}

fn remove_autostart(config: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Недопустимый desktop ID".into());
    }
    let path = config.join("autostart").join(id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Не удалось удалить из автозапуска: {e}")),
    }
}

/// Used by Files as a short-lived launcher; disk lookup never runs in the compositor.
pub fn launch_terminal(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let saved = crate::settings::Settings::load();
    let apps = scan_apps();
    let command = saved
        .default_terminal
        .and_then(|id| {
            apps.into_iter()
                .find(|a| a.id == id && a.terminal)
                .map(|a| a.exec)
        })
        .unwrap_or_else(crate::apps::terminal_command);
    std::process::Command::new("sh")
        .args(["-c", &format!("exec {command}")])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}
fn update_ini(text: &str, section: &str, entries: &[(&str, String)]) -> String {
    let header = format!("[{section}]");
    let mut out = String::new();
    let mut in_section = false;
    let mut wrote = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_section && !wrote {
                for (k, v) in entries {
                    out.push_str(&format!("{k}={v}\n"));
                }
                wrote = true;
            }
            in_section = trimmed == header;
        }
        if in_section
            && line
                .split_once('=')
                .is_some_and(|(key, _)| entries.iter().any(|(k, _)| *k == key.trim()))
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !wrote {
        if !in_section {
            out.push_str(&format!("\n{header}\n"));
        }
        for (k, v) in entries {
            out.push_str(&format!("{k}={v}\n"));
        }
    }
    out
}
fn write_browser(config: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Недопустимый desktop ID".into());
    }
    let update = |path: &Path| -> Result<(), String> {
        let old = std::fs::read_to_string(path).unwrap_or_default();
        let kinds = [
            "x-scheme-handler/http",
            "x-scheme-handler/https",
            "text/html",
            "application/xhtml+xml",
        ];
        let entries = kinds.map(|mime| (mime, prepend_id(&old, "Default Applications", mime, id)));
        let text = update_ini(&old, "Default Applications", &entries);
        let entries = kinds.map(|mime| (mime, prepend_id(&old, "Added Associations", mime, id)));
        let text = update_ini(&text, "Added Associations", &entries);
        atomic_write(path, &text).map_err(|e| format!("Не удалось сохранить браузер: {e}"))
    };
    update(&config.join("mimeapps.list"))?;
    let specific = config.join("featherde-mimeapps.list");
    if specific.exists() {
        update(&specific)?;
    }
    Ok(())
}
fn prepend_id(text: &str, section: &str, key: &str, id: &str) -> String {
    let mut active = false;
    let mut values = vec![id.to_string()];
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            active = line == format!("[{section}]");
        } else if active {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == key {
                    for previous in v.split(';').filter(|s| !s.is_empty() && *s != id) {
                        if !values.iter().any(|s| s == previous) {
                            values.push(previous.to_string());
                        }
                    }
                }
            }
        }
    }
    format!("{};", values.join(";"))
}
fn write_terminal(config: &Path, data: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Недопустимый desktop ID".into());
    }
    if id == "feather-terminal.desktop" {
        let text=format!("[Desktop Entry]\nType=Application\nName=Терминал Feather\nExec={}\nIcon=utilities-terminal\nTerminal=false\nCategories=System;TerminalEmulator;\nX-TerminalArgExec=-e\n",crate::apps::terminal_command());
        atomic_write(&data.join("applications/feather-terminal.desktop"), &text)
            .map_err(|e| e.to_string())?;
    }
    let path = config.join("featherde-xdg-terminals.list");
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let rest = old
        .lines()
        .filter(|line| line.trim() != id)
        .collect::<Vec<_>>()
        .join("\n");
    atomic_write(&path, &format!("{id}\n{rest}\n")).map_err(|e| e.to_string())
}
fn default_browser() -> Option<String> {
    std::process::Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/http"])
        .env("XDG_CURRENT_DESKTOP", "FeatherDE")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| valid_id(s))
}

async fn time_proxy(connection: &zbus::Connection) -> Result<Proxy<'_>, zbus::Error> {
    zbus::proxy::Builder::new(connection)
        .destination(SERVICE)?
        .path(OBJECT)?
        .interface(SERVICE)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}
async fn snapshot(proxy: &Proxy<'_>) -> Result<TimeState, zbus::Error> {
    Ok(TimeState {
        available: true,
        zone: proxy.get_property("Timezone").await?,
        ntp: proxy.get_property("NTP").await?,
        can_ntp: proxy.get_property("CanNTP").await?,
        synchronized: proxy.get_property("NTPSynchronized").await.unwrap_or(false),
        local_input: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    })
}
fn parse_time(value: &str) -> Result<i64, String> {
    use chrono::{Datelike, TimeZone};
    let date = chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M")
        .map_err(|_| "Введите дату и время: ГГГГ-ММ-ДД ЧЧ:ММ".to_string())?;
    if !(1970..=2100).contains(&date.year()) {
        return Err("Год должен быть между 1970 и 2100".into());
    }
    chrono::Local
        .from_local_datetime(&date)
        .single()
        .map(|d| d.timestamp_micros())
        .ok_or_else(|| "Это время неоднозначно или отсутствует в данном часовом поясе".into())
}
async fn set_time(proxy: &Proxy<'_>, command: &Command, zones: &[String]) -> Result<(), String> {
    match command {
        Command::Ntp(value) => proxy
            .call::<_, _, ()>("SetNTP", &(*value, true))
            .await
            .map_err(|e| e.to_string()),
        Command::Timezone(zone) => {
            if !zones.contains(zone) {
                return Err("Неизвестный часовой пояс".into());
            }
            proxy
                .call::<_, _, ()>("SetTimezone", &(zone, true))
                .await
                .map_err(|e| e.to_string())
        }
        Command::SetTime(value) => {
            if proxy
                .get_property::<bool>("NTP")
                .await
                .map_err(|e| e.to_string())?
            {
                return Err("Сначала отключите автоматическую синхронизацию".into());
            }
            let stamp = parse_time(value)?;
            proxy
                .call::<_, _, ()>("SetTime", &(stamp, false, true))
                .await
                .map_err(|e| e.to_string())
        }
        _ => Ok(()),
    }
}
impl Worker {
    pub fn new(
        handle: &LoopHandle<'static, crate::FeatherCompositor>,
        browser: Option<String>,
        terminal: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, rx) = async_channel::bounded(8);
        let (done, events) = channel::sync_channel(2);
        handle.insert_source(events, |event, _, state| {
            if let channel::Event::Msg(snapshot) = event {
                state.system_preferences = snapshot;
                state.system_settings.app_count = state.system_preferences.apps.len();
                if let Some(error) = &state.system_preferences.error {
                    state.system_settings.error = Some(error.clone());
                }
                state.maybe_launch_autostart();
                state.mark_settings_dirty();
                state.save_settings();
                state.needs_redraw = true;
            }
        })?;
        std::thread::Builder::new()
            .name("feather-system".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                futures_lite::future::block_on(async move {
                    let mut state = State::default();
                    state.apps = Arc::new(scan_apps());
                    state.autostart = Arc::new(scan_autostart(&config_home()));
                    state.loaded = true;
                    state.browser = default_browser().or(browser);
                    if let Some(id) =
                        terminal.filter(|id| state.apps.iter().any(|a| a.id == *id && a.terminal))
                    {
                        state.terminal = id;
                    }
                    let connection = match zbus::connection::Builder::system() {
                        Ok(builder) => builder
                            .method_timeout(Duration::from_secs(60))
                            .build()
                            .await
                            .ok(),
                        Err(_) => None,
                    };
                    let proxy = if let Some(connection) = &connection {
                        time_proxy(connection).await.ok()
                    } else {
                        None
                    };
                    if let Some(proxy) = &proxy {
                        if let Ok(time) = snapshot(proxy).await {
                            state.time = time;
                        }
                        state.zones = Arc::new(
                            proxy
                                .call("ListTimezones", &())
                                .await
                                .unwrap_or_else(|_| vec!["UTC".into()]),
                        );
                    }
                    if done.send(state.clone()).is_err() {
                        return;
                    }
                    while let Ok(command) = rx.recv().await {
                        state.error = None;
                        let result = match &command {
                            Command::DefaultApp { browser, id } => {
                                if !state.apps.iter().any(|a| {
                                    a.id == *id && if *browser { a.browser } else { a.terminal }
                                }) {
                                    Err("Приложение больше не установлено".into())
                                } else if *browser {
                                    write_browser(&config_home(), id)
                                        .map(|_| state.browser = Some(id.clone()))
                                } else {
                                    write_terminal(&config_home(), &data_home(), id)
                                        .map(|_| state.terminal = id.clone())
                                }
                            }
                            Command::AddAutostart(id) => {
                                let cfg = config_home();
                                if let Some(app) = state.apps.iter().find(|a| a.id == *id) {
                                    write_autostart(&cfg, app)
                                        .map(|_| state.autostart = Arc::new(scan_autostart(&cfg)))
                                } else {
                                    Err("Приложение не найдено".into())
                                }
                            }
                            Command::ToggleAutostart { id, enabled } => {
                                let cfg = config_home();
                                toggle_autostart(&cfg, id, *enabled)
                                    .map(|_| state.autostart = Arc::new(scan_autostart(&cfg)))
                            }
                            Command::RemoveAutostart(id) => {
                                let cfg = config_home();
                                remove_autostart(&cfg, id)
                                    .map(|_| state.autostart = Arc::new(scan_autostart(&cfg)))
                            }
                            Command::Refresh => {
                                state.apps = Arc::new(scan_apps());
                                state.autostart = Arc::new(scan_autostart(&config_home()));
                                state.cursor_themes = Arc::new(scan_cursor_themes());
                                state.browser = default_browser().or(state.browser);
                                Ok(())
                            }
                            _ => {
                                if let Some(proxy) = &proxy {
                                    set_time(proxy, &command, &state.zones).await
                                } else {
                                    Err("Служба даты и времени недоступна".into())
                                }
                            }
                        };
                        if let Err(error) = result {
                            state.error = Some(error);
                        }
                        if let Some(proxy) = &proxy {
                            match snapshot(proxy).await {
                                Ok(time) => state.time = time,
                                Err(e) => {
                                    state.time.available = false;
                                    if !matches!(
                                        command,
                                        Command::DefaultApp { .. }
                                            | Command::AddAutostart(_)
                                            | Command::ToggleAutostart { .. }
                                            | Command::RemoveAutostart(_)
                                            | Command::Refresh
                                    ) {
                                        state.error = Some(e.to_string());
                                    }
                                }
                            }
                        }
                        state.busy = false;
                        if done.send(state.clone()).is_err() {
                            break;
                        }
                    }
                })
            })?;
        Ok(Self(tx))
    }
    pub fn send(&self, command: Command) -> bool {
        self.0.try_send(command).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TimeFixture(std::sync::Arc<std::sync::Mutex<(bool, String, Vec<String>)>>);
    #[zbus::interface(name = "org.freedesktop.timedate1")]
    impl TimeFixture {
        #[zbus(property)]
        fn timezone(&self) -> String {
            self.0.lock().unwrap().1.clone()
        }
        #[zbus(property, name = "NTP")]
        fn ntp(&self) -> bool {
            self.0.lock().unwrap().0
        }
        #[zbus(property, name = "CanNTP")]
        fn can_ntp(&self) -> bool {
            true
        }
        #[zbus(property, name = "NTPSynchronized")]
        fn synchronized(&self) -> bool {
            false
        }
        fn list_timezones(&self) -> Vec<String> {
            vec!["UTC".into(), "Asia/Barnaul".into(), "Etc/Denied".into()]
        }
        #[zbus(name = "SetNTP")]
        fn set_ntp(&self, value: bool, interactive: bool) {
            assert!(interactive);
            let mut state = self.0.lock().unwrap();
            state.0 = value;
            state.2.push(format!("ntp:{value}"));
        }
        fn set_timezone(&self, zone: &str, interactive: bool) -> zbus::fdo::Result<()> {
            assert!(interactive);
            if zone == "Etc/Denied" {
                return Err(zbus::fdo::Error::AccessDenied("fixture denied".into()));
            }
            let mut state = self.0.lock().unwrap();
            state.1 = zone.into();
            state.2.push(format!("zone:{zone}"));
            Ok(())
        }
        fn set_time(&self, stamp: i64, relative: bool, interactive: bool) {
            assert!(stamp > 0);
            assert!(!relative);
            assert!(interactive);
            self.0.lock().unwrap().2.push("time:absolute".into());
        }
    }
    #[test]
    #[ignore = "Requires a private dbus-run-session; never changes system time"]
    fn private_timedated_wire_types_ntp_guard_timezone_and_permission_errors() {
        futures_lite::future::block_on(async {
            let data = std::sync::Arc::new(std::sync::Mutex::new((true, "UTC".into(), Vec::new())));
            let _server = zbus::connection::Builder::session()
                .unwrap()
                .name(SERVICE)
                .unwrap()
                .serve_at(OBJECT, TimeFixture(data.clone()))
                .unwrap()
                .build()
                .await
                .unwrap();
            let connection = zbus::connection::Builder::session()
                .unwrap()
                .method_timeout(Duration::from_secs(3))
                .build()
                .await
                .unwrap();
            let proxy = time_proxy(&connection).await.unwrap();
            assert!(snapshot(&proxy).await.unwrap().ntp);
            let zones: Vec<String> = proxy.call("ListTimezones", &()).await.unwrap();
            assert!(
                set_time(&proxy, &Command::SetTime("2026-10-24 12:30".into()), &zones)
                    .await
                    .unwrap_err()
                    .contains("синхронизацию")
            );
            assert!(data.lock().unwrap().2.is_empty());
            set_time(&proxy, &Command::Ntp(false), &zones)
                .await
                .unwrap();
            set_time(&proxy, &Command::Timezone("Asia/Barnaul".into()), &zones)
                .await
                .unwrap();
            set_time(&proxy, &Command::SetTime("2026-10-24 12:30".into()), &zones)
                .await
                .unwrap();
            let result = snapshot(&proxy).await.unwrap();
            assert!(!result.ntp);
            assert_eq!(result.zone, "Asia/Barnaul");
            assert_eq!(
                data.lock().unwrap().2,
                ["ntp:false", "zone:Asia/Barnaul", "time:absolute"]
            );
            assert!(
                set_time(&proxy, &Command::Timezone("missing".into()), &zones)
                    .await
                    .is_err()
            );
            assert!(
                set_time(&proxy, &Command::Timezone("Etc/Denied".into()), &zones)
                    .await
                    .unwrap_err()
                    .contains("denied")
            );
        });
    }
    #[test]
    fn mime_edit_preserves_other_sections_and_default_preferences() {
        let text="# keep\n[Default Applications]\nx-scheme-handler/http=old.desktop;\nimage/png=viewer.desktop;\n[Removed Associations]\ntext/plain=other.desktop;\n";
        let text = update_ini(
            text,
            "Default Applications",
            &[("x-scheme-handler/http", "new.desktop;".into())],
        );
        assert!(text.contains("# keep\n"));
        assert!(text.contains("image/png=viewer.desktop;"));
        assert!(text.contains("text/plain=other.desktop;"));
        assert!(!text.contains("old.desktop"));
        assert_eq!(text.matches("x-scheme-handler/http=").count(), 1);
        assert!(!valid_id("../bad.desktop"));
        assert!(!valid_id("app.desktop\n[bad]"));
        assert!(parse_time("2026-02-30 10:00").is_err());
        assert!(parse_time("2026-10-24 25:00").is_err());
        assert!(parse_time("1960-01-01 00:00").is_err());
    }
    #[test]
    fn writes_browser_and_terminal_to_private_configuration_only() {
        let root = std::env::temp_dir().join(format!("feather-defaults-{}", std::process::id()));
        let config = root.join("config");
        let data = root.join("data");
        write_browser(&config, "test-browser.desktop").unwrap();
        write_terminal(&config, &data, "feather-terminal.desktop").unwrap();
        let text = std::fs::read_to_string(config.join("mimeapps.list")).unwrap();
        for mime in ["http", "https", "text/html", "application/xhtml+xml"] {
            assert!(text.contains(mime));
        }
        assert!(data.join("applications/feather-terminal.desktop").exists());
        assert!(
            std::fs::read_to_string(config.join("featherde-xdg-terminals.list"))
                .unwrap()
                .starts_with("feather-terminal.desktop\n")
        );
        let app = DesktopApp {
            id: "feather-notes.desktop".into(),
            name: "Заметки".into(),
            exec: "feather-notes".into(),
            browser: false,
            terminal: false,
        };
        write_autostart(&config, &app).unwrap();
        atomic_write(
            &config.join("autostart/kde-only.desktop"),
            "[Desktop Entry]\nType=Application\nName=KDE Only\nExec=kde-tool\nOnlyShowIn=KDE;\n",
        )
        .unwrap();
        atomic_write(
            &config.join("autostart/not-feather.desktop"),
            "[Desktop Entry]\nType=Application\nName=Not Feather\nExec=other-tool\nNotShowIn=FeatherDE;GNOME;\n",
        )
        .unwrap();
        let items = scan_autostart(&config);
        assert_eq!(
            items,
            vec![AutostartItem {
                id: "feather-notes.desktop".into(),
                name: "Заметки".into(),
                exec: "feather-notes".into(),
                enabled: true,
            }]
        );
        toggle_autostart(&config, "feather-notes.desktop", false).unwrap();
        let items = scan_autostart(&config);
        assert!(!items[0].enabled);
        toggle_autostart(&config, "feather-notes.desktop", true).unwrap();
        let items = scan_autostart(&config);
        assert!(items[0].enabled);
        remove_autostart(&config, "feather-notes.desktop").unwrap();
        assert!(scan_autostart(&config).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
