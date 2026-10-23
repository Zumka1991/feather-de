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

pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(&a.name, &b.name)));
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

/// Mounted removable volumes (udisks mounts under /run/media/$USER or /media)
pub fn volumes() -> Vec<(String, PathBuf)> {
    let user = std::env::var("USER").unwrap_or_default();
    let mut out = Vec::new();
    for base in [PathBuf::from("/run/media").join(&user), PathBuf::from("/media").join(&user), PathBuf::from("/media")] {
        if let Ok(dir) = std::fs::read_dir(&base) {
            for item in dir.flatten() {
                let path = item.path();
                if path.is_dir() && !out.iter().any(|(_, p)| *p == path) && path != PathBuf::from("/media").join(&user) {
                    out.push((item.file_name().to_string_lossy().into_owned(), path));
                }
            }
        }
    }
    out
}

/// Opens a file with its default application; children are reaped by SIGCHLD=SIG_IGN
pub fn open_external(path: &Path) {
    let _ = std::process::Command::new("xdg-open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

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
}
