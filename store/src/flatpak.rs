//! Flathub & Flatpak integration for `feather-store`.
//!
//! Zero C dependencies (`libflatpak` is not linked) and 0% idle CPU:
//! - Reads Flathub's local AppStream XML (`appstream.xml.gz` / `appstream.xml`) with a
//!   streaming component-by-component parser (~32 KB working buffer, even on a 65 MB catalog)
//!   and resolves cached 128×128 / 64×64 icons next to it.
//! - Drives the `flatpak` CLI on background worker threads (`--user` by default so installing,
//!   updating and removing Flathub apps needs no root password, while also recognizing and
//!   managing `--system` installations).

use crate::catalog::{decode_entities, paragraphs, AppInfo, Source};
use crate::packagekit::{self, Event, Op, Update};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FLATHUB_REPO_URL: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";
const MAX_COMPONENT_BYTES: usize = 512 * 1024;

/// State of Flatpak & the Flathub catalog on this machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// `flatpak` binary is not installed yet (`packagekit` can install it in 1 click).
    #[default]
    MissingBinary,
    /// `flatpak` is installed, `flathub` remote or its AppStream catalog is not synced yet.
    NotConnected,
    /// Flathub AppStream catalog is on disk and loaded.
    Ready,
}

/// Scopes in which a Flatpak application is currently installed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstalledScope {
    pub user: bool,
    pub system: bool,
}

/// Progress & completion events of the 1-click Flathub onboarding flow.
pub enum ConnectEvent {
    Progress(Option<f32>, String),
    Done(crate::catalog::Catalog),
    Cancelled,
    Failed(String),
}

/// Checks `$PATH` (plus standard system dirs) for the `flatpak` binary without spawning a child.
pub fn has_binary() -> bool {
    binary_path().is_some()
}

fn binary_path() -> Option<PathBuf> {
    let env_paths = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    env_paths
        .into_iter()
        .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin"), PathBuf::from("/usr/local/bin")])
        .map(|dir| dir.join("flatpak"))
        .find(|p| p.is_file())
}

fn user_flatpak_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FLATPAK_USER_DIR").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(xdg).join("flatpak"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/flatpak"))
}

fn system_flatpak_dirs() -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("FLATPAK_SYSTEM_DIR").filter(|s| !s.is_empty()) {
        return vec![PathBuf::from(dir)];
    }
    vec![PathBuf::from("/var/lib/flatpak")]
}

/// Preferred Flatpak architecture name for this CPU.
fn flatpak_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "i386",
        "arm" => "arm",
        other => other,
    }
}

/// Finds `active/` directories containing `appstream.xml.gz` or `appstream.xml` for `flathub`
/// (system first, then user so user metadata wins if newer/present).
fn flathub_active_dirs() -> Vec<(PathBuf, PathBuf)> {
    let mut roots = system_flatpak_dirs();
    if let Some(user) = user_flatpak_dir() {
        roots.push(user);
    }
    let mut found = Vec::new();
    let wanted_arch = flatpak_arch();
    for root in roots {
        let remote_dir = root.join("appstream").join("flathub");
        let Ok(entries) = std::fs::read_dir(&remote_dir) else { continue };
        let mut arch_dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        // Prefer native arch first
        arch_dirs.sort_by_key(|p| p.file_name().and_then(|n| n.to_str()) != Some(wanted_arch));
        if let Some(arch_dir) = arch_dirs.first() {
            let active = arch_dir.join("active");
            let xml_gz = active.join("appstream.xml.gz");
            let xml = active.join("appstream.xml");
            if xml_gz.is_file() {
                found.push((active, xml_gz));
            } else if xml.is_file() {
                found.push((active, xml));
            }
        }
    }
    found
}

/// Loads Flathub's AppStream catalog from disk if available, returning `(status, apps)`.
pub fn load_catalog(keys: &[&str]) -> (Status, Vec<AppInfo>) {
    let bin = has_binary();
    let dirs = flathub_active_dirs();
    if dirs.is_empty() {
        let status = if bin { Status::NotConnected } else { Status::MissingBinary };
        return (status, Vec::new());
    }
    let mut by_id: HashMap<String, AppInfo> = HashMap::new();
    for (active, xml_path) in dirs {
        let icons = active.join("icons");
        for app in parse_appstream_file(&xml_path, &icons, keys) {
            by_id.insert(app.flatpak_id.clone(), app);
        }
    }
    if by_id.is_empty() {
        let status = if bin { Status::NotConnected } else { Status::MissingBinary };
        return (status, Vec::new());
    }
    let status = if bin { Status::Ready } else { Status::MissingBinary };
    (status, by_id.into_values().collect())
}

