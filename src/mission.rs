//! Mission Control: the windows of the current desktop spread out without overlapping,
//! a strip of desktops on top. Windows move with the shared spring as GPU transforms of
//! their real buffers; only the strip and the hovered title are CPU-drawn (overlay).
//! While it is open clients get no pointer input; a click picks a window or a desktop,
//! a window dragged onto a desktop tile moves there.
use crate::{shell::R, FeatherCompositor};

/// Room for the strip above the windows and the Dock below
const STRIP_TOP: f32 = 34.0;
const TILE_W: f32 = 168.0;
const TILE_H: f32 = 96.0;
const ADD_W: f32 = 64.0;
const GAP: f32 = 18.0;
const DOCK_ROOM: f32 = 104.0;
/// Movement before a press on a window becomes a drag
const DRAG_START: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Window(u64),
    Space(u32),
    Add,
}

#[derive(Debug, Default)]
pub struct Mission {
    /// Overview rectangles (logical) of the windows, in stacking order
    pub targets: Vec<(u64, R)>,
    pub hover: Option<Hit>,
    press: Option<(u64, (f64, f64))>,
    /// Dragged window: grab point inside its overview rectangle
    drag: Option<(u64, (f64, f64))>,
    pub closing: bool,
}

/// Fits windows (id, width, height) into `area` on a grid, largest first in reading
/// order; never larger than the real window
pub fn layout(windows: &[(u64, f32, f32)], area: R) -> Vec<(u64, R)> {
    let n = windows.len();
    if n == 0 {
        return Vec::new();
    }
    // The column count whose cells best match the average window shape
    let avg = windows.iter().map(|(_, w, h)| w / h.max(1.0)).sum::<f32>() / n as f32;
    let cols = (1..=n)
        .min_by(|&a, &b| {
            let score = |c: usize| {
                let rows = n.div_ceil(c);
                let cell = (area.w / c as f32) / (area.h / rows as f32);
                (cell / avg).ln().abs()
            };
            score(a).total_cmp(&score(b))
        })
        .unwrap_or(1);
    let rows = n.div_ceil(cols);
    let (cw, ch) = (area.w / cols as f32, area.h / rows as f32);
    windows
        .iter()
        .enumerate()
        .map(|(i, &(id, w, h))| {
            let (row, col) = (i / cols, i % cols);
            // The last, shorter row is centred
            let in_row = if row == rows - 1 { n - row * cols } else { cols };
            let x0 = area.x + (area.w - in_row as f32 * cw) * 0.5 + col as f32 * cw;
            let y0 = area.y + row as f32 * ch;
            let k = ((cw - GAP * 2.0) / w).min((ch - GAP * 2.0) / h).min(1.0).max(0.05);
            let (sw, sh) = (w * k, h * k);
            (id, R::new(x0 + (cw - sw) * 0.5, y0 + (ch - sh) * 0.5, sw, sh))
        })
        .collect()
}

/// Desktop tiles and the «+» tile, centred under the menu bar
pub fn strip(count: u32, screen_w: f32) -> Vec<(Hit, R)> {
    let total = count as f32 * (TILE_W + GAP) + ADD_W;
    let mut x = (screen_w - total) * 0.5;
    let y = crate::theme::TOP_BAR_HEIGHT as f32 + STRIP_TOP - 20.0;
    let mut out: Vec<_> = (0..count)
        .map(|i| {
            let r = R::new(x, y, TILE_W, TILE_H);
            x += TILE_W + GAP;
            (Hit::Space(i), r)
        })
        .collect();
    if count < crate::spaces::MAX_SPACES {
        out.push((Hit::Add, R::new(x, y + (TILE_H - ADD_W) * 0.5, ADD_W, ADD_W)));
    }
    out
}

fn windows_area(screen: (i32, i32)) -> R {
    let top = crate::theme::TOP_BAR_HEIGHT as f32 + STRIP_TOP + TILE_H + 12.0;
    R::new(24.0, top, screen.0 as f32 - 48.0, (screen.1 as f32 - top - DOCK_ROOM).max(120.0))
}

impl FeatherCompositor {
    pub fn mission_open(&self) -> bool {
        self.mission.as_ref().is_some_and(|m| !m.closing)
    }

    pub fn toggle_mission(&mut self) {
        if self.mission_open() {
            self.close_mission(None);
        } else {
            self.open_mission();
        }
    }

