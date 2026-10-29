//! org.freedesktop.FileManager1 on the session bus: Chrome/Firefox "Show in folder", Telegram
//! "Open folder" and similar call it instead of xdg-open. Owned by the compositor only while a
//! Feather session runs, so other desktops keep their own file manager; when another one already
//! holds the name (e.g. a `dolphin --daemon` left from earlier), the request waits in the bus
//! queue and takes over once it exits. Every call spawns the file manager chosen in Settings →
//! Defaults (Файлы unless changed), idle costs nothing.
use std::{path::PathBuf, sync::Mutex};

const NAME: &str = "org.freedesktop.FileManager1";
const OBJECT: &str = "/org/freedesktop/FileManager1";
/// One call opens at most this many windows
const MAX_WINDOWS: usize = 8;
/// Shell command of the chosen file manager; empty until Settings load, then Файлы is used
static COMMAND: Mutex<String> = Mutex::new(String::new());

pub fn set_command(command: String) {
    *COMMAND.lock().unwrap() = command;
}

struct FileManager {
    socket: String,
}

impl FileManager {
    fn open(&self, uris: Vec<String>, reveal: bool) {
        let program = COMMAND.lock().unwrap().clone();
        let program = if program.is_empty() { crate::apps::files_command() } else { program };
        for path in uris.iter().filter_map(|u| uri_path(u)).take(MAX_WINDOWS) {
            let cmd = command(&program, &path, reveal);
            crate::apps::spawn_wayland_command(&cmd, &self.socket);
        }
    }
}

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    /// Folders open as they are
    async fn show_folders(&self, uris: Vec<String>, _startup_id: String) {
        self.open(uris, false);
    }
    /// Files open their folder with the file selected (feather-files does that for a file path)
    async fn show_items(&self, uris: Vec<String>, _startup_id: String) {
        self.open(uris, true);
    }
    /// No properties window yet: show the item in its folder
    async fn show_item_properties(&self, uris: Vec<String>, _startup_id: String) {
        self.open(uris, true);
    }
}

/// Файлы selects a file given by path. Dolphin and Nautilus need `--select`; any other manager
/// gets the containing folder, since a file path would open the file itself.
fn command(program: &str, path: &std::path::Path, reveal: bool) -> String {
    let quote = |p: &std::path::Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let name = program
        .split_whitespace()
        .next()
        .map(|p| p.trim_matches(['\'', '"']).rsplit('/').next().unwrap_or(p))
        .unwrap_or_default();
    if !reveal || path.is_dir() || name == "feather-files" {
        format!("{program} {}", quote(path))
    } else if matches!(name, "dolphin" | "nautilus") {
        format!("{program} --select {}", quote(path))
    } else {
        format!("{program} {}", quote(path.parent().unwrap_or(path)))
    }
}

/// `file:///a%20b` or a plain absolute path → an existing local path
fn uri_path(uri: &str) -> Option<PathBuf> {
    let path = match uri.strip_prefix("file://") {
        Some(rest) => feather_ui::fileops::percent_decode(&rest[rest.find('/')?..]),
        None => PathBuf::from(uri),
    };
    (path.is_absolute() && path.exists()).then_some(path)
}

pub fn start(socket: String) {
    let _ = std::thread::Builder::new()
        .name("feather-filemanager1".into())
        .spawn(move || {
            let result: zbus::Result<()> = futures_lite::future::block_on(async {
                let conn = zbus::connection::Builder::session()?
                    .serve_at(OBJECT, FileManager { socket })?
                    .build()
                    .await?;
                conn.request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::ReplaceExisting.into())
                    .await?;
                std::future::pending::<()>().await;
                Ok(())
            });
            if let Err(e) = result {
                tracing::warn!("FileManager1: {e}");
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uris_resolve_to_existing_local_paths() {
        let dir = std::env::temp_dir().join(format!("feather fm1 {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b.txt");
        std::fs::write(&file, b"x").unwrap();
        let uri = format!("file://{}", dir.join("a%20b.txt").display()).replace(' ', "%20");
        assert_eq!(uri_path(&uri), Some(file.clone()));
        assert_eq!(uri_path(&format!("file://localhost{}", dir.display()).replace(' ', "%20")), Some(dir.clone()));
        assert_eq!(uri_path(file.to_str().unwrap()), Some(file.clone()));
        assert_eq!(uri_path("file:///nonexistent/feather"), None);
        assert_eq!(uri_path("relative/path"), None);
        assert_eq!(uri_path("https://example.com/"), None);
        let files = command("'/opt/x/feather-files'", &file, true);
        assert!(files.starts_with("'/opt/x/feather-files' '") && files.ends_with("a b.txt'"));
        assert!(command("dolphin", &file, true).starts_with("dolphin --select '"));
        assert_eq!(command("thunar", &file, true), format!("thunar '{}'", dir.display()));
        assert_eq!(command("thunar", &dir, true), format!("thunar '{}'", dir.display()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
