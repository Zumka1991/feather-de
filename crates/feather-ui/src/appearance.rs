//! Appearance of Feather clients: startup snapshot and directory inotify for atomic saves.
//! File reads and waits belong to one sleeping worker, never the UI event loop.
use crate::scale::{self, AccentColor, Appearance, TextSize};
use std::{
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeConfig {
    pub appearance: Appearance,
    pub accent: AccentColor,
    pub highlight: Option<AccentColor>,
    pub text_size: TextSize,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            appearance: Appearance::Light,
            accent: AccentColor::Blue,
            highlight: None,
            text_size: TextSize::Default,
        }
    }
}

pub fn settings_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::fileops::home().join(".config"))
        .join("feather-de/settings.conf")
}

fn parse(text: &str) -> ThemeConfig {
    let mut result = ThemeConfig::default();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim();
            match key.trim() {
                "appearance" => match value {
                    "dark" => result.appearance = Appearance::Dark,
                    "light" => result.appearance = Appearance::Light,
                    _ => {}
                },
                "accent_color" => {
                    if let Some(c) = AccentColor::parse(value) {
                        result.accent = c;
                    }
                }
                "highlight_color" => match value {
                    "accent" | "default" | "" => result.highlight = None,
                    other => {
                        if let Some(c) = AccentColor::parse(other) {
                            result.highlight = Some(c);
                        }
                    }
                },
                "text_size" => {
                    if let Some(ts) = TextSize::parse(value) {
                        result.text_size = ts;
                    }
                }
                _ => {}
            }
        }
    }
    result
}

fn read(path: &Path) -> ThemeConfig {
    let mut text = String::new();
    if let Ok(file) = std::fs::File::open(path) {
        if file.take(65536).read_to_string(&mut text).is_ok() {
            return parse(&text);
        }
    }
    ThemeConfig::default()
}

fn apply(cfg: ThemeConfig) {
    scale::set_appearance(cfg.appearance);
    scale::set_accent_color(cfg.accent);
    scale::set_highlight_choice(cfg.highlight);
    scale::set_text_size(cfg.text_size);
}

static LATEST_CONFIG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x01_ff_00_00);

fn pack_config(cfg: ThemeConfig) -> u32 {
    let a = matches!(cfg.appearance, Appearance::Dark) as u32;
    let ac = (cfg.accent as u32) << 8;
    let hl = (cfg.highlight.map_or(255u8, |c| c as u8) as u32) << 16;
    let ts = (cfg.text_size as u32) << 24;
    a | ac | hl | ts
}

fn unpack_config(raw: u32) -> ThemeConfig {
    let appearance = if (raw & 0xff) != 0 {
        Appearance::Dark
    } else {
        Appearance::Light
    };
    let accent = AccentColor::from_u8(((raw >> 8) & 0xff) as u8);
    let hl_raw = ((raw >> 16) & 0xff) as u8;
    let highlight = if hl_raw < 8 {
        Some(AccentColor::from_u8(hl_raw))
    } else {
        None
    };
    let text_size = TextSize::from_u8(((raw >> 24) & 0xff) as u8);
    ThemeConfig {
        appearance,
        accent,
        highlight,
        text_size,
    }
}

fn store_latest(cfg: ThemeConfig) {
    LATEST_CONFIG.store(pack_config(cfg), std::sync::atomic::Ordering::Release);
}

/// Applies the latest watched theme snapshot on the event-loop thread. Returns `true` if any
/// appearance/accent/highlight/text_size setting changed.
pub fn sync(appearance: Appearance) -> bool {
    let mut cfg = unpack_config(LATEST_CONFIG.load(std::sync::atomic::Ordering::Acquire));
    cfg.appearance = appearance;
    let changed = scale::appearance() != cfg.appearance
        || scale::accent_color() != cfg.accent
        || scale::highlight_choice() != cfg.highlight
        || scale::text_size() != cfg.text_size;
    if changed {
        apply(cfg);
    }
    changed
}

pub fn init() {
    let cfg = read(&settings_path());
    store_latest(cfg);
    apply(cfg);
}

/// Dropping the handle closes the shutdown socket; the worker leaves poll immediately.
pub struct Watcher {
    _stop: UnixStream,
}

