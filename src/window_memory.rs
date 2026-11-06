//! Remembers where programs' windows were: a program opens at its last size and place, and the
//! programs open at the end of a session come back at the next login (like «Reopen windows» on
//! macOS). Nothing runs while idle: a change arms one timer, and the file is written by a
//! short-lived thread. The timer is long enough that the windows dying together at logout or
//! shutdown do not overwrite the session with an empty list.
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::monitors::Rect;
use crate::tiling::Tile;
use crate::window::ManagedWindow;
use crate::FeatherCompositor;

const SAVE_DELAY: Duration = Duration::from_secs(3);
/// How many programs are remembered; the least recently used go first
const MAX_APPS: usize = 200;
/// A remembered window stays this much on screen at least, whatever became of the monitors
const MIN_VISIBLE: i32 = 80;
/// How long after start a remembered window joins the Подложка without opening it
const LOGIN_QUIET: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// Free frame: position of the frame and size of the content
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub space: u32,
    pub maximized: bool,
    pub tile: Option<Tile>,
    /// Lives on the Подложка: opens there again
    pub scratch: bool,
}

#[derive(Default)]
pub struct WindowMemory {
    places: HashMap<String, (u64, Place)>,
    /// Program ids open at the end of the previous session, to start again at login
    pub reopen: Vec<String>,
    /// Windows expected from the programs being reopened: they go back to their desktop
    pub reopening: Vec<String>,
    clock: u64,
    save_armed: bool,
    /// The session is ending: what is open now is what must come back
    frozen: bool,
}

fn tile_name(tile: Tile) -> &'static str {
    match tile {
        Tile::Left => "left",
        Tile::Right => "right",
        Tile::Top => "top",
        Tile::Bottom => "bottom",
        Tile::TopLeft => "top-left",
        Tile::TopRight => "top-right",
        Tile::BottomLeft => "bottom-left",
        Tile::BottomRight => "bottom-right",
    }
}

fn parse_tile(s: &str) -> Option<Tile> {
    [Tile::Left, Tile::Right, Tile::Top, Tile::Bottom, Tile::TopLeft, Tile::TopRight, Tile::BottomLeft, Tile::BottomRight]
        .into_iter()
        .find(|t| tile_name(*t) == s)
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("feather-de/windows.conf"))
}

impl WindowMemory {
    pub fn load() -> Self {
        path().and_then(|p| std::fs::read_to_string(p).ok()).map_or_else(Self::default, |text| Self::parse(&text))
    }

    fn parse(text: &str) -> Self {
        let mut memory = Self::default();
        for line in text.lines() {
            let mut fields = line.split('\t');
            match fields.next() {
                Some("open") => {
                    if let Some(id) = fields.next().filter(|id| !id.is_empty()) {
                        memory.reopen.push(id.to_string());
                    }
                }
                Some("window") => {
                    let f: Vec<&str> = fields.collect();
                    // The last field came later: older files have none
                    let [id, x, y, w, h, space, maximized, tile, ref rest @ ..] = f[..] else { continue };
                    let (Ok(x), Ok(y), Ok(w), Ok(h), Ok(space)) = (x.parse(), y.parse(), w.parse(), h.parse(), space.parse()) else { continue };
                    if id.is_empty() || w <= 0 || h <= 0 {
                        continue;
                    }
                    memory.clock += 1;
                    memory.places.insert(id.to_string(), (memory.clock, Place { x, y, w, h, space, maximized: maximized == "1", tile: parse_tile(tile), scratch: rest.first() == Some(&"1") }));
                }
                _ => {}
            }
        }
        memory
    }

    fn text(&self, open: &[String]) -> String {
        let mut rows: Vec<_> = self.places.iter().collect();
        rows.sort_by_key(|(_, (stamp, _))| *stamp);
        let mut text = String::with_capacity(rows.len() * 48 + 32);
        for id in open {
            text.push_str(&format!("open\t{id}\n"));
        }
        for (id, (_, p)) in rows {
            text.push_str(&format!(
                "window\t{id}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                p.x, p.y, p.w, p.h, p.space, u8::from(p.maximized), p.tile.map_or("", tile_name), u8::from(p.scratch)
            ));
        }
        text
    }

    pub fn place(&self, app_id: &str) -> Option<&Place> {
        self.places.get(app_id).map(|(_, p)| p)
    }

    fn record(&mut self, app_id: &str, place: Place) {
        self.clock += 1;
        self.places.insert(app_id.to_string(), (self.clock, place));
        if self.places.len() > MAX_APPS {
            if let Some(oldest) = self.places.iter().min_by_key(|(_, (stamp, _))| *stamp).map(|(id, _)| id.clone()) {
                self.places.remove(&oldest);
            }
        }
    }
}

