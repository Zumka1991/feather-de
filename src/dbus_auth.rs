//! Авторизация отправителей на сессионной шине D-Bus для приватных и управляющих интерфейсов.
//!
//! Все функции вызываются исключительно на фоновых D-Bus потоках (`feather-api` и
//! `--file-portal`), поэтому никогда не блокируют главный цикл событий композитора.
use std::{
    collections::HashMap,
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::Mutex,
};
use zbus::{message::Header, Connection};

const PORTAL_FRONTEND: &str = "org.freedesktop.portal.Desktop";
const PORTAL_BACKEND: &str = "org.freedesktop.impl.portal.desktop.feather";
const MAX_CALLER_CACHE: usize = 64;

/// Снимает суффикс `" (deleted)"`, который ядро Linux добавляет к `/proc/<pid>/exe`,
/// если исполняемый файл обновили на диске во время работы сеанса.
pub fn strip_deleted_exe(raw: PathBuf) -> PathBuf {
    let bytes = raw.as_os_str().as_bytes();
    if let Some(stripped) = bytes.strip_suffix(b" (deleted)") {
        PathBuf::from(std::ffi::OsStr::from_bytes(stripped))
    } else {
        raw
    }
}

pub fn read_exe_clean(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(strip_deleted_exe)
}

fn pid_uid(pid: u32) -> Option<u32> {
    std::fs::metadata(format!("/proc/{pid}"))
        .ok()
        .map(|m| m.uid())
}

fn read_proc_bounded(path: &str, limit: usize) -> Vec<u8> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let _ = file.take(limit as u64).read_to_end(&mut buf);
    buf
}

pub fn cmdline_has_arg(raw: &[u8], needle: &str) -> bool {
    raw.split(|b| *b == 0)
        .any(|part| !part.is_empty() && part == needle.as_bytes())
}

#[cfg(not(test))]
pub fn pid_has_cmdline_arg(pid: u32, needle: &str) -> bool {
    let raw = read_proc_bounded(&format!("/proc/{pid}/cmdline"), 16 * 1024);
    cmdline_has_arg(&raw, needle)
}

pub fn is_sandboxed_exe(exe: &Path) -> bool {
    matches!(
        exe.file_name().and_then(|s| s.to_str()),
        Some("xdg-dbus-proxy" | "bwrap")
    )
}

pub fn is_sandboxed_cgroup(cgroup_text: &str) -> bool {
    cgroup_text.lines().any(|line| {
        line.contains("app-flatpak-")
            || line.contains("/flatpak-")
            || line.contains("/flatpak/")
            || line.contains("snap.")
    })
}

pub fn is_sandboxed_apparmor(attr_current: &str) -> bool {
    let trimmed = attr_current.trim();
    trimmed.starts_with("snap.") || trimmed.contains("flatpak")
}

pub fn is_sandboxed_environ(environ_bytes: &[u8]) -> bool {
    environ_bytes.split(|b| *b == 0).any(|entry| {
        entry.starts_with(b"FLATPAK_ID=")
            || entry.starts_with(b"FLATPAK_SANDBOX_DIR=")
            || entry.starts_with(b"SNAP=")
            || entry.starts_with(b"SNAP_NAME=")
            || entry == b"container=flatpak"
            || entry == b"container=bwrap"
    })
}

/// Проверяет, запущен ли процесс `pid` внутри изолированной песочницы (Flatpak, `xdg-dbus-proxy`,
/// Bubblewrap или Snap).
pub fn is_sandboxed_pid(pid: u32) -> bool {
    if let Some(exe) = read_exe_clean(pid) {
        if is_sandboxed_exe(&exe) {
            return true;
        }
    }
    if std::fs::symlink_metadata(format!("/proc/{pid}/root/.flatpak-info")).is_ok() {
        return true;
    }
    let cgroup = String::from_utf8_lossy(&read_proc_bounded(&format!("/proc/{pid}/cgroup"), 8192))
        .into_owned();
    if is_sandboxed_cgroup(&cgroup) {
        return true;
    }
    let apparmor =
        String::from_utf8_lossy(&read_proc_bounded(&format!("/proc/{pid}/attr/current"), 1024))
            .into_owned();
    if is_sandboxed_apparmor(&apparmor) {
        return true;
    }
    let environ = read_proc_bounded(&format!("/proc/{pid}/environ"), 64 * 1024);
    is_sandboxed_environ(&environ)
}

