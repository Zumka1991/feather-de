//! Filesystem side. Listing, copying and trashing run on short-lived threads; the UI thread
//! only receives finished results. Nothing polls: inotify reports changes of the open folder.

use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub use feather_ui::file_kind::{kind_of, Kind};
pub use feather_ui::fileops::*;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub is_dir: bool,
    pub hidden: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Lowercase extension (shown on document icons)
    pub ext: String,
}

/// «Упорядочить»: what the list is sorted by; folders always stay on top, as in Finder
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortBy {
    #[default]
    Name,
    Kind,
    Modified,
    Size,
}
impl SortBy {
    pub const ALL: [SortBy; 4] = [SortBy::Name, SortBy::Kind, SortBy::Modified, SortBy::Size];
    /// Dates and sizes start from the newest and the largest
    pub fn descending_first(self) -> bool {
        matches!(self, Self::Modified | Self::Size)
    }
    /// A row under «Упорядочить» in the menu
    pub fn menu_label(self) -> &'static str {
        match self {
            Self::Name => "Имя",
            Self::Kind => "Тип",
            Self::Modified => "Дата изменения",
            Self::Size => "Размер",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sort {
    pub by: SortBy,
    pub descending: bool,
}

pub fn sort_entries(entries: &mut [Entry]) {
    sort_entries_by(entries, Sort::default());
}

/// Ties (same type, date or size; folders by size) fall back to the name
pub fn sort_entries_by(entries: &mut [Entry], sort: Sort) {
    use std::cmp::Ordering;
    entries.sort_by(|a, b| {
        let key = match sort.by {
            SortBy::Name => natural_cmp(&a.name, &b.name),
            SortBy::Kind => a.kind.label().cmp(b.kind.label()).then_with(|| a.ext.cmp(&b.ext)),
            SortBy::Modified => a.modified.cmp(&b.modified),
            SortBy::Size if a.is_dir && b.is_dir => Ordering::Equal,
            SortBy::Size => a.size.cmp(&b.size),
        };
        let key = if sort.descending { key.reverse() } else { key };
        b.is_dir.cmp(&a.is_dir).then(key).then_with(|| natural_cmp(&a.name, &b.name))
    });
}

/// Reads a folder (one `statx`-equivalent per entry); symlinks to folders count as folders
pub fn list(dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for item in std::fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        let path = item.path();
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => match item.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            },
        };
        let is_dir = meta.is_dir();
        let (kind, ext) = kind_of(&name, is_dir, meta.mode() & 0o111 != 0);
        out.push(Entry {
            hidden: name.starts_with('.'),
            name,
            path,
            kind,
            is_dir,
            size: if is_dir { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            ext,
        });
    }
    sort_entries(&mut out);
    Ok(out)
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["байт", "КБ", "МБ", "ГБ", "ТБ"];
    if bytes < 1000 {
        return format!("{bytes} {}", UNITS[0]);
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value < 10.0 { format!("{value:.1} {}", UNITS[unit]).replace('.', ",") } else { format!("{value:.0} {}", UNITS[unit]) }
}

pub fn human_date(time: Option<SystemTime>) -> String {
    let Some(time) = time else { return "—".into() };
    let dt: chrono::DateTime<chrono::Local> = time.into();
    let today = chrono::Local::now().date_naive();
    let date = dt.date_naive();
    if date == today {
        format!("Сегодня, {}", dt.format("%H:%M"))
    } else if today.pred_opt() == Some(date) {
        format!("Вчера, {}", dt.format("%H:%M"))
    } else {
        dt.format("%d.%m.%Y, %H:%M").to_string()
    }
}

/// A filesystem the sidebar lists: a mounted one opens, the others mount on click (udisks)
#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    pub name: String,
    /// /dev/sdc1
    pub device: PathBuf,
    pub mount: Option<PathBuf>,
    /// Mounted by udisks (/media, /run/media): «Извлечь» unmounts it without root
    pub ejectable: bool,
    /// USB or removable media: «Извлечь» also powers the drive off
    pub removable: bool,
}

/// Mount points of the user's disks; the rest (/, /boot, /home…) belong to the system
fn user_mount(path: &Path) -> bool {
    ["/media/", "/run/media/", "/mnt/"].iter().any(|base| path.starts_with(base))
}

fn ejectable_mount(path: &Path) -> bool {
    path.starts_with("/media/") || path.starts_with("/run/media/")
}