/// Lists Flatpak applications currently installed in `--user` and `--system`.
pub fn installed_apps() -> HashMap<String, InstalledScope> {
    let Some(bin) = binary_path() else { return HashMap::new() };
    let Ok(out) = Command::new(bin)
        .env("LC_ALL", "C.UTF-8")
        .args(["list", "--app", "--columns=application,installation"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return HashMap::new();
    };
    parse_installed(&String::from_utf8_lossy(&out.stdout))
}

fn parse_installed(output: &str) -> HashMap<String, InstalledScope> {
    let mut map: HashMap<String, InstalledScope> = HashMap::new();
    for line in output.lines() {
        let mut parts = line.split(['\t', ' ']).filter(|s| !s.is_empty());
        let Some(app_id) = parts.next() else { continue };
        if !app_id.contains('.') {
            continue;
        }
        let inst = parts.next().unwrap_or("user");
        let entry = map.entry(app_id.to_string()).or_default();
        if inst.starts_with("system") {
            entry.system = true;
        } else {
            entry.user = true;
        }
    }
    map
}

/// Launches an installed Flatpak application detached from `feather-store`.
pub fn launch(app_id: &str) {
    let Some(bin) = binary_path() else { return };
    let mut cmd = Command::new(bin);
    cmd.arg("run").arg(app_id).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let _ = feather_ui::process::new_session(&mut cmd).spawn();
}

/// Pings `~/.local/share/applications` so a running `feather-de` session refreshes Launchpad
/// and Spotlight immediately after a Flatpak install or removal, even if the Flatpak export dir
/// did not exist when the session started.
fn notify_desktop_shell() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let dir = home.join(".local/share/applications");
    let _ = std::fs::create_dir_all(&dir);
    let marker = dir.join(".feather-sync");
    if std::fs::write(&marker, b"").is_ok() {
        let _ = std::fs::remove_file(&marker);
    }
}

/// One-click Flathub setup:
/// 1. Installs `flatpak` via PackageKit if missing,
/// 2. Adds the `--user` `flathub` remote,
/// 3. Downloads Flathub's AppStream metadata,
/// 4. Reloads and sends the merged catalog.
pub fn connect(send: impl Fn(ConnectEvent) + Send + 'static) {
    std::thread::spawn(move || {
        if !has_binary() {
            send(ConnectEvent::Progress(Some(0.05), "Установка поддержки Flatpak…".into()));
            let pk_event = packagekit::install_blocking("flatpak", |fraction, msg| {
                let scaled = fraction.map(|f| 0.05 + f * 0.45);
                send(ConnectEvent::Progress(scaled, msg));
            });
            match pk_event {
                Event::Done => {}
                Event::Cancelled => {
                    send(ConnectEvent::Cancelled);
                    return;
                }
                Event::Failed(err) => {
                    send(ConnectEvent::Failed(err));
                    return;
                }
                _ => {}
            }
        }
        let Some(bin) = binary_path() else {
            send(ConnectEvent::Failed("Не удалось найти команду flatpak после установки".into()));
            return;
        };
        if let Some(user_dir) = user_flatpak_dir() {
            let _ = std::fs::create_dir_all(user_dir.join("exports/share/applications"));
        }

        send(ConnectEvent::Progress(Some(0.55), "Подключение репозитория Flathub…".into()));
        let add = Command::new(&bin)
            .env("LC_ALL", "C.UTF-8")
            .args(["remote-add", "--user", "--if-not-exists", "flathub", FLATHUB_REPO_URL])
            .stdin(Stdio::null())
            .output();
        match add {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                send(ConnectEvent::Failed(friendly_cli_error(&String::from_utf8_lossy(&out.stderr))));
                return;
            }
            Err(e) => {
                send(ConnectEvent::Failed(format!("Flatpak: {e}")));
                return;
            }
        }

        send(ConnectEvent::Progress(Some(0.72), "Загрузка каталога программ Flathub…".into()));
        let sync = Command::new(&bin)
            .env("LC_ALL", "C.UTF-8")
            .args(["update", "--user", "--appstream", "--noninteractive", "-y", "flathub"])
            .stdin(Stdio::null())
            .output();
        if let Ok(out) = &sync {
            if !out.status.success() && flathub_active_dirs().is_empty() {
                // Fallback without --noninteractive for older flatpak versions
                let retry = Command::new(&bin)
                    .env("LC_ALL", "C.UTF-8")
                    .args(["update", "--user", "--appstream", "-y", "flathub"])
                    .stdin(Stdio::null())
                    .output();
                if let Ok(r) = retry {
                    if !r.status.success() && flathub_active_dirs().is_empty() {
                        send(ConnectEvent::Failed(friendly_cli_error(&String::from_utf8_lossy(&r.stderr))));
                        return;
                    }
                }
            }
        }

        send(ConnectEvent::Progress(Some(0.92), "Загрузка каталога…".into()));
        let catalog = crate::catalog::load();
        notify_desktop_shell();
        if catalog.flathub == Status::Ready {
            send(ConnectEvent::Done(catalog));
        } else {
            send(ConnectEvent::Failed("Не удалось загрузить каталог Flathub. Проверьте подключение к интернету".into()));
        }
    });
}