pub fn watch(mut notify: impl FnMut(Appearance) -> bool + Send + 'static) -> io::Result<Watcher> {
    watch_path(settings_path(), move |a| notify(a))
}

fn watch_path(
    path: PathBuf,
    mut notify: impl FnMut(Appearance) -> bool + Send + 'static,
) -> io::Result<Watcher> {
    let (stop, shutdown) = UnixStream::pair()?;
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    std::thread::Builder::new()
        .name("feather-appearance".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let mut watched = PathBuf::new();
            let mut watch_id = -1;
            let mut expected = Vec::new();
            let arm =
                |watched: &mut PathBuf, watch_id: &mut i32, expected: &mut Vec<u8>| -> bool {
                    let Some(dir) = path.parent().and_then(|p| p.ancestors().find(|p| p.is_dir()))
                    else {
                        return false;
                    };
                    if dir != watched || *watch_id < 0 {
                        if *watch_id >= 0 {
                            unsafe {
                                libc::inotify_rm_watch(fd.as_raw_fd(), *watch_id);
                            }
                        }
                        let Ok(name) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
                            return false;
                        };
                        *watch_id = unsafe {
                            libc::inotify_add_watch(
                                fd.as_raw_fd(),
                                name.as_ptr(),
                                libc::IN_CLOSE_WRITE
                                    | libc::IN_MOVED_TO
                                    | libc::IN_MOVED_FROM
                                    | libc::IN_CREATE
                                    | libc::IN_DELETE
                                    | libc::IN_DELETE_SELF
                                    | libc::IN_MOVE_SELF,
                            )
                        };
                        if *watch_id < 0 {
                            return false;
                        }
                        *watched = dir.to_path_buf();
                        *expected = path
                            .strip_prefix(dir)
                            .ok()
                            .and_then(|p| p.components().next())
                            .map(|c| c.as_os_str().as_bytes().to_vec())
                            .unwrap_or_default();
                    }
                    true
                };
            if !arm(&mut watched, &mut watch_id, &mut expected) {
                return;
            }
            let mut current = read(&path);
            store_latest(current);
            if !notify(current.appearance) {
                return;
            }
            let mut bytes = [0u8; 4096];
            loop {
                let mut poll = [
                    libc::pollfd {
                        fd: fd.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: shutdown.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                let ready = unsafe { libc::poll(poll.as_mut_ptr(), 2, -1) };
                if ready < 0 {
                    if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                if poll[1].revents != 0 {
                    return;
                }
                if poll[0].revents & libc::POLLIN == 0 {
                    return;
                }
                let n =
                    unsafe { libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
                if n <= 0 {
                    return;
                }
                let mut offset = 0;
                let mut relevant = false;
                while offset + std::mem::size_of::<libc::inotify_event>() <= n as usize {
                    let event = unsafe {
                        std::ptr::read_unaligned(
                            bytes.as_ptr().add(offset).cast::<libc::inotify_event>(),
                        )
                    };
                    let start = offset + std::mem::size_of::<libc::inotify_event>();
                    let end = start + event.len as usize;
                    if end > n as usize {
                        break;
                    }
                    let name = &bytes[start..end];
                    let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
                    if name == expected
                        || event.mask
                            & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_Q_OVERFLOW)
                            != 0
                    {
                        relevant = true;
                    }
                    if event.mask & libc::IN_IGNORED != 0 && event.wd == watch_id {
                        watch_id = -1;
                        relevant = true;
                    }
                    offset = end;
                }
                if !relevant {
                    continue;
                }
                if !arm(&mut watched, &mut watch_id, &mut expected) {
                    return;
                }
                let next = read(&path);
                if next != current {
                    current = next;
                    store_latest(current);
                    if !notify(current.appearance) {
                        return;
                    }
                }
            }
        })?;
    Ok(Watcher { _stop: stop })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_theme_changes_follow_atomic_replace_and_new_config_directory() {
        let root = std::env::temp_dir().join(format!("feather-appearance-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config/settings.conf");
        let (tx, rx) = std::sync::mpsc::channel();
        let watch = watch_path(path.clone(), move |a| tx.send(a).is_ok()).unwrap();
        let receive = || rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(receive(), Appearance::Light);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, "appearance=dark\n").unwrap();
        std::fs::rename(&temporary, &path).unwrap();
        assert_eq!(receive(), Appearance::Dark);
        std::fs::write(&temporary, "appearance=light\n").unwrap();
        std::fs::rename(&temporary, &path).unwrap();
        assert_eq!(receive(), Appearance::Light);
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(60))
                .is_err(),
            "unchanged settings produced a wakeup"
        );
        std::fs::write(
            &temporary,
            "appearance=light\naccent_color=purple\nhighlight_color=green\ntext_size=large\n",
        )
        .unwrap();
        std::fs::rename(&temporary, &path).unwrap();
        let a = receive();
        assert_eq!(a, Appearance::Light);
        assert!(sync(a));
        assert_eq!(scale::accent_color(), AccentColor::Purple);
        assert_eq!(scale::highlight_choice(), Some(AccentColor::Green));
        assert_eq!(scale::text_size(), TextSize::Large);
        drop(watch);
        store_latest(ThemeConfig::default());
        apply(ThemeConfig::default());
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(
            parse("appearance=dark\nappearance=invalid\n").appearance,
            Appearance::Dark
        );
    }
}