/// mountinfo escapes space, tab, newline and backslash as \ooo
fn unescape_mount(field: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let code = bytes.get(i + 1..i + 4).and_then(|d| std::str::from_utf8(d).ok()).and_then(|d| u8::from_str_radix(d, 8).ok());
        match code {
            Some(byte) if bytes[i] == b'\\' => {
                out.push(byte);
                i += 4;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// udev's \x20 escapes in ID_FS_LABEL_ENC
fn unescape_udev(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let code = bytes.get(i + 2..i + 4).and_then(|d| std::str::from_utf8(d).ok()).and_then(|d| u8::from_str_radix(d, 16).ok());
        match code {
            Some(byte) if bytes[i] == b'\\' && bytes[i + 1] == b'x' => {
                out.push(byte);
                i += 4;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Partition types that are never user data: EFI system, Windows recovery, BIOS boot
const SYSTEM_PARTITIONS: [&str; 5] = [
    "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
    "de94bba4-06d1-4d40-a16a-bfd50179d6ac",
    "21686148-6449-6e6f-744e-656564454649",
    "0xef",
    "0x27",
];

/// Disks and partitions with a filesystem, read from sysfs, the udev database and mountinfo:
/// only small files, no subprocess, so it is cheap enough for the event loop
pub fn volumes() -> Vec<Volume> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    volumes_in(Path::new("/sys/class/block"), Path::new("/run/udev/data"), &mountinfo)
}

fn volumes_in(sys: &Path, udev: &Path, mountinfo: &str) -> Vec<Volume> {
    // major:minor → first mount point
    let mut mounts: Vec<(&str, PathBuf)> = Vec::new();
    for line in mountinfo.lines() {
        let mut fields = line.split(' ');
        let (Some(dev), Some(point)) = (fields.nth(2), fields.nth(1)) else { continue };
        if !mounts.iter().any(|(d, _)| *d == dev) {
            mounts.push((dev, unescape_mount(point)));
        }
    }
    let Ok(dir) = std::fs::read_dir(sys) else { return Vec::new() };
    let mut names: Vec<String> = dir.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort_by(|a, b| device_order(a, b));
    let mut out = Vec::new();
    for name in names {
        if ["loop", "ram", "zram", "dm-", "md", "nbd"].iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let block = sys.join(&name);
        let Ok(dev) = std::fs::read_to_string(block.join("dev")) else { continue };
        let dev = dev.trim();
        let Ok(props) = std::fs::read_to_string(udev.join(format!("b{dev}"))) else { continue };
        let prop = |key: &str| props.lines().find_map(|l| l.strip_prefix("E:")?.strip_prefix(key)?.strip_prefix('=').map(str::to_owned));
        let part_type = prop("ID_PART_ENTRY_TYPE").unwrap_or_default().to_ascii_lowercase();
        if prop("ID_FS_USAGE").as_deref() != Some("filesystem")
            || prop("UDISKS_IGNORE").as_deref() == Some("1")
            || SYSTEM_PARTITIONS.contains(&part_type.as_str())
        {
            continue;
        }
        let mount = mounts.iter().find(|(d, _)| *d == dev).map(|(_, p)| p.clone());
        if mount.as_deref().is_some_and(|m| !user_mount(m)) {
            continue;
        }
        // A partition's own `removable` is always 0: the disk above it says
        let disk = if block.join("partition").exists() { block.join("..") } else { block.clone() };
        let removable = prop("ID_BUS").as_deref() == Some("usb")
            || std::fs::read_to_string(disk.join("removable")).is_ok_and(|r| r.trim() == "1");
        let label = prop("ID_FS_LABEL_ENC").map(|l| unescape_udev(&l)).filter(|l| !l.trim().is_empty());
        let size = std::fs::read_to_string(block.join("size")).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0) * 512;
        // udisks names the folder of a disk without a label by its UUID: «Диск 249 ГБ» reads
        // better; a folder the user named in /mnt stays
        let name_of = label
            .or_else(|| mount.as_ref().filter(|m| !ejectable_mount(m)).and_then(|m| m.file_name()).map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| format!("Диск {}", human_size(size)));
        out.push(Volume {
            name: name_of,
            device: PathBuf::from("/dev").join(&name),
            ejectable: mount.as_deref().is_some_and(ejectable_mount),
            mount,
            removable,
        });
    }
    out
}

fn device_order(a: &str, b: &str) -> std::cmp::Ordering {
    // sdb < sdb1 < sdb2 < sdb10; nvme0n1p2 < nvme0n1p10
    let split = |s: &str| {
        let digits = s.len() - s.bytes().rev().take_while(u8::is_ascii_digit).count();
        (s[..digits].to_owned(), s[digits..].parse::<u64>().unwrap_or(0))
    };
    split(a).cmp(&split(b))
}

fn udisksctl(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let out = std::process::Command::new("udisksctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "Для дисков нужен пакет udisks2".to_string() } else { e.to_string() })?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(if err.contains("NotAuthorized") {
        "нет прав".into()
    } else if err.contains("target is busy") || err.contains("DeviceBusy") {
        "диск используется — закройте открытые на нём файлы".into()
    } else if err.contains("AlreadyMounted") {
        "уже подключён".into()
    } else {
        // "Error mounting /dev/sdc1: GDBus.Error:org.freedesktop.UDisks2.Error.Failed: <why>"
        err.trim().rsplit(": ").next().unwrap_or("ошибка udisks").to_string()
    })
}

/// Mounts through udisks (the compositor's polkit agent asks for a password if needed) and
/// returns where; runs on a worker
pub fn mount(device: &Path) -> Result<PathBuf, String> {
    let found = || volumes().into_iter().find(|v| v.device == device).and_then(|v| v.mount);
    match udisksctl(&["mount".as_ref(), "-b".as_ref(), device.as_os_str()]) {
        Ok(()) => found().ok_or_else(|| "диск подключён, но не найден".into()),
        Err(e) => found().ok_or(e),
    }
}

/// Unmounts, then powers a removable drive off so it can be pulled out; runs on a worker
pub fn eject(device: &Path, removable: bool) -> Result<(), String> {
    udisksctl(&["unmount".as_ref(), "-b".as_ref(), device.as_os_str()])?;
    if removable {
        // A card reader cannot be powered off; the data is already safe
        let _ = udisksctl(&["power-off".as_ref(), "-b".as_ref(), device.as_os_str()]);
    }
    Ok(())
}

/// Sends the new list whenever it changes: a worker waits on udev's database (a disk came or
/// went) and on mountinfo (POLLPRI on every mount or unmount), never polls
pub fn watch_volumes(mut notify: impl FnMut(Vec<Volume>) -> bool + Send + 'static) {
    let _ = std::thread::Builder::new().name("feather-volumes".into()).stack_size(128 * 1024).spawn(move || {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return;
        }
        unsafe { libc::inotify_add_watch(fd, c"/run/udev/data".as_ptr(), libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE) };
        let mountinfo = std::fs::File::open("/proc/self/mountinfo").ok();
        let mut last = volumes();
        let mut buf = [0u8; 4096];
        loop {
            let mut fds = vec![libc::pollfd { fd, events: libc::POLLIN, revents: 0 }];
            if let Some(m) = &mountinfo {
                fds.push(libc::pollfd { fd: std::os::fd::AsRawFd::as_raw_fd(m), events: libc::POLLPRI, revents: 0 });
            }
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) } < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            // udev rewrites a device's entry several times while probing it: let it settle
            std::thread::sleep(std::time::Duration::from_millis(300));
            while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
            let now = volumes();
            if now != last {
                last = now.clone();
                if !notify(now) {
                    return;
                }
            }
        }
    });
}