/// Installs or removes a Flatpak application on a background thread.
pub fn run(op: Op, app_id: String, scope: InstalledScope, send: impl Fn(Event) + Send + 'static) {
    std::thread::spawn(move || {
        let Some(bin) = binary_path() else {
            send(Event::Failed("Flatpak не установлен".into()));
            return;
        };
        send(Event::Progress(None, "Подготовка…".into()));
        let event = match op {
            Op::Install => run_cli_with_progress(
                &bin,
                &["install", "--user", "--noninteractive", "-y", "flathub", &app_id],
                false,
                &send,
            ),
            Op::Remove => {
                // Remove from whichever scope(s) have it installed; default to --user
                let mut result = Event::Done;
                if scope.user || !scope.system {
                    result = run_cli_with_progress(
                        &bin,
                        &["uninstall", "--user", "--noninteractive", "-y", &app_id],
                        false,
                        &send,
                    );
                }
                if matches!(result, Event::Done) && scope.system {
                    result = run_cli_with_progress(
                        &bin,
                        &["uninstall", "--system", "--noninteractive", "-y", &app_id],
                        true,
                        &send,
                    );
                }
                result
            }
        };
        if matches!(event, Event::Done) {
            notify_desktop_shell();
        }
        send(event);
    });
}

/// Lists available Flatpak application updates across `--user` and `--system`.
pub fn check_updates(refresh: bool) -> Vec<Update> {
    let Some(bin) = binary_path() else { return Vec::new() };
    if refresh {
        let _ = Command::new(&bin)
            .env("LC_ALL", "C.UTF-8")
            .args(["update", "--user", "--appstream", "-y"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let mut updates = Vec::new();
    for flag in ["--user", "--system"] {
        let Ok(out) = Command::new(&bin)
            .env("LC_ALL", "C.UTF-8")
            .args(["remote-ls", flag, "--updates", "--app", "--columns=application,name,version,branch"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            continue;
        };
        if out.status.success() {
            updates.extend(parse_updates(&String::from_utf8_lossy(&out.stdout), flag));
        }
    }
    updates.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    updates.dedup_by(|a, b| a.id == b.id);
    updates
}

fn parse_updates(output: &str, scope_flag: &str) -> Vec<Update> {
    let scope_tag = if scope_flag == "--system" { "system" } else { "user" };
    let mut list = Vec::new();
    for line in output.lines() {
        let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
        let Some(&app_id) = cols.first() else { continue };
        if !app_id.contains('.') {
            continue;
        }
        let name = cols.get(1).copied().filter(|s| !s.is_empty()).unwrap_or(app_id);
        let version = cols
            .get(2)
            .copied()
            .filter(|s| !s.is_empty())
            .or_else(|| cols.get(3).copied().filter(|s| !s.is_empty()))
            .unwrap_or("");
        list.push(Update {
            id: format!("flatpak:{scope_tag}:{app_id}"),
            name: name.to_string(),
            version: version.to_string(),
            summary: "Flathub (Flatpak)".to_string(),
            security: false,
            source: Source::Flathub,
        });
    }
    list
}

/// Runs Flatpak updates (`--user` and/or `--system` depending on `flatpak:<scope>:<id>` items).
pub fn run_updates_blocking(flatpak_ids: &[String], send: &(impl Fn(Event) + Send)) -> Event {
    let Some(bin) = binary_path() else { return Event::Done };
    let has_user = flatpak_ids.iter().any(|id| id.starts_with("flatpak:user:"));
    let has_system = flatpak_ids.iter().any(|id| id.starts_with("flatpak:system:"));
    if has_user {
        let ev = run_cli_with_progress(&bin, &["update", "--user", "--noninteractive", "-y"], false, send);
        if !matches!(ev, Event::Done) {
            return ev;
        }
    }
    if has_system {
        let ev = run_cli_with_progress(&bin, &["update", "--system", "--noninteractive", "-y"], true, send);
        if !matches!(ev, Event::Done) {
            return ev;
        }
    }
    notify_desktop_shell();
    Event::Done
}

fn run_cli_with_progress(
    bin: &Path,
    args: &[&str],
    via_pkexec: bool,
    send: &(impl Fn(Event) + Send),
) -> Event {
    let mut cmd = if via_pkexec {
        let mut c = Command::new("pkexec");
        c.arg(bin);
        c
    } else {
        Command::new(bin)
    };
    cmd.env("LC_ALL", "C.UTF-8")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let Ok(mut child) = cmd.spawn() else {
        return Event::Failed("Не удалось запустить Flatpak".into());
    };

    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            s
        })
    });

    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        let mut step_ratio: Option<(u32, u32)> = None;
        while reader.read(&mut byte).ok().filter(|&n| n > 0).is_some() {
            if byte[0] == b'\n' || byte[0] == b'\r' {
                if !buf.is_empty() {
                    let line = String::from_utf8_lossy(&buf);
                    if let Some((fraction, msg)) = parse_progress(&line, &mut step_ratio) {
                        send(Event::Progress(fraction, msg.into()));
                    }
                    buf.clear();
                }
            } else if buf.len() < 1024 {
                buf.push(byte[0]);
            }
        }
    }

    let Ok(status) = child.wait() else {
        return Event::Failed("Операция не удалась".into());
    };
    let stderr = stderr_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    if status.success() {
        return Event::Done;
    }
    // pkexec returns 126 / 127 on dismissed password dialog
    if via_pkexec && matches!(status.code(), Some(126 | 127)) {
        return Event::Cancelled;
    }
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("not authorized") || lower.contains("dismissed") || lower.contains("cancelled") {
        return Event::Cancelled;
    }
    Event::Failed(friendly_cli_error(&stderr))
}

