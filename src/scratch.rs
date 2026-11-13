//! Подложка: a layer of tiled windows over every desktop, like Hyprland's special workspace.
//! Super+S slides it over the screen on a frosted backdrop and back; Super+Shift+S moves the
//! focused window there and back. Its windows live on a desktop number of their own, so the
//! Dock, Alt+Tab and Mission Control treat them like windows of another desktop. Nothing runs
//! while idle: tiles change only when the set of windows, their splits, or the screen does.
use std::time::Instant;

use crate::banner::{BannerIcon, Message};
use crate::monitors::Rect;
use crate::tiling::Dir;
use crate::window::{Edges, MIN_WINDOW_H, MIN_WINDOW_W};
use crate::FeatherCompositor;

/// The desktop number of its windows, next to Larvis's
pub const SCRATCH_SPACE: u32 = u32::MAX - 1;
const GAP: i32 = 10;
const DOUBLE_CLICK_MS: u128 = 350;
const KEY_RESIZE_STEP: f32 = 0.06;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Split {
    pub ratio: f32,
    /// `None`: across the longer side (`w >= h`); `Some(true)`: left/right; `Some(false)`: top/bottom
    pub side_by_side: Option<bool>,
}

impl Default for Split {
    fn default() -> Self {
        Self {
            ratio: 0.5,
            side_by_side: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileSlot {
    pub rect: Rect,
    /// Parent area divided by split `slot` (meaningful when `slot + 1 < count`)
    pub split_parent: Rect,
    /// Split index moved by dragging this tile's `(left, right, top, bottom)` edge
    pub splits: (Option<usize>, Option<usize>, Option<usize>, Option<usize>),
}

impl TileSlot {
    pub fn inner_edges(&self) -> Edges {
        Edges {
            left: self.splits.0.is_some(),
            right: self.splits.1.is_some(),
            top: self.splits.2.is_some(),
            bottom: self.splits.3.is_some(),
        }
    }
}

#[derive(Default)]
pub struct Scratch {
    /// Its windows, in tile order; stack order changes with every click
    order: Vec<u64>,
    /// User-adjusted split ratios and orientations for `order.len() - 1` dividers
    splits: Vec<Split>,
    /// Snapshot of `splits` captured when a mouse divider drag starts
    drag_splits: Option<Vec<Split>>,
    /// Single tile temporarily zoomed across the whole Подложка (`Super+F`)
    zoomed: Option<u64>,
    /// Double-clicking an inner divider resets its split(s) to 50/50
    last_divider_click: Option<(Instant, (Option<usize>, Option<usize>))>,
    /// What the tiles were last computed for
    laid: Option<(Vec<u64>, Rect, Vec<Split>, Option<u64>)>,
}

impl Scratch {
    pub fn is_hidden_by_zoom(&self, win_id: u64) -> bool {
        self.zoomed
            .is_some_and(|z| z != win_id && self.order.len() > 1 && self.order.contains(&z))
    }

    pub fn invalidate(&mut self) {
        self.laid = None;
    }

    pub fn set_zoomed(&mut self, zoomed: Option<u64>) {
        self.zoomed = zoomed;
        self.laid = None;
    }

    fn ensure_splits(&mut self, count: usize) {
        let needed = count.saturating_sub(1);
        if self.splits.len() > needed {
            self.splits.truncate(needed);
        } else {
            while self.splits.len() < needed {
                self.splits.push(Split::default());
            }
        }
    }
}

/// One window is the whole screen and nothing else. More tile like Hyprland's dwindle: each takes
/// half (or its configured split ratio) of what is left, across the longer side by default.
pub fn layout(count: usize, area: Rect) -> Vec<Rect> {
    layout_slots(count, area, &[]).into_iter().map(|s| s.rect).collect()
}

pub fn layout_slots(count: usize, area: Rect, splits: &[Split]) -> Vec<TileSlot> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![TileSlot {
            rect: area,
            split_parent: area,
            splits: (None, None, None, None),
        }];
    }
    let mut rest = Rect {
        x: area.x + GAP,
        y: area.y + GAP,
        w: (area.w - 2 * GAP).max(2),
        h: (area.h - 2 * GAP).max(2),
    };
    let mut rest_left_split: Option<usize> = None;
    let mut rest_top_split: Option<usize> = None;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        if i + 1 == count {
            out.push(TileSlot {
                rect: rest,
                split_parent: rest,
                splits: (rest_left_split, None, rest_top_split, None),
            });
            break;
        }
        let sp = splits.get(i).copied().unwrap_or_default();
        let side_by_side = sp.side_by_side.unwrap_or(rest.w >= rest.h);
        let parent = rest;
        if side_by_side {
            let usable = (rest.w - GAP).max(2);
            let min_w = MIN_WINDOW_W.min(usable / 2).max(1);
            let raw_w = if (sp.ratio - 0.5).abs() < 1e-5 {
                usable / 2
            } else {
                (usable as f32 * sp.ratio).round() as i32
            };
            let w = raw_w.clamp(min_w, (usable - min_w).max(min_w));
            out.push(TileSlot {
                rect: Rect { w, ..rest },
                split_parent: parent,
                splits: (rest_left_split, Some(i), rest_top_split, None),
            });
            rest = Rect {
                x: rest.x + w + GAP,
                w: (usable - w).max(1),
                ..rest
            };
            rest_left_split = Some(i);
        } else {
            let usable = (rest.h - GAP).max(2);
            let min_h = MIN_WINDOW_H.min(usable / 2).max(1);
            let raw_h = if (sp.ratio - 0.5).abs() < 1e-5 {
                usable / 2
            } else {
                (usable as f32 * sp.ratio).round() as i32
            };
            let h = raw_h.clamp(min_h, (usable - min_h).max(min_h));
            out.push(TileSlot {
                rect: Rect { h, ..rest },
                split_parent: parent,
                splits: (rest_left_split, None, rest_top_split, Some(i)),
            });
            rest = Rect {
                y: rest.y + h + GAP,
                h: (usable - h).max(1),
                ..rest
            };
            rest_top_split = Some(i);
        }
    }
    out
}