/// Opens a file with its default application; children are reaped by our SIGCHLD handler
pub fn open_external(path: &Path) {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "svg" | "pdf") {
        if let Some(own) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("feather-preview")))
            .filter(|p| p.is_file())
        {
            if std::process::Command::new(own)
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
    // Packages open in Feather's installer, like a .pkg on macOS
    if ext == "deb" {
        if let Some(own) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("feather-installer")))
            .filter(|p| p.is_file())
        {
            if std::process::Command::new(own)
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
    if ext == "appimage" && run_appimage(path).is_ok() {
        return;
    }
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let (kind, _) = kind_of(name, false, false);
    if matches!(kind, Kind::Text | Kind::Code) {
        if let Some(own) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("feather-textedit")))
            .filter(|p| p.is_file())
        {
            if std::process::Command::new(own)
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
    let _ = std::process::Command::new("xdg-open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// An AppImage is a program: double-click runs it, as on other desktops. A fresh download has
/// no execute bit, so it gets one (for whoever may read it).
fn run_appimage(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    let mode = perms.mode();
    if mode & 0o100 == 0 {
        perms.set_mode(mode | (mode & 0o444) >> 2);
        std::fs::set_permissions(path, perms)?;
    }
    appimage_command(path, has_fuse2()).spawn().map(drop)
}

fn appimage_command(path: &Path, fuse2: bool) -> std::process::Command {
    let mut cmd = std::process::Command::new(path);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = path.parent() {
        cmd.current_dir(dir);
    }
    // Type 2 AppImages mount themselves with libfuse 2, which Ubuntu 24.04+ no longer ships
    // by default; without it the runtime unpacks to /tmp and runs from there (slower start)
    if !fuse2 {
        cmd.env("APPIMAGE_EXTRACT_AND_RUN", "1");
    }
    cmd
}

fn has_fuse2() -> bool {
    ["/usr/lib/x86_64-linux-gnu", "/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib64", "/usr/lib"]
        .iter()
        .any(|dir| Path::new(dir).join("libfuse.so.2").exists())
}

pub fn open_terminal(dir: &Path) {
    if let Some(helper)=std::env::current_exe().ok().and_then(|e|e.parent().map(|d|d.join("feather-de"))).filter(|p|p.is_file()){
        if std::process::Command::new(helper).arg("--open-terminal").arg(dir).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().is_ok(){return;}
    }
    // Feather's own terminal next to this program first
    if let Some(own) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("feather-terminal"))).filter(|p| p.is_file()) {
        if std::process::Command::new(own).arg("--cwd").arg(dir).stdin(std::process::Stdio::null()).spawn().is_ok() {
            return;
        }
    }
    for term in ["konsole", "foot", "alacritty", "kitty", "gnome-terminal", "xterm"] {
        let spawned = std::process::Command::new(term)
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return;
        }
    }
}

/// Archives Files extracts on double-click: everything bsdtar (libarchive) reads
/// Compound endings first: «x.tar.gz» unpacks as a tar, «x.txt.gz» as one file
const ARCHIVES: [&str; 28] = [
    ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tar.lz", ".tar.lzma", ".tgz", ".txz", ".tzst", ".tbz2", ".tbz", ".tlz", ".tar",
    ".zip", ".7z", ".rar", ".gz", ".xz", ".bz2", ".zst", ".lz", ".lzma", ".cab", ".arj", ".lzh", ".lha", ".cpio", ".ace",
];

pub fn is_archive(name: &str) -> bool {
    let lower = name.to_lowercase();
    ARCHIVES.iter().any(|ext| lower.ends_with(ext) && lower.len() > ext.len())
}

/// "Фото.tar.gz" → "Фото", "Фильм.part01.rar" → "Фильм"
pub fn archive_stem(name: &str) -> &str {
    let lower = name.to_lowercase();
    let Some(ext) = ARCHIVES.iter().find(|ext| lower.ends_with(*ext)) else { return name };
    let stem = &name[..name.len() - ext.len()];
    match rar_volume(name) {
        Some((start, _)) => &stem[..start - ".part".len()],
        None => stem,
    }
}

/// Byte range of N in a "….partN.rar" volume name
fn rar_volume(name: &str) -> Option<(usize, usize)> {
    let lower = name.to_ascii_lowercase();
    let end = lower.strip_suffix(".rar")?.len();
    let start = lower[..end].trim_end_matches(|c: char| c.is_ascii_digit()).len();
    (start < end && lower[..start].ends_with(".part") && start > ".part".len()).then_some((start, end))
}

/// Volumes unpack from the first one, whichever was double-clicked: "x.part3.rar" → "x.part1.rar"
/// (or part01…) when it is there
fn first_volume(archive: &Path) -> PathBuf {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some((start, end)) = rar_volume(&name) else { return archive.to_path_buf() };
    let first = archive.with_file_name(format!("{}{:0>width$}{}", &name[..start], 1, &name[end..], width = end - start));
    if first.is_file() { first } else { archive.to_path_buf() }
}

/// unar from the unar package (free, universe): unlike libarchive it reads RAR5 reliably,
/// volumes and encrypted archives. FEATHER_UNAR points tests at another copy
fn unar() -> std::process::Command {
    std::process::Command::new(std::env::var_os("FEATHER_UNAR").unwrap_or_else(|| "unar".into()))
}

const MISSING_PART: &str = "Архив повреждён или не хватает одной из его частей";
/// `extract` failed for want of a password: Files asks for one and tries again
pub const NEEDS_PASSWORD: &str = "Архив защищён паролем";
/// The password did not fit: asked again
pub const WRONG_PASSWORD: &str = "Неверный пароль";
const NO_UNAR_CRYPT: &str = "Архив защищён паролем. Распаковать его может только программа unar (пакет unar)";

/// unar's failure in Russian; `volumes` — the archive is one of several parts, `password` —
/// one was given
fn unar_error(out: &std::process::Output, volumes: bool, password: bool) -> String {
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if text.contains("requires a password") {
        NEEDS_PASSWORD.into()
    } else if text.contains("Couldn't recognize") {
        "Архив повреждён или это не архив".into()
    } else if password {
        // A wrong key reads as garbage: "Missing or wrong password", "Error on decrunching",
        // "Data is corrupted" — the same words as real damage, so the password is blamed
        WRONG_PASSWORD.into()
    } else if volumes {
        MISSING_PART.into()
    } else {
        "Архив повреждён".into()
    }
}

/// bsdtar from libarchive-tools; FEATHER_BSDTAR points tests at another copy
fn bsdtar() -> std::process::Command {
    std::process::Command::new(std::env::var_os("FEATHER_BSDTAR").unwrap_or_else(|| "bsdtar".into()))
}

/// Runs an unpacker that may want a password with no way to ask for one: bsdtar without
/// --passphrase (or with a wrong one) reads it from the terminal, else from stdin, and an
/// empty stdin makes it ask forever. No controlling terminal (setsid) and a write-only stdin
/// turn that into "Couldn't read passphrase".
fn unattended(mut cmd: std::process::Command) -> std::io::Result<std::process::Output> {
    use std::os::unix::process::CommandExt;
    let stdin = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
    // SAFETY: setsid is async-signal-safe and touches nothing of ours
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.stdin(stdin).output()
}