async fn resolve_caller_pid(
    conn: &Connection,
    header: &Header<'_>,
) -> Result<(zbus::fdo::DBusProxy<'static>, String, u32), String> {
    let sender = header
        .sender()
        .ok_or_else(|| "отсутствует отправитель D-Bus".to_string())?;
    let sender_str = sender.as_str().to_string();
    let dbus = zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| e.to_string())?;
    let pid = dbus
        .get_connection_unix_process_id(sender.to_owned().into())
        .await
        .map_err(|e| e.to_string())?;
    let me = unsafe { libc::getuid() };
    if pid_uid(pid) != Some(me) {
        return Err("процесс принадлежит другому пользователю".into());
    }
    Ok((dbus, sender_str, pid))
}

/// Строгая проверка для приватного интерфейса `org.feather.ScreenCast1`:
/// вызывать его имеет право только наш собственный процесс `feather-de --file-portal`,
/// владеющий именем `org.freedesktop.impl.portal.desktop.feather` на сессионной шине.
pub async fn verify_portal_backend_for_screencast(
    conn: &Connection,
    header: &Header<'_>,
) -> Result<String, feather_api::Error> {
    let deny = || {
        feather_api::Error::AccessDenied(
            "Доступ к org.feather.ScreenCast1 разрешён только порталу Пёрышка".into(),
        )
    };
    let (dbus, sender, pid) = resolve_caller_pid(conn, header).await.map_err(|_| deny())?;
    let expected_name: zbus::names::BusName<'_> =
        PORTAL_BACKEND.try_into().map_err(|_| deny())?;
    let owner = dbus.get_name_owner(expected_name).await.map_err(|_| deny())?;
    if owner.as_str() != sender.as_str() || is_sandboxed_pid(pid) {
        return Err(deny());
    }
    let Some(caller_exe) = read_exe_clean(pid) else {
        return Err(deny());
    };
    let Some(self_exe) = read_exe_clean(std::process::id()) else {
        return Err(deny());
    };
    if caller_exe != self_exe {
        return Err(deny());
    }
    #[cfg(not(test))]
    if !pid_has_cmdline_arg(pid, "--file-portal") {
        return Err(deny());
    }
    Ok(sender)
}

/// Строгая проверка для приватного бэкенда `org.freedesktop.impl.portal.desktop.feather`:
/// вызывать `org.freedesktop.impl.portal.*` имеет право только брокер `xdg-desktop-portal`
/// (`org.freedesktop.portal.Desktop`), который определяет неподдельный `app_id` клиента.
pub async fn verify_xdg_portal_frontend(
    conn: &Connection,
    header: &Header<'_>,
) -> zbus::fdo::Result<String> {
    let deny = || {
        zbus::fdo::Error::AccessDenied(
            "Вызывать org.freedesktop.impl.portal.* может только xdg-desktop-portal".into(),
        )
    };
    let (dbus, sender, pid) = resolve_caller_pid(conn, header).await.map_err(|_| deny())?;
    let expected_name: zbus::names::BusName<'_> =
        PORTAL_FRONTEND.try_into().map_err(|_| deny())?;
    let owner = dbus.get_name_owner(expected_name).await.map_err(|_| deny())?;
    if owner.as_str() != sender.as_str() || is_sandboxed_pid(pid) {
        return Err(deny());
    }
    let Some(caller_exe) = read_exe_clean(pid) else {
        return Err(deny());
    };
    let is_xdg_portal = caller_exe.file_name().and_then(|s| s.to_str()) == Some("xdg-desktop-portal");
    #[cfg(test)]
    let is_xdg_portal = is_xdg_portal || Some(&caller_exe) == read_exe_clean(std::process::id()).as_ref();
    if !is_xdg_portal {
        return Err(deny());
    }
    Ok(sender)
}

