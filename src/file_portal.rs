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
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    futures_lite::future::block_on(async {
        let _connection = zbus::connection::Builder::session()?
            .name(NAME)?
            .serve_at(OBJECT, Chooser::default())?
            .build()
            .await?;
        std::future::pending::<()>().await;
        Ok(())
    })
}
/// User-scoped descriptor + DE-specific preference; do not replace other DEs or user overrides.
pub fn configure(config: &Path, data: &Path, exe: &Path) -> std::io::Result<()> {
    let portals = data.join("xdg-desktop-portal/portals");
    std::fs::create_dir_all(&portals)?;
    std::fs::write(portals.join("feather.portal"),format!("[portal]\nDBusName={NAME}\nInterfaces=org.freedesktop.impl.portal.FileChooser;\nUseIn=FeatherDE;\n"))?;
    let conf = config.join("xdg-desktop-portal");
    std::fs::create_dir_all(&conf)?;
    // Existing user preference wins. No global portals.conf is modified.
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(conf.join("featherde-portals.conf"))
    {
        Ok(mut f) => f.write_all(
            b"[preferred]\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=feather;gtk\n",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
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
}