    pub fn open_mission(&mut self) {
        if self.lock.active {
            return;
        }
        self.shell.close_menus();
        self.shell.switcher = None;
        self.shell.launchpad = None;
        let mut m = self.mission.take().unwrap_or_default();
        m.closing = false;
        m.press = None;
        m.drag = None;
        self.mission = Some(m);
        self.relayout_mission();
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
    }

    /// Recomputes the overview for the desktop in front (opened, desktop switched, moved)
    pub fn relayout_mission(&mut self) {
        let screen = (self.shell.screen_w, self.shell.screen_h);
        let shown: Vec<(u64, f32, f32)> = self
            .windows
            .iter()
            .filter(|w| w.is_visible())
            .map(|w| (w.id, w.width as f32, w.total_height() as f32))
            .collect();
        let targets = layout(&shown, windows_area(screen));
        self.animations.set_mission(true, &targets);
        if let Some(m) = self.mission.as_mut() {
            m.targets = targets;
            m.hover = None;
        }
        self.sync_mission_overlay();
    }

    /// Spring back; `focus` is raised first so it lands on top
    pub fn close_mission(&mut self, focus: Option<u64>) {
        let Some(m) = self.mission.as_mut() else { return };
        m.closing = true;
        m.drag = None;
        m.press = None;
        m.hover = None;
        self.animations.set_mission(false, &[]);
        if let Some(id) = focus {
            self.execute_shell_action(crate::shell::ShellClickAction::FocusWindow(id));
        }
        self.shell.mission = None;
        self.shell.overlay_dirty = true;
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
    }

    /// After each animation step: a finished close drops the state
    pub fn finish_mission(&mut self) {
        if self.mission.as_ref().is_some_and(|m| m.closing) && !self.animations.mission_visible() {
            self.mission = None;
            self.pointer_focus_dirty = true;
        }
    }

    fn mission_hit(&self, x: f64, y: f64) -> Option<Hit> {
        let m = self.mission.as_ref()?;
        let (fx, fy) = (x as f32, y as f32);
        for (hit, r) in strip(self.spaces.count, self.shell.screen_w as f32) {
            if r.contains(fx, fy) {
                return Some(hit);
            }
        }
        m.targets.iter().rev().find(|(_, r)| r.contains(fx, fy)).map(|(id, _)| Hit::Window(*id))
    }

    pub fn mission_motion(&mut self, x: f64, y: f64) {
        let Some(m) = self.mission.as_ref() else { return };
        if m.closing {
            return;
        }
        if let Some((id, start)) = m.press {
            if m.drag.is_none() && ((x - start.0).powi(2) + (y - start.1).powi(2)).sqrt() > DRAG_START {
                let grab = m.targets.iter().find(|(w, _)| *w == id).map(|(_, r)| (start.0 - r.x as f64, start.1 - r.y as f64));
                if let (Some(grab), Some(m)) = (grab, self.mission.as_mut()) {
                    m.drag = Some((id, grab));
                }
            }
        }
        if let Some((id, grab)) = self.mission.as_ref().and_then(|m| m.drag) {
            let moved = self.mission.as_mut().and_then(|m| {
                let r = &mut m.targets.iter_mut().find(|(w, _)| *w == id)?.1;
                r.x = (x - grab.0) as f32;
                r.y = (y - grab.1) as f32;
                Some(*r)
            });
            if let Some(r) = moved {
                self.animations.move_mission_target(id, r);
            }
        }
        let hover = self.mission_hit(x, y).filter(|h| !self.mission.as_ref().and_then(|m| m.drag).is_some_and(|(id, _)| *h == Hit::Window(id)));
        if self.mission.as_ref().is_some_and(|m| m.hover != hover) || self.mission.as_ref().is_some_and(|m| m.drag.is_some()) {
            if let Some(m) = self.mission.as_mut() {
                m.hover = hover;
            }
            self.sync_mission_overlay();
        }
        self.needs_redraw = true;
    }