/// Nearest adjacent tile from `from` in direction `dir`
pub fn neighbor_slot(slots: &[TileSlot], from: usize, dir: Dir) -> Option<usize> {
    let a = slots.get(from)?.rect;
    slots
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != from)
        .filter_map(|(i, slot)| {
            let b = slot.rect;
            let overlap_x = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
            let overlap_y = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
            let dist = match dir {
                Dir::Left if b.x + b.w <= a.x && overlap_y > 0 => a.x - (b.x + b.w),
                Dir::Right if b.x >= a.x + a.w && overlap_y > 0 => b.x - (a.x + a.w),
                Dir::Up if b.y + b.h <= a.y && overlap_x > 0 => a.y - (b.y + b.h),
                Dir::Down if b.y >= a.y + a.h && overlap_x > 0 => b.y - (a.y + a.h),
                _ => return None,
            };
            Some(((dist, b.y, b.x), i))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, i)| i)
}

fn splits_for_edges(slot: &TileSlot, edges: Edges) -> (Option<usize>, Option<usize>) {
    let sx = if edges.right {
        slot.splits.1
    } else if edges.left {
        slot.splits.0
    } else {
        None
    };
    let sy = if edges.bottom {
        slot.splits.3
    } else if edges.top {
        slot.splits.2
    } else {
        None
    };
    (sx, sy)
}

impl FeatherCompositor {
    /// The whole primary screen: the menu bar and the Dock step aside for it
    pub(crate) fn scratch_area(&self) -> Rect {
        self.primary_monitor()
            .map(|m| m.info.rect)
            .unwrap_or_else(|| self.monitor_at(0.0, 0.0))
    }