/// A zip whose names are neither ASCII nor marked UTF-8 nor valid UTF-8: written by Windows
/// in the OEM code page. Reads only the central directory at the end of the file
fn zip_legacy_names(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let Ok(len) = file.metadata().map(|m| m.len()) else { return false };
    let tail_len = len.min(65_557);
    let mut tail = vec![0; tail_len as usize];
    if file.seek(SeekFrom::Start(len - tail_len)).is_err() || file.read_exact(&mut tail).is_err() {
        return false;
    }
    let u16_at = |b: &[u8], i: usize| b.get(i..i + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]) as usize);
    let u32_at = |b: &[u8], i: usize| b.get(i..i + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as u64);
    let Some(end) = (0..tail.len().saturating_sub(21)).rev().find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]) else { return false };
    let (size, offset) = (u32_at(&tail, end + 12), u32_at(&tail, end + 16));
    // Zip64 and absurd directories are left to bsdtar as they are
    if size > 16 << 20 || offset == 0xFFFF_FFFF || offset + size > len {
        return false;
    }
    let mut dir = vec![0; size as usize];
    if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut dir).is_err() {
        return false;
    }
    let mut i = 0;
    while dir.get(i..i + 4) == Some(&[0x50, 0x4b, 0x01, 0x02]) {
        let (flags, name_len) = (u16_at(&dir, i + 8), u16_at(&dir, i + 28));
        let Some(name) = dir.get(i + 46..i + 46 + name_len) else { return false };
        if flags & 0x800 == 0 && !name.is_ascii() && std::str::from_utf8(name).is_err() {
            return true;
        }
        i += 46 + name_len + u16_at(&dir, i + 30) + u16_at(&dir, i + 32);
    }
    false
}

/// libarchive reads the archive but cannot decrypt it (RAR, 7z): unar can
fn needs_unar(out: &std::process::Output) -> bool {
    let text = String::from_utf8_lossy(&out.stderr);
    text.contains("Encryption is not supported") || text.contains("encrypted, but currently not supported")
}

/// bsdtar's own words about a failure
fn tar_error(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    if text.contains("Couldn't read passphrase") {
        return NEEDS_PASSWORD.into();
    }
    if text.contains("Incorrect passphrase") {
        return WRONG_PASSWORD.into();
    }
    if needs_unar(out) {
        return NO_UNAR_CRYPT.into();
    }
    text.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("bsdtar завершился с ошибкой").trim_start_matches("bsdtar: ").to_string()
}

/// "name.ext", or "name 2.ext"… if taken
fn unique_file(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    std::iter::once(dir.join(format!("{stem}{ext}")))
        .chain((2..).map(|n| dir.join(format!("{stem} {n}{ext}"))))
        .find(|p| !p.exists())
        .unwrap()
}

/// «Сжать»: items of one folder into a zip next to them («имя.zip», several — «Архив.zip»).
/// The zip is written under a hidden name and appears only when complete.
pub fn compress(paths: &[PathBuf]) -> Result<PathBuf, String> {
    let dir = paths.first().and_then(|p| p.parent()).ok_or("Нечего сжимать")?;
    let names: Vec<&std::ffi::OsStr> = paths.iter().filter_map(|p| p.file_name()).collect();
    let stem = match names.as_slice() {
        [one] => one.to_string_lossy().into_owned(),
        _ => "Архив".to_string(),
    };
    let target = unique_file(dir, &stem, ".zip");
    let part = dir.join(format!(".{}.feather-part", target.file_name().unwrap().to_string_lossy()));
    let out = bsdtar().args(["--format", "zip", "-cf"]).arg(&part).arg("-C").arg(dir).arg("--").args(&names).output();
    match out {
        Ok(out) if out.status.success() => std::fs::rename(&part, &target).map(|_| target).map_err(|e| e.to_string()),
        Ok(out) => {
            let _ = std::fs::remove_file(&part);
            Err(tar_error(&out))
        }
        Err(_) => Err("Нет программы bsdtar (пакет libarchive-tools)".into()),
    }
}

