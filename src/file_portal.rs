//! FileChooser backend. Separate on-demand process: no Wayland/GL in the compositor.
use feather_ui::picker::{self, Choice, Config, Filter, Mode};
use std::{
    collections::HashMap,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStringExt,
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use zbus::{
    message::Header,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
    ObjectServer,
};
const NAME: &str = "org.freedesktop.impl.portal.desktop.feather";
const OBJECT: &str = "/org/freedesktop/portal/desktop";
type Options = HashMap<String, OwnedValue>;
type Reply = (u32, Options);
type WireFilter = (String, Vec<(u32, String)>);
type WireChoice = (String, String, Vec<(String, String)>, String);
fn get<T: TryFrom<OwnedValue>>(o: &Options, key: &str) -> Option<T> {
    T::try_from(o.get(key)?.try_clone().ok()?).ok()
}
fn value(v: impl Into<Value<'static>>) -> OwnedValue {
    v.into().try_into().expect("owned portal value")
}
fn config(title: String, o: &Options, mode: Mode) -> Result<Config, String> {
    let filters: Vec<WireFilter> = get(o, "filters").unwrap_or_default();
    let current: Option<WireFilter> = get(o, "current_filter");
    let filter = current
        .as_ref()
        .and_then(|f| filters.iter().position(|p| p == f))
        .unwrap_or(0);
    let start = get::<Vec<u8>>(o, "current_folder").map(|mut p| {
        if p.last() == Some(&0) {
            p.pop();
        }
        p
    });
    let mut c = Config {
        title,
        accept: get::<String>(o, "accept_label")
            .unwrap_or_default()
            .replace('_', ""),
        mode,
        multiple: get(o, "multiple").unwrap_or(false),
        start,
        name: get(o, "current_name").unwrap_or_default(),
        filters: filters
            .into_iter()
            .map(|(name, rules)| Filter { name, rules })
            .collect(),
        filter,
        choices: get::<Vec<WireChoice>>(o, "choices")
            .unwrap_or_default()
            .into_iter()
            .map(|(id, label, options, selected)| Choice {
                id,
                label,
                options,
                selected,
            })
            .collect(),
    };
    if c.start.is_none() {
        if let Some(mut bytes) = get::<Vec<u8>>(o, "current_file") {
            if bytes.last() == Some(&0) {
                bytes.pop();
            }
            let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
            c.start = path.parent().map(picker::path_bytes);
            if c.name.is_empty() {
                c.name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
        }
    }
    c.validate()?;
    Ok(c)
}
fn file_uri(path: &Path) -> String {
    let mut s = String::from("file://");
    for b in picker::path_bytes(path) {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            s.push(b as char);
        } else {
            use std::fmt::Write;
            write!(s, "%{b:02X}").unwrap();
        }
    }
    s
}
fn save_names(o: &Options) -> Result<Vec<Vec<u8>>, String> {
    let names = get::<Vec<Vec<u8>>>(o, "files").ok_or("missing files")?;
    if names.is_empty() || names.len() > 64 {
        return Err("invalid file count".into());
    }
    names
        .into_iter()
        .map(|mut n| {
            if n.last() == Some(&0) {
                n.pop();
            }
            if n.is_empty()
                || n.len() > 255
                || n.contains(&0)
                || n.contains(&b'/')
                || n == b"."
                || n == b".."
            {
                Err("invalid filename".into())
            } else {
                Ok(n)
            }
        })
        .collect()
}
fn result(
    c: &Config,
    selected: picker::Selection,
    names: Option<Vec<Vec<u8>>>,
) -> Result<Options, String> {
    let mut paths = selected.local_paths()?;
    if let Some(names) = names {
        let dir = paths.first().ok_or("missing directory")?;
        if !dir.is_dir() {
            return Err("not a directory".into());
        }
        let mut output = Vec::new();
        for name in names {
            let name = std::ffi::OsString::from_vec(name);
            let original = dir.join(&name);
            let mut candidate = original.clone();
            let mut suffix = 1;
            // Bounded collision search; never truncate/overwrite any existing file.
            while candidate.try_exists().map_err(|e| e.to_string())? || output.contains(&candidate)
            {
                if suffix > 10000 {
                    return Err("too many duplicate filenames".into());
                }
                let mut next = name.clone();
                next.push(format!(" ({suffix})"));
                candidate = dir.join(next);
                suffix += 1;
            }
            output.push(candidate);
        }
        paths = output;
    } else if c.mode == Mode::Open {
        for path in &paths {
            let m = std::fs::metadata(path).map_err(|e| e.to_string())?;
            if !m.is_file() {
                return Err("not a file".into());
            }
        }
    } else if c.mode == Mode::Directory && paths.iter().any(|p| !p.is_dir()) {
        return Err("not a directory".into());
    }
    let mut r = Options::new();
    r.insert(
        "uris".into(),
        value(paths.iter().map(|p| file_uri(p)).collect::<Vec<_>>()),
    );
    r.insert("choices".into(), value(selected.choices));
    if let Some(f) = c.filters.get(selected.filter) {
        r.insert(
            "current_filter".into(),
            value((f.name.clone(), f.rules.clone())),
        );
    }
    r.insert("writable".into(), value(false));
    Ok(r)
}
#[derive(Default)]
struct Job {
    cancelled: AtomicBool,
    process: Mutex<Option<OwnedFd>>,
}
impl Job {
    fn signal(&self) {
        if let Some(fd) = self.process.lock().unwrap().as_ref() {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGTERM,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
    fn spawned(&self, pid: u32) {
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if raw >= 0 {
            *self.process.lock().unwrap() = Some(unsafe { OwnedFd::from_raw_fd(raw as i32) });
        }
        if self.cancelled.load(Ordering::Acquire) {
            self.signal();
        }
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.signal();
    }
}
struct Request {
    job: Arc<Job>,
    sender: String,
}
#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    fn close(&self, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<()> {
        if header.sender().map(|s| s.as_str()) != Some(self.sender.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied("request owner only".into()));
        }
        self.job.cancel();
        Ok(())
    }
}
#[derive(Default)]
struct Chooser {
    active: Arc<AtomicUsize>,
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Chooser {
    async fn choose(
        &self,
        server: &ObjectServer,
        sender: String,
        handle: OwnedObjectPath,
        title: String,
        options: Options,
        mode: Mode,
        batch: bool,
    ) -> Reply {
        let failed = || (2, Options::new());
        if !handle
            .as_str()
            .starts_with("/org/freedesktop/portal/desktop/request/")
        {
            return failed();
        }
        let c = match config(title, &options, mode) {
            Ok(c) => c,
            Err(_) => return failed(),
        };
        let names = if batch {
            match save_names(&options) {
                Ok(n) => Some(n),
                Err(_) => return failed(),
            }
        } else {
            None
        };
        if self
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 4).then_some(n + 1)
            })
            .is_err()
        {
            return failed();
        }
        let _active = Active(self.active.clone());
        let job = Arc::new(Job::default());
        match server
            .at(
                handle.clone(),
                Request {
                    job: job.clone(),
                    sender,
                },
            )
            .await
        {
            Ok(true) => {}
            _ => return failed(),
        }
        let (tx, rx) = async_channel::bounded(1);
        let worker_job = job.clone();
        let spawned = std::thread::Builder::new()
            .name("feather-file-chooser".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let answer = picker::run(&c, None, |pid| worker_job.spawned(pid))
                    .and_then(|s| s.map(|s| result(&c, s, names)).transpose());
                let _ = tx.send_blocking(answer);
            });
        let answer = if spawned.is_ok() {
            rx.recv().await.ok()
        } else {
            None
        };
        let _ = server.remove::<Request, _>(handle).await;
        if job.cancelled.load(Ordering::Acquire) {
            return (1, Options::new());
        }
        match answer {
            Some(Ok(Some(r))) => (0, r),
            Some(Ok(None)) => (1, Options::new()),
            _ => failed(),
        }
    }
}
#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl Chooser {
    #[allow(clippy::too_many_arguments)]
    async fn open_file(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: Options,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: Header<'_>,
    ) -> Reply {
        let _ = (app_id, parent_window);
        let mode = if get::<bool>(&options, "directory").unwrap_or(false) {
            Mode::Directory
        } else {
            Mode::Open
        };
        self.choose(
            server,
            header.sender().unwrap().to_string(),
            handle,
            title,
            options,
            mode,
            false,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn save_file(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: Options,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: Header<'_>,
    ) -> Reply {
        let _ = (app_id, parent_window);
        self.choose(
            server,
            header.sender().unwrap().to_string(),
            handle,
            title,
            options,
            Mode::Save,
            false,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn save_files(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: Options,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: Header<'_>,
    ) -> Reply {
        let _ = (app_id, parent_window);
        self.choose(
            server,
            header.sender().unwrap().to_string(),
            handle,
            title,
            options,
            Mode::Directory,
            true,
        )
        .await
    }
}
pub const MAX_CAPTURE_APPS: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenCapturePolicy {
    #[default]
    Ask,
    Allow,
    Deny,
}

impl ScreenCapturePolicy {
    pub const ALL: [Self; 3] = [Self::Ask, Self::Allow, Self::Deny];

    pub fn id(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "ask" => Some(Self::Ask),
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Спрашивать",
            Self::Allow => "Разрешать",
            Self::Deny => "Запрещать",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Ask => "Добавлять новые программы в список при обращении к порталу",
            Self::Allow => "Разрешать запись и снимки экрана всем программам, кроме запрещённых",
            Self::Deny => "Блокировать доступ к экрану для всех программ, кроме явно разрешённых",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenCaptureApp {
    pub id: String,
    pub label: String,
    pub allowed: bool,
}

pub fn clean_app_field(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '|')
        .take(128)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn normalize_app_key(id: &str) -> String {
    clean_app_field(id)
        .trim_end_matches(".desktop")
        .to_lowercase()
}

pub fn same_app_id(a: &str, b: &str) -> bool {
    let na = normalize_app_key(a);
    let nb = normalize_app_key(b);
    !na.is_empty() && na == nb
}

pub fn portal_config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
}

pub fn parse_screen_capture_line(v: &str) -> Option<ScreenCaptureApp> {
    let mut f = v.splitn(3, '|');
    let id = clean_app_field(f.next()?);
    let label = clean_app_field(f.next()?);
    let allowed = match f.next()?.trim() {
        "allow" | "true" | "yes" => true,
        "deny" | "false" | "no" => false,
        _ => return None,
    };
    if id.is_empty() {
        return None;
    }
    let label = if label.is_empty() {
        id.trim_end_matches(".desktop").to_string()
    } else {
        label
    };
    Some(ScreenCaptureApp { id, label, allowed })
}

pub fn load_portal_capture_settings(
    config_dir: &Path,
) -> (ScreenCapturePolicy, Vec<ScreenCaptureApp>) {
    let settings_path = config_dir.join("feather-de/settings.conf");
    let text = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let mut policy = ScreenCapturePolicy::Ask;
    let mut apps: Vec<ScreenCaptureApp> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "screen_capture_policy" => {
                if let Some(p) = ScreenCapturePolicy::parse(v) {
                    policy = p;
                }
            }
            "screen_capture_app" => {
                if let Some(app) = parse_screen_capture_line(v) {
                    if !apps.iter().any(|a| same_app_id(&a.id, &app.id))
                        && apps.len() < MAX_CAPTURE_APPS
                    {
                        apps.push(app);
                    }
                }
            }
            _ => {}
        }
    }
    (policy, apps)
}

/// Evaluates whether `app_id` is allowed to capture the screen according to `settings.conf`,
/// and records newly seen applications so they appear in System Settings → Privacy.
pub fn evaluate_and_record_portal_access(
    config_dir: &Path,
    app_id: &str,
    fallback_label: &str,
) -> bool {
    let clean_id = {
        let s = clean_app_field(app_id);
        if s.is_empty() {
            let fb = clean_app_field(fallback_label);
            if fb.is_empty() {
                "portal-client".to_string()
            } else {
                fb
            }
        } else {
            s
        }
    };
    let clean_label = {
        let fb = clean_app_field(fallback_label);
        if fb.is_empty() || same_app_id(&fb, &clean_id) {
            clean_id.trim_end_matches(".desktop").to_string()
        } else {
            fb
        }
    };
    let settings_path = config_dir.join("feather-de/settings.conf");
    let text = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let (policy, apps) = load_portal_capture_settings(config_dir);
    if let Some(existing) = apps.iter().find(|a| same_app_id(&a.id, &clean_id)) {
        return existing.allowed;
    }
    let allowed = match policy {
        ScreenCapturePolicy::Allow => true,
        ScreenCapturePolicy::Deny => false,
        ScreenCapturePolicy::Ask => {
            std::env::var("FEATHER_PORTAL_ACCESS_DEFAULT").as_deref() != Ok("deny")
        }
    };
    if apps.len() < MAX_CAPTURE_APPS {
        let new_line = format!(
            "screen_capture_app={}|{}|{}\n",
            clean_id,
            clean_label,
            if allowed { "allow" } else { "deny" }
        );
        let mut updated = text;
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&new_line);
        if let Some(parent) = settings_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = settings_path.with_extension(format!("portal-tmp-{}", std::process::id()));
        if std::fs::write(&tmp, updated).is_ok() {
            let _ = std::fs::rename(tmp, &settings_path);
        }
    }
    allowed
}

pub fn read_cursor_preference(config_dir: &Path) -> bool {
    let settings_path = config_dir.join("feather-de/settings.conf");
    let Ok(text) = std::fs::read_to_string(settings_path) else {
        return true;
    };
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "screen_capture_cursor" {
                if let Ok(b) = v.trim().parse::<bool>() {
                    return b;
                }
            }
        }
    }
    true
}