/// Whether this window's place is ours to remember. Games and other X11 programs manage their
/// own geometry, and Proton shares one id between unrelated games.
pub fn remembers(win: &ManagedWindow) -> bool {
    !win.larvis
        && !win.app_id.is_empty()
        && !win.is_builtin()
        && !win.app_id.eq_ignore_ascii_case("steam_app_default")
        && win.wl_surface().is_some_and(|s| !crate::xwayland::is_xwayland_surface(s))
}

/// The window's size and place apart from any tile or maximized state
pub fn place_of(win: &ManagedWindow) -> Option<Place> {
    let scratch = win.space == crate::scratch::SCRATCH_SPACE;
    let (x, y, w, h) = if win.tiled.is_some() || scratch {
        win.tile_free?
    } else if win.maximized || win.fullscreen {
        win.saved_rect?
    } else {
        (win.x, win.y, win.width, win.height)
    };
    (w > 0 && h > 0).then_some(Place { x, y, w, h, space: if scratch { 0 } else { win.space }, maximized: win.maximized, tile: win.tiled, scratch })
}

/// Keeps a remembered frame reachable on the monitors there are now
pub fn fit(place: &Place, r: Rect, top: i32) -> (i32, i32, i32, i32) {
    let w = place.w.min(r.w - 16).max(crate::window::MIN_WINDOW_W);
    let h = place.h.min(r.h - top).max(crate::window::MIN_WINDOW_H);
    let x = place.x.clamp(r.x - w + MIN_VISIBLE, r.x + r.w - MIN_VISIBLE);
    let y = place.y.clamp(r.y + top, (r.y + r.h - MIN_VISIBLE).max(r.y + top));
    (x, y, w, h)
}

impl FeatherCompositor {
    /// A new window's first commit, before its first configure: the size rides along with it,
    /// so the program draws the right size from the first frame. A second window of a program
    /// that is already open cascades instead.
    pub fn restore_window_place(&mut self, idx: usize) {
        let win = &mut self.windows[idx];
        win.sync_app_id();
        if !remembers(win) || win.maximized || win.fullscreen || win.mapped {
            return;
        }
        let app_id = win.app_id.clone();
        let Some(place) = self.window_memory.place(&app_id).cloned() else { return };
        let id = self.windows[idx].id;
        let program = self.app_registry.app_for_app_id(&app_id).map(|i| self.app_registry.apps[i].id.clone());
        let reopening = program.and_then(|p| self.window_memory.reopening.iter().position(|a| *a == p));
        if self.windows.iter().any(|w| w.id != id && w.mapped && w.app_id == app_id) {
            return;
        }
        let rects = self.monitor_rects();
        let r = rects.iter().find(|r| r.contains(place.x as f64, place.y as f64)).copied().unwrap_or_else(|| self.monitor_at(self.pointer_location.x, self.pointer_location.y));
        let top = if r.x == 0 && r.y == 0 { crate::theme::TOP_BAR_HEIGHT } else { 0 };
        let (x, y, w, h) = fit(&place, r, top);
        let free = (x, y, w, h);
        let win = &mut self.windows[idx];
        win.remembered_place = Some((x, y));
        if let Some(i) = reopening {
            self.window_memory.reopening.swap_remove(i);
            self.windows[idx].remembered_space = Some(place.space);
        }
        // Quietly at login; a program opened by hand brings the Подложка forward
        let quiet = reopening.is_some() || self.start_time.elapsed() < LOGIN_QUIET;
        let win = &mut self.windows[idx];
        if place.scratch {
            win.resize_client(w, h);
            win.remembered_scratch = Some(quiet);
        } else if place.maximized {
            win.maximize_on(true, r);
            win.saved_rect = Some(free);
        } else if let Some(tile) = place.tile {
            win.tile_to(tile, tile.frame(crate::tiling::work_area(r)));
            win.tile_free = Some(free);
        } else {
            win.resize_client(w, h);
        }
    }

    /// Something about the windows changed: save in a while, and only once
    pub fn window_memory_changed(&mut self) {
        if self.window_memory.save_armed || self.window_memory.frozen || self.larvis_desktop {
            return;
        }
        self.window_memory.save_armed = true;
        let _ = self.loop_handle.insert_source(Timer::from_duration(SAVE_DELAY), |_, _, state| {
            state.window_memory.save_armed = false;
            state.save_window_memory();
            TimeoutAction::Drop
        });
    }

    /// The programs now open that can be started again from their .desktop entry
    fn reopenable_programs(&self) -> Vec<String> {
        let mut open = Vec::new();
        for win in self.windows.iter().filter(|w| w.mapped && remembers(w)) {
            let Some(i) = self.app_registry.app_for_app_id(&win.app_id) else { continue };
            let app = &self.app_registry.apps[i];
            if matches!(app.action, crate::apps::AppAction::Exec(_)) && !open.contains(&app.id) {
                open.push(app.id.clone());
            }
        }
        open
    }