/// Shared light/dark client palette; queried only while painting changed content.
pub mod colors {
    use crate::{
        draw::rgba,
        scale::{accent_color, appearance, highlight_color, Appearance},
    };
    use tiny_skia::Color;
    fn dark() -> bool {
        appearance() == Appearance::Dark
    }
    pub fn window() -> Color {
        if dark() {
            rgba(30, 30, 34, 255)
        } else {
            rgba(255, 255, 255, 255)
        }
    }
    pub fn sidebar() -> Color {
        if dark() {
            rgba(42, 42, 47, 255)
        } else {
            rgba(236, 236, 240, 255)
        }
    }
    pub fn sidebar_edge() -> Color {
        if dark() {
            rgba(255, 255, 255, 22)
        } else {
            rgba(0, 0, 0, 22)
        }
    }
    pub fn text() -> Color {
        if dark() {
            rgba(240, 240, 244, 255)
        } else {
            rgba(29, 29, 31, 255)
        }
    }
    pub fn secondary() -> Color {
        if dark() {
            rgba(173, 173, 183, 255)
        } else {
            rgba(110, 110, 115, 255)
        }
    }
    pub fn tertiary() -> Color {
        if dark() {
            rgba(132, 132, 143, 255)
        } else {
            rgba(160, 160, 166, 255)
        }
    }
    pub fn accent() -> Color {
        let (r, g, b) = accent_color().rgb(appearance());
        rgba(r, g, b, 255)
    }
    pub fn highlight() -> Color {
        let (r, g, b) = highlight_color().rgb(appearance());
        rgba(r, g, b, 255)
    }
    pub fn hover() -> Color {
        if dark() {
            rgba(255, 255, 255, 20)
        } else {
            rgba(0, 0, 0, 14)
        }
    }
    pub fn selected_gray() -> Color {
        if dark() {
            rgba(255, 255, 255, 28)
        } else {
            rgba(0, 0, 0, 26)
        }
    }
    pub fn traffic_inactive() -> Color {
        if dark() {
            rgba(82, 82, 91, 255)
        } else {
            rgba(205, 205, 210, 255)
        }
    }
    pub fn inactive_selection() -> Color {
        if dark() {
            rgba(68, 68, 78, 255)
        } else {
            rgba(220, 220, 224, 255)
        }
    }
    pub fn separator() -> Color {
        if dark() {
            rgba(255, 255, 255, 24)
        } else {
            rgba(0, 0, 0, 20)
        }
    }
    pub fn stripe() -> Color {
        if dark() {
            rgba(255, 255, 255, 8)
        } else {
            rgba(0, 0, 0, 7)
        }
    }
    pub fn control() -> Color {
        if dark() {
            rgba(255, 255, 255, 18)
        } else {
            rgba(0, 0, 0, 13)
        }
    }
    pub fn glass() -> Color {
        if dark() {
            rgba(40, 40, 45, 252)
        } else {
            rgba(248, 248, 250, 252)
        }
    }
    pub fn note_selection(focused: bool) -> Color {
        if dark() {
            if focused {
                rgba(101, 83, 22, 255)
            } else {
                rgba(67, 61, 41, 255)
            }
        } else {
            rgba(255, 215, 60, if focused { 167 } else { 100 })
        }
    }
}