fn friendly_cli_error(stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("no space left") || lower.contains("min-free-space") || lower.contains("not enough disk space") {
        return "Недостаточно места на диске".into();
    }
    if lower.contains("temporary failure in name resolution")
        || lower.contains("network is unreachable")
        || lower.contains("could not connect")
        || lower.contains("cannot resolve hostname")
        || lower.contains("server returned status")
        || lower.contains("while fetching")
    {
        return "Не удалось скачать пакеты. Проверьте подключение к интернету".into();
    }
    let cleaned: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.strip_prefix("error:").map(str::trim).unwrap_or(l))
        .take(2)
        .collect();
    if cleaned.is_empty() {
        "Операция не удалась".into()
    } else {
        cleaned.join(" ")
    }
}

/// Parses a single stdout line from `flatpak install/update/uninstall` into `(fraction, status)`.
fn parse_progress(line: &str, step_ratio: &mut Option<(u32, u32)>) -> Option<(Option<f32>, &'static str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Detect step counter like "Installing 1/3…" or "Updating 2/4…"
    for word in trimmed.split_whitespace() {
        let clean = word.trim_end_matches(|c: char| !c.is_ascii_digit());
        if let Some((a, b)) = clean.split_once('/') {
            if let (Ok(k), Ok(n)) = (a.parse::<u32>(), b.parse::<u32>()) {
                if k >= 1 && k <= n && n <= 200 {
                    *step_ratio = Some((k, n));
                }
            }
        }
    }
    // Detect percentage like "45%"
    let mut pct: Option<f32> = None;
    for token in trimmed.split_whitespace() {
        if let Some(num) = token.strip_suffix('%') {
            if let Ok(p) = num.parse::<u32>() {
                if p <= 100 {
                    pct = Some(p as f32 / 100.0);
                    break;
                }
            }
        }
    }

    let fraction = match (*step_ratio, pct) {
        (Some((k, n)), Some(p)) => Some(((k - 1) as f32 + p) / n as f32),
        (Some((k, n)), None) => Some(((k - 1) as f32 + 0.15) / n as f32),
        (None, Some(p)) => Some(p),
        (None, None) => None,
    };

    let lower = trimmed.to_ascii_lowercase();
    let msg = if lower.contains("looking for") || lower.contains("resolving") || lower.contains("required runtime") {
        "Проверка зависимостей…"
    } else if lower.contains("uninstalling") {
        "Удаление…"
    } else if pct.is_some() || lower.contains("downloading") {
        "Загрузка…"
    } else if lower.contains("installing") || lower.contains("updating") || lower.contains("deploying") {
        "Установка…"
    } else {
        return None;
    };
    Some((fraction, msg))
}

// ---------- Streaming AppStream XML parser ----------

fn parse_appstream_file(path: &Path, icons: &Path, keys: &[&str]) -> Vec<AppInfo> {
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    if path.extension().is_some_and(|e| e == "gz") {
        let gz = flate2::read::GzDecoder::new(file);
        parse_appstream_reader(BufReader::with_capacity(64 * 1024, gz), icons, keys)
    } else {
        parse_appstream_reader(BufReader::with_capacity(64 * 1024, file), icons, keys)
    }
}

/// Streams `<component type="desktop[-application]">…</component>` blocks one at a time,
/// skipping runtimes, addons and SDKs without allocating memory for them.
pub fn parse_appstream_reader<R: BufRead>(mut reader: R, icons: &Path, keys: &[&str]) -> Vec<AppInfo> {
    let mut apps = Vec::new();
    let mut token = Vec::with_capacity(1024);
    let mut comp = Vec::with_capacity(16 * 1024);
    let mut state = StreamState::Outside;

    loop {
        token.clear();
        let Ok(n) = reader.read_until(b'>', &mut token) else { break };
        if n == 0 {
            break;
        }
        // Handle XML comments `<!-- ... -->` cleanly so a `>` inside a comment never desyncs tags
        if ends_with_unclosed_comment(&token) {
            while !token.ends_with(b"-->") {
                let Ok(m) = reader.read_until(b'>', &mut token) else { break };
                if m == 0 || token.len() > MAX_COMPONENT_BYTES {
                    break;
                }
            }
        }

        match state {
            StreamState::Outside => {
                if let Some(pos) = find_subslice(&token, b"<component") {
                    let tag_bytes = &token[pos..];
                    // Ensure `<component` is followed by whitespace or `>`, not `<components>`
                    if matches!(tag_bytes.get(10), Some(b' ' | b'\t' | b'\r' | b'\n' | b'>')) && !tag_bytes.ends_with(b"/>") {
                        let tag_str = String::from_utf8_lossy(tag_bytes);
                        let kind = attr_value(&tag_str, "type").unwrap_or("");
                        if kind == "desktop" || kind == "desktop-application" {
                            comp.clear();
                            comp.extend_from_slice(tag_bytes);
                            state = StreamState::Capturing;
                        } else {
                            state = StreamState::Skipping;
                        }
                    }
                }
            }
            StreamState::Capturing => {
                if comp.len() + token.len() <= MAX_COMPONENT_BYTES {
                    comp.extend_from_slice(&token);
                } else {
                    state = StreamState::Skipping;
                    continue;
                }
                if comp.ends_with(b"</component>") {
                    if let Ok(xml) = std::str::from_utf8(&comp) {
                        if let Some(app) = parse_component(xml, icons, keys) {
                            apps.push(app);
                        }
                    }
                    comp.clear();
                    state = StreamState::Outside;
                }
            }
            StreamState::Skipping => {
                if token.ends_with(b"</component>") {
                    state = StreamState::Outside;
                }
            }
        }
    }
    apps
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Outside,
    Capturing,
    Skipping,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn ends_with_unclosed_comment(buf: &[u8]) -> bool {
    if let Some(pos) = find_subslice(buf, b"<!--") {
        return find_subslice(&buf[pos + 4..], b"-->").is_none();
    }
    false
}

/// Extracts `attr="value"` or `attr='value'` from an opening tag `<tag ...>`.
fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(idx) = rest.find(name) {
        let before_ok = idx == 0 || rest.as_bytes()[idx - 1].is_ascii_whitespace();
        let after = &rest[idx + name.len()..];
        let after_trimmed = after.trim_start();
        if before_ok {
            if let Some(val_start) = after_trimmed.strip_prefix('=') {
                let val_start = val_start.trim_start();
                if let Some(quote) = val_start.chars().next().filter(|&c| c == '"' || c == '\'') {
                    let inner = &val_start[1..];
                    if let Some(end) = inner.find(quote) {
                        return Some(&inner[..end]);
                    }
                }
            }
        }
        rest = after;
    }
    None
}

