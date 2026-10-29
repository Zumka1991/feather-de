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
const ARCHIVES: [&str; 13] = [".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tgz", ".txz", ".tzst", ".tbz2", ".tbz", ".tar", ".zip", ".7z", ".rar"];

pub fn is_archive(name: &str) -> bool {
    let lower = name.to_lowercase();
    ARCHIVES.iter().any(|ext| lower.ends_with(ext) && lower.len() > ext.len())
}

/// "Фото.tar.gz" → "Фото"
pub fn archive_stem(name: &str) -> &str {
    let lower = name.to_lowercase();
    ARCHIVES.iter().find(|ext| lower.ends_with(*ext)).map_or(name, |ext| &name[..name.len() - ext.len()])
}

/// bsdtar from libarchive-tools; FEATHER_BSDTAR points tests at another copy
fn bsdtar() -> std::process::Command {
    std::process::Command::new(std::env::var_os("FEATHER_BSDTAR").unwrap_or_else(|| "bsdtar".into()))
}

/// bsdtar's own words about a failure
fn tar_error(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
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
/// refuses ".." and strips leading "/" from member names.
pub fn extract(archive: &Path) -> Result<PathBuf, String> {
    let dir = archive.parent().ok_or("Нет папки архива")?;
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = archive_stem(&name).to_string();
    let tmp = dir.join(format!(".{stem}.feather-extract-{}", std::process::id()));
    std::fs::create_dir(&tmp).map_err(|e| e.to_string())?;
    let out = bsdtar().arg("-xf").arg(archive).arg("-C").arg(&tmp).output();
    let fail = |e: String| {
        let _ = std::fs::remove_dir_all(&tmp);
        Err(e)
    };
    match out {
        Ok(out) if out.status.success() => {}
        Ok(out) => return fail(tar_error(&out)),
        Err(_) => return fail("Нет программы bsdtar (пакет libarchive-tools)".into()),
    }
    let items: Vec<PathBuf> = match std::fs::read_dir(&tmp) {
        Ok(d) => d.flatten().map(|e| e.path()).collect(),
        Err(e) => return fail(e.to_string()),
    };
    let result = match items.as_slice() {
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
        assert_eq!(extract(&one).unwrap(), root.join("Фото 2"));
        assert_eq!(std::fs::read_to_string(root.join("Фото 2/кот.txt")).unwrap(), "мяу");
        let folder = extract(&many).unwrap();
        assert_eq!(folder, root.join("Архив"));
        assert!(folder.join("Фото/кот.txt").is_file() && folder.join("заметка.txt").is_file());
        // A broken archive leaves nothing behind
        std::fs::write(root.join("битый.zip"), "not a zip").unwrap();
        assert!(extract(&root.join("битый.zip")).is_err());
        let leftovers = std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with('.')).count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
