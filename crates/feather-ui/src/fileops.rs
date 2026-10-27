//! File operations shared by Files and the desktop: XDG folders, freedesktop trash,
//! natural name order and an inotify watcher for one folder (no polling).

use std::{
    cmp::Ordering,
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

/// Finder order: names compared case-insensitively with digit runs as numbers
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// XDG user folders (Desktop, Documents, …) from user-dirs.dirs, with English fallbacks
pub fn user_dir(key: &str, fallback: &str) -> PathBuf {
    let home = home();
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.trim().strip_prefix(&format!("XDG_{key}_DIR="))?.trim().trim_matches('"');
                Some(match value.strip_prefix("$HOME") {
                    Some(rest) => home.join(rest.trim_start_matches('/')),
                    None => PathBuf::from(value),
                })
            })
        })
        .unwrap_or_else(|| home.join(fallback))
}

pub fn trash_files_dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"));
    data.join("Trash/files")
}

/// "Новая папка", "Новая папка 2", …
pub fn unique_name(dir: &Path, base: &str) -> PathBuf {
    let first = dir.join(base);
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{base} {n}"))).find(|p| !p.exists()).unwrap()
}

pub fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else if meta.is_dir() {
        std::fs::create_dir(to)?;
        for item in std::fs::read_dir(from)? {
            let item = item?;
            copy_recursive(&item.path(), &to.join(item.file_name()))?;
        }
        std::fs::set_permissions(to, meta.permissions())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

pub fn remove_recursive(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) }
}

/// freedesktop.org trash: files/<name> plus info/<name>.trashinfo (home trash only)
pub fn trash(paths: &[PathBuf]) -> usize {
    let files = trash_files_dir();
    let info = files.with_file_name("info");
    if std::fs::create_dir_all(&files).is_err() || std::fs::create_dir_all(&info).is_err() {
        return paths.len();
    }
    let mut failed = 0;
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let target = unique_name(&files, &name);
        let target_name = target.file_name().unwrap().to_string_lossy().into_owned();
        let absolute = std::fs::canonicalize(path.parent().unwrap_or(Path::new("/")))
            .map(|p| p.join(&name))
            .unwrap_or_else(|_| path.clone());
        let record = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            percent_encode(&absolute),
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
        );
        let info_path = info.join(format!("{target_name}.trashinfo"));
        let ok = std::fs::write(&info_path, record).is_ok()
            && (std::fs::rename(path, &target).is_ok()
                || (copy_recursive(path, &target).is_ok() && remove_recursive(path).is_ok()));
        if !ok {
            let _ = std::fs::remove_file(&info_path);
            failed += 1;
        }
    }
    failed
}

pub fn percent_encode(path: &Path) -> String {
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// True when both paths are on the same filesystem (a drop there moves, elsewhere copies)
pub fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// Copy name for a paste into the same folder: "a.txt" → "a копия.txt" ("a copy.txt" in English)
fn paste_target(dir: &Path, source: &Path) -> PathBuf {
    let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let target = dir.join(&name);
    if !target.exists() {
        return target;
    }
    let (stem, ext) = match (source.file_stem(), source.extension()) {
        (Some(stem), Some(ext)) if !source.is_dir() => (stem.to_string_lossy().into_owned(), format!(".{}", ext.to_string_lossy())),
        _ => (name.clone(), String::new()),
    };
    let copy = crate::i18n::tr_static("копия");
    let first = dir.join(format!("{stem} {copy}{ext}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem} {copy} {n}{ext}"))).find(|p| !p.exists()).unwrap()
}

/// Copies or moves `sources` into `dir`; returns how many failed
pub fn paste(sources: &[PathBuf], dir: &Path, cut: bool) -> usize {
    let mut failed = 0;
    for source in sources {
        if dir.starts_with(source) {
            failed += 1; // a folder into itself
            continue;
        }
        if cut && source.parent() == Some(dir) {
            continue;
        }
        let target = paste_target(dir, source);
        let ok = if cut {
            std::fs::rename(source, &target).is_ok()
                || (copy_recursive(source, &target).is_ok() && remove_recursive(source).is_ok())
        } else {
            copy_recursive(source, &target).is_ok()
        };
        failed += usize::from(!ok);
    }
    failed
}

/// `text/uri-list` for file paths (percent-encoded, CRLF lines)
pub fn uri_list(paths: &[PathBuf]) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for path in paths {
        out.push_str("file://");
        for &b in path.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out.push_str("\r\n");
    }
    out
}

/// Local paths from a `text/uri-list` (comments and other schemes are skipped)
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("file://"))
        .map(|rest| rest.find('/').map_or(rest, |i| &rest[i..]))
        .map(percent_decode)
        .collect()
}