fn lang_rank(lang: Option<&str>, keys: &[&str]) -> Option<usize> {
    let l = lang.unwrap_or("C");
    keys.iter().position(|&k| k == l)
}

#[derive(Default)]
struct BestText {
    rank: Option<usize>,
    value: String,
    fallback: String,
}

impl BestText {
    fn offer(&mut self, lang: Option<&str>, text: String, keys: &[&str]) {
        if text.is_empty() {
            return;
        }
        if lang.is_none() || lang == Some("C") {
            if self.fallback.is_empty() {
                self.fallback = text.clone();
            }
        }
        if let Some(r) = lang_rank(lang, keys) {
            if self.rank.is_none_or(|cur| r < cur) {
                self.rank = Some(r);
                self.value = text;
            }
        } else if self.fallback.is_empty() {
            self.fallback = text;
        }
    }

    fn finish(self) -> String {
        if !self.value.is_empty() {
            self.value
        } else {
            self.fallback
        }
    }
}

fn parse_component(xml: &str, icons: &Path, keys: &[&str]) -> Option<AppInfo> {
    // Strip outer `<component ...>` and `</component>` so we walk top-level children cleanly
    let inner_start = xml.find('>')? + 1;
    let inner_end = xml.rfind("</component>")?;
    let mut rest = &xml[inner_start..inner_end];

    let mut id = String::new();
    let mut bundle_app_id = String::new();
    let mut name = BestText::default();
    let mut summary = BestText::default();
    let mut developer = BestText::default();
    let mut desc_blocks: Vec<(Option<String>, String)> = Vec::new();
    let mut categories: Vec<String> = Vec::new();
    let mut keywords: Vec<String> = Vec::new();
    let mut cached_icons: Vec<(u32, String)> = Vec::new();
    let mut homepage = String::new();
    let mut license = String::new();
    let mut desktop_id: Option<String> = None;
    let mut version = String::new();

    while let Some(open) = rest.find('<') {
        rest = &rest[open..];
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(end) => {
                    rest = &rest[end + 3..];
                    continue;
                }
                None => break,
            }
        }
        let Some(tag_end) = rest.find('>') else { break };
        let raw_tag = &rest[1..tag_end];
        rest = &rest[tag_end + 1..];

        if raw_tag.starts_with('/') || raw_tag.starts_with('?') || raw_tag.starts_with('!') {
            continue;
        }
        let self_closing = raw_tag.ends_with('/');
        let tag_clean = raw_tag.trim_end_matches('/').trim();
        let tag_name = tag_clean.split_whitespace().next().unwrap_or("");
        if self_closing {
            continue;
        }

        // Fast-skip heavy sections we don't render
        if matches!(tag_name, "screenshots" | "languages" | "Provides" | "provides" | "content_rating" | "branding" | "custom") {
            let close_pat = format!("</{tag_name}>");
            match rest.find(&close_pat) {
                Some(end) => rest = &rest[end + close_pat.len()..],
                None => break,
            }
            continue;
        }

        if tag_name == "releases" {
            if let Some(end) = rest.find("</releases>") {
                let rel_block = &rest[..end];
                if version.is_empty() {
                    if let Some(r_pos) = rel_block.find("<release") {
                        let after = &rel_block[r_pos..];
                        if let Some(gt) = after.find('>') {
                            if let Some(v) = attr_value(&after[..gt], "version") {
                                version = decode_entities(v.trim());
                            }
                        }
                    }
                }
                rest = &rest[end + "</releases>".len()..];
                continue;
            }
            break;
        }

        let close_pat = format!("</{tag_name}>");
        let Some(end) = rest.find(&close_pat) else { continue };
        let body = &rest[..end];
        rest = &rest[end + close_pat.len()..];

        let xml_lang = attr_value(tag_clean, "xml:lang");

        match tag_name {
            "id" => id = decode_entities(body.trim()),
            "bundle" if attr_value(tag_clean, "type") == Some("flatpak") => {
                let b = body.trim();
                if b.starts_with("runtime/") {
                    return None;
                }
                if let Some(rest_ref) = b.strip_prefix("app/") {
                    if let Some(app_id) = rest_ref.split('/').next().filter(|s| !s.is_empty()) {
                        bundle_app_id = decode_entities(app_id);
                    }
                }
            }
            "name" => name.offer(xml_lang, decode_entities(body.trim()), keys),
            "summary" => summary.offer(xml_lang, decode_entities(body.trim()), keys),
            "description" => desc_blocks.push((xml_lang.map(str::to_string), body.to_string())),
            "developer_name" => developer.offer(xml_lang, decode_entities(body.trim()), keys),
            "developer" => {
                for (sub_tag, sub_body) in child_elements(body, "name") {
                    let sub_lang = attr_value(sub_tag, "xml:lang");
                    developer.offer(sub_lang, decode_entities(sub_body.trim()), keys);
                }
            }
            "categories" => {
                for (_, cat) in child_elements(body, "category") {
                    let c = decode_entities(cat.trim());
                    if !c.is_empty() && !categories.contains(&c) {
                        categories.push(c);
                    }
                }
            }
            "keywords" => {
                let parent_ok = xml_lang.is_none() || xml_lang == Some("C") || lang_rank(xml_lang, keys).is_some();
                if parent_ok {
                    for (kw_tag, kw_body) in child_elements(body, "keyword") {
                        let kw_lang = attr_value(kw_tag, "xml:lang");
                        if kw_lang.is_none() || kw_lang == Some("C") || lang_rank(kw_lang, keys).is_some() {
                            let kw = decode_entities(kw_body.trim());
                            if !kw.is_empty() {
                                keywords.push(kw);
                            }
                        }
                    }
                }
            }
            "icon" if attr_value(tag_clean, "type") == Some("cached") => {
                let file = decode_entities(body.trim());
                if !file.is_empty() {
                    let w = attr_value(tag_clean, "width").and_then(|s| s.parse::<u32>().ok()).unwrap_or(64);
                    cached_icons.push((w, file));
                }
            }
            "url" if attr_value(tag_clean, "type") == Some("homepage") => {
                if homepage.is_empty() {
                    homepage = decode_entities(body.trim());
                }
            }
            "project_license" => license = decode_entities(body.trim()),
            "launchable" if attr_value(tag_clean, "type") == Some("desktop-id") => {
                if desktop_id.is_none() {
                    let d = decode_entities(body.trim());
                    if !d.is_empty() {
                        desktop_id = Some(d);
                    }
                }
            }
            _ => {}
        }
    }

    let flatpak_id = if !bundle_app_id.is_empty() {
        bundle_app_id
    } else {
        infer_flatpak_id(&id)
    };
    if flatpak_id.is_empty() {
        return None;
    }
    if id.is_empty() {
        id = flatpak_id.clone();
    }

    let untranslated_name = name.fallback.clone();
    let final_name = name.finish();
    if final_name.is_empty() {
        return None;
    }
    let final_summary = summary.finish();
    let description = parse_localized_description(&desc_blocks, keys);
    let icon = resolve_flatpak_icon(&cached_icons, &flatpak_id, icons);

    Some(AppInfo {
        search_name: format!(
            "{} {} {}",
            final_name.to_lowercase(),
            untranslated_name.to_lowercase(),
            flatpak_id.to_lowercase()
        ),
        search_keywords: format!("{} {}", keywords.join(" ").to_lowercase(), final_summary.to_lowercase()),
        id,
        package: String::new(),
        flatpak_id,
        flatpak_version: version,
        name: final_name,
        summary: final_summary,
        description,
        categories,
        icon,
        homepage,
        developer: developer.finish(),
        license,
        desktop_id,
    })
}