/// Double-click on an archive: unpacked next to it, like macOS. One folder (or file) inside
/// appears as is; several items go into a folder named after the archive. Everything is
/// unpacked into a hidden folder first, so a failure leaves nothing half-extracted. bsdtar
/// and unar refuse ".." and strip leading "/" from member names. RAR goes to unar; without
/// it bsdtar tries (old RAR mostly works, RAR5 and volumes often do not). `password` goes to
/// unar as -p and to bsdtar as --passphrase (zip); an encrypted 7z, which libarchive cannot
/// decrypt, goes to unar too. A missing or wrong password comes back as `NEEDS_PASSWORD` or
/// `WRONG_PASSWORD`.
pub fn extract(archive: &Path, password: Option<&str>) -> Result<PathBuf, String> {
    let dir = archive.parent().ok_or("Нет папки архива")?;
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = archive_stem(&name).to_string();
    let rar = name.to_lowercase().ends_with(".rar");
    let volumes = rar_volume(&name).is_some();
    let archive = &first_volume(archive);
    let tmp = dir.join(format!(".{stem}.feather-extract-{}", std::process::id()));
    std::fs::create_dir(&tmp).map_err(|e| e.to_string())?;
    let fail = |e: String| {
        let _ = std::fs::remove_dir_all(&tmp);
        Err(e)
    };
    // -D: the folder for several items is ours; -nr: an inner archive stays an archive
    let run_unar = || {
        let mut cmd = unar();
        cmd.args(["-q", "-nr", "-D", "-k", "skip", "-o"]).arg(&tmp);
        if let Some(password) = password {
            cmd.arg("-p").arg(password);
        }
        cmd.arg(archive).stdin(std::process::Stdio::null()).output().ok()
    };
    let unpacked = if rar { run_unar() } else { None };
    match unpacked {
        Some(out) if out.status.success() => {}
        Some(out) => return fail(unar_error(&out, volumes, password.is_some())),
        None => {
            let mut tar = bsdtar();
            // Windows writes Russian names in zip as CP866 without the UTF-8 flag
            if name.to_lowercase().ends_with(".zip") && zip_legacy_names(archive) {
                tar.args(["--options", "zip:hdrcharset=CP866"]);
            }
            tar.arg("-xf").arg(archive).arg("-C").arg(&tmp);
            if let Some(password) = password {
                tar.arg(&*zeroize::Zeroizing::new(format!("--passphrase={password}")));
            }
            let tar = match unattended(tar) {
                Ok(out) if out.status.success() => None,
                Ok(out) => Some(Some(out)),
                Err(_) => Some(None),
            };
            if let Some(tar) = tar {
                // A password question is bsdtar's to ask (zip); the rest goes to unar
                let tar_says = tar.as_ref().map(tar_error);
                if let Some(e) = tar_says.as_ref().filter(|e| *e == NEEDS_PASSWORD || *e == WRONG_PASSWORD) {
                    return fail(e.clone());
                }
                let encrypted = tar.as_ref().is_some_and(needs_unar);
                // bsdtar cannot read it (a lone .gz, ARJ, deflate64, an encrypted 7z…): unar may.
                // Whatever bsdtar managed goes; unar starts clean
                let _ = std::fs::remove_dir_all(&tmp);
                if let Err(e) = std::fs::create_dir(&tmp) {
                    return fail(e.to_string());
                }
                match (run_unar(), tar_says) {
                    (Some(out), _) if out.status.success() => {}
                    (Some(out), _) if encrypted => return fail(unar_error(&out, volumes, password.is_some())),
                    (Some(out), None) => return fail(unar_error(&out, volumes, password.is_some())),
                    (Some(_), Some(e)) => return fail(e),
                    (None, _) if encrypted => return fail(NO_UNAR_CRYPT.into()),
                    (None, Some(e)) if rar => return fail(format!("{e}. Для RAR нужна программа unar (пакет unar)")),
                    (None, Some(e)) => return fail(e),
                    (None, None) => return fail("Нет программ bsdtar и unar (пакеты libarchive-tools и unar)".into()),
                }
            }
        }
    }
    let items: Vec<PathBuf> = match std::fs::read_dir(&tmp) {
        Ok(d) => d.flatten().map(|e| e.path()).collect(),
        Err(e) => return fail(e.to_string()),
    };
    let result = match items.as_slice() {
        // unar skips what it cannot read (a file whose next part is missing) and still succeeds
        [] if volumes => return fail(MISSING_PART.into()),
        [] if rar => return fail("Архив повреждён".into()),
        [] => return fail("Архив пуст".into()),
        [only] => {
            let item = only.file_name().unwrap().to_string_lossy().into_owned();
            let target = if only.is_dir() {
                unique_name(dir, &item)
            } else {
                let (stem, ext) = item.rfind('.').filter(|&i| i > 0).map_or((item.as_str(), ""), |i| item.split_at(i));
                unique_file(dir, stem, ext)
            };
            std::fs::rename(only, &target).and_then(|_| std::fs::remove_dir(&tmp)).map(|_| target)
        }
        _ => {
            let target = unique_name(dir, &stem);
            std::fs::rename(&tmp, &target).map(|_| target)
        }
    };
    result.or_else(|e| fail(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A USB stick not mounted yet, a data disk in fstab, the system disk, Windows' recovery
    /// partition and a disk without a filesystem
    #[test]
    fn volumes_from_sysfs_udev_and_mountinfo() {
        let root = std::env::temp_dir().join(format!("feather-volumes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (sys, udev) = (root.join("block"), root.join("udev"));
        std::fs::create_dir_all(&udev).unwrap();
        let disk = |name: &str, parent: Option<&str>, dev: &str, sectors: u64, props: &str| {
            let dir = match parent {
                Some(p) => sys.join(p).join(name),
                None => sys.join(name),
            };
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("dev"), format!("{dev}\n")).unwrap();
            std::fs::write(dir.join("size"), format!("{sectors}\n")).unwrap();
            if parent.is_some() {
                std::fs::write(dir.join("partition"), "1").unwrap();
                // sysfs links partitions under their disk; the flat list holds a link too
                std::os::unix::fs::symlink(&dir, sys.join(name)).unwrap();
            } else {
                std::fs::write(dir.join("removable"), if name == "sdc" { "1\n" } else { "0\n" }).unwrap();
            }
            std::fs::write(udev.join(format!("b{dev}")), props).unwrap();
        };
        let fs = "E:ID_FS_USAGE=filesystem\n";
        disk("sda", None, "8:0", 0, "E:ID_PART_TABLE_TYPE=gpt\n");
        disk("sda2", Some("sda"), "8:2", 2_000_000, &format!("{fs}E:ID_FS_TYPE=ntfs\n"));
        disk("sda3", Some("sda"), "8:3", 2_000_000, &format!("{fs}E:UDISKS_IGNORE=1\n"));
        disk("sda4", Some("sda"), "8:4", 2_000, &format!("{fs}E:ID_PART_ENTRY_TYPE=C12A7328-F81F-11D2-BA4B-00A0C93EC93B\n"));
        disk("sdb", None, "8:16", 0, "");
        disk("sdb1", Some("sdb"), "8:17", 2_000, &format!("{fs}E:ID_FS_LABEL_ENC=DATA\n"));
        disk("sdb2", Some("sdb"), "8:18", 2_000, &format!("{fs}E:ID_FS_LABEL_ENC=Мои\\x20фото\n"));
        disk("sdc", None, "8:32", 0, "");
        disk("sdc1", Some("sdc"), "8:33", 60_000_000, &format!("{fs}E:ID_FS_LABEL_ENC=WINRE\nE:ID_BUS=usb\n"));
        disk("loop0", None, "7:0", 100, fs);
        let mountinfo = "44 1 252:0 / / rw - ext4 /dev/mapper/vg-root rw\n\
                         60 44 8:17 / /mnt/data rw - ext4 /dev/sdb1 rw\n\
                         61 44 8:18 / /media/u/Мои\\040фото rw - ext4 /dev/sdb2 rw\n\
                         62 44 8:2 / /boot/win rw - ntfs /dev/sda2 rw\n";
        let got: Vec<_> = volumes_in(&sys, &udev, mountinfo).into_iter().map(|v| (v.name, v.mount, v.ejectable, v.removable)).collect();
        assert_eq!(
            got,
            [
                ("DATA".into(), Some(PathBuf::from("/mnt/data")), false, false),
                ("Мои фото".into(), Some(PathBuf::from("/media/u/Мои фото")), true, false),
                ("WINRE".into(), None, false, true),
            ]
        );
        let by_uuid = "62 44 8:2 / /media/u/F6C08D59C08D214B rw - ntfs /dev/sda2 rw\n";
        let disk = volumes_in(&sys, &udev, by_uuid).into_iter().find(|v| v.device == Path::new("/dev/sda2")).unwrap();
        assert_eq!((disk.name.as_str(), disk.ejectable), ("Диск 1,0 ГБ", true));
        let unmounted = "44 1 252:0 / / rw - ext4 /dev/mapper/vg-root rw\n";
        let names: Vec<_> = volumes_in(&sys, &udev, unmounted).into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["Диск 1,0 ГБ", "DATA", "Мои фото", "WINRE"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
    use std::cmp::Ordering;

    #[test]
    fn appimages_run_without_fuse2() {
        let path = Path::new("/home/u/Загрузки/App.AppImage");
        let env = |fuse2| {
            appimage_command(path, fuse2)
                .get_envs()
                .find(|(k, _)| *k == "APPIMAGE_EXTRACT_AND_RUN")
                .and_then(|(_, v)| v.map(|v| v.to_owned()))
        };
        assert_eq!(env(false).as_deref(), Some(std::ffi::OsStr::new("1")));
        assert_eq!(env(true), None);
        assert_eq!(appimage_command(path, true).get_current_dir(), Some(Path::new("/home/u/Загрузки")));
    }

    #[test]
    fn downloaded_appimage_gets_execute_bit_and_runs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("feather-appimage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let app = dir.join("Тест.AppImage");
        std::fs::write(&app, "#!/bin/sh\necho \"$APPIMAGE_EXTRACT_AND_RUN\" > ran\n").unwrap();
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o644)).unwrap();
        open_external(&app);
        let ran = dir.join("ran");
        for _ in 0..100 {
            if std::fs::read_to_string(&ran).is_ok_and(|s| s.ends_with('\n')) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mode = std::fs::metadata(&app).unwrap().permissions().mode() & 0o777;
        let env = std::fs::read_to_string(&ran);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode, 0o755);
        assert_eq!(env.unwrap().trim(), if has_fuse2() { "" } else { "1" });
    }

    #[test]
    fn natural_order_and_folders_first() {
        let mut names = vec!["file10.txt", "File2.txt", "file1.txt", "a", "Б", "б2"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["a", "file1.txt", "File2.txt", "file10.txt", "Б", "б2"]);
        assert_eq!(natural_cmp("x01", "x1"), Ordering::Greater);
    }

    #[test]
    fn kinds_sizes_and_trash_paths() {
        assert_eq!(kind_of("Photo.JPG", false, false), (Kind::Image, "jpg".into()));
        assert_eq!(kind_of("run", false, true).0, Kind::Executable);
        assert_eq!(kind_of("dir.png", true, false).0, Kind::Folder);
        assert_eq!(human_size(999), "999 байт");
        assert_eq!(human_size(1_500), "1,5 КБ");
        assert_eq!(human_size(25_000_000), "25 МБ");
        assert_eq!(percent_encode(Path::new("/home/a b/ф")), "/home/a%20b/%D1%84");
    }

    #[test]
    fn paste_copies_with_copy_suffix_and_moves() {
        let root = std::env::temp_dir().join(format!("feather-files-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("x.txt"), "1").unwrap();
        std::fs::write(a.join("sub/y"), "2").unwrap();
        assert_eq!(paste(&[a.join("x.txt"), a.join("sub")], &a, false), 0);
        assert!(a.join("x копия.txt").exists() && a.join("sub копия/y").exists());
        assert_eq!(paste(&[a.join("x.txt")], &b, true), 0);
        assert!(b.join("x.txt").exists() && !a.join("x.txt").exists());
        assert_eq!(paste(&[a.clone()], &a.join("sub"), false), 1);
        let listed: Vec<_> = list(&a).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(listed, ["sub", "sub копия", "x копия.txt"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// FEATHER_BSDTAR=/path/to/bsdtar when libarchive-tools is not installed
    #[test]
    fn compress_and_extract_like_macos() {
        if std::process::Command::new(std::env::var_os("FEATHER_BSDTAR").unwrap_or_else(|| "bsdtar".into())).arg("--version").output().is_err() {
            eprintln!("нет bsdtar — тест пропущен");
            return;
        }
        let root = std::env::temp_dir().join(format!("feather-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Фото")).unwrap();
        std::fs::write(root.join("Фото/кот.txt"), "мяу").unwrap();
        std::fs::write(root.join("заметка.txt"), "x").unwrap();
        assert!(is_archive("Фото.tar.gz") && is_archive("a.ZIP") && !is_archive(".zip") && !is_archive("a.deb"));
        assert_eq!(archive_stem("Фото.Tar.Gz"), "Фото");
        // One folder → «Фото.zip»; several items → «Архив.zip», then «Архив 2.zip»
        let one = compress(&[root.join("Фото")]).unwrap();
        assert_eq!(one, root.join("Фото.zip"));
        let many = compress(&[root.join("Фото"), root.join("заметка.txt")]).unwrap();
        assert_eq!(many, root.join("Архив.zip"));
        assert_eq!(compress(&[root.join("заметка.txt")]).unwrap(), root.join("заметка.txt.zip"));
        assert_eq!(compress(&[root.join("Фото"), root.join("заметка.txt")]).unwrap(), root.join("Архив 2.zip"));
        // The single folder comes out as itself (taken name → «Фото 2»), several items in a folder
        assert_eq!(extract(&one, None).unwrap(), root.join("Фото 2"));
        assert_eq!(std::fs::read_to_string(root.join("Фото 2/кот.txt")).unwrap(), "мяу");
        let folder = extract(&many, None).unwrap();
        assert_eq!(folder, root.join("Архив"));
        assert!(folder.join("Фото/кот.txt").is_file() && folder.join("заметка.txt").is_file());
        // A broken archive leaves nothing behind
        std::fs::write(root.join("битый.zip"), "not a zip").unwrap();
        assert!(extract(&root.join("битый.zip"), None).is_err());
        let leftovers = std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with('.')).count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn crc32(data: &[u8]) -> u32 {
        !data.iter().fold(!0u32, |crc, &b| (0..8).fold(crc ^ b as u32, |c, _| if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 }))
    }

    fn vint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    /// One RAR5 block: CRC32, size, then `body` (type, flags, fields) and the data
    fn rar_block(body: &[u8], data: &[u8]) -> Vec<u8> {
        let mut sized = Vec::new();
        vint(&mut sized, body.len() as u64);
        sized.extend_from_slice(body);
        let mut block = crc32(&sized).to_le_bytes().to_vec();
        block.extend(sized);
        block.extend_from_slice(data);
        block
    }

    /// Part of a file in a RAR volume: its bytes here, the whole size, split flags (0x08 —
    /// continues from the previous volume, 0x10 — into the next) and the whole file's CRC32
    /// (only in its last part)
    type RarPart<'a> = (&'a [u8], u64, u64, Option<u32>);

    /// A stored (uncompressed) RAR5 archive, as WinRAR writes it with -m0. `volume`: its number
    /// and whether it is the last; items without a part are folders
    fn rar5(volume: Option<(u64, bool)>, items: &[(&str, Option<RarPart>)]) -> Vec<u8> {
        let mut out = b"Rar!\x1a\x07\x01\x00".to_vec();
        let mut main = vec![1, 0];
        match volume {
            None => main.push(0),
            Some((0, _)) => main.push(1),
            Some((n, _)) => {
                main.push(3);
                vint(&mut main, n);
            }
        }
        out.extend(rar_block(&main, &[]));
        for &(name, part) in items {
            let mut head = vec![2];
            match part {
                Some((data, size, split, crc)) => {
                    vint(&mut head, 2 | split);
                    vint(&mut head, data.len() as u64);
                    vint(&mut head, if crc.is_some() { 4 } else { 0 });
                    vint(&mut head, size);
                    vint(&mut head, 0o100644);
                    head.extend(crc.map(u32::to_le_bytes).unwrap_or_default());
                }
                None => {
                    // No data, a folder of size 0
                    head.extend([0, 1, 0]);
                    vint(&mut head, 0o40755);
                }
            }
            // Stored, Unix, the UTF-8 name
            head.extend([0, 1]);
            vint(&mut head, name.len() as u64);
            head.extend_from_slice(name.as_bytes());
            out.extend(rar_block(&head, part.map_or(&[][..], |p| p.0)));
        }
        let more = volume.is_some_and(|(_, last)| !last);
        out.extend(rar_block(&[5, 0, more as u8], &[]));
        out
    }

    /// FEATHER_UNAR=/path/to/unar when the unar package is not installed. Without unar, plain
    /// RAR goes through bsdtar, volumes cannot unpack
    /// A stored zip whose one name is written as raw bytes, with or without the UTF-8 flag
    fn stored_zip(name: &[u8], data: &[u8], utf8: bool) -> Vec<u8> {
        let flags: u16 = if utf8 { 0x800 } else { 0 };
        let (crc, len, name_len) = (crc32(data), data.len() as u32, name.len() as u16);
        let mut out = Vec::new();
        out.extend([0x50, 0x4b, 0x03, 0x04, 20, 0]);
        out.extend(flags.to_le_bytes());
        out.extend([0u8; 6]);
        out.extend(crc.to_le_bytes());
        out.extend(len.to_le_bytes());
        out.extend(len.to_le_bytes());
        out.extend(name_len.to_le_bytes());
        out.extend([0, 0]);
        out.extend(name);
        out.extend(data);
        let dir = out.len() as u32;
        out.extend([0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0]);
        out.extend(flags.to_le_bytes());
        out.extend([0u8; 6]);
        out.extend(crc.to_le_bytes());
        out.extend(len.to_le_bytes());
        out.extend(len.to_le_bytes());
        out.extend(name_len.to_le_bytes());
        out.extend([0u8; 12]);
        out.extend(0u32.to_le_bytes());
        out.extend(name);
        let dir_len = out.len() as u32 - dir;
        out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 1, 0, 1, 0]);
        out.extend(dir_len.to_le_bytes());
        out.extend(dir.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    /// Windows zips name files in CP866; UTF-8 ones (flagged or not) stay as they are; a lone
    /// .gz, which bsdtar cannot read, goes to unar
    #[test]
    fn windows_zip_names_and_single_compressed_files() {
        let root = std::env::temp_dir().join(format!("feather-zip-cp866-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // CP866 for «Отчёт.txt»
        let cp866: Vec<u8> = [0x8e, 0xe2, 0xe7, 0xf1, 0xe2].into_iter().chain(*b".txt").collect();
        let windows = root.join("windows.zip");
        std::fs::write(&windows, stored_zip(&cp866, b"hi", false)).unwrap();
        let linux = root.join("linux.zip");
        std::fs::write(&linux, stored_zip("Отчёт.txt".as_bytes(), b"hi", false)).unwrap();
        let flagged = root.join("flagged.zip");
        std::fs::write(&flagged, stored_zip("Отчёт.txt".as_bytes(), b"hi", true)).unwrap();
        assert!(zip_legacy_names(&windows));
        assert!(!zip_legacy_names(&linux) && !zip_legacy_names(&flagged));
        for zip in [&windows, &linux, &flagged] {
            let out = extract(zip, None).unwrap();
            assert_eq!(out.file_name().unwrap(), "Отчёт.txt", "{zip:?}");
            std::fs::remove_file(out).unwrap();
        }
        let gz = root.join("заметка.txt.gz");
        let mut gzip = std::process::Command::new("gzip").arg("-c").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        std::io::Write::write_all(gzip.stdin.as_mut().unwrap(), b"note").unwrap();
        std::fs::write(&gz, gzip.wait_with_output().unwrap().stdout).unwrap();
        assert!(is_archive("заметка.txt.gz") && archive_stem("x.tar.gz") == "x" && archive_stem("a.txt.xz") == "a.txt");
        let out = extract(&gz, None).unwrap();
        assert_eq!(out, root.join("заметка.txt"));
        assert_eq!(std::fs::read(out).unwrap(), b"note");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sort_by_name_kind_date_and_size_keeps_folders_on_top() {
        let at = |s: u64| Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s));
        let e = |name: &str, dir: bool, size: u64, modified: u64, kind: Kind| Entry {
            name: name.into(),
            path: PathBuf::from(name),
            kind,
            is_dir: dir,
            hidden: false,
            size,
            modified: at(modified),
            ext: name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default(),
        };
        let mut list = vec![
            e("b.png", false, 300, 30, Kind::Image),
            e("Папка", true, 0, 5, Kind::Folder),
            e("a.txt", false, 10, 50, Kind::Text),
            e("c.png", false, 20, 40, Kind::Image),
            e("Архив", true, 0, 60, Kind::Folder),
        ];
        let names = |l: &[Entry]| l.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        let by = |by: SortBy, descending: bool| Sort { by, descending };
        sort_entries_by(&mut list, by(SortBy::Name, false));
        assert_eq!(names(&list), ["Архив", "Папка", "a.txt", "b.png", "c.png"]);
        sort_entries_by(&mut list, by(SortBy::Name, true));
        assert_eq!(names(&list), ["Папка", "Архив", "c.png", "b.png", "a.txt"]);
        sort_entries_by(&mut list, by(SortBy::Modified, true));
        assert_eq!(names(&list), ["Архив", "Папка", "a.txt", "c.png", "b.png"]);
        sort_entries_by(&mut list, by(SortBy::Size, true));
        assert_eq!(names(&list), ["Архив", "Папка", "b.png", "c.png", "a.txt"]);
        sort_entries_by(&mut list, by(SortBy::Kind, false));
        // «Изображение» before «Текст»; the same type by name
        assert_eq!(names(&list), ["Архив", "Папка", "b.png", "c.png", "a.txt"]);
        assert!(SortBy::Modified.descending_first() && !SortBy::Name.descending_first());
    }

    #[test]
    fn rar_unpacks_with_volumes() {
        let has_unar = unar().stdin(std::process::Stdio::null()).output().is_ok();
        if !has_unar && bsdtar().output().is_err() {
            eprintln!("нет ни unar, ни bsdtar — тест пропущен");
            return;
        }
        let root = std::env::temp_dir().join(format!("feather-rar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(is_archive("Фильм.part2.rar") && archive_stem("Фильм.Part01.RAR") == "Фильм" && archive_stem("a.part.rar") == "a.part");
        assert_eq!(archive_stem(".part1.rar"), ".part1");
        // unar's words for an encrypted archive (WinRAR's encryption is not generated here)
        let said = |text: &str| {
            use std::os::unix::process::ExitStatusExt;
            std::process::Output { status: std::process::ExitStatus::from_raw(2 << 8), stdout: text.into(), stderr: vec![] }
        };
        let locked = said("копилка.rar: This archive requires a password to unpack. Use the -p option to provide one.\n");
        assert_eq!(unar_error(&locked, false, false), "Архив защищён паролем");
        assert_eq!(unar_error(&said("x.rar: Couldn't recognize the archive format.\n"), false, false), "Архив повреждён или это не архив");
        let wrong = said("  кадр.txt  (13 B)... Failed! (Missing or wrong password)\n");
        assert_eq!(unar_error(&wrong, false, true), WRONG_PASSWORD);

        // Several items → a folder named after the archive
        let cat = "мяу".repeat(100);
        let cat = cat.as_bytes();
        let note = b"x";
        let one = rar5(None, &[
            ("Фото", None),
            ("Фото/кот.txt", Some((cat, cat.len() as u64, 0, Some(crc32(cat))))),
            ("заметка.txt", Some((note, 1, 0, Some(crc32(note))))),
        ]);
        std::fs::write(root.join("Снимки.rar"), one).unwrap();
        let folder = extract(&root.join("Снимки.rar"), None).unwrap();
        assert_eq!(folder, root.join("Снимки"));
        assert_eq!(std::fs::read(folder.join("Фото/кот.txt")).unwrap(), cat);
        assert_eq!(std::fs::read(folder.join("заметка.txt")).unwrap(), note);

        // A broken one says so and leaves nothing behind
        std::fs::write(root.join("битый.rar"), b"Rar!\x1a\x07\x01\x00garbage").unwrap();
        let broken = extract(&root.join("битый.rar"), None).unwrap_err();
        assert!(!has_unar || broken == "Архив повреждён", "{broken}");

        if has_unar {
            // A file split over two volumes; double-click on the second still starts from the first
            let film = b"0123456789".repeat(50);
            let (a, b) = film.split_at(200);
            let size = film.len() as u64;
            let crc = Some(crc32(&film));
            std::fs::write(root.join("Фильм.part1.rar"), rar5(Some((0, false)), &[("кадр.txt", Some((a, size, 0x10, None)))])).unwrap();
            std::fs::write(root.join("Фильм.part2.rar"), rar5(Some((1, true)), &[("кадр.txt", Some((b, size, 0x08, crc)))])).unwrap();
            assert_eq!(extract(&root.join("Фильм.part2.rar"), None).unwrap(), root.join("кадр.txt"));
            assert_eq!(std::fs::read(root.join("кадр.txt")).unwrap(), film);
            // Without the second part: a clear message
            std::fs::remove_file(root.join("Фильм.part2.rar")).unwrap();
            assert_eq!(extract(&root.join("Фильм.part1.rar"), None).unwrap_err(), "Архив повреждён или не хватает одной из его частей");
        }
        let leftovers = std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with('.')).count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A zip with a password goes through bsdtar --passphrase; an encrypted 7z (libarchive
    /// cannot decrypt it) through unar -p, when unar and 7z are there
    #[test]
    fn encrypted_archives_ask_for_a_password() {
        if bsdtar().output().is_err() {
            eprintln!("нет bsdtar — тест пропущен");
            return;
        }
        let root = std::env::temp_dir().join(format!("feather-crypt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/секрет.txt"), "привет").unwrap();
        let zip = root.join("копилка.zip");
        let made = bsdtar().args(["--format", "zip", "--options", "zip:encryption=zipcrypt", "--passphrase", "-тайна"])
            .arg("-cf").arg(&zip).arg("-C").arg(root.join("src")).arg("секрет.txt").status().unwrap();
        assert!(made.success());
        // No password: asked for one at once, not a hang on bsdtar's own prompt
        assert_eq!(extract(&zip, None).unwrap_err(), NEEDS_PASSWORD);
        assert_eq!(extract(&zip, Some("неверно")).unwrap_err(), WRONG_PASSWORD);
        assert_eq!(extract(&zip, Some("-тайна")).unwrap(), root.join("секрет.txt"));
        assert_eq!(std::fs::read_to_string(root.join("секрет.txt")).unwrap(), "привет");

        let seven = root.join("сейф.7z");
        let has_7z = std::process::Command::new("7z").arg("a").arg("-bd").arg("-pтайна").arg("-mhe=on").arg(&seven)
            .arg(root.join("src/секрет.txt")).output().is_ok_and(|o| o.status.success());
        if has_7z && unar().stdin(std::process::Stdio::null()).output().is_ok() {
            assert_eq!(extract(&seven, None).unwrap_err(), NEEDS_PASSWORD);
            assert_eq!(extract(&seven, Some("неверно")).unwrap_err(), WRONG_PASSWORD);
            assert_eq!(extract(&seven, Some("тайна")).unwrap(), root.join("секрет 2.txt"));
        } else if has_7z {
            // Without unar it cannot be done, and asking for a password would never end
            assert_eq!(extract(&seven, Some("тайна")).unwrap_err(), NO_UNAR_CRYPT);
        }
        let leftovers = std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with('.')).count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