/// Проверка вызывающего процесса для управляющего интерфейса `org.feather.Shell1`:
/// разрешает локальные неизолированные процессы текущего пользователя (`featherctl`, скрипты,
/// голосовой ассистент) и блокирует вызовы из песочниц (Flatpak, `xdg-dbus-proxy`, Snap).
pub async fn verify_shell_caller(
    conn: &Connection,
    header: &Header<'_>,
    cache: &Mutex<HashMap<String, bool>>,
) -> Result<(), feather_api::Error> {
    let deny = || {
        feather_api::Error::AccessDenied(
            "Доступ к org.feather.Shell1 из изолированной песочницы запрещён".into(),
        )
    };
    let sender = header.sender().ok_or_else(deny)?.as_str().to_string();
    if let Some(&allowed) = cache.lock().unwrap().get(&sender) {
        return if allowed { Ok(()) } else { Err(deny()) };
    }
    let allowed = match resolve_caller_pid(conn, header).await {
        Ok((_, _, pid)) => !is_sandboxed_pid(pid),
        Err(_) => false,
    };
    {
        let mut guard = cache.lock().unwrap();
        if guard.len() >= MAX_CALLER_CACHE {
            guard.clear();
        }
        guard.insert(sender, allowed);
    }
    if allowed {
        Ok(())
    } else {
        Err(deny())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_deleted_suffix_handles_upgraded_binaries() {
        assert_eq!(
            strip_deleted_exe(PathBuf::from("/usr/bin/feather-de (deleted)")),
            PathBuf::from("/usr/bin/feather-de")
        );
        assert_eq!(
            strip_deleted_exe(PathBuf::from("/usr/libexec/xdg-desktop-portal")),
            PathBuf::from("/usr/libexec/xdg-desktop-portal")
        );
    }

    #[test]
    fn sandbox_detectors_catch_flatpak_dbus_proxy_and_snap() {
        assert!(is_sandboxed_exe(Path::new("/usr/bin/xdg-dbus-proxy")));
        assert!(is_sandboxed_exe(Path::new("/usr/bin/bwrap")));
        assert!(!is_sandboxed_exe(Path::new("/usr/bin/featherctl")));

        assert!(is_sandboxed_cgroup(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-app.zen_browser.zen-1234.scope\n"
        ));
        assert!(is_sandboxed_cgroup(
            "0::/user.slice/user-1000.slice/snap.chromium.chromium-5678.scope\n"
        ));
        assert!(!is_sandboxed_cgroup(
            "0::/user.slice/user-1000.slice/session-1.scope\n"
        ));

        assert!(is_sandboxed_apparmor("snap.firefox.firefox (enforce)\n"));
        assert!(!is_sandboxed_apparmor("unconfined\n"));

        assert!(is_sandboxed_environ(b"PATH=/app/bin\0FLATPAK_ID=app.zen_browser.zen\0"));
        assert!(is_sandboxed_environ(b"HOME=/home/u\0container=flatpak\0"));
        assert!(is_sandboxed_environ(b"SNAP_NAME=telegram-desktop\0"));
        assert!(!is_sandboxed_environ(b"PATH=/usr/bin\0XDG_CURRENT_DESKTOP=FeatherDE\0"));
        assert!(cmdline_has_arg(b"/usr/bin/feather-de\0--file-portal\0", "--file-portal"));
        assert!(!cmdline_has_arg(b"/usr/bin/feather-de\0--windowed\0", "--file-portal"));
    }
}
