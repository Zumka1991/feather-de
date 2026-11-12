//! Clipboard history (Super+Shift+V) and the emoji panel (Super+.). Both use Spotlight's panel;
//! the choice goes to the clipboard and into the front program. The history stays in memory
//! only, keeps text and file lists (no images) and skips what password managers mark as secret.
//! A copy costs one short thread; nothing runs while idle.
use std::collections::VecDeque;
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use smithay::reexports::calloop::{channel, timer::{TimeoutAction, Timer}};
use smithay::wayland::selection::data_device::request_data_device_client_selection;

use crate::apps::IconKind;
use crate::shell::{AppIcon, ShellClickAction};
use crate::spotlight::Hit;
use crate::FeatherCompositor;

const MAX_ENTRIES: usize = 30;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_CLIP: usize = 256 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// The panel's window gets the keyboard back first
const PASTE_DELAY: Duration = Duration::from_millis(60);
const URI_LIST: &str = "text/uri-list";
const TEXT_MIMES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];
const SECRET_MIMES: [&str; 3] = ["x-kde-passwordManagerHint", "application/x-nspasteboard-concealedtype", "org.nspasteboard.ConcealedType"];
pub const EMOJI_COLUMNS: usize = 10;
const EMOJI_MAX: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    pub mime: String,
    pub data: String,
}

pub struct ClipHistory {
    pub enabled: bool,
    pub entries: VecDeque<Clip>,
    results: channel::SyncSender<(u64, Clip)>,
    generation: u64,
    /// Emoji picked recently, newest first
    pub recent_emoji: Vec<char>,
}

impl ClipHistory {
    pub fn new(enabled: bool, handle: &smithay::reexports::calloop::LoopHandle<'static, FeatherCompositor>) -> Result<Self, Box<dyn std::error::Error>> {
        let (results, source) = channel::sync_channel(2);
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg((generation, clip)) = event {
                if generation == state.clip_history.generation && state.clip_history.enabled {
                    state.clip_history.push(clip);
                }
            }
        })?;
        Ok(Self { enabled, entries: VecDeque::new(), results, generation: 0, recent_emoji: Vec::new() })
    }

    /// Newest first, without duplicates and within the size limits
    pub fn push(&mut self, clip: Clip) {
        if clip.data.trim().is_empty() {
            return;
        }
        self.entries.retain(|c| c.data != clip.data);
        self.entries.push_front(clip);
        self.entries.truncate(MAX_ENTRIES);
        while self.entries.iter().map(|c| c.data.len()).sum::<usize>() > MAX_BYTES && self.entries.len() > 1 {
            self.entries.pop_back();
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.entries = VecDeque::new();
            self.generation += 1;
        }
    }
}

/// The best format to keep from what a program offers; None for secrets and images
pub fn pick_mime(mimes: &[String]) -> Option<String> {
    if mimes.iter().any(|m| SECRET_MIMES.iter().any(|s| m.eq_ignore_ascii_case(s))) {
        return None;
    }
    if mimes.iter().any(|m| m == URI_LIST) {
        return Some(URI_LIST.to_string());
    }
    TEXT_MIMES.iter().find(|t| mimes.iter().any(|m| m == *t)).map(|t| t.to_string())
}

/// Reads the pipe with a deadline: a program that never finishes writing must not keep a thread
fn read_limited(fd: OwnedFd) -> Option<Vec<u8>> {
    let mut file = std::fs::File::from(fd);
    let started = Instant::now();
    let mut data = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let left = READ_TIMEOUT.checked_sub(started.elapsed())?;
        let mut poll = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: `poll` points to one valid pollfd for the duration of the call
        if unsafe { libc::poll(&mut poll, 1, left.as_millis() as libc::c_int) } <= 0 {
            return None;
        }
        match file.read(&mut chunk) {
            Ok(0) => return Some(data),
            Ok(n) if data.len() + n <= MAX_CLIP => data.extend_from_slice(&chunk[..n]),
            _ => return None,
        }
    }
}

fn label_of(clip: &Clip) -> (String, String, IconKind) {
    if clip.mime == URI_LIST {
        let names: Vec<String> = clip
            .data
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let tail = l.trim().rsplit('/').next().unwrap_or(l);
                feather_ui::fileops::percent_decode(tail).to_string_lossy().into_owned()
            })
            .collect();
        let sub = feather_ui::i18n::tr("Файлы").into_owned();
        return (names.join(", ").chars().take(70).collect(), sub, IconKind::Finder);
    }
    let first = clip.data.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let chars = clip.data.chars().count();
    (first.chars().take(70).collect(), format!("{chars} {}", feather_ui::i18n::tr("симв.")), IconKind::TextEdit)
}

/// Programs that paste with Ctrl+Shift+V because Ctrl+V is theirs
fn pastes_with_shift(app_id: &str) -> bool {
    let id = app_id.to_lowercase();
    ["terminal", "konsole", "kitty", "alacritty", "foot", "wezterm", "tilix", "xterm", "ghostty", "rio"].iter().any(|t| id.contains(t))
}

impl FeatherCompositor {
    /// A program set a new clipboard. Asking for its data has to wait until this request is
    /// done being handled.
    pub fn clipboard_changed(&mut self, mimes: Vec<String>) {
        self.clip_history.generation += 1;
        if !self.clip_history.enabled || self.lock.active || self.larvis_desktop {
            return;
        }
        let Some(mime) = pick_mime(&mimes) else { return };
        let generation = self.clip_history.generation;
        self.loop_handle.insert_idle(move |state| state.capture_clipboard(generation, mime));
    }

