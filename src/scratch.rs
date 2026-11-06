//! Подложка: a layer of tiled windows over every desktop, like Hyprland's special workspace.
//! Super+S slides it over the screen on a frosted backdrop and back; Super+Shift+S moves the
//! focused window there and back. Its windows live on a desktop number of their own, so the
//! Dock, Alt+Tab and Mission Control treat them like windows of another desktop. Nothing runs
//! while idle: tiles change only when the set of windows or the screen does.
use crate::banner::{BannerIcon, Message};
use crate::monitors::Rect;
use crate::FeatherCompositor;

/// The desktop number of its windows, next to Larvis's
pub const SCRATCH_SPACE: u32 = u32::MAX - 1;
const GAP: i32 = 10;

#[derive(Default)]
pub struct Scratch {
    /// Its windows, in tile order; stack order changes with every click
    order: Vec<u64>,
    /// What the tiles were last computed for
    laid: Option<(Vec<u64>, Rect)>,
}

/// One window is the whole screen and nothing else. More tile like Hyprland's dwindle: each takes
/// half of what is left, across the longer side.
pub fn layout(count: usize, area: Rect) -> Vec<Rect> {
    if count == 1 {
        return vec![area];
    }
    let mut rest = Rect { x: area.x + GAP, y: area.y + GAP, w: area.w - 2 * GAP, h: area.h - 2 * GAP };
    let mut rects = Vec::with_capacity(count);
    for i in 0..count {
        if i + 1 == count {
            rects.push(rest);
            break;
        }
        if rest.w >= rest.h {
            let w = (rest.w - GAP) / 2;
            rects.push(Rect { w, ..rest });
            rest = Rect { x: rest.x + w + GAP, w: rest.w - w - GAP, ..rest };
        } else {
            let h = (rest.h - GAP) / 2;
            rects.push(Rect { h, ..rest });
            rest = Rect { y: rest.y + h + GAP, h: rest.h - h - GAP, ..rest };
        }
    }
    rects
}

impl FeatherCompositor {
    /// The whole primary screen: the menu bar and the Dock step aside for it
    fn scratch_area(&self) -> Rect {
        self.primary_monitor().map(|m| m.info.rect).unwrap_or_else(|| self.monitor_at(0.0, 0.0))
    }

    /// Its windows that take a tile now
    fn scratch_tiled(&self) -> Vec<u64> {
        self.scratch
            .order
            .iter()
            .copied()
            .filter(|id| self.windows.iter().any(|w| w.id == *id && w.mapped && !w.minimized && !w.maximized && !w.fullscreen))
            .collect()
    }

    /// Follows windows that closed or left by other means, and a changed screen
    pub fn scratch_sync(&mut self) {
        if self.scratch.order.is_empty() && self.scratch.laid.is_none() {
            return;
        }
        // Moved away (the API, restored from the Dock): back to its own size
        let order = self.scratch.order.clone();
        for id in order {
            let idx = self.windows.iter().position(|w| w.id == id);
            match idx {
                Some(i) if self.windows[i].space == SCRATCH_SPACE => {}
                Some(i) => {
                    let r = self.window_monitor(i);
                    self.windows[i].leave_scratch(r);
                    self.scratch.order.retain(|o| *o != id);
                }
                None => self.scratch.order.retain(|o| *o != id),
            }
        }
        let ids = self.scratch_tiled();
        let area = self.scratch_area();
        if self.scratch.laid.as_ref() != Some(&(ids.clone(), area)) {
            self.scratch_apply(&ids, area);
        }
        let count = self.scratch.order.len();
        if self.shell.scratch_count != count {
            self.shell.scratch_count = count;
            self.shell.topbar_dirty = true;
        }
        if count == 0 && self.shell.scratch_open {
            self.set_scratch_open(false);
        }
    }

    fn scratch_apply(&mut self, ids: &[u64], area: Rect) {
        for (id, rect) in ids.iter().zip(layout(ids.len(), area)) {
            if let Some(win) = self.windows.iter_mut().find(|w| w.id == *id) {
                win.fit_tile_frame(rect);
            }
        }
        self.scratch.laid = (!self.scratch.order.is_empty()).then(|| (ids.to_vec(), area));
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
    }

    /// Super+S and the menu bar's item
    pub fn toggle_scratch(&mut self) {
        if self.shell.scratch_open {
            self.set_scratch_open(false);
        } else if self.scratch.order.is_empty() {
            self.show_banner(Message { icon: BannerIcon::Scratch, title: "Подложка пуста", detail: "Super + Shift + S переносит сюда окно с фокусом" });
        } else {
            self.set_scratch_open(true);
        }
    }