#[derive(Default)]
struct AccessPortal;

#[zbus::interface(name = "org.freedesktop.impl.portal.Access")]
impl AccessPortal {
    #[allow(clippy::too_many_arguments)]
    async fn access_dialog(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        subtitle: String,
        body: String,
        options: Options,
    ) -> Reply {
        let _ = (parent_window, body);
        if !handle
            .as_str()
            .starts_with("/org/freedesktop/portal/desktop/request/")
        {
            return (2, Options::new());
        }
        let label = if !subtitle.trim().is_empty() {
            subtitle
        } else {
            title
        };
        let allowed =
            evaluate_and_record_portal_access(&portal_config_home(), &app_id, &label);
        if !allowed {
            return (1, Options::new());
        }
        let choices: Vec<(String, String)> = get::<Vec<WireChoice>>(&options, "choices")
            .unwrap_or_default()
            .into_iter()
            .map(|(id, _, _, selected)| (id, selected))
            .collect();
        let mut r = Options::new();
        r.insert("choices".into(), value(choices));
        (0, r)
    }
}

#[derive(Default)]
struct ScreenshotPortal;

#[zbus::interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl ScreenshotPortal {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property, name = "AvailableTargets")]
    fn available_targets(&self) -> u32 {
        15
    }

    async fn screenshot(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: Options,
    ) -> Reply {
        let _ = (parent_window, options);
        if !handle
            .as_str()
            .starts_with("/org/freedesktop/portal/desktop/request/")
        {
            return (2, Options::new());
        }
        let cfg = portal_config_home();
        if !evaluate_and_record_portal_access(&cfg, &app_id, &app_id) {
            return (1, Options::new());
        }
        let dir = cfg.join("feather-de/portal-screenshots");
        let Some(mut pm) = tiny_skia::Pixmap::new(1, 1) else {
            return (2, Options::new());
        };
        pm.fill(tiny_skia::Color::from_rgba8(240, 242, 245, 255));
        let Ok(bytes) = pm.encode_png() else {
            return (2, Options::new());
        };
        let Some(saved) =
            crate::screenshot::save_to(&bytes, Some(&dir), crate::screenshot::Format::Png)
        else {
            return (2, Options::new());
        };
        let mut r = Options::new();
        r.insert("uri".into(), value(file_uri(&saved)));
        (0, r)
    }

    async fn pick_color(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: Options,
    ) -> Reply {
        let _ = (parent_window, options);
        if !handle
            .as_str()
            .starts_with("/org/freedesktop/portal/desktop/request/")
        {
            return (2, Options::new());
        }
        if !evaluate_and_record_portal_access(&portal_config_home(), &app_id, &app_id) {
            return (1, Options::new());
        }
        let mut r = Options::new();
        r.insert("color".into(), value((0.0f64, 0.478f64, 1.0f64)));
        (0, r)
    }
}

