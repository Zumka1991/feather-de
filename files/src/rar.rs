//! RAR without an outside program: `rars` (pure Rust, Apache-2.0) reads RAR 1.3 to 7, RAR4 and
//! RAR5 encryption, encrypted names and volumes. It runs in a process of its own, this same
//! binary as `feather-files --unrar <folder> <first volume> <next volume>…`: a dictionary of
//! gigabytes leaves with the process, an archive that crashes the decoder takes no window with
//! it, and ✕ on the card kills it at once. The password comes on stdin, because the arguments
//! of a process are visible to everyone in `ps`.
//!
//! The outcome is the exit code ([`NEEDS_PASSWORD`]…); anything else is a crash.

use rars::{ArchiveFamily, ArchiveReadOptions, ArchiveReader, AttrSource, Error, ErrorKind, ExtractedEntryMeta, ExtractedLink};
use std::{
    borrow::Cow,
    cell::RefCell,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

/// The first argument that starts this role
pub const FLAG: &str = "--unrar";

pub const NEEDS_PASSWORD: i32 = 10;
pub const WRONG_PASSWORD: i32 = 11;
pub const DAMAGED: i32 = 12;
pub const MISSING_PART: i32 = 13;
pub const UNSUPPORTED: i32 = 14;
pub const NO_MEMORY: i32 = 15;
/// Could not read or write a file; why is on stderr
pub const IO_ERROR: i32 = 16;

/// RAR stops at 127 characters; this is far above any real password
const PASSWORD_MAX: usize = 4096;

/// `feather-files --unrar …`: returns the exit code
pub fn main(args: impl Iterator<Item = OsString>) -> i32 {
    let mut args = args.map(PathBuf::from);
    let Some(folder) = args.next() else { return UNSUPPORTED };
    let volumes: Vec<PathBuf> = args.collect();
    // Capacity for all of it up front: no copy of the password stays behind in a reallocation
    let mut password = zeroize::Zeroizing::new(Vec::with_capacity(PASSWORD_MAX + 1));
    if io::stdin().take(PASSWORD_MAX as u64).read_to_end(&mut password).is_err() {
        return IO_ERROR;
    }
    let password = (!password.is_empty()).then_some(password.as_slice());
    match unpack(&folder, &volumes, password) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn unpack(folder: &Path, volumes: &[PathBuf], password: Option<&[u8]>) -> Result<(), i32> {
    let first = volumes.first().ok_or(UNSUPPORTED)?;
    let mut options = ArchiveReadOptions::with_optional_password(password);
    // A hostile header can declare a dictionary of a terabyte: refused instead of an OOM kill
    if let Some(limit) = memory_total() {
        options = options.with_rar50_dictionary_size_limit(limit);
    }
    // RAR4 with encrypted names has no password check: a wrong key only reads as garbage headers
    let legacy_names = password.is_some() && legacy_signature(first);
    let archives = volumes
        .iter()
        .map(|path| ArchiveReader::read_path_with_options(path, options))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| match e.kind() {
            ErrorKind::ChecksumMismatch | ErrorKind::InvalidArchive if legacy_names => WRONG_PASSWORD,
            _ => exit_code(&e),
        })?;
    let family = archives.first().ok_or(UNSUPPORTED)?.family();
    let output = RefCell::new(Output { root: folder, family, unpacked: Vec::new() });
    let open = |meta: &ExtractedEntryMeta| output.borrow_mut().open(meta);
    let link = |meta: &ExtractedEntryMeta, link: ExtractedLink<'_>| output.borrow_mut().link(meta, link);
    let result = match archives.as_slice() {
        [one] => one.extract_with_links(options, open, link),
        all => rars::extract_volumes_with_links(all, options, open, link),
    };
    result.map_err(|e| {
        let code = exit_code(&e);
        // A file running past the end of the set, or begun before its start
        let cut = archives.first().is_some_and(|a| a.members().any(|m| m.meta.is_split_before))
            || archives.last().is_some_and(|a| a.members().any(|m| m.meta.is_split_after));
        if code == DAMAGED && cut {
            MISSING_PART
        } else {
            code
        }
    })?;
    output.into_inner().restore_metadata();
    Ok(())
}

fn exit_code(e: &Error) -> i32 {
    match e.kind() {
        ErrorKind::PasswordRequired => NEEDS_PASSWORD,
        ErrorKind::BadPassword => WRONG_PASSWORD,
        ErrorKind::UnsupportedFormat | ErrorKind::UnsupportedFeature => UNSUPPORTED,
        ErrorKind::ResourceLimit => NO_MEMORY,
        ErrorKind::Io => {
            match e.root_cause() {
                Error::Io(io) if io.kind == io::ErrorKind::StorageFull => eprintln!("Не хватает места на диске"),
                Error::Io(io) if io.kind == io::ErrorKind::PermissionDenied => eprintln!("Нет прав на запись в эту папку"),
                Error::Io(io) => eprintln!("{}", io.message),
                _ => {}
            }
            IO_ERROR
        }
        _ => DAMAGED,
    }
}

fn legacy_signature(path: &Path) -> bool {
    let mut head = [0; 7];
    fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == *b"Rar!\x1a\x07\x00"
}

fn memory_total() -> Option<u64> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let kib: u64 = info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kib * 1024)
}