    /// Its windows that take a tile now
    pub(crate) fn scratch_tiled(&self) -> Vec<u64> {
        self.scratch
            .order
            .iter()
            .copied()
            .filter(|id| {
                self.windows.iter().any(|w| {
                    w.id == *id && w.mapped && !w.minimized && !w.maximized && !w.fullscreen
                })
            })
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
        if self
            .scratch
            .zoomed
            .is_some_and(|z| !self.scratch.order.contains(&z) || self.scratch.order.len() <= 1)
        {
            self.scratch.zoomed = None;
            self.apply_space_visibility();
        }
        let ids = self.scratch_tiled();
        self.scratch.ensure_splits(ids.len());
        if self.scratch.order.is_empty() {
            self.scratch.splits.clear();
            self.scratch.drag_splits = None;
            self.scratch.zoomed = None;
        }
        let area = self.scratch_area();
        let state_key = (
            ids.clone(),
            area,
            self.scratch.splits.clone(),
            self.scratch.zoomed,
        );
        if self.scratch.laid.as_ref() != Some(&state_key) {
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

    pub(crate) fn scratch_apply(&mut self, ids: &[u64], area: Rect) {
        self.scratch.ensure_splits(ids.len());
        let zoomed = self
            .scratch
            .zoomed
            .filter(|z| ids.len() > 1 && ids.contains(z));
        if let Some(zid) = zoomed {
            for id in ids {
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == *id) {
                    win.scratch_resize_edges = Edges::default();
                    if *id == zid {
                        win.fit_tile_frame(area);
                    }
                }
            }
            self.apply_space_visibility();
        } else {
            let slots = layout_slots(ids.len(), area, &self.scratch.splits);
            for (id, slot) in ids.iter().zip(slots) {
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == *id) {
                    win.scratch_resize_edges = slot.inner_edges();
                    win.fit_tile_frame(slot.rect);
                }
            }
        }
        self.scratch.laid = (!self.scratch.order.is_empty()).then(|| {
            (
                ids.to_vec(),
                area,
                self.scratch.splits.clone(),
                zoomed,
            )
        });
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
    }

    /// Super+S and the menu bar's item
    pub fn toggle_scratch(&mut self) {
        if self.shell.scratch_open {
            self.set_scratch_open(false);
        } else if self.scratch.order.is_empty() {
            self.show_banner(Message {
                icon: BannerIcon::Scratch,
                title: "Подложка пуста",
                detail: "Super + Shift + S переносит сюда окно с фокусом",
            });
        } else {
            self.set_scratch_open(true);
        }
    }

    pub fn set_scratch_open(&mut self, open: bool) {
        if open == self.shell.scratch_open
            || (open && (self.lock.active || self.scratch.order.is_empty()))
        {
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
            let (mut own, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.windows)
                .into_iter()
                .partition(|w| w.space == SCRATCH_SPACE);
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
        let Some(idx) = self.windows.iter().rposition(|w| w.is_visible()) else {
            return;
        };
        if self.windows[idx].larvis {
            return;
        }
        if self.windows[idx].space == SCRATCH_SPACE {
            let id = self.windows[idx].id;
            if self.scratch.zoomed == Some(id) {
                self.scratch.zoomed = None;
            }
            let mut win = self.windows.remove(idx);
            let r = self.monitor_at(win.x as f64, win.y as f64);
            win.leave_scratch(r);
            win.space = self.spaces.current;
            win.off_space = false;
            self.scratch.order.retain(|o| *o != id);
            self.apply_space_visibility();
            // Under the rest of the Подложка, which stays over the desktop
            let at = self
                .windows
                .iter()
                .position(|w| w.space == SCRATCH_SPACE)
                .unwrap_or(self.windows.len());
            self.windows.insert(at, win);
            self.scratch_sync();
            self.dock_model_dirty = true;
            self.refresh_focus();
            self.window_memory_changed();
            return;
        }
        self.scratch_adopt(idx);
        if !self.shell.scratch_open {
            self.set_scratch_open(true);
        } else {
            self.dock_model_dirty = true;
            self.refresh_focus();
        }
    }

    /// A window joins: moved there, or opened while the Подложка is in front
    pub fn scratch_adopt(&mut self, idx: usize) {
        if self.windows[idx].fullscreen {
            let r = self.window_monitor(idx);
            self.set_window_fullscreen(idx, false, r);
        }
        self.scratch.zoomed = None;
        let open = self.shell.scratch_open;
        let win = &mut self.windows[idx];
        win.resize_anchor = None;
        win.enter_scratch();
        win.space = SCRATCH_SPACE;
        win.off_space = !open;
        let id = win.id;
        if !self.scratch.order.contains(&id) {
            self.scratch.order.push(id);
        }
        self.apply_space_visibility();
        self.scratch_sync();
        self.window_memory_changed();
    }

    /// Super+F on the Подложка: zoom the focused tile across the whole screen and back
    pub fn scratch_toggle_zoom(&mut self) {
        let ids = self.scratch_tiled();
        if ids.len() < 2 {
            return;
        }
        let Some(top_id) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == SCRATCH_SPACE && w.is_visible())
            .map(|w| w.id)
        else {
            return;
        };
        if self.scratch.zoomed == Some(top_id) {
            self.scratch.zoomed = None;
        } else {
            self.scratch.zoomed = Some(top_id);
        }
        self.apply_space_visibility();
        let area = self.scratch_area();
        self.scratch_apply(&ids, area);
        self.refresh_focus();
    }