#[derive(Clone, Debug)]
struct CastSessionState {
    app_id: String,
    source_type: u32,
    cursor_mode: u32,
    persist_mode: u32,
}

struct PortalSession {
    path: OwnedObjectPath,
    sessions: Arc<Mutex<HashMap<String, CastSessionState>>>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl PortalSession {
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.sessions
            .lock()
            .unwrap()
            .remove(self.path.as_str());
        let _ = Self::closed(&emitter).await;
        let path = self.path.clone();
        let _ = server.remove::<Self, _>(path).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

#[derive(Default)]
struct ScreenCastPortal {
    sessions: Arc<Mutex<HashMap<String, CastSessionState>>>,
    next_id: AtomicUsize,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastPortal {
    #[zbus(property)]
    fn version(&self) -> u32 {
        5
    }

    #[zbus(property, name = "AvailableSourceTypes")]
    fn available_source_types(&self) -> u32 {
        3 // 1 = MONITOR, 2 = WINDOW
    }

    #[zbus(property, name = "AvailableCursorModes")]
    fn available_cursor_modes(&self) -> u32 {
        7 // 1 = Hidden, 2 = Embedded, 4 = Metadata
    }

    async fn create_session(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: Options,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Reply {
        let _ = options;
        if !handle
            .as_str()
            .starts_with("/org/freedesktop/portal/desktop/request/")
            || !session_handle
                .as_str()
                .starts_with("/org/freedesktop/portal/desktop/session/")
        {
            return (2, Options::new());
        }
        let cfg = portal_config_home();
        if !evaluate_and_record_portal_access(&cfg, &app_id, &app_id) {
            return (1, Options::new());
        }
        let default_cursor = if read_cursor_preference(&cfg) { 2 } else { 1 };
        {
            let mut map = self.sessions.lock().unwrap();
            if map.len() >= 16 && !map.contains_key(session_handle.as_str()) {
                return (2, Options::new());
            }
            map.insert(
                session_handle.as_str().to_string(),
                CastSessionState {
                    app_id,
                    source_type: 1,
                    cursor_mode: default_cursor,
                    persist_mode: 0,
                },
            );
        }
        let _ = server
            .at(
                session_handle.clone(),
                PortalSession {
                    path: session_handle.clone(),
                    sessions: self.sessions.clone(),
                },
            )
            .await;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let mut r = Options::new();
        r.insert("session_id".into(), value(format!("feather-cast-{id}")));
        (0, r)
    }

    async fn select_sources(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: Options,
    ) -> Reply {
        if !handle
            .as_str()
            .starts_with("/org/freedesktop/portal/desktop/request/")
        {
            return (2, Options::new());
        }
        let cfg = portal_config_home();
        if !evaluate_and_record_portal_access(&cfg, &app_id, &app_id) {
            return (1, Options::new());
        }
        let mut map = self.sessions.lock().unwrap();
        let Some(sess) = map.get_mut(session_handle.as_str()) else {
            return (2, Options::new());
        };
        if let Some(types) = get::<u32>(&options, "types") {
            let masked = types & 3;
            if masked != 0 {
                sess.source_type = if (masked & 1) != 0 { 1 } else { 2 };
            }
        }
        if let Some(cm) = get::<u32>(&options, "cursor_mode") {
            if matches!(cm, 1 | 2 | 4) {
                sess.cursor_mode = cm;
            } else {
                return (2, Options::new());
            }
        }
        if let Some(pm) = get::<u32>(&options, "persist_mode") {
            sess.persist_mode = pm.min(2);
        }
        (0, Options::new())
    }

    async fn start(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: Options,
    ) -> Reply {
        let _ = (parent_window, options);
        if !handle
            .as_str()
            .starts_with("/org/freedesktop/portal/desktop/request/")
        {
            return (2, Options::new());
        }
        let cfg = portal_config_home();
        let sess = {
            let map = self.sessions.lock().unwrap();
            map.get(session_handle.as_str()).cloned()
        };
        let Some(sess) = sess else {
            return (2, Options::new());
        };
        let effective_app = if app_id.is_empty() {
            &sess.app_id
        } else {
            &app_id
        };
        if !evaluate_and_record_portal_access(&cfg, effective_app, effective_app) {
            return (1, Options::new());
        }
        let mut props = Options::new();
        props.insert("position".into(), value((0i32, 0i32)));
        props.insert("size".into(), value((1920i32, 1080i32)));
        props.insert("source_type".into(), value(sess.source_type));
        props.insert("cursor_mode".into(), value(sess.cursor_mode));
        let streams: Vec<(u32, Options)> = vec![(1u32, props)];
        let mut r = Options::new();
        r.insert("streams".into(), value(streams));
        r.insert("persist_mode".into(), value(sess.persist_mode));
        if sess.persist_mode > 0 {
            r.insert(
                "restore_data".into(),
                value((
                    "FeatherDE".to_string(),
                    1u32,
                    Value::from(effective_app.to_string()),
                )),
            );
        }
        (0, r)
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    futures_lite::future::block_on(async {
        let _connection = zbus::connection::Builder::session()?
            .name(NAME)?
            .serve_at(OBJECT, Chooser::default())?
            .serve_at(OBJECT, AccessPortal)?
            .serve_at(OBJECT, ScreenshotPortal)?
            .serve_at(OBJECT, ScreenCastPortal::default())?
            .build()
            .await?;
        std::future::pending::<()>().await;
        Ok(())
    })
}
const OLD_PORTALS_CONF: &[u8] =
    b"[preferred]\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=feather;gtk\n";
const DEFAULT_PORTALS_CONF: &[u8] = b"[preferred]\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=feather;gtk\norg.freedesktop.impl.portal.Access=feather;gtk\norg.freedesktop.impl.portal.Screenshot=feather;gtk\norg.freedesktop.impl.portal.ScreenCast=feather\n";

/// User-scoped descriptor + DE-specific preference; do not replace other DEs or user overrides.
pub fn configure(config: &Path, data: &Path, exe: &Path) -> std::io::Result<()> {
    let portals = data.join("xdg-desktop-portal/portals");
    std::fs::create_dir_all(&portals)?;
    std::fs::write(
        portals.join("feather.portal"),
        format!(
            "[portal]\nDBusName={NAME}\nInterfaces=org.freedesktop.impl.portal.FileChooser;org.freedesktop.impl.portal.Access;org.freedesktop.impl.portal.Screenshot;org.freedesktop.impl.portal.ScreenCast;\nUseIn=FeatherDE;\n"
        ),
    )?;
    let conf = config.join("xdg-desktop-portal");
    std::fs::create_dir_all(&conf)?;
    let conf_file = conf.join("featherde-portals.conf");
    // Existing user preference wins. Upgrade untouched old default; never modify global portals.conf.
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&conf_file)
    {
        Ok(mut f) => f.write_all(DEFAULT_PORTALS_CONF)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(&conf_file).ok().as_deref() == Some(OLD_PORTALS_CONF) {
                std::fs::write(&conf_file, DEFAULT_PORTALS_CONF)?;
            }
        }
        Err(e) => return Err(e),
    }
    let services = data.join("dbus-1/services");
    std::fs::create_dir_all(&services)?;
    let exe = exe
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    if exe.contains(['\n', '\r']) {
        return Err(std::io::Error::other("invalid executable path"));
    }
    std::fs::write(
        services.join(format!("{NAME}.service")),
        format!("[D-BUS Service]\nName={NAME}\nExec=\"{exe}\" --file-portal\n"),
    )?;
    Ok(())
}
pub fn session_start(socket: String) {
    let _ = std::thread::Builder::new()
        .name("feather-portal-setup".into())
        .spawn(move || {
            let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"));
            if let Err(e) = std::env::current_exe().and_then(|exe| configure(&config, &data, &exe))
            {
                tracing::warn!("FileChooser setup: {e}");
            }
            let _ = std::process::Command::new("dbus-update-activation-environment")
                .args([
                    "--systemd",
                    &format!("WAYLAND_DISPLAY={socket}"),
                    "XDG_CURRENT_DESKTOP=FeatherDE",
                    "XDG_SESSION_TYPE=wayland",
                    "QT_QPA_PLATFORM=wayland;xcb",
                    "GDK_BACKEND=wayland,x11",
                    "GTK_USE_PORTAL=1",
                    "PLASMA_INTEGRATION_USE_PORTAL=1",
                ])
                .status();
        });
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portal_filters_choices_raw_paths_and_non_overwriting_batch_save() {
        let root = std::env::temp_dir().join(format!("feather-portal-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("photo.png"), b"keep").unwrap();
        let mut o = Options::new();
        o.insert(
            "filters".into(),
            value(vec![(
                "Images".to_string(),
                vec![(1u32, "image/png".to_string())],
            )]),
        );
        o.insert(
            "choices".into(),
            value(vec![(
                "preview".to_string(),
                "Preview".to_string(),
                Vec::<(String, String)>::new(),
                "true".to_string(),
            )]),
        );
        o.insert(
            "current_folder".into(),
            value([picker::path_bytes(&root), vec![0]].concat()),
        );
        let c = config("Pick".into(), &o, Mode::Open).unwrap();
        assert!(c.matches(&root.join("photo.png"), 0));
        assert!(!c.matches(&root.join("photo.pdf"), 0));
        let r = result(
            &c,
            picker::Selection {
                paths: vec![picker::path_bytes(&root.join("photo.png"))],
                choices: vec![("preview".into(), "true".into())],
                filter: 0,
            },
            None,
        )
        .unwrap();
        assert_eq!(
            get::<Vec<(String, String)>>(&r, "choices").unwrap()[0].1,
            "true"
        );
        assert!(get::<WireFilter>(&r, "current_filter").is_some());
        assert_eq!(
            file_uri(Path::new("/tmp/a b#%?")),
            "file:///tmp/a%20b%23%25%3F"
        );
        o.insert(
            "files".into(),
            value(vec![b"photo.png\0".to_vec(), b"photo.png\0".to_vec()]),
        );
        let r = result(
            &c,
            picker::Selection {
                paths: vec![picker::path_bytes(&root)],
                ..Default::default()
            },
            Some(save_names(&o).unwrap()),
        )
        .unwrap();
        let uris = get::<Vec<String>>(&r, "uris").unwrap();
        assert!(uris[0].ends_with("photo.png%20%281%29"));
        assert!(uris[1].ends_with("photo.png%20%282%29"));
        assert_eq!(std::fs::read(root.join("photo.png")).unwrap(), b"keep");
        o.insert("files".into(), value(vec![b"../bad\0".to_vec()]));
        assert!(save_names(&o).is_err());
        configure(
            &root.join("config"),
            &root.join("data"),
            Path::new("/tmp/Feather DE/feather-de"),
        )
        .unwrap();
        let conf = root.join("config/xdg-desktop-portal/featherde-portals.conf");
        std::fs::write(&conf, "user override").unwrap();
        configure(
            &root.join("config"),
            &root.join("data"),
            Path::new("/tmp/feather-de"),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(conf).unwrap(), "user override");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn screen_capture_portal_policy_and_per_app_rules() {
        let root = std::env::temp_dir().join(format!("feather-portal-privacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("feather-de")).unwrap();
        let conf = root.join("feather-de/settings.conf");

        // Default Ask policy records new app and grants access unless FEATHER_PORTAL_ACCESS_DEFAULT=deny
        assert!(evaluate_and_record_portal_access(
            &root,
            "com.obsproject.Studio.desktop",
            "OBS Studio"
        ));
        let saved = std::fs::read_to_string(&conf).unwrap();
        assert!(saved.contains("screen_capture_app=com.obsproject.Studio.desktop|OBS Studio|allow"));

        // Explicit deny rule overrides Allow policy
        std::fs::write(
            &conf,
            "screen_capture_policy=allow\nscreen_capture_cursor=false\nscreen_capture_app=com.obsproject.Studio|OBS Studio|deny\n",
        )
        .unwrap();
        assert!(!read_cursor_preference(&root));
        assert!(!evaluate_and_record_portal_access(
            &root,
            "com.obsproject.Studio.desktop",
            "OBS Studio"
        ));
        // Unknown app under Allow policy is granted and recorded
        assert!(evaluate_and_record_portal_access(
            &root,
            "org.mozilla.firefox",
            "Firefox"
        ));
        let saved = std::fs::read_to_string(&conf).unwrap();
        assert!(saved.contains("screen_capture_app=org.mozilla.firefox|Firefox|allow"));

        // Deny policy blocks unknown apps and records them as denied
        std::fs::write(
            &conf,
            "screen_capture_policy=deny\nscreen_capture_app=org.mozilla.firefox|Firefox|allow\n",
        )
        .unwrap();
        assert!(evaluate_and_record_portal_access(
            &root,
            "org.mozilla.firefox.desktop",
            "Firefox"
        ));
        assert!(!evaluate_and_record_portal_access(
            &root,
            "org.chromium.Chromium",
            "Chromium"
        ));
        let saved = std::fs::read_to_string(&conf).unwrap();
        assert!(saved.contains("screen_capture_app=org.chromium.Chromium|Chromium|deny"));

        std::fs::remove_dir_all(root).unwrap();
    }
}