/// Preserves `.desktop` when part of a 3-segment reverse-DNS ID (like `org.telegram.desktop`),
/// and strips legacy `.desktop` suffix on 4+ segment IDs (like `org.kde.kdenlive.desktop`).
fn infer_flatpak_id(id: &str) -> String {
    if let Some(stem) = id.strip_suffix(".desktop") {
        if stem.split('.').count() >= 3 {
            return stem.to_string();
        }
    }
    id.to_string()
}

fn child_elements<'a>(mut xml: &'a str, wanted: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    let close_pat = format!("</{wanted}>");
    while let Some(open) = xml.find('<') {
        xml = &xml[open..];
        let Some(gt) = xml.find('>') else { break };
        let tag = xml[1..gt].trim();
        xml = &xml[gt + 1..];
        if tag.starts_with('/') || tag.ends_with('/') {
            continue;
        }
        let name = tag.split_whitespace().next().unwrap_or("");
        if name == wanted {
            let Some(end) = xml.find(&close_pat) else { break };
            out.push((tag, &xml[..end]));
            xml = &xml[end + close_pat.len()..];
        }
    }
    out
}

/// Parses AppStream `<description>` supporting BOTH:
/// 1. Separate `<description xml:lang="ru">…</description>` blocks, and
/// 2. A single `<description>` containing `<p>` / `<p xml:lang="ru">` and `<li>` / `<li xml:lang="ru">`
///    with per-paragraph fallback to `"C"` when a translation is partial.
fn parse_localized_description(blocks: &[(Option<String>, String)], keys: &[&str]) -> Vec<String> {
    if blocks.is_empty() {
        return Vec::new();
    }
    // Pick the best `<description xml:lang="...">` block if localized outer blocks exist
    let mut best_block: Option<(usize, &str)> = None;
    let mut fallback_block: Option<&str> = None;
    for (lang, body) in blocks {
        if lang.is_none() || lang.as_deref() == Some("C") {
            if fallback_block.is_none() {
                fallback_block = Some(body.as_str());
            }
        }
        if let Some(r) = lang_rank(lang.as_deref(), keys) {
            if best_block.is_none_or(|(cur, _)| r < cur) {
                best_block = Some((r, body.as_str()));
            }
        }
    }
    let chosen = best_block.map(|(_, b)| b).or(fallback_block).unwrap_or(blocks[0].1.as_str());

    // Fast path: if no inner `xml:lang` attributes exist, reuse `catalog::paragraphs`
    if !chosen.contains("xml:lang") {
        return paragraphs(chosen);
    }

    // Per-element (`<p>` / `<li>`) localization with fallback to the preceding `"C"` slot
    let mut slots: Vec<(Option<usize>, String)> = Vec::new();
    let mut rest = chosen;
    while let Some(open) = rest.find('<') {
        rest = &rest[open..];
        let Some(gt) = rest.find('>') else { break };
        let tag = rest[1..gt].trim();
        rest = &rest[gt + 1..];
        if tag.starts_with('/') || tag.ends_with('/') {
            continue;
        }
        let name = tag.split_whitespace().next().unwrap_or("");
        if name != "p" && name != "li" {
            continue;
        }
        let close_pat = if name == "p" { "</p>" } else { "</li>" };
        let Some(end) = rest.find(close_pat) else { break };
        let inner = &rest[..end];
        rest = &rest[end + close_pat.len()..];

        let lang = attr_value(tag, "xml:lang");
        let rank = lang_rank(lang, keys);
        if lang.is_some() && lang != Some("C") && rank.is_none() {
            // Foreign language not in `keys` — skip without allocating
            continue;
        }
        let text = strip_inline_markup(inner);
        if text.is_empty() {
            continue;
        }
        let formatted = if name == "li" { format!("• {text}") } else { text };
        if lang.is_none() || lang == Some("C") {
            slots.push((rank, formatted));
        } else if let Some(r) = rank {
            if let Some((cur_rank, cur_text)) = slots.last_mut() {
                if cur_rank.is_none_or(|cr| r < cr) {
                    *cur_rank = Some(r);
                    *cur_text = formatted;
                }
            } else {
                slots.push((Some(r), formatted));
            }
        }
    }
    slots.into_iter().map(|(_, t)| t).collect()
}