/// Where a member goes, relative to the folder. RAR5 from Windows has no backslash separators
/// and on Linux keeps a backslash as part of the name. Old RAR uses backslashes, and a name
/// without a Unicode copy is in the OEM code page of whoever made it, in Russia CP866
fn member_path(family: ArchiveFamily, meta: &ExtractedEntryMeta, name: &[u8]) -> rars::Result<PathBuf> {
    if family == ArchiveFamily::Rar50Plus {
        let name: Cow<[u8]> = if meta.attr_source == AttrSource::Unix {
            rars::filename::decode_rar50(name)
        } else {
            name.iter().map(|&b| if b == b'\\' { b'_' } else { b }).collect::<Vec<_>>().into()
        };
        return rars::filename::relative_path(&name, false);
    }
    if meta.name_is_unicode || std::str::from_utf8(name).is_ok() {
        return rars::filename::relative_path(name, true);
    }
    let text = rars::filename::decoded_name(name, false, Some(rars::filename::LegacyNameEncoding::Cp866))?;
    rars::filename::relative_path(&text, true)
}

/// A symlink stays only if it cannot lead out of the folder: relative, with any ".." up front
/// and no deeper than the link itself. Together with `checked_output_path`, which refuses to
/// write through a symlink, no member can land outside
fn safe_link_target(target: &[u8], depth: usize) -> Option<&Path> {
    let path = Path::new(OsStr::from_bytes(target));
    let (mut up, mut down) = (0, false);
    for part in path.components() {
        match part {
            Component::ParentDir if !down => up += 1,
            Component::Normal(_) => down = true,
            Component::CurDir => {}
            _ => return None,
        }
    }
    ((up > 0 || down) && up <= depth).then_some(path)
}

/// Access bits for a new file. setuid never survives; the owner can always read
fn file_mode(meta: &ExtractedEntryMeta) -> u32 {
    match meta.attr_source {
        AttrSource::Unix if meta.file_attr & 0o170000 != 0 => (meta.file_attr & 0o777) as u32 | 0o400,
        AttrSource::Dos if meta.file_attr & 1 != 0 => 0o444,
        _ => 0o666,
    }
}

/// Times and folder access bits go on at the end: writing inside a folder changes its time, and
/// a read-only folder could not be filled
struct Unpacked {
    path: PathBuf,
    time: Option<SystemTime>,
    folder_mode: Option<u32>,
}

struct Output<'a> {
    root: &'a Path,
    family: ArchiveFamily,
    unpacked: Vec<Unpacked>,
}