    fn save_window_memory(&mut self) {
        let places: Vec<_> = self.windows.iter().filter(|w| w.mapped && remembers(w)).filter_map(|w| place_of(w).map(|p| (w.app_id.clone(), p))).collect();
        for (app_id, place) in places {
            self.window_memory.record(&app_id, place);
        }
        let open = self.reopenable_programs();
        let text = self.window_memory.text(&open);
        let Some(file) = path() else { return };
        let _ = std::thread::Builder::new().name("feather-windows".into()).spawn(move || {
            let tmp = file.with_extension("conf.tmp");
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
        });
    }

    /// Our own «Выйти»: remember the session now, before its programs are told to exit
    pub fn freeze_window_memory(&mut self) {
        if !self.larvis_desktop {
            self.save_window_memory();
        }
        self.window_memory.frozen = true;
    }

    /// Before a window goes away: its last place is what the next window of the program gets
    pub fn remember_closing_window(&mut self, idx: usize) {
        let win = &self.windows[idx];
        if remembers(win) {
            if let Some(place) = place_of(win) {
                let app_id = win.app_id.clone();
                self.window_memory.record(&app_id, place);
                self.window_memory_changed();
            }
        }
    }

    /// At login, after the login items: the programs of the previous session come back
    pub fn reopen_previous_session(&mut self, already_started: &[String]) {
        let list = std::mem::take(&mut self.window_memory.reopen);
        if !self.restore_session || self.larvis_desktop {
            return;
        }
        for id in list {
            let Some(app) = self.app_registry.apps.iter().find(|a| a.id == id) else { continue };
            let crate::apps::AppAction::Exec(cmd) = &app.action else { continue };
            if already_started.iter().any(|s| s.contains(cmd.as_str()) || cmd.contains(s.as_str())) {
                continue;
            }
            crate::apps::spawn_wayland_command(cmd, &self.wayland_socket_name);
            self.window_memory.reopening.push(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_and_the_open_list_survive_the_file() {
        let mut m = WindowMemory::default();
        m.record("firefox", Place { x: -40, y: 60, w: 900, h: 700, space: 2, maximized: false, tile: None, scratch: false });
        m.record("kate", Place { x: 10, y: 40, w: 640, h: 480, space: 0, maximized: true, tile: Some(Tile::TopLeft), scratch: true });
        let back = WindowMemory::parse(&m.text(&["firefox.desktop".to_string()]));
        assert_eq!(back.reopen, ["firefox.desktop"]);
        assert_eq!(back.place("firefox"), m.place("firefox"));
        assert_eq!(back.place("kate"), m.place("kate"));
    }

    #[test]
    fn damaged_lines_are_skipped() {
        let old = WindowMemory::parse("window\told\t1\t2\t3\t4\t0\t0\t\n");
        assert!(!old.place("old").unwrap().scratch, "a file from before the Подложка still loads");
        let m = WindowMemory::parse("window\tx\t1\nwindow\ty\ta\t1\t1\t1\t0\t0\t\nwindow\tz\t1\t2\t0\t4\t0\t0\t\nunknown\nopen\t\nwindow\tok\t1\t2\t3\t4\t0\t0\tnonsense\n");
        assert!(m.place("x").is_none() && m.place("y").is_none() && m.place("z").is_none());
        assert!(m.reopen.is_empty());
        assert_eq!(m.place("ok").unwrap().tile, None);
    }

    #[test]
    fn the_least_recently_used_program_is_forgotten() {
        let mut m = WindowMemory::default();
        let p = Place { x: 0, y: 0, w: 10, h: 10, space: 0, maximized: false, tile: None, scratch: false };
        for i in 0..=MAX_APPS {
            m.record(&format!("app{i}"), p.clone());
        }
        assert!(m.place("app0").is_none());
        assert!(m.place(&format!("app{MAX_APPS}")).is_some());
    }

    #[test]
    fn a_window_from_a_lost_monitor_stays_reachable() {
        let r = Rect { x: 0, y: 0, w: 1280, h: 800 };
        let p = Place { x: 4000, y: 3000, w: 2600, h: 1500, space: 0, maximized: false, tile: None, scratch: false };
        let (x, y, w, h) = fit(&p, r, 28);
        assert!(x + MIN_VISIBLE <= 1280 && y + MIN_VISIBLE <= 800);
        assert!(w <= 1264 && h <= 772);
        let p = Place { x: 100, y: 0, w: 400, h: 300, space: 0, maximized: false, tile: None, scratch: false };
        assert_eq!(fit(&p, r, 28).1, 28, "never under the menu bar");
    }
}