    /// Super+arrows on the Подложка: move focus to the adjacent tile
    pub fn scratch_focus_dir(&mut self, dir: Dir) {
        let ids = self.scratch_tiled();
        if ids.len() < 2 {
            return;
        }
        let Some(active_id) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == SCRATCH_SPACE && w.is_visible())
            .map(|w| w.id)
        else {
            return;
        };
        if self.scratch.zoomed.take().is_some() {
            self.apply_space_visibility();
            let area = self.scratch_area();
            self.scratch_apply(&ids, area);
        }
        let Some(from) = ids.iter().position(|id| *id == active_id) else {
            return;
        };
        let area = self.scratch_area();
        let slots = layout_slots(ids.len(), area, &self.scratch.splits);
        let Some(to) = neighbor_slot(&slots, from, dir) else {
            return;
        };
        let target_id = ids[to];
        if let Some(idx) = self.windows.iter().position(|w| w.id == target_id) {
            let win = self.windows.remove(idx);
            self.windows.push(win);
            self.pointer_focus_dirty = true;
            self.needs_redraw = true;
            self.refresh_focus();
        }
    }

    /// Super+Shift+arrows on the Подложка: swap with the neighbor in `dir`, or rotate the
    /// split axis when there is no neighbor along that axis (e.g. side-by-side ↔ top/bottom).
    pub fn scratch_swap_dir(&mut self, dir: Dir) {
        let ids = self.scratch_tiled();
        if ids.len() < 2 {
            return;
        }
        let Some(active_id) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == SCRATCH_SPACE && w.is_visible())
            .map(|w| w.id)
        else {
            return;
        };
        if self.scratch.zoomed.take().is_some() {
            self.apply_space_visibility();
        }
        let Some(from) = ids.iter().position(|id| *id == active_id) else {
            return;
        };
        let area = self.scratch_area();
        self.scratch.ensure_splits(ids.len());
        let slots = layout_slots(ids.len(), area, &self.scratch.splits);
        if let Some(to) = neighbor_slot(&slots, from, dir) {
            let other_id = ids[to];
            if let (Some(a), Some(b)) = (
                self.scratch.order.iter().position(|id| *id == active_id),
                self.scratch.order.iter().position(|id| *id == other_id),
            ) {
                self.scratch.order.swap(a, b);
            }
        } else {
            // Rotate the enclosing split onto the requested axis so two side-by-side tiles can
            // become top/bottom (and vice versa) with a single chord.
            let sp_idx = from.min(ids.len() - 2);
            let want_side_by_side = matches!(dir, Dir::Left | Dir::Right);
            self.scratch.splits[sp_idx].side_by_side = Some(want_side_by_side);
            let want_first = matches!(dir, Dir::Left | Dir::Up);
            let is_first = from == sp_idx;
            if want_first != is_first && sp_idx + 1 < ids.len() {
                let id_a = ids[sp_idx];
                let id_b = ids[sp_idx + 1];
                if let (Some(a), Some(b)) = (
                    self.scratch.order.iter().position(|id| *id == id_a),
                    self.scratch.order.iter().position(|id| *id == id_b),
                ) {
                    self.scratch.order.swap(a, b);
                }
            }
        }
        let new_ids = self.scratch_tiled();
        self.scratch_apply(&new_ids, area);
    }

    /// Ctrl+Super+arrows / Alt+Super+arrows on the Подложка: move the active tile's split divider
    pub fn scratch_resize_step(&mut self, dir: Dir) {
        let ids = self.scratch_tiled();
        if ids.len() < 2 {
            return;
        }
        let Some(active_id) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == SCRATCH_SPACE && w.is_visible())
            .map(|w| w.id)
        else {
            return;
        };
        if self.scratch.zoomed.take().is_some() {
            self.apply_space_visibility();
        }
        let Some(slot_idx) = ids.iter().position(|id| *id == active_id) else {
            return;
        };
        let area = self.scratch_area();
        self.scratch.ensure_splits(ids.len());
        let slots = layout_slots(ids.len(), area, &self.scratch.splits);
        let slot = slots[slot_idx];
        match dir {
            Dir::Left | Dir::Right => {
                let Some(sp_idx) = slot.splits.1.or(slot.splits.0) else {
                    return;
                };
                let usable = (slots[sp_idx].split_parent.w - GAP).max(2);
                let min_w = MIN_WINDOW_W.min(usable / 2).max(1);
                let step = ((usable as f32 * KEY_RESIZE_STEP).round() as i32).max(24);
                let delta = if dir == Dir::Right { step } else { -step };
                let w = (slots[sp_idx].rect.w + delta).clamp(min_w, (usable - min_w).max(min_w));
                self.scratch.splits[sp_idx].ratio = w as f32 / usable as f32;
            }
            Dir::Up | Dir::Down => {
                let Some(sp_idx) = slot.splits.3.or(slot.splits.2) else {
                    return;
                };
                let usable = (slots[sp_idx].split_parent.h - GAP).max(2);
                let min_h = MIN_WINDOW_H.min(usable / 2).max(1);
                let step = ((usable as f32 * KEY_RESIZE_STEP).round() as i32).max(24);
                let delta = if dir == Dir::Down { step } else { -step };
                let h = (slots[sp_idx].rect.h + delta).clamp(min_h, (usable - min_h).max(min_h));
                self.scratch.splits[sp_idx].ratio = h as f32 / usable as f32;
            }
        }
        self.scratch_apply(&ids, area);
    }

    /// Super+0 / Ctrl+Super+0: reset every split back to equal halves
    pub fn scratch_equalize(&mut self) {
        let ids = self.scratch_tiled();
        if ids.is_empty() {
            return;
        }
        if self.scratch.zoomed.take().is_some() {
            self.apply_space_visibility();
        }
        for sp in &mut self.scratch.splits {
            sp.ratio = 0.5;
        }
        let area = self.scratch_area();
        self.scratch_apply(&ids, area);
    }

    /// Inner resize edges for `Super + RMB` anywhere inside a Подложка tile
    pub fn scratch_super_resize_edges(&self, idx: usize) -> Option<Edges> {
        let win = &self.windows[idx];
        if win.space != SCRATCH_SPACE {
            return None;
        }
        let e = win.scratch_resize_edges;
        let right = e.right;
        let left = !right && e.left;
        let bottom = e.bottom;
        let top = !bottom && e.top;
        let out = Edges {
            left,
            right,
            top,
            bottom,
        };
        out.any().then_some(out)
    }

    /// Captures split state when a divider drag starts. Returns `true` if a double-click reset
    /// the divider to 50/50 instead of starting a drag.
    pub fn scratch_begin_resize(&mut self, win_id: u64, edges: Edges, allow_double_click: bool) -> bool {
        if !self
            .windows
            .iter()
            .any(|w| w.id == win_id && w.space == SCRATCH_SPACE)
        {
            return false;
        }
        let ids = self.scratch_tiled();
        let area = self.scratch_area();
        self.scratch.ensure_splits(ids.len());
        let slots = layout_slots(ids.len(), area, &self.scratch.splits);
        if let Some(slot_idx) = ids.iter().position(|id| *id == win_id) {
            let pair = splits_for_edges(&slots[slot_idx], edges);
            let now = Instant::now();
            if allow_double_click
                && (pair.0.is_some() || pair.1.is_some())
                && self.scratch.last_divider_click.is_some_and(|(t, prev)| {
                    prev == pair && now.duration_since(t).as_millis() < DOUBLE_CLICK_MS
                })
            {
                self.scratch.last_divider_click = None;
                if let Some(i) = pair.0 {
                    self.scratch.splits[i].ratio = 0.5;
                }
                if let Some(i) = pair.1 {
                    self.scratch.splits[i].ratio = 0.5;
                }
                self.scratch_apply(&ids, area);
                return true;
            }
            if allow_double_click {
                self.scratch.last_divider_click = Some((now, pair));
            }
        }
        self.scratch.drag_splits = Some(self.scratch.splits.clone());
        false
    }

    /// Live divider resize during a mouse drag on the Подложка
    pub fn scratch_resize_drag(&mut self, win_id: u64, dx: i32, dy: i32, edges: Edges) -> bool {
        if !self
            .windows
            .iter()
            .any(|w| w.id == win_id && w.space == SCRATCH_SPACE)
        {
            return false;
        }
        let ids = self.scratch_tiled();
        let Some(slot_idx) = ids.iter().position(|id| *id == win_id) else {
            return true;
        };
        let area = self.scratch_area();
        self.scratch.ensure_splits(ids.len());
        let base_splits = self
            .scratch
            .drag_splits
            .clone()
            .unwrap_or_else(|| self.scratch.splits.clone());
        let base = layout_slots(ids.len(), area, &base_splits);
        let (sx, sy) = splits_for_edges(&base[slot_idx], edges);
        if let Some(i) = sx {
            let usable = (base[i].split_parent.w - GAP).max(2);
            let min_w = MIN_WINDOW_W.min(usable / 2).max(1);
            let w = (base[i].rect.w + dx).clamp(min_w, (usable - min_w).max(min_w));
            self.scratch.splits[i].ratio = w as f32 / usable as f32;
        }
        if let Some(i) = sy {
            let usable = (base[i].split_parent.h - GAP).max(2);
            let min_h = MIN_WINDOW_H.min(usable / 2).max(1);
            let h = (base[i].rect.h + dy).clamp(min_h, (usable - min_h).max(min_h));
            self.scratch.splits[i].ratio = h as f32 / usable as f32;
        }
        self.scratch_apply(&ids, area);
        true
    }

    pub fn scratch_end_resize(&mut self) {
        self.scratch.drag_splits = None;
    }

    /// Highlights the destination tile slot while dragging a window on the Подложка
    pub fn scratch_drag_preview(&mut self, win_id: u64, px: f64, py: f64) {
        let ids = self.scratch_tiled();
        let target = if ids.len() >= 2 && self.scratch.zoomed.is_none() {
            let area = self.scratch_area();
            let slots = layout_slots(ids.len(), area, &self.scratch.splits);
            slots
                .iter()
                .position(|s| s.rect.contains(px, py))
                .filter(|&slot| ids[slot] != win_id)
                .map(|slot| (win_id, slots[slot].rect, crate::tiling::Step::Fill))
        } else {
            None
        };
        if self.shell.tile_preview.target != target {
            if target.is_some() {
                self.shell.tile_preview.ensure();
            }
            self.shell.tile_preview.target = target;
            self.needs_redraw = true;
        }
    }

    /// The button went up after moving one of its windows: it takes the tile it was dropped
    /// on, and everything snaps back into place
    pub fn scratch_drop(&mut self, win_id: u64) {
        if !self
            .windows
            .iter()
            .any(|w| w.id == win_id && w.space == SCRATCH_SPACE)
        {
            return;
        }
        let ids = self.scratch_tiled();
        let area = self.scratch_area();
        let slots = layout_slots(ids.len(), area, &self.scratch.splits);
        let (px, py) = (self.pointer_location.x, self.pointer_location.y);
        let target = slots
            .iter()
            .position(|s| s.rect.contains(px, py))
            .and_then(|slot| ids.get(slot).copied());
        if let (Some(target), Some(from)) = (
            target,
            self.scratch.order.iter().position(|o| *o == win_id),
        ) {
            if let Some(to) = self.scratch.order.iter().position(|o| *o == target) {
                self.scratch.order.swap(from, to);
            }
        }
        let ids = self.scratch_tiled();
        self.scratch_apply(&ids, area);
    }

    pub fn scratch_contains(&self, px: f64, py: f64) -> bool {
        self.windows.iter().any(|w| {
            w.space == SCRATCH_SPACE
                && w.is_visible()
                && (w.contains_point(px, py) || w.resize_edges_at(px, py).is_some())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };

    #[test]
    fn one_window_takes_the_whole_area() {
        assert_eq!(layout(1, AREA), [AREA]);
        assert!(layout(0, AREA).is_empty());
        assert!(!layout_slots(1, AREA, &[])[0].inner_edges().any());
    }

    #[test]
    fn tiles_never_overlap_and_stay_inside() {
        for count in 2..=9 {
            let rects = layout(count, AREA);
            assert_eq!(rects.len(), count);
            for (i, a) in rects.iter().enumerate() {
                assert!(
                    a.x >= AREA.x + GAP && a.y >= AREA.y + GAP,
                    "{count}: {a:?}"
                );
                assert!(
                    a.x + a.w <= AREA.x + AREA.w - GAP && a.y + a.h <= AREA.y + AREA.h - GAP,
                    "{count}: {a:?}"
                );
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

    #[test]
    fn custom_splits_and_orientations_resize_and_preserve_gap() {
        let splits = [
            Split {
                ratio: 0.7,
                side_by_side: Some(true),
            },
            Split {
                ratio: 0.3,
                side_by_side: Some(false),
            },
        ];
        let slots = layout_slots(3, AREA, &splits);
        assert!(slots[0].rect.w > slots[1].rect.w);
        assert!(slots[1].rect.h < slots[2].rect.h);
        assert_eq!(slots[0].rect.x + slots[0].rect.w + GAP, slots[1].rect.x);
        assert_eq!(slots[1].rect.y + slots[1].rect.h + GAP, slots[2].rect.y);
        assert_eq!(slots[0].splits, (None, Some(0), None, None));
        assert_eq!(slots[1].splits, (Some(0), None, None, Some(1)));
        assert_eq!(slots[2].splits, (Some(0), None, Some(1), None));

        // Top/bottom two-tile override on a wide monitor
        let vertical = layout_slots(
            2,
            AREA,
            &[Split {
                ratio: 0.5,
                side_by_side: Some(false),
            }],
        );
        assert_eq!(vertical[0].rect.w, vertical[1].rect.w);
        assert_eq!(vertical[0].rect.y + vertical[0].rect.h + GAP, vertical[1].rect.y);
    }

    #[test]
    fn neighbor_lookup_walks_between_adjacent_tiles() {
        let slots = layout_slots(3, AREA, &[]);
        assert_eq!(neighbor_slot(&slots, 0, Dir::Right), Some(1));
        assert_eq!(neighbor_slot(&slots, 0, Dir::Left), None);
        assert_eq!(neighbor_slot(&slots, 1, Dir::Down), Some(2));
        assert_eq!(neighbor_slot(&slots, 2, Dir::Up), Some(1));
        assert_eq!(neighbor_slot(&slots, 2, Dir::Left), Some(0));
    }
}