    pub fn mission_button(&mut self, pressed: bool, x: f64, y: f64) {
        let hit = self.mission_hit(x, y);
        let Some(m) = self.mission.as_mut() else { return };
        if m.closing {
            return;
        }
        if pressed {
            m.press = match hit {
                Some(Hit::Window(id)) => Some((id, (x, y))),
                _ => None,
            };
            return;
        }
        let press = m.press.take();
        if let Some((id, _)) = m.drag.take() {
            // Dropped on a desktop tile: the window moves there; anywhere else it returns
            let target_space = strip(self.spaces.count, self.shell.screen_w as f32)
                .into_iter()
                .find(|(_, r)| r.contains(x as f32, y as f32))
                .and_then(|(h, _)| match h {
                    Hit::Space(s) => Some(s),
                    Hit::Add => Some(self.spaces.count),
                    Hit::Window(_) => None,
                });
            if let Some(space) = target_space.filter(|s| *s != self.spaces.current) {
                if space >= self.spaces.count {
                    self.spaces.count = (space + 1).min(crate::spaces::MAX_SPACES);
                }
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                    w.space = space.min(self.spaces.count - 1);
                }
                self.apply_space_visibility();
                self.refresh_focus();
                self.dock_model_dirty = true;
            }
            self.relayout_mission();
            return;
        }
        match hit {
            Some(Hit::Window(id)) if press.is_some_and(|(p, _)| p == id) => self.close_mission(Some(id)),
            Some(Hit::Space(s)) => {
                self.switch_space(s);
                self.close_mission(None);
            }
            Some(Hit::Add) => {
                self.spaces.count = (self.spaces.count + 1).min(crate::spaces::MAX_SPACES);
                self.sync_mission_overlay();
            }
            Some(Hit::Window(_)) => {}
            None => self.close_mission(None),
        }
        self.needs_redraw = true;
    }

    /// Strip tiles and the hovered window's title, drawn by the shell overlay
    pub fn sync_mission_overlay(&mut self) {
        let Some(m) = self.mission.as_ref().filter(|m| !m.closing) else {
            self.shell.mission = None;
            return;
        };
        let tiles = strip(self.spaces.count, self.shell.screen_w as f32)
            .into_iter()
            .map(|(hit, r)| {
                let label = match hit {
                    Hit::Space(s) => format!("Рабочий стол {}", s + 1),
                    _ => "+".into(),
                };
                let count = match hit {
                    Hit::Space(s) => self.windows.iter().filter(|w| w.mapped && !w.minimized && w.space == s).count(),
                    _ => 0,
                };
                crate::shell::MissionTile { label, rect: r, current: hit == Hit::Space(self.spaces.current), hover: m.hover == Some(hit), add: hit == Hit::Add, windows: count }
            })
            .collect();
        let title = match m.hover {
            Some(Hit::Window(id)) => m.targets.iter().find(|(w, _)| *w == id).and_then(|(_, r)| {
                self.windows.iter().find(|w| w.id == id).map(|w| {
                    let name = if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() };
                    (name, *r)
                })
            }),
            _ => None,
        };
        self.shell.mission = Some(crate::shell::MissionOverlay { tiles, title });
        self.shell.overlay_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_never_overlaps_or_enlarges_and_stays_in_its_area() {
        let area = R::new(24.0, 200.0, 1392.0, 560.0);
        for n in 1..=9 {
            let wins: Vec<_> = (0..n).map(|i| (i as u64, 600.0 + i as f32 * 40.0, 400.0)).collect();
            let out = layout(&wins, area);
            assert_eq!(out.len(), n);
            for (i, (id, r)) in out.iter().enumerate() {
                assert!(r.w <= wins[*id as usize].1 + 0.01, "not larger than the window");
                assert!(r.x >= area.x - 0.01 && r.y >= area.y - 0.01 && r.x + r.w <= area.x + area.w + 0.01 && r.y + r.h <= area.y + area.h + 0.01);
                for (_, o) in &out[i + 1..] {
                    let apart = r.x + r.w <= o.x || o.x + o.w <= r.x || r.y + r.h <= o.y || o.y + o.h <= r.y;
                    assert!(apart, "{n} windows overlap");
                }
            }
        }
        // One small window keeps its size
        let one = layout(&[(7, 300.0, 200.0)], area);
        assert_eq!((one[0].1.w, one[0].1.h), (300.0, 200.0));
        assert!(layout(&[], area).is_empty());
    }

    #[test]
    fn strip_has_one_tile_per_desktop_and_an_add_tile_until_full() {
        let s = strip(3, 1440.0);
        assert_eq!(s.len(), 4);
        assert_eq!(s[3].0, Hit::Add);
        assert!(s.windows(2).all(|p| p[0].1.x + p[0].1.w < p[1].1.x), "left to right, apart");
        assert!(strip(crate::spaces::MAX_SPACES, 4000.0).iter().all(|(h, _)| *h != Hit::Add));
    }
}