    fn capture_clipboard(&mut self, generation: u64, mime: String) {
        if generation != self.clip_history.generation {
            return;
        }
        let Ok((read, write)) = std::io::pipe() else { return };
        if request_data_device_client_selection(&self.seat, mime.clone(), OwnedFd::from(write)).is_err() {
            return;
        }
        let results = self.clip_history.results.clone();
        let _ = std::thread::Builder::new().name("feather-clip".into()).stack_size(128 * 1024).spawn(move || {
            let Some(bytes) = read_limited(OwnedFd::from(read)) else { return };
            let Ok(data) = String::from_utf8(bytes) else { return };
            let _ = results.send((generation, Clip { mime, data }));
        });
    }

    /// Text or a file list becomes the clipboard and goes into the front program
    pub fn paste_clip(&mut self, clip: Clip) {
        let entries = if clip.mime == URI_LIST {
            vec![(URI_LIST.to_string(), clip.data.into_bytes())]
        } else {
            TEXT_MIMES.iter().map(|m| (m.to_string(), clip.data.clone().into_bytes())).collect()
        };
        // Our own selection: the generation keeps a read in progress from coming back on top
        self.clip_history.generation += 1;
        self.set_clipboard(entries);
        self.refresh_focus();
        let _ = self.loop_handle.insert_source(Timer::from_duration(PASTE_DELAY), |_, _, state| {
            state.send_paste_keys();
            TimeoutAction::Drop
        });
    }

    fn send_paste_keys(&mut self) {
        let Some(win) = self.windows.iter().rev().find(|w| w.is_visible() && !w.is_builtin()) else { return };
        // Evdev codes: Ctrl, Shift, V
        let keys: &[u32] = if pastes_with_shift(&win.app_id) { &[29, 42, 47] } else { &[29, 47] };
        let time = self.start_time.elapsed().as_millis() as u32;
        self.automation_keys(keys, time);
    }

    pub fn open_picker(&mut self, mode: crate::shell::SpotlightMode) {
        let again = self.shell.spotlight_open && self.shell.spotlight_mode == mode;
        self.shell.close_menus();
        self.shell.spotlight_query.clear();
        self.shell.spotlight_input.clear();
        self.shell.spotlight_selected = 0;
        self.shell.spotlight_scroll = 0;
        if !again {
            self.shell.spotlight_mode = mode;
            self.shell.spotlight_open = true;
            if mode == crate::shell::SpotlightMode::MenuHud {
                self.prefetch_menu_hud();
            }
        }
        self.spotlight_refresh();
        self.needs_redraw = true;
        self.refresh_focus();
    }

    pub fn clipboard_hits(&self, query: &str) -> Vec<Hit> {
        self.clip_history
            .entries
            .iter()
            .filter(|c| query.is_empty() || c.data.to_lowercase().contains(query))
            .take(10)
            .map(|clip| {
                let (name, sub, kind) = label_of(clip);
                Hit { name, sub, icon: AppIcon { kind, name: None }, emoji: None, badge: String::new(), action: ShellClickAction::PasteClip(clip.clone()) }
            })
            .collect()
    }

    pub fn emoji_hits(&self, query: &str) -> Vec<Hit> {
        let hit = |ch: char, name: &str| Hit {
            name: name.split_whitespace().next().unwrap_or("").to_string(),
            sub: String::new(),
            icon: AppIcon { kind: IconKind::GenericApp, name: None },
            emoji: Some(ch),
            badge: String::new(),
            action: ShellClickAction::PasteEmoji(ch),
        };
        let words = |ch: char| crate::emoji::TABLE.iter().find(|(c, _)| *c == ch).map_or("", |(_, w)| *w);
        let mut hits: Vec<Hit> = Vec::with_capacity(EMOJI_MAX);
        if query.is_empty() {
            hits.extend(self.clip_history.recent_emoji.iter().map(|ch| hit(*ch, words(*ch))));
        }
        for (ch, names) in crate::emoji::TABLE {
            if hits.len() >= EMOJI_MAX {
                break;
            }
            if query.split_whitespace().all(|q| names.contains(q)) && !hits.iter().any(|h| h.emoji == Some(*ch)) {
                hits.push(hit(*ch, names));
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mimes(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn secrets_and_images_stay_out() {
        assert_eq!(pick_mime(&mimes(&["text/plain", "x-kde-passwordManagerHint"])), None);
        assert_eq!(pick_mime(&mimes(&["image/png"])), None);
        assert_eq!(pick_mime(&mimes(&["STRING", "text/plain;charset=utf-8"])).as_deref(), Some("text/plain;charset=utf-8"));
        assert_eq!(pick_mime(&mimes(&["text/plain", URI_LIST])).as_deref(), Some(URI_LIST));
    }

    #[test]
    fn terminals_paste_with_shift() {
        assert!(pastes_with_shift("feather-terminal"));
        assert!(pastes_with_shift("org.kde.konsole"));
        assert!(!pastes_with_shift("firefox"));
    }

    #[test]
    fn labels_show_the_first_line_and_the_files() {
        let (name, _, kind) = label_of(&Clip { mime: "text/plain".into(), data: "\n  привет\nмир".into() });
        assert_eq!((name.as_str(), kind), ("привет", IconKind::TextEdit));
        let (name, _, kind) = label_of(&Clip { mime: URI_LIST.into(), data: "file:///home/a/%D0%B4.txt\r\nfile:///b.png\r\n".into() });
        assert_eq!((name.as_str(), kind), ("д.txt, b.png", IconKind::Finder));
    }
}
