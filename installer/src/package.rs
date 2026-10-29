//! A .deb file: its control fields, what dpkg already has of it, and installing it with apt
//! as root (pkexec runs our `install-deb` helper, which has its own polkit action).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub arch: String,
    /// First line of Description
    pub summary: String,
    /// The rest of Description, paragraphs separated by '\n'
    pub details: String,
    pub maintainer: String,
    /// Installed-Size, KiB
    pub size_kib: u64,
}

/// What dpkg already has under this package name
#[derive(Debug, Clone, PartialEq)]
pub enum Installed {
    No,
    Same,
    /// An older version is installed (this one updates it)
    Older(String),
    /// A newer version is installed (this one downgrades it)
    Newer(String),
}

/// Fields of `dpkg-deb --field` output: "Field: value"; Description continues on lines that
/// start with a space, where " ." is an empty line
pub fn parse_control(text: &str) -> Package {
    let mut p = Package::default();
    let mut in_description = false;
    let mut details = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')) {
            if in_description {
                let rest = rest.trim();
                if rest == "." {
                    if !details.is_empty() && !details.ends_with('\n') {
                        details.push('\n');
                    }
                } else if !rest.is_empty() {
                    if !details.is_empty() && !details.ends_with('\n') {
                        details.push(' ');
                    }
                    details.push_str(rest);
                }
            }
            continue;
        }
        in_description = false;
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().to_string();
        match key {
            "Package" => p.name = value,
            "Version" => p.version = value,
            "Architecture" => p.arch = value,
            "Maintainer" => p.maintainer = value,
            "Installed-Size" => p.size_kib = value.parse().unwrap_or(0),
            "Description" => {
                p.summary = value;
                in_description = true;
            }
            _ => {}
        }
    }
    p.details = details.trim_end().to_string();
    p
}

/// Reads the control fields of a .deb
pub fn read(path: &Path) -> Result<Package, String> {
    let out = Command::new("dpkg-deb")
        .arg("--field")
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "Не найдена программа dpkg-deb".to_string())?;
    let package = parse_control(&String::from_utf8_lossy(&out.stdout));
    if !out.status.success() || package.name.is_empty() {
        return Err("Это не пакет .deb, или файл повреждён".into());
    }
    Ok(package)
}

pub fn installed(package: &Package) -> Installed {
    let Ok(out) = Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Abbrev}|${Version}", &package.name])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Installed::No;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some((status, version)) = text.split_once('|') else { return Installed::No };
    if !status.starts_with("ii") {
        return Installed::No;
    }
    if version == package.version {
        return Installed::Same;
    }
    let older = Command::new("dpkg")
        .args(["--compare-versions", version, "lt", &package.version])
        .status()
        .is_ok_and(|s| s.success());
    if older {
        Installed::Older(version.to_string())
    } else {
        Installed::Newer(version.to_string())
    }
}

/// The file a launcher argument names: a path or a file:// URI (from a browser or the portal)
pub fn path_from_arg(arg: &str) -> PathBuf {
    let path = match arg.strip_prefix("file://") {
        // file://host/path: only the local host is ours
        Some(rest) => PathBuf::from(percent_decode(rest.strip_prefix("localhost").unwrap_or(rest))),
        None => PathBuf::from(arg),
    };
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16);
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// "23,4 МБ" from KiB, with a decimal comma like the rest of Feather
pub fn human_size(kib: u64) -> String {
    let bytes = kib as f64 * 1024.0;
    let (value, unit) = if bytes >= 1024.0 * 1024.0 * 1024.0 {
        (bytes / (1024.0 * 1024.0 * 1024.0), "ГБ")
    } else if bytes >= 1024.0 * 1024.0 {
        (bytes / (1024.0 * 1024.0), "МБ")
    } else {
        (bytes / 1024.0, "КБ")
    };
    let text = if value >= 100.0 { format!("{value:.0}") } else { format!("{value:.1}") };
    format!("{} {unit}", text.replace('.', ","))
}

/// The root helper: installed next to us (<prefix>/libexec/feather-de), or the source tree's
pub fn helper() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("../libexec/feather-de/install-deb")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../packaging/installer/install-deb")))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    /// Overall fraction 0…1 and apt's message
    Status(f32, String),
    Done,
    /// The password dialog was dismissed
    Cancelled,
    Failed(String),
}

/// One line of apt's `APT::Status-Fd`: "pmstatus:pkg:percent:message" (dpkg) or
/// "dlstatus:…" (downloading dependencies). Downloads fill the first fifth of the bar.
pub fn parse_status(line: &str) -> Option<(f32, String)> {
    let mut parts = line.splitn(4, ':');
    let kind = parts.next()?;
    let _package = parts.next()?;
    let percent: f32 = parts.next()?.trim().parse().ok()?;
    let message = parts.next().unwrap_or("").trim().to_string();
    let fraction = (percent / 100.0).clamp(0.0, 1.0);
    match kind {
        "dlstatus" => Some((fraction * 0.2, message)),
        "pmstatus" => Some((0.2 + fraction * 0.8, message)),
        _ => None,
    }
}

/// apt's own explanation of a failure: its "E:" lines, else the last lines it printed
pub fn failure_text(lines: &[String]) -> String {
    let errors: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("E: ").or_else(|| l.strip_prefix("dpkg: error")))
        .collect();
    let text = if errors.is_empty() {
        lines.iter().rev().take(3).rev().cloned().collect::<Vec<_>>().join("\n")
    } else {
        errors.join("\n")
    };
    if text.trim().is_empty() {
        "Установка не удалась".into()
    } else {
        text
    }
}