    pub fn set_scratch_open(&mut self, open: bool) {
        if open == self.shell.scratch_open || (open && (self.lock.active || self.scratch.order.is_empty())) {
            return;
        }
        if open {
            if self.mission_open() {
                self.close_mission(None);
            }
            self.leave_larvis_desktop();
        }
        // The Подложка itself is the answer now
        self.banner = Default::default();
        self.shell.scratch_open = open;
        self.animations.set_scratch(open, self.shell.screen_h as f64);
        self.apply_space_visibility();
        if open {
            // Over every window of the desktop, in the order they were last used
            let (mut own, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.windows).into_iter().partition(|w| w.space == SCRATCH_SPACE);
            self.windows = rest;
            self.windows.append(&mut own);
        }
        self.shell.close_menus();
        self.unfocus_desktop();
        self.shell.topbar_dirty = true;
        self.dock_model_dirty = true;
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
        self.refresh_focus();
    }

    /// Super+Shift+S: the focused window there, or back to the desktop in front
    pub fn scratch_move_focused(&mut self) {
        let Some(idx) = self.windows.iter().rposition(|w| w.is_visible()) else { return };
        if self.windows[idx].larvis {
            return;
        }
        if self.windows[idx].space == SCRATCH_SPACE {
            let id = self.windows[idx].id;
            let mut win = self.windows.remove(idx);
            let r = self.monitor_at(win.x as f64, win.y as f64);
            win.leave_scratch(r);
            win.space = self.spaces.current;
            win.off_space = false;
            self.scratch.order.retain(|o| *o != id);
            // Under the rest of the Подложка, which stays over the desktop
            let at = self.windows.iter().position(|w| w.space == SCRATCH_SPACE).unwrap_or(self.windows.len());
            self.windows.insert(at, win);
            self.dock_model_dirty = true;
            self.refresh_focus();
            self.window_memory_changed();
            return;
        }
        self.scratch_adopt(idx);
        if !self.shell.scratch_open {
            self.show_banner(Message { icon: BannerIcon::Scratch, title: "Окно на подложке", detail: "Super + S показывает и прячет подложку" });
        }
        self.dock_model_dirty = true;
        self.refresh_focus();
    }

    /// A window joins: moved there, or opened while the Подложка is in front
    pub fn scratch_adopt(&mut self, idx: usize) {
        if self.windows[idx].fullscreen {
            let r = self.window_monitor(idx);
            self.set_window_fullscreen(idx, false, r);
        }
        let open = self.shell.scratch_open;
        let win = &mut self.windows[idx];
        win.resize_anchor = None;
        win.enter_scratch();
        win.space = SCRATCH_SPACE;
        win.off_space = !open;
        let id = win.id;
        self.scratch.order.push(id);
        self.scratch_sync();
        self.window_memory_changed();
    }

    /// The button went up after dragging one of its windows: it takes the tile it was dropped
    /// on, and everything snaps back into place
    pub fn scratch_drop(&mut self, win_id: u64) {
        if !self.windows.iter().any(|w| w.id == win_id && w.space == SCRATCH_SPACE) {
            return;
        }
        let ids = self.scratch_tiled();
        let area = self.scratch_area();
        let rects = layout(ids.len(), area);
        let (px, py) = (self.pointer_location.x, self.pointer_location.y);
        let target = rects.iter().position(|r| r.contains(px, py)).and_then(|slot| ids.get(slot).copied());
        if let (Some(target), Some(from)) = (target, self.scratch.order.iter().position(|o| *o == win_id)) {
            if let Some(to) = self.scratch.order.iter().position(|o| *o == target) {
                self.scratch.order.swap(from, to);
            }
        }
        let ids = self.scratch_tiled();
        self.scratch_apply(&ids, area);
    }

    pub fn scratch_contains(&self, px: f64, py: f64) -> bool {
        self.windows.iter().any(|w| w.space == SCRATCH_SPACE && w.is_visible() && (w.contains_point(px, py) || w.resize_edges_at(px, py).is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };

    #[test]
    fn one_window_takes_the_whole_area() {
        assert_eq!(layout(1, AREA), [AREA]);
        assert!(layout(0, AREA).is_empty());
    }

    #[test]
    fn tiles_never_overlap_and_stay_inside() {
        for count in 2..=9 {
            let rects = layout(count, AREA);
            assert_eq!(rects.len(), count);
            for (i, a) in rects.iter().enumerate() {
                assert!(a.x >= AREA.x + GAP && a.y >= AREA.y + GAP, "{count}: {a:?}");
                assert!(a.x + a.w <= AREA.x + AREA.w - GAP && a.y + a.h <= AREA.y + AREA.h - GAP, "{count}: {a:?}");
                for b in &rects[i + 1..] {
                    assert_eq!(a.intersection(*b), 0, "{count}: {a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn each_window_halves_what_is_left_across_the_longer_side() {
        let two = layout(2, AREA);
        assert_eq!(two[0].h, two[1].h, "side by side on a wide screen");
        assert_eq!(two[0].x + two[0].w + GAP, two[1].x);
        let three = layout(3, AREA);
        assert_eq!(three[1].w, three[2].w, "the right half splits in two");
        assert_eq!(three[1].y + three[1].h + GAP, three[2].y);
    }
}