fn strip_inline_markup(markup: &str) -> String {
    let mut raw = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(open) = rest.find('<') {
        raw.push_str(&rest[..open]);
        match rest[open..].find('>') {
            Some(close) => rest = &rest[open + close + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    raw.push_str(rest);
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    decode_entities(&collapsed)
}

fn resolve_flatpak_icon(cached: &[(u32, String)], flatpak_id: &str, icons_dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    // Prefer 128×128 first (crisp on Detail page and HiDPI), then 64×64
    let mut sorted = cached.to_vec();
    sorted.sort_by_key(|(w, _)| if *w >= 128 { *w - 128 } else { 1000 + 128 - *w });
    for (w, name) in &sorted {
        candidates.push(icons_dir.join(format!("{w}x{w}")).join(name));
        candidates.push(icons_dir.join("128x128").join(name));
        candidates.push(icons_dir.join("64x64").join(name));
    }
    let fallback_png = format!("{flatpak_id}.png");
    candidates.push(icons_dir.join("128x128").join(&fallback_png));
    candidates.push(icons_dir.join("64x64").join(&fallback_png));
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<components version="0.8" origin="flatpak">
  <!-- Runtime must be ignored -->
  <component type="runtime">
    <id>org.gnome.Platform</id>
    <name>GNOME Application Platform</name>
  </component>
  <!-- Desktop component with itstool-style per-<p> xml:lang -->
  <component type="desktop-application">
    <id>net.nokyan.Resources</id>
    <name>Resources</name>
    <name xml:lang="de">Ressourcen</name>
    <name xml:lang="ru">Ресурсы</name>
    <summary>Keep an eye on system resources</summary>
    <summary xml:lang="ru">Отслеживайте системные ресурсы</summary>
    <project_license>GPL-3.0-or-later</project_license>
    <developer id="net.nokyan">
      <name>Nokyan</name>
    </developer>
    <description>
      <p>Monitor your system processes and GPU &amp; CPU usage.</p>
      <p xml:lang="de">Überwachen Sie Prozesse.</p>
      <p xml:lang="ru">Контролируйте запущенные процессы и нагрузку ЦП &amp; ГП.</p>
      <p>Untranslated second paragraph stays readable.</p>
      <ul>
        <li>CPU</li>
        <li xml:lang="ru">ЦП</li>
        <li>Memory &#8212; RAM</li>
        <li xml:lang="ru">Память &#8212; ОЗУ</li>
      </ul>
    </description>
    <categories>
      <category>System</category>
      <category>Monitor</category>
    </categories>
    <keywords>
      <keyword>task</keyword>
      <keyword xml:lang="ru">монитор</keyword>
    </keywords>
    <url type="homepage">https://nokyan.net/resources</url>
    <launchable type="desktop-id">net.nokyan.Resources.desktop</launchable>
    <bundle type="flatpak" runtime="org.gnome.Platform/x86_64/47">app/net.nokyan.Resources/x86_64/stable</bundle>
    <icon type="cached" width="128" height="128">net.nokyan.Resources.png</icon>
    <screenshots>
      <screenshot type="default"><image>https://example.org/1.png</image></screenshot>
    </screenshots>
    <releases>
      <release version="1.7.1" timestamp="1730000000">
        <description><p>Bug fixes</p></description>
      </release>
      <release version="1.7.0" timestamp="1720000000"/>
    </releases>
  </component>
  <!-- Telegram's reverse-DNS ends with .desktop and must NOT lose .desktop -->
  <component type="desktop">
    <id>org.telegram.desktop</id>
    <name>Telegram</name>
    <summary>Official desktop client</summary>
    <developer_name>Telegram Messenger Inc.</developer_name>
    <description xml:lang="C"><p>Fast messaging.</p></description>
    <description xml:lang="ru"><p>Быстрый мессенджер.</p></description>
    <bundle type="flatpak">app/org.telegram.desktop/x86_64/stable</bundle>
  </component>
  <!-- Addon bundled as runtime/ must be skipped even if mistyped -->
  <component type="desktop">
    <id>org.gimp.GIMP.Plugin.GMic</id>
    <name>G'MIC</name>
    <bundle type="flatpak">runtime/org.gimp.GIMP.Plugin.GMic/x86_64/3</bundle>
  </component>
</components>
"#;

    #[test]
    fn streams_flathub_appstream_xml_accurately() {
        let apps = parse_appstream_reader(
            BufReader::new(SAMPLE_XML.as_bytes()),
            Path::new("/nonexistent"),
            &["ru", "ru_RU", "C", "en"],
        );
        assert_eq!(apps.len(), 2, "{apps:#?}");

        let res = &apps[0];
        assert_eq!(res.flatpak_id, "net.nokyan.Resources");
        assert_eq!(res.package, "");
        assert_eq!(res.name, "Ресурсы");
        assert_eq!(res.summary, "Отслеживайте системные ресурсы");
        assert_eq!(
            res.description,
            [
                "Контролируйте запущенные процессы и нагрузку ЦП & ГП.",
                "Untranslated second paragraph stays readable.",
                "• ЦП",
                "• Память — ОЗУ",
            ]
        );
        assert_eq!(res.developer, "Nokyan");
        assert_eq!(res.license, "GPL-3.0-or-later");
        assert_eq!(res.flatpak_version, "1.7.1");
        assert_eq!(res.homepage, "https://nokyan.net/resources");
        assert_eq!(res.categories, ["System", "Monitor"]);
        assert!(res.search_name.contains("resources") && res.search_keywords.contains("монитор"));

        let tg = &apps[1];
        assert_eq!(tg.flatpak_id, "org.telegram.desktop");
        assert_eq!(tg.developer, "Telegram Messenger Inc.");
        assert_eq!(tg.description, ["Быстрый мессенджер."]);
    }

    #[test]
    fn parses_installed_and_updates_and_progress() {
        let installed = parse_installed("org.gimp.GIMP\tuser\ncom.obsproject.Studio\tsystem\norg.gimp.GIMP\tsystem\n");
        assert_eq!(installed.get("org.gimp.GIMP"), Some(&InstalledScope { user: true, system: true }));
        assert_eq!(installed.get("com.obsproject.Studio"), Some(&InstalledScope { user: false, system: true }));

        let updates = parse_updates("org.gimp.GIMP\tGIMP\t3.0.6\tstable\nio.mpv.Mpv\tmpv\t\tstable\n", "--user");
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].id, "flatpak:user:org.gimp.GIMP");
        assert_eq!(updates[0].version, "3.0.6");
        assert_eq!(updates[0].source, Source::Flathub);
        assert_eq!(updates[1].version, "stable");

        let mut step = None;
        let p1 = parse_progress("Installing 1/2…", &mut step).unwrap();
        assert_eq!(step, Some((1, 2)));
        assert_eq!(p1.1, "Установка…");
        let p2 = parse_progress("Installing 2/2…                        50%  4.2 MB/s", &mut step).unwrap();
        assert!((p2.0.unwrap() - 0.75).abs() < 0.01);
        assert_eq!(p2.1, "Загрузка…");
    }
}
