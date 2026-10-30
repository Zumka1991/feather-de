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

/// unar's failure in Russian; `volumes` — the archive is one of several parts
fn unar_error(out: &std::process::Output, volumes: bool) -> String {
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // Files gives no password, so a protected archive always asks for one
    if text.contains("requires a password") {
        "Архив защищён паролем".into()
    } else if text.contains("Couldn't recognize") {
        "Архив повреждён или это не архив".into()
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

/// bsdtar's own words about a failure
fn tar_error(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    if text.contains("Encryption is not supported") {
        return "Архив защищён паролем".into();
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
/// it bsdtar tries (old RAR mostly works, RAR5 and volumes often do not).
pub fn extract(archive: &Path) -> Result<PathBuf, String> {
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
    let unpacked = if rar {
        unar().args(["-q", "-nr", "-D", "-k", "skip", "-o"]).arg(&tmp).arg(archive).stdin(std::process::Stdio::null()).output().ok()
    } else {
        None
    };
    match unpacked {
        Some(out) if out.status.success() => {}
        Some(out) => return fail(unar_error(&out, volumes)),
        None => match bsdtar().arg("-xf").arg(archive).arg("-C").arg(&tmp).output() {
            Ok(out) if out.status.success() => {}
            Ok(out) if rar => return fail(format!("{}. Для RAR нужна программа unar (пакет unar)", tar_error(&out))),
            Ok(out) => return fail(tar_error(&out)),
            Err(_) => return fail("Нет программы bsdtar (пакет libarchive-tools)".into()),
        },
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
        assert_eq!(unar_error(&locked, false), "Архив защищён паролем");
        assert_eq!(unar_error(&said("x.rar: Couldn't recognize the archive format.\n"), false), "Архив повреждён или это не архив");

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
        let folder = extract(&root.join("Снимки.rar")).unwrap();
        assert_eq!(folder, root.join("Снимки"));
        assert_eq!(std::fs::read(folder.join("Фото/кот.txt")).unwrap(), cat);
        assert_eq!(std::fs::read(folder.join("заметка.txt")).unwrap(), note);

        // A broken one says so and leaves nothing behind
        std::fs::write(root.join("битый.rar"), b"Rar!\x1a\x07\x01\x00garbage").unwrap();
        let broken = extract(&root.join("битый.rar")).unwrap_err();
        assert!(!has_unar || broken == "Архив повреждён", "{broken}");

        if has_unar {
            // A file split over two volumes; double-click on the second still starts from the first
            let film = b"0123456789".repeat(50);
            let (a, b) = film.split_at(200);
            let size = film.len() as u64;
            let crc = Some(crc32(&film));
            std::fs::write(root.join("Фильм.part1.rar"), rar5(Some((0, false)), &[("кадр.txt", Some((a, size, 0x10, None)))])).unwrap();
            std::fs::write(root.join("Фильм.part2.rar"), rar5(Some((1, true)), &[("кадр.txt", Some((b, size, 0x08, crc)))])).unwrap();
            assert_eq!(extract(&root.join("Фильм.part2.rar")).unwrap(), root.join("кадр.txt"));
            assert_eq!(std::fs::read(root.join("кадр.txt")).unwrap(), film);
            // Without the second part: a clear message
            std::fs::remove_file(root.join("Фильм.part2.rar")).unwrap();
            assert_eq!(extract(&root.join("Фильм.part1.rar")).unwrap_err(), "Архив повреждён или не хватает одной из его частей");
        }
        let leftovers = std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with('.')).count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