pub fn percent_decode(encoded: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&encoded[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

fn trash_info_path(trashed: &Path) -> Option<PathBuf> {
    let name = trashed.file_name()?;
    let mut info = name.to_os_string();
    info.push(".trashinfo");
    Some(trash_files_dir().with_file_name("info").join(info))
}

/// Anything in the home trash? (One directory entry is read: cheap, but still I/O.)
pub fn trash_is_empty() -> bool {
    std::fs::read_dir(trash_files_dir()).map_or(true, |mut d| d.next().is_none())
}

/// Where a trashed item came from (its `.trashinfo` `Path=`).
pub fn trash_origin(trashed: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(trash_info_path(trashed)?).ok()?;
    let value = text.lines().find_map(|l| l.strip_prefix("Path="))?;
    let path = percent_decode(value.trim());
    path.is_absolute().then_some(path)
}

/// Puts items back where they were deleted from; an occupied name gets " 2", " 3"…
/// Returns the failures.
pub fn restore(trashed: &[PathBuf]) -> usize {
    let files = trash_files_dir();
    let mut failed = 0;
    for item in trashed {
        let ok = (|| {
            if item.parent() != Some(files.as_path()) {
                return None;
            }
            let origin = trash_origin(item)?;
            let dir = origin.parent()?;
            std::fs::create_dir_all(dir).ok()?;
            let target = unique_name(dir, &origin.file_name()?.to_string_lossy());
            let moved = std::fs::rename(item, &target).is_ok()
                || (copy_recursive(item, &target).is_ok() && remove_recursive(item).is_ok());
            moved.then(|| {
                let _ = std::fs::remove_file(trash_info_path(item)?);
                Some(())
            })?
        })();
        if ok.is_none() {
            failed += 1;
        }
    }
    failed
}

/// Permanently removes items that are in the trash (never anything outside it).
pub fn delete_from_trash(trashed: &[PathBuf]) -> usize {
    let files = trash_files_dir();
    trashed
        .iter()
        .filter(|item| {
            let inside = item.parent() == Some(files.as_path());
            let removed = inside && remove_recursive(item).is_ok();
            if removed {
                if let Some(info) = trash_info_path(item) {
                    let _ = std::fs::remove_file(info);
                }
            }
            !removed
        })
        .count()
}

/// Empties the home trash: files/, info/ and the size cache. Returns the failures.
pub fn empty_trash() -> usize {
    let files = trash_files_dir();
    let items: Vec<PathBuf> = std::fs::read_dir(&files)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let failed = delete_from_trash(&items);
    // Orphaned .trashinfo records (their file is already gone).
    if let Ok(dir) = std::fs::read_dir(files.with_file_name("info")) {
        for e in dir.flatten() {
            let name = e.file_name();
            let stem = name.to_string_lossy();
            if let Some(stem) = stem.strip_suffix(".trashinfo") {
                if std::fs::symlink_metadata(files.join(stem)).is_err() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let _ = std::fs::remove_file(files.with_file_name("directorysizes"));
    failed
}

/// inotify on the open folder only; the fd is watched by the event loop (no polling)
pub struct Watcher {
    fd: i32,
    wd: Option<i32>,
}

impl Watcher {
    pub fn new() -> Option<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        (fd >= 0).then_some(Self { fd, wd: None })
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }

    pub fn watch(&mut self, dir: &Path) {
        if let Some(wd) = self.wd.take() {
            unsafe { libc::inotify_rm_watch(self.fd, wd) };
        }
        let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else { return };
        let mask = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO | libc::IN_ATTRIB
            | libc::IN_CLOSE_WRITE | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF;
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), mask) };
        self.wd = (wd >= 0).then_some(wd);
    }

    /// Drains pending events; true if anything arrived
    pub fn drain(&self) -> bool {
        let mut buf = [0u8; 4096];
        let mut any = false;
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return any;
            }
            any = true;
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}


#[cfg(test)]
mod trash_tests {
    use super::*;
    #[test]
    fn trash_restore_delete_and_empty() {
        let root = std::env::temp_dir().join(format!("feather-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let docs = root.join("Документы с пробелом");
        std::fs::create_dir_all(&docs).unwrap();
        // Tests in this crate do not otherwise read XDG_DATA_HOME.
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        assert!(trash_is_empty());
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(docs.join(name), name).unwrap();
        }
        assert_eq!(trash(&[docs.join("a.txt"), docs.join("b.txt"), docs.join("c.txt")]), 0);
        assert!(!trash_is_empty());
        let files = trash_files_dir();
        assert_eq!(trash_origin(&files.join("a.txt")).unwrap(), docs.join("a.txt"));
        // Occupied original name: restored next to it, nothing overwritten.
        std::fs::write(docs.join("a.txt"), "new").unwrap();
        assert_eq!(restore(&[files.join("a.txt")]), 0);
        assert_eq!(std::fs::read_to_string(docs.join("a.txt 2")).unwrap(), "a.txt");
        assert_eq!(std::fs::read_to_string(docs.join("a.txt")).unwrap(), "new");
        assert!(!files.with_file_name("info").join("a.txt.trashinfo").exists());
        // Nothing outside the trash can be deleted through it.
        assert_eq!(delete_from_trash(&[docs.join("a.txt")]), 1);
        assert!(docs.join("a.txt").exists());
        assert_eq!(delete_from_trash(&[files.join("b.txt")]), 0);
        assert_eq!(empty_trash(), 0);
        assert!(trash_is_empty());
        assert_eq!(std::fs::read_dir(files.with_file_name("info")).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