/// Runs the installation on a thread; `send` gets the progress and then one final event
pub fn install(deb: PathBuf, send: impl Fn(Progress) + Send + 'static) {
    std::thread::spawn(move || {
        let child = Command::new("pkexec")
            .arg(helper())
            .arg(&deb)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(_) => return send(Progress::Failed("Не найдена программа pkexec (пакет pkexec)".into())),
        };
        // stderr on its own thread, so a chatty apt cannot block on a full pipe
        let errors = child.stderr.take().map(|mut err| {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                text
            })
        });
        let mut lines: Vec<String> = Vec::new();
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Some((fraction, message)) = parse_status(&line) {
                    send(Progress::Status(fraction, message));
                } else if !line.trim().is_empty() {
                    lines.push(line);
                    if lines.len() > 40 {
                        lines.remove(0);
                    }
                }
            }
        }
        let status = child.wait();
        if let Some(errors) = errors.and_then(|t| t.join().ok()) {
            lines.extend(errors.lines().filter(|l| !l.trim().is_empty()).map(str::to_string));
        }
        send(match status.ok().and_then(|s| s.code()) {
            Some(0) => Progress::Done,
            Some(126) => Progress::Cancelled,
            Some(127) => Progress::Failed("Нет прав администратора, или пароль не принят".into()),
            _ => Progress::Failed(failure_text(&lines)),
        });
    });
}

/// The program the package installed, to offer "Открыть": its first visible .desktop entry
pub fn launcher(package: &str) -> Option<(String, String)> {
    let out = Command::new("dpkg").args(["-L", package]).stderr(Stdio::null()).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("/applications/") && l.ends_with(".desktop"))
        .find_map(|file| desktop_entry(&std::fs::read_to_string(file).ok()?))
}

/// Name and command of a .desktop entry that is meant to be shown
pub fn desktop_entry(text: &str) -> Option<(String, String)> {
    let (mut name, mut exec, mut hidden, mut in_entry) = (None, None, false, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            name.get_or_insert_with(|| v.to_string());
        } else if let Some(v) = line.strip_prefix("Exec=") {
            let command: Vec<&str> = v.split_whitespace().filter(|p| !p.starts_with('%')).collect();
            exec = Some(command.join(" "));
        } else if matches!(line, "NoDisplay=true" | "Hidden=true" | "Terminal=true") {
            hidden = true;
        }
    }
    (!hidden).then_some(())?;
    Some((name?, exec.filter(|e| !e.is_empty())?))
}

/// Starts a program detached from us, so closing the installer leaves it running
pub fn launch(command: &str) {
    use std::os::unix::process::CommandExt;
    let _ = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_fields_and_description_paragraphs() {
        let text = "Package: mouse-vpn\nVersion: 0.2.3\nArchitecture: amd64\nMaintainer: Mouse <a@b.c>\nInstalled-Size: 23950\nDepends: iproute2, pkexec\nDescription: Fast private MouseVPN client\n Connects in one click.\n Works everywhere.\n .\n Second paragraph.\n";
        let p = parse_control(text);
        assert_eq!(p.name, "mouse-vpn");
        assert_eq!(p.version, "0.2.3");
        assert_eq!(p.arch, "amd64");
        assert_eq!(p.size_kib, 23950);
        assert_eq!(p.summary, "Fast private MouseVPN client");
        assert_eq!(p.details, "Connects in one click. Works everywhere.\nSecond paragraph.");
        assert_eq!(human_size(p.size_kib), "23,4 МБ");
        assert_eq!(human_size(300), "300 КБ");
    }

    #[test]
    fn apt_status_lines_and_failures() {
        assert_eq!(parse_status("dlstatus:1:50.0:Downloading x"), Some((0.1, "Downloading x".into())));
        let (f, m) = parse_status("pmstatus:mouse-vpn:50:Распаковывается mouse-vpn").unwrap();
        assert!((f - 0.6).abs() < 1e-6);
        assert_eq!(m, "Распаковывается mouse-vpn");
        assert_eq!(parse_status("Reading package lists..."), None);
        let lines = vec!["Reading".to_string(), "E: Unable to locate package x".to_string()];
        assert_eq!(failure_text(&lines), "Unable to locate package x");
    }

    #[test]
    fn uris_and_desktop_entries() {
        assert_eq!(path_from_arg("file:///tmp/%D0%BA%20x.deb"), PathBuf::from("/tmp/к x.deb"));
        let entry = "[Desktop Entry]\nName=MouseVPN\nExec=/usr/bin/mousevpn %u\nType=Application\n[Desktop Action New]\nName=Other\n";
        assert_eq!(desktop_entry(entry), Some(("MouseVPN".into(), "/usr/bin/mousevpn".into())));
        assert_eq!(desktop_entry("[Desktop Entry]\nName=X\nExec=x\nNoDisplay=true\n"), None);
    }

    /// A real .deb, if one is around: FEATHER_TEST_DEB=/path/to.deb
    #[test]
    #[ignore]
    fn reads_a_real_package() {
        let Some(path) = std::env::var_os("FEATHER_TEST_DEB") else { return };
        let p = read(Path::new(&path)).unwrap();
        assert!(!p.name.is_empty() && !p.version.is_empty(), "{p:?}");
        eprintln!("{p:?} {:?}", installed(&p));
    }
}