impl Output<'_> {
    /// The checked destination of a member, its parent folders made
    fn place(&self, meta: &ExtractedEntryMeta) -> rars::Result<(PathBuf, usize)> {
        let relative = member_path(self.family, meta, &meta.name)?;
        let path = rars::filename::checked_output_path(self.root, &relative)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(Error::from)?;
        }
        Ok((path, relative.components().count() - 1))
    }

    fn remember(&mut self, path: PathBuf, meta: &ExtractedEntryMeta, folder_mode: Option<u32>) {
        let time = rars::timestamp::extracted_system_time(self.family, meta.file_time, meta.mtime_refinement);
        self.unpacked.push(Unpacked { path, time, folder_mode });
    }

    fn open(&mut self, meta: &ExtractedEntryMeta) -> rars::Result<Box<dyn Write>> {
        let (path, _) = self.place(meta)?;
        if meta.is_directory {
            fs::DirBuilder::new().recursive(true).create(&path).map_err(Error::from)?;
            // Our own hidden folder must stay ours to rename and to clean up
            let mode = (meta.attr_source == AttrSource::Unix && meta.file_attr & 0o170000 != 0).then_some((meta.file_attr & 0o777) as u32 | 0o700);
            self.remember(path, meta, mode);
            return Ok(Box::new(io::sink()));
        }
        // The same name again in one archive (versions of a file): the last one stays
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
            fs::remove_file(&path).map_err(Error::from)?;
        }
        let file = fs::OpenOptions::new().write(true).create_new(true).mode(file_mode(meta)).open(&path).map_err(Error::from)?;
        self.remember(path, meta, None);
        Ok(Box::new(file))
    }

    fn link(&mut self, meta: &ExtractedEntryMeta, link: ExtractedLink<'_>) -> rars::Result<()> {
        let (path, depth) = self.place(meta)?;
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
            fs::remove_file(&path).map_err(Error::from)?;
        }
        match link {
            // Links made on Windows point at Windows paths
            ExtractedLink::Symlink { target, .. } if self.family != ArchiveFamily::Rar50Plus || meta.attr_source == AttrSource::Unix => {
                let target = if self.family == ArchiveFamily::Rar50Plus { rars::filename::decode_rar50(target) } else { Cow::Borrowed(target) };
                if let Some(target) = safe_link_target(&target, depth) {
                    std::os::unix::fs::symlink(target, &path).map_err(Error::from)?;
                }
            }
            ExtractedLink::HardLink { target } | ExtractedLink::FileCopy { target } => {
                let source = rars::filename::checked_output_path(self.root, &member_path(self.family, meta, target)?)?;
                if !fs::symlink_metadata(&source).map_err(Error::from)?.is_file() {
                    return Err(Error::InvalidHeader("link to something other than a file"));
                }
                // A flash drive may have no hard links
                if !matches!(link, ExtractedLink::HardLink { .. }) || fs::hard_link(&source, &path).is_err() {
                    fs::copy(&source, &path).map_err(Error::from)?;
                }
                self.remember(path, meta, None);
            }
            _ => {}
        }
        Ok(())
    }

    /// Filesystems that cannot keep these (a flash drive) are not a reason to fail
    fn restore_metadata(mut self) {
        // Deepest first: a folder's time changes while something in it changes
        self.unpacked.sort_by_cached_key(|u| std::cmp::Reverse(u.path.components().count()));
        for item in &self.unpacked {
            if let (Some(time), Ok(file)) = (item.time, fs::File::open(&item.path)) {
                let _ = file.set_modified(time);
            }
            if let Some(mode) = item.folder_mode {
                let _ = fs::set_permissions(&item.path, fs::Permissions::from_mode(mode));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::fsops::{self, extract};
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
    };

    /// The test binary playing `feather-files --unrar`: see `fsops::rar_helper`
    #[test]
    fn helper_entry() {
        use std::os::unix::ffi::OsStrExt;
        if let Some(args) = std::env::var_os(fsops::TEST_HELPER_ARGS) {
            let args = args.as_bytes().split(|&b| b == b'\n').map(|a| std::ffi::OsStr::from_bytes(a).to_owned()).collect::<Vec<_>>();
            std::process::exit(super::main(args.into_iter()));
        }
    }

    const PASSWORD: &str = "тайна-1";

    fn fixtures(names: &[&str], test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("feather-rar-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rar");
        for name in names {
            fs::copy(from.join(name), root.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        root
    }

    fn numbers() -> String {
        (1..=5000).map(|n| format!("{n}\n")).collect()
    }

    /// What every fixture holds (see tests/fixtures/rar/README.md)
    fn check_contents(folder: &Path) {
        assert_eq!(fs::read_to_string(folder.join("числа.txt")).unwrap(), numbers());
        assert_eq!(fs::read_to_string(folder.join("заметка.txt")).unwrap(), "привет\n");
        assert_eq!(fs::read_to_string(folder.join("Фото/кот.txt")).unwrap(), "мяу\n".repeat(500));
    }

    fn check_nothing_left(root: &Path) {
        let left: Vec<_> = fs::read_dir(root).unwrap().flatten().map(|e| e.file_name()).filter(|n| n.to_string_lossy().starts_with('.')).collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn plain_archives_of_both_formats() {
        let names = ["plain5.rar", "plain4.rar", "solid5.rar", "solid4.rar", "stored5.rar"];
        let root = fixtures(&names, "plain");
        for name in names {
            let folder = extract(&root.join(name), None).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(folder, root.join(name.trim_end_matches(".rar")));
            check_contents(&folder);
        }
        check_nothing_left(&root);
        fs::remove_dir_all(&root).unwrap();
    }

    /// Asks for the password, refuses a wrong one, unpacks with the right one. RAR4 with
    /// encrypted names has no password check at all, the hardest case for "wrong"
    #[test]
    fn encrypted_archives_of_both_formats() {
        let names = ["data5.rar", "data4.rar", "names5.rar", "names4.rar", "ppmd4.rar"];
        let root = fixtures(&names, "password");
        for name in names {
            let archive = root.join(name);
            assert_eq!(extract(&archive, None).unwrap_err(), fsops::NEEDS_PASSWORD, "{name}");
            for wrong in ["неверный", "wrong", "тайна-2"] {
                assert_eq!(extract(&archive, Some(wrong)).unwrap_err(), fsops::WRONG_PASSWORD, "{name} {wrong}");
                check_nothing_left(&root);
            }
            check_contents(&extract(&archive, Some(PASSWORD)).unwrap_or_else(|e| panic!("{name}: {e}")));
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// Both ways of naming volumes, from any part, with a password too
    #[test]
    fn volumes_from_any_part() {
        let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rar");
        let names: Vec<String> = fs::read_dir(from).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("vol")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let root = fixtures(&names, "volumes");
        assert!(names.contains(&"vol5.part03.rar") && names.contains(&"vol4.r01"), "{names:?}");
        assert!(fsops::is_archive("vol4.r00") && fsops::archive_stem("vol4.r01") == "vol4" && !fsops::is_archive("note.r2d") && !fsops::is_archive(".r00"));
        let folder = extract(&root.join("vol5.part02.rar"), None).unwrap();
        assert_eq!(folder, root.join("vol5"));
        check_contents(&folder);
        fs::remove_dir_all(folder).unwrap();
        // The old naming: from the .rar and from a .rNN alike
        let folder = extract(&root.join("vol4.r01"), None).unwrap();
        assert_eq!(folder, root.join("vol4"));
        check_contents(&folder);
        fs::remove_dir_all(&folder).unwrap();
        check_contents(&extract(&root.join("vol4.rar"), None).unwrap());
        for first in ["volnames5.part01.rar", "volnames4.rar"] {
            assert_eq!(extract(&root.join(first), None).unwrap_err(), fsops::NEEDS_PASSWORD, "{first}");
            assert_eq!(extract(&root.join(first), Some("неверный")).unwrap_err(), fsops::WRONG_PASSWORD, "{first}");
            check_contents(&extract(&root.join(first), Some(PASSWORD)).unwrap_or_else(|e| panic!("{first}: {e}")));
        }
        check_nothing_left(&root);
        fs::remove_dir_all(&root).unwrap();
    }

    /// ✕ on the card kills the unpacker, and nothing stays behind
    #[test]
    fn cancelling_leaves_nothing() {
        let root = fixtures(&["names5.rar"], "cancel");
        let progress = fsops::Progress::default();
        progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let result = fsops::extract_with(&root.join("names5.rar"), Some(PASSWORD), &progress, &mut || {});
        assert_eq!(result.unwrap_err(), fsops::EXTRACT_STOPPED);
        check_nothing_left(&root);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_parts_and_damage_are_explained() {
        let names = ["vol5.part01.rar", "vol5.part02.rar", "vol5.part03.rar", "vol4.rar", "vol4.r00", "vol4.r01", "plain5.rar", "plain4.rar"];
        let root = fixtures(&names, "damage");
        const MISSING: &str = "Архив повреждён или не хватает одной из его частей";
        fs::rename(root.join("vol5.part03.rar"), root.join("в сторону")).unwrap();
        assert_eq!(extract(&root.join("vol5.part01.rar"), None).unwrap_err(), MISSING);
        fs::remove_file(root.join("в сторону")).unwrap();
        fs::remove_file(root.join("vol5.part01.rar")).unwrap();
        assert_eq!(extract(&root.join("vol5.part02.rar"), None).unwrap_err(), MISSING);
        fs::remove_file(root.join("vol4.r00")).unwrap();
        assert_eq!(extract(&root.join("vol4.rar"), None).unwrap_err(), MISSING);
        // Bytes changed in the data, a file cut short
        for name in ["plain5.rar", "plain4.rar"] {
            let mut bytes = fs::read(root.join(name)).unwrap();
            let middle = bytes.len() / 2;
            bytes[middle..middle + 8].copy_from_slice(b"XXXXXXXX");
            fs::write(root.join(name), &bytes).unwrap();
            assert_eq!(extract(&root.join(name), None).unwrap_err(), "Архив повреждён", "{name}");
            fs::write(root.join(name), &bytes[..middle]).unwrap();
            assert_eq!(extract(&root.join(name), None).unwrap_err(), "Архив повреждён", "{name} обрезан");
        }
        check_nothing_left(&root);
        fs::remove_dir_all(&root).unwrap();
    }

    /// Access bits and dates come through; a link stays only if it leads nowhere outside
    #[test]
    fn links_modes_and_dates() {
        let root = fixtures(&["links5.rar", "links4.rar"], "links");
        for name in ["links5.rar", "links4.rar"] {
            let folder = extract(&root.join(name), None).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(fs::read_link(folder.join("внутрь")).unwrap(), Path::new("папка/файл.txt"), "{name}");
            assert_eq!(fs::read_link(folder.join("папка/соседняя")).unwrap(), Path::new("../корень.txt"), "{name}");
            assert_eq!(fs::read_to_string(folder.join("внутрь")).unwrap(), "внутри\n", "{name}");
            for gone in ["наружу", "абсолютная", "папка/петля"] {
                assert!(fs::symlink_metadata(folder.join(gone)).is_err(), "{name}: {gone}");
            }
            let script = fs::metadata(folder.join("сценарий.sh")).unwrap();
            assert_eq!(script.permissions().mode() & 0o777 & !0o022, 0o750, "{name}");
            // 2021-03-04 05:06:07 UTC; the old format keeps local time, in two-second steps
            let file = fs::metadata(folder.join("папка/файл.txt")).unwrap();
            assert!((file.mtime() - 1_614_834_367).abs() <= 2 || name == "links4.rar", "{name}: {}", file.mtime());
            let dir = fs::metadata(folder.join("папка")).unwrap();
            assert!((dir.mtime() - 1_546_398_245).abs() <= 2 || name == "links4.rar", "{name}: {}", dir.mtime());
        }
        check_nothing_left(&root);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn link_targets_cannot_leave_the_folder() {
        let safe = |target: &str, depth| super::safe_link_target(target.as_bytes(), depth).is_some();
        assert!(safe("файл", 0) && safe("а/б", 0) && safe("../файл", 1) && safe("../../а", 2) && safe("..", 1) && safe("./а", 0));
        assert!(!safe("../файл", 0) && !safe("../../а", 1) && !safe("/etc/passwd", 3) && !safe("а/../../б", 3) && !safe("", 0) && !safe(".", 0));
    }

    #[test]
    fn names_without_unicode_are_oem() {
        use rars::{ArchiveFamily, ExtractedEntryMeta};
        let name = [0x8e, 0xe2, 0xe7, 0xf1, 0xe2, b'\\', b'a'];
        let meta = ExtractedEntryMeta::new(name.to_vec(), None, 0, false);
        assert_eq!(super::member_path(ArchiveFamily::Rar15To40, &meta, &name).unwrap(), Path::new("Отчёт/a"));
        assert!(super::member_path(ArchiveFamily::Rar15To40, &meta, b"..\\..\\evil").is_err());
        assert!(super::member_path(ArchiveFamily::Rar50Plus, &meta, b"../evil").is_err());
        assert!(super::member_path(ArchiveFamily::Rar50Plus, &meta, b"/etc/evil").is_err());
    }

    /// The real window installs the SIGCHLD reaper, which once took the unpackers' exit status
    /// before they were asked for it: every archive failed there, and only there. Signal state
    /// is per process, so this runs in a copy of the test binary
    #[test]
    fn archives_unpack_with_the_windows_child_reaper() {
        const ROLE: &str = "FEATHER_TEST_REAPER";
        if std::env::var_os(ROLE).is_some() {
            feather_ui::process::reap_children();
            let root = fixtures(&["plain5.rar", "data5.rar"], "reaper");
            check_contents(&extract(&root.join("plain5.rar"), None).unwrap());
            assert_eq!(extract(&root.join("data5.rar"), None).unwrap_err(), fsops::NEEDS_PASSWORD);
            assert_eq!(extract(&root.join("data5.rar"), Some("неверный")).unwrap_err(), fsops::WRONG_PASSWORD);
            check_contents(&extract(&root.join("data5.rar"), Some(PASSWORD)).unwrap());
            if std::process::Command::new("bsdtar").arg("--version").output().is_ok() {
                fs::write(root.join("note.txt"), "hi").unwrap();
                let zip = fsops::compress(&[root.join("note.txt")]).unwrap();
                fs::remove_file(root.join("note.txt")).unwrap();
                assert_eq!(extract(&zip, None).unwrap(), root.join("note.txt"));
            }
            fs::remove_dir_all(&root).unwrap();
            return;
        }
        let status = std::process::Command::new("/proc/self/exe")
            .args(["rar::tests::archives_unpack_with_the_windows_child_reaper", "--exact", "--test-threads=1"])
            .env(ROLE, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
