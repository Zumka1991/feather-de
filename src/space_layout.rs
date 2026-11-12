//! Per-desktop window layout modes:
//! - `Free` — classic macOS floating windows with edge/quarter snap (`src/tiling.rs`);
//! - `Tile` — automatic BSP/dwindle tiling inside the desktop work area;
//! - `Tape` — infinite horizontal scrolling ribbon of full-height columns (Niri-style).
//!
//! Zero work while idle: layout runs only when a desktop's windows, mode, splits/widths,
//! active column, or monitor work area change. Ribbon scrolling uses an analytic GPU spring
//! (`Animations::scroll_tape`) so client buffers are never repainted while gliding.

use std::time::Instant;

use crate::banner::{BannerIcon, Message};
use crate::monitors::Rect;
use crate::scratch::{neighbor_slot, Split, TileSlot};
use crate::spaces::MAX_SPACES;
use crate::tiling::{work_area, Dir, Step};
use crate::window::{Edges, MIN_WINDOW_H, MIN_WINDOW_W};
use crate::FeatherCompositor;

pub(crate) const TILE_GAP: i32 = 6;
const TAPE_PAD: i32 = 6;
pub(crate) const TAPE_GAP: i32 = 8;
pub(crate) const TAPE_PEEK: i32 = 28;
const DOUBLE_CLICK_MS: u128 = 350;
const KEY_RESIZE_STEP: f32 = 0.06;

/// Preset column widths cycled by `Super + R` in `Tape` mode: 1/2 → 2/3 → Full → 1/3.
pub const TAPE_PRESETS: [f32; 4] = [0.5, 2.0 / 3.0, 1.0, 1.0 / 3.0];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpaceMode {
    #[default]
    Free,
    Tile,
    Tape,
}

impl SpaceMode {
    pub const ALL: [Self; 3] = [Self::Free, Self::Tile, Self::Tape];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Tile => "tile",
            Self::Tape => "tape",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "free" | "float" | "floating" | "macos" => Some(Self::Free),
            "tile" | "tiled" | "bsp" | "grid" => Some(Self::Tile),
            "tape" | "ribbon" | "scroll" | "niri" | "strip" => Some(Self::Tape),
            _ => None,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Free => Self::Tile,
            Self::Tile => Self::Tape,
            Self::Tape => Self::Free,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Free => "Свободные окна",
            Self::Tile => "Авто-плитка",
            Self::Tape => "Бесконечная лента",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Free => "Окна",
            Self::Tile => "Плитка",
            Self::Tape => "Лента",
        }
    }

    pub const fn banner_detail(self) -> &'static str {
        match self {
            Self::Free => "Окна свободно перемещаются и прилипают к краям",
            Self::Tile => "Окна делят стол автоматически · Super + стрелки",
            Self::Tape => "Горизонтальная лента окон · Super + ←/→ и колесо",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DividerTarget {
    Tile(Option<usize>, Option<usize>),
    Tape(u64),
}

#[derive(Clone, Default)]
struct PerSpace {
    mode: SpaceMode,
    order: Vec<u64>,
    splits: Vec<Split>,
    drag_splits: Option<Vec<Split>>,
    zoomed: Option<u64>,
    /// Per-window column width fraction `(win_id, frac)` in `Tape` mode (default `0.5`).
    widths: Vec<(u64, f32)>,
    /// Previous fraction saved when `Super + F` expands a `Tape` column to `1.0`.
    saved_before_full: Option<(u64, f32)>,
    /// Starting pixel width `(win_id, px_w)` during a mouse drag of a `Tape` column border.
    drag_width: Option<(u64, i32, i32)>,
    scroll: i32,
    focused_col: Option<u64>,
    last_divider_click: Option<(Instant, DividerTarget)>,
    laid_tile: Option<(Vec<u64>, Rect, Vec<Split>, Option<u64>)>,
    laid_tape: Option<(Vec<u64>, Rect, Vec<(u64, i32)>, i32)>,
}

impl PerSpace {
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

    fn width_frac(&self, win_id: u64) -> f32 {
        self.widths
            .iter()
            .find(|(id, _)| *id == win_id)
            .map_or(0.5, |(_, f)| *f)
    }

    fn set_width_frac(&mut self, win_id: u64, frac: f32) {
        let clamped = frac.clamp(0.15, 1.0);
        if let Some((_, f)) = self.widths.iter_mut().find(|(id, _)| *id == win_id) {
            *f = clamped;
        } else {
            self.widths.push((win_id, clamped));
        }
    }

    fn invalidate(&mut self) {
        self.laid_tile = None;
        self.laid_tape = None;
    }
}

pub struct SpaceLayouts {
    spaces: [PerSpace; MAX_SPACES as usize],
}

impl Default for SpaceLayouts {
    fn default() -> Self {
        Self {
            spaces: std::array::from_fn(|_| PerSpace::default()),
        }
    }
}

impl SpaceLayouts {
    pub fn mode(&self, space: u32) -> SpaceMode {
        self.spaces
            .get(space as usize)
            .map_or(SpaceMode::Free, |s| s.mode)
    }

    pub(crate) fn order(&self, space: u32) -> &[u64] {
        self.spaces
            .get(space as usize)
            .map_or(&[], |s| s.order.as_slice())
    }

    pub(crate) fn zoomed(&self, space: u32) -> Option<u64> {
        self.spaces.get(space as usize).and_then(|s| s.zoomed)
    }

    pub(crate) fn scroll(&self, space: u32) -> i32 {
        self.spaces.get(space as usize).map_or(0, |s| s.scroll)
    }

    pub fn is_hidden_by_zoom(&self, space: u32, win_id: u64) -> bool {
        let Some(ps) = self.spaces.get(space as usize) else {
            return false;
        };
        ps.mode == SpaceMode::Tile
            && ps
                .zoomed
                .is_some_and(|z| z != win_id && ps.order.len() > 1 && ps.order.contains(&z))
    }

    pub fn configured_modes(&self) -> Vec<(u32, SpaceMode)> {
        self.spaces
            .iter()
            .enumerate()
            .filter(|(_, s)| s.mode != SpaceMode::Free)
            .map(|(i, s)| (i as u32, s.mode))
            .collect()
    }

    pub fn load_modes(&mut self, modes: &[(u32, SpaceMode)]) {
        for &(space, mode) in modes {
            if let Some(ps) = self.spaces.get_mut(space as usize) {
                ps.mode = mode;
            }
        }
    }

    pub fn hold_focus_on_resize_click(&mut self, space: u32, win_id: u64) {
        if let Some(ps) = self.spaces.get_mut(space as usize) {
            ps.focused_col = Some(win_id);
        }
    }

    pub fn invalidate_space(&mut self, space: u32) {
        if let Some(ps) = self.spaces.get_mut(space as usize) {
            ps.focused_col = None;
            ps.invalidate();
        }
    }

    pub fn on_space_removed(&mut self, removed: u32) {
        let idx = removed as usize;
        if idx >= self.spaces.len() {
            return;
        }
        for i in idx..self.spaces.len() - 1 {
            self.spaces[i] = std::mem::take(&mut self.spaces[i + 1]);
            self.spaces[i].invalidate();
        }
        self.spaces[self.spaces.len() - 1] = PerSpace::default();
    }
}

/// Dwindle/BSP slots inside `area` with uniform `TILE_GAP` outer & inner padding.
pub fn tile_slots(count: usize, area: Rect, splits: &[Split]) -> Vec<TileSlot> {
    if count == 0 {
        return Vec::new();
    }
    let mut rest = Rect {
        x: area.x + TILE_GAP,
        y: area.y + TILE_GAP,
        w: (area.w - 2 * TILE_GAP).max(2),
        h: (area.h - 2 * TILE_GAP).max(2),
    };
    if count == 1 {
        return vec![TileSlot {
            rect: rest,
            split_parent: rest,
            splits: (None, None, None, None),
        }];
    }
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
            let usable = (rest.w - TILE_GAP).max(2);
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
                x: rest.x + w + TILE_GAP,
                w: (usable - w).max(1),
                ..rest
            };
            rest_left_split = Some(i);
        } else {
            let usable = (rest.h - TILE_GAP).max(2);
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
                y: rest.y + h + TILE_GAP,
                h: (usable - h).max(1),
                ..rest
            };
            rest_top_split = Some(i);
        }
    }
    out
}

pub fn tape_inner(area: Rect) -> Rect {
    Rect {
        x: area.x + TAPE_PAD,
        y: area.y + TAPE_PAD,
        w: (area.w - 2 * TAPE_PAD).max(MIN_WINDOW_W),
        h: (area.h - 2 * TAPE_PAD).max(MIN_WINDOW_H),
    }
}

/// Exact column width so `N` columns of fraction `1/N` (or `2/3 + 1/3`) fit `inner_w` cleanly.
pub fn tape_col_width(frac: f32, inner_w: i32, min_w: i32, max_w: i32) -> i32 {
    let lo = min_w.max(MIN_WINDOW_W).min(inner_w);
    let hi = max_w.min(inner_w).max(lo);
    let raw = (((inner_w + TAPE_GAP) as f32 * frac).round() as i32) - TAPE_GAP;
    raw.clamp(lo, hi)
}

pub fn tape_frac_for_width(px_w: i32, inner_w: i32) -> f32 {
    ((px_w + TAPE_GAP) as f32 / (inner_w + TAPE_GAP).max(1) as f32).clamp(0.15, 1.0)
}

pub fn tape_spans(widths: &[i32]) -> (Vec<(i32, i32)>, i32) {
    let mut x = 0;
    let mut out = Vec::with_capacity(widths.len());
    for (i, &w) in widths.iter().enumerate() {
        if i > 0 {
            x += TAPE_GAP;
        }
        out.push((x, w));
        x += w;
    }
    (out, x)
}

pub fn tape_clamp_scroll(total_w: i32, inner_w: i32, scroll: i32) -> i32 {
    if total_w <= inner_w {
        -((inner_w - total_w) / 2)
    } else {
        scroll.clamp(0, total_w - inner_w)
    }
}

pub fn tape_ensure_visible(
    spans: &[(i32, i32)],
    total_w: i32,
    idx: usize,
    inner_w: i32,
    scroll: i32,
) -> i32 {
    if spans.is_empty() || idx >= spans.len() || total_w <= inner_w {
        return tape_clamp_scroll(total_w, inner_w, scroll);
    }
    let (sx, cw) = spans[idx];
    let max_scroll = (total_w - inner_w).max(0);
    let mut s = scroll.clamp(0, max_scroll);
    let room = ((inner_w - cw).max(0) / 2).min(TAPE_PEEK);
    let left_pad = if idx == 0 { 0 } else { room };
    let right_pad = if idx + 1 == spans.len() { 0 } else { room };
    if sx - left_pad < s {
        s = (sx - left_pad).max(0);
    } else if sx + cw + right_pad > s + inner_w {
        s = (sx + cw + right_pad - inner_w).min(max_scroll);
    }
    s
}

pub fn tape_rects(spans: &[(i32, i32)], inner: Rect, scroll: i32) -> Vec<Rect> {
    spans
        .iter()
        .map(|&(sx, cw)| Rect {
            x: inner.x + sx - scroll,
            y: inner.y,
            w: cw,
            h: inner.h,
        })
        .collect()
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
    pub fn desktop_work_area(&self) -> Rect {
        let m = self
            .primary_monitor()
            .map(|m| m.info.rect)
            .unwrap_or_else(|| self.monitor_at(0.0, 0.0));
        work_area(m)
    }

    pub fn space_mode(&self, space: u32) -> SpaceMode {
        self.space_layouts.mode(space)
    }

    pub fn current_space_mode(&self) -> SpaceMode {
        if self.larvis_shown {
            SpaceMode::Free
        } else {
            self.space_layouts.mode(self.spaces.current)
        }
    }

    /// True when the active user desktop manages window placement (`Tile` or `Tape`)
    /// and neither Larvis nor the Подложка is covering it.
    pub fn current_space_managed(&self) -> bool {
        !self.larvis_shown
            && !self.shell.scratch_open
            && self.current_space_mode() != SpaceMode::Free
    }

    /// True when `win_id` is currently managed by a `Tile` or `Tape` desktop.
    pub fn window_in_managed_space(&self, win_id: u64) -> bool {
        self.windows.iter().any(|w| {
            w.id == win_id
                && w.managed_layout
                && w.space < MAX_SPACES
                && self.space_layouts.mode(w.space) != SpaceMode::Free
        })
    }

    fn space_eligible_windows(&self, space: u32) -> Vec<u64> {
        let Some(ps) = self.space_layouts.spaces.get(space as usize) else {
            return Vec::new();
        };
        ps.order
            .iter()
            .copied()
            .filter(|id| {
                self.windows.iter().any(|w| {
                    w.id == *id
                        && w.space == space
                        && w.mapped
                        && !w.pip
                        && !w.minimized
                        && !w.maximized
                        && !w.fullscreen
                        && !w.larvis
                        && !w.has_parent()
                })
            })
            .collect()
    }

    pub fn cycle_current_space_mode(&mut self) {
        if self.larvis_shown || self.lock.active {
            return;
        }
        let next = self.current_space_mode().next();
        self.set_space_mode(self.spaces.current, next, true);
    }

    pub fn set_space_mode(&mut self, space: u32, mode: SpaceMode, show_banner: bool) {
        if space >= MAX_SPACES || self.lock.active {
            return;
        }
        let prev = self.space_layouts.mode(space);
        if prev == mode {
            if show_banner {
                self.show_banner(Message {
                    icon: BannerIcon::SpaceMode(mode),
                    title: mode.label(),
                    detail: mode.banner_detail(),
                });
            }
            return;
        }
        if space >= self.spaces.count {
            self.spaces.count = (space + 1).min(MAX_SPACES);
        }
        self.animations.clear_tape(space);
        {
            let ps = &mut self.space_layouts.spaces[space as usize];
            ps.mode = mode;
            ps.zoomed = None;
            ps.drag_splits = None;
            ps.drag_width = None;
            ps.invalidate();
            if mode == SpaceMode::Free {
                ps.order.clear();
                ps.splits.clear();
                ps.focused_col = None;
                ps.scroll = 0;
            }
        }
        if mode == SpaceMode::Free {
            for i in 0..self.windows.len() {
                if self.windows[i].space == space && self.windows[i].managed_layout {
                    let r = self.window_monitor(i);
                    self.windows[i].leave_managed_layout(r);
                }
            }
        } else if prev == SpaceMode::Free {
            // Seed order left-to-right from existing desktop windows so visual positions feel natural
            let mut existing: Vec<(i32, i32, u64)> = self
                .windows
                .iter()
                .filter(|w| {
                    w.space == space
                        && w.mapped
                        && !w.pip
                        && !w.larvis
                        && !w.has_parent()
                        && !w.fullscreen
                })
                .map(|w| (w.x, w.y, w.id))
                .collect();
            existing.sort_by_key(|&(x, y, id)| (x, y, id));
            for &(_, _, id) in &existing {
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == id) {
                    win.enter_managed_layout();
                }
            }
            let ps = &mut self.space_layouts.spaces[space as usize];
            ps.order = existing.into_iter().map(|(_, _, id)| id).collect();
            ps.focused_col = self
                .windows
                .iter()
                .rev()
                .find(|w| w.space == space && w.is_visible() && w.managed_layout)
                .map(|w| w.id);
        }
        self.apply_space_visibility();
        self.space_layout_sync();
        if self.mission_open() {
            if space == self.spaces.current {
                self.relayout_mission();
            } else {
                self.sync_mission_overlay();
            }
        }
        if show_banner {
            self.show_banner(Message {
                icon: BannerIcon::SpaceMode(mode),
                title: mode.label(),
                detail: mode.banner_detail(),
            });
        }
        self.window_memory_changed();
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
    }

    /// Inserts a newly mapped window on `space` into its managed layout (right after the
    /// active column in `Tape` mode, or at the end in `Tile` mode).
    pub fn space_layout_adopt(&mut self, idx: usize) {
        let space = self.windows[idx].space;
        if space >= MAX_SPACES
            || self.space_layouts.mode(space) == SpaceMode::Free
            || self.windows[idx].pip
            || self.windows[idx].larvis
            || self.windows[idx].has_parent()
        {
            return;
        }
        let mode = self.space_layouts.mode(space);
        self.windows[idx].resize_anchor = None;
        self.windows[idx].enter_managed_layout();
        let id = self.windows[idx].id;
        let ps = &mut self.space_layouts.spaces[space as usize];
        ps.zoomed = None;
        if !ps.order.contains(&id) {
            if mode == SpaceMode::Tape {
                if let Some(after) = ps
                    .focused_col
                    .and_then(|fc| ps.order.iter().position(|&o| o == fc))
                {
                    ps.order.insert(after + 1, id);
                } else {
                    ps.order.push(id);
                }
            } else {
                ps.order.push(id);
            }
        }
        ps.invalidate();
    }

    /// Synchronizes all `Tile` and `Tape` desktops. Fast-paths when all desktops are `Free`
    /// or memoized state keys are unchanged.
    pub fn space_layout_sync(&mut self) {
        let ui_mode = self.current_space_mode();
        if self.shell.space_mode != ui_mode {
            self.shell.space_mode = ui_mode;
            self.shell.topbar_dirty = true;
        }

        // Release any window that moved from a managed desktop to a Free desktop
        for i in 0..self.windows.len() {
            if self.windows[i].managed_layout {
                let s = self.windows[i].space;
                if s < MAX_SPACES && self.space_layouts.mode(s) == SpaceMode::Free {
                    let r = self.window_monitor(i);
                    self.windows[i].leave_managed_layout(r);
                }
            }
        }

        let any_managed = (0..self.spaces.count.min(MAX_SPACES))
            .any(|s| self.space_layouts.mode(s) != SpaceMode::Free);
        if !any_managed {
            return;
        }

        // Do not snap windows back while the user is actively dragging one to swap slots
        let dragging_move = matches!(
            self.active_drag,
            Some(crate::DragOperation::MoveWindow { .. })
        );
        let area = self.desktop_work_area();

        for space in 0..self.spaces.count.min(MAX_SPACES) {
            let mode = self.space_layouts.mode(space);
            if mode == SpaceMode::Free {
                continue;
            }
            // Adopt any eligible window that arrived on this desktop (e.g., moved via Mission Control or API)
            let newcomers: Vec<usize> = (0..self.windows.len())
                .filter(|&i| {
                    let w = &self.windows[i];
                    w.space == space
                        && w.mapped
                        && !w.pip
                        && !w.larvis
                        && !w.has_parent()
                        && !w.fullscreen
                        && !self.space_layouts.spaces[space as usize].order.contains(&w.id)
                })
                .collect();
            for i in newcomers {
                self.space_layout_adopt(i);
            }

            // Prune windows that closed or left this desktop
            let prev_order = self.space_layouts.spaces[space as usize].order.clone();
            for id in prev_order {
                let keep = self
                    .windows
                    .iter()
                    .any(|w| w.id == id && w.space == space && w.mapped);
                if !keep {
                    let ps = &mut self.space_layouts.spaces[space as usize];
                    ps.order.retain(|&o| o != id);
                    ps.widths.retain(|(wid, _)| *wid != id);
                    if ps.saved_before_full.is_some_and(|(wid, _)| wid == id) {
                        ps.saved_before_full = None;
                    }
                }
            }

            if self.space_layouts.spaces[space as usize]
                .zoomed
                .is_some_and(|z| {
                    let o = &self.space_layouts.spaces[space as usize].order;
                    !o.contains(&z) || o.len() <= 1 || self.mission_open()
                })
            {
                self.space_layouts.spaces[space as usize].zoomed = None;
                self.apply_space_visibility();
            }

            // While a managed window on this desktop is in fullscreen on top (e.g. HTML5 video),
            // keep the remaining columns/tiles steady at their current positions and scroll offset
            // instead of re-centering/resizing them in the background.
            let top_fullscreen_here = self
                .windows
                .iter()
                .rev()
                .find(|w| w.space == space && w.mapped && !w.pip && !w.minimized && !w.larvis)
                .is_some_and(|w| {
                    w.fullscreen && self.space_layouts.spaces[space as usize].order.contains(&w.id)
                });
            if top_fullscreen_here {
                continue;
            }

            let ids = self.space_eligible_windows(space);
            if ids.is_empty() {
                let ps = &mut self.space_layouts.spaces[space as usize];
                ps.splits.clear();
                ps.scroll = 0;
                ps.focused_col = None;
                ps.invalidate();
                continue;
            }

            if dragging_move
                && self.active_drag.is_some_and(|d| match d {
                    crate::DragOperation::MoveWindow { win_id, .. } => ids.contains(&win_id),
                    _ => false,
                })
            {
                continue;
            }

            match mode {
                SpaceMode::Tile => self.sync_tile_space(space, &ids, area),
                SpaceMode::Tape => self.sync_tape_space(space, &ids, area),
                SpaceMode::Free => {}
            }
        }
    }

    fn sync_tile_space(&mut self, space: u32, ids: &[u64], area: Rect) {
        self.space_layouts.spaces[space as usize].ensure_splits(ids.len());
        let zoomed = self.space_layouts.spaces[space as usize]
            .zoomed
            .filter(|z| ids.len() > 1 && ids.contains(z));
        let splits = self.space_layouts.spaces[space as usize].splits.clone();
        let key = (ids.to_vec(), area, splits.clone(), zoomed);
        if self.space_layouts.spaces[space as usize].laid_tile.as_ref() == Some(&key) {
            return;
        }
        if let Some(zid) = zoomed {
            let single = tile_slots(1, area, &[]);
            for id in ids {
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == *id) {
                    win.scratch_resize_edges = Edges::default();
                    if *id == zid {
                        win.fit_tile_frame(single[0].rect);
                    }
                }
            }
            self.apply_space_visibility();
        } else {
            let slots = tile_slots(ids.len(), area, &splits);
            for (id, slot) in ids.iter().zip(slots) {
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == *id) {
                    win.scratch_resize_edges = slot.inner_edges();
                    win.fit_tile_frame(slot.rect);
                }
            }
        }
        self.space_layouts.spaces[space as usize].laid_tile = Some(key);
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
    }

    fn tape_column_widths(&self, space: u32, ids: &[u64], inner_w: i32) -> Vec<(u64, i32)> {
        let ps = &self.space_layouts.spaces[space as usize];
        ids.iter()
            .map(|&id| {
                let frac = ps.width_frac(id);
                let (min, max) = self
                    .windows
                    .iter()
                    .find(|w| w.id == id)
                    .map_or(((MIN_WINDOW_W, MIN_WINDOW_H), (i32::MAX, i32::MAX)), |w| {
                        w.size_limits()
                    });
                (id, tape_col_width(frac, inner_w, min.0, max.0))
            })
            .collect()
    }

    fn sync_tape_space(&mut self, space: u32, ids: &[u64], area: Rect) {
        let inner = tape_inner(area);
        let id_widths = self.tape_column_widths(space, ids, inner.w);
        let widths_only: Vec<i32> = id_widths.iter().map(|&(_, w)| w).collect();
        let (spans, total_w) = tape_spans(&widths_only);

        let front_id = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == space && !w.minimized && ids.contains(&w.id))
            .map(|w| w.id);

        let resizing_here = matches!(
            self.active_drag,
            Some(crate::DragOperation::ResizeWindow { win_id, .. }) if ids.contains(&win_id)
        );

        let old_scroll = self.space_layouts.spaces[space as usize].scroll;
        let was_laid = self.space_layouts.spaces[space as usize].laid_tape.is_some();
        let prev_focus = self.space_layouts.spaces[space as usize].focused_col;

        let new_scroll = if !resizing_here && front_id != prev_focus {
            if let Some(idx) = front_id.and_then(|fid| ids.iter().position(|&id| id == fid)) {
                tape_ensure_visible(&spans, total_w, idx, inner.w, old_scroll)
            } else {
                tape_clamp_scroll(total_w, inner.w, old_scroll)
            }
        } else {
            tape_clamp_scroll(total_w, inner.w, old_scroll)
        };

        self.space_layouts.spaces[space as usize].focused_col = front_id;
        self.space_layouts.spaces[space as usize].scroll = new_scroll;

        let key = (ids.to_vec(), area, id_widths.clone(), new_scroll);
        if self.space_layouts.spaces[space as usize].laid_tape.as_ref() == Some(&key) {
            return;
        }

        let rects = tape_rects(&spans, inner, new_scroll);
        let lr_edges = Edges {
            left: true,
            right: true,
            top: false,
            bottom: false,
        };
        for (&id, rect) in ids.iter().zip(rects) {
            if let Some(win) = self.windows.iter_mut().find(|w| w.id == id) {
                win.scratch_resize_edges = lr_edges;
                win.fit_tile_frame(rect);
            }
        }

        if was_laid && space == self.spaces.current && new_scroll != old_scroll && !self.mission_open() {
            self.animations
                .scroll_tape(space, (new_scroll - old_scroll) as f64);
        }
        self.space_layouts.spaces[space as usize].laid_tape = Some(key);
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
    }

    fn unfullscreen_top_on_space(&mut self, space: u32) {
        if let Some(fs_idx) = self
            .windows
            .iter()
            .rposition(|w| w.space == space && w.mapped && !w.minimized && !w.larvis)
            .filter(|&i| {
                let w = &self.windows[i];
                w.fullscreen && self.space_layouts.spaces[space as usize].order.contains(&w.id)
            })
        {
            let r = self.window_monitor(fs_idx);
            self.set_window_fullscreen(fs_idx, false, r);
        }
    }

    /// `Super + стрелки` on a `Tile` or `Tape` desktop.
    pub fn space_layout_focus_dir(&mut self, dir: Dir) {
        if !self.current_space_managed() {
            return;
        }
        let space = self.spaces.current;
        self.unfullscreen_top_on_space(space);
        let ids = self.space_eligible_windows(space);
        if ids.is_empty() {
            return;
        }
        let active_id = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == space && w.is_visible() && ids.contains(&w.id))
            .map(|w| w.id)
            .unwrap_or(ids[0]);
        let Some(from) = ids.iter().position(|&id| id == active_id) else {
            return;
        };

        let target_idx = match self.space_layouts.mode(space) {
            SpaceMode::Free => None,
            SpaceMode::Tile => {
                if self.space_layouts.spaces[space as usize].zoomed.take().is_some() {
                    self.space_layouts.spaces[space as usize].invalidate();
                    self.apply_space_visibility();
                }
                let area = self.desktop_work_area();
                self.space_layouts.spaces[space as usize].ensure_splits(ids.len());
                let splits = &self.space_layouts.spaces[space as usize].splits;
                let slots = tile_slots(ids.len(), area, splits);
                neighbor_slot(&slots, from, dir)
            }
            SpaceMode::Tape => match dir {
                Dir::Left if from > 0 => Some(from - 1),
                Dir::Right if from + 1 < ids.len() => Some(from + 1),
                _ => {
                    // Even if already at the end of the ribbon, ensure the edge column is scrolled cleanly into view
                    self.space_layouts.spaces[space as usize].focused_col = None;
                    self.space_layout_sync();
                    None
                }
            },
        };

        if let Some(to) = target_idx {
            let target_id = ids[to];
            // Force `sync_tape_space` to snap/glide target_id into view even if user manually scrolled earlier
            self.space_layouts.spaces[space as usize].focused_col = None;
            if let Some(idx) = self.windows.iter().position(|w| w.id == target_id) {
                let win = self.windows.remove(idx);
                self.windows.push(win);
                self.pointer_focus_dirty = true;
                self.needs_redraw = true;
                self.refresh_focus();
            }
        }
    }

    /// `Super + Home / End` on a `Tape` desktop: jump straight to the first or last column.
    pub fn space_layout_focus_edge(&mut self, first: bool) {
        if self.current_space_mode() != SpaceMode::Tape || self.shell.scratch_open {
            return;
        }
        let space = self.spaces.current;
        self.unfullscreen_top_on_space(space);
        let ids = self.space_eligible_windows(space);
        let Some(&target_id) = (if first { ids.first() } else { ids.last() }) else {
            return;
        };
        self.space_layouts.spaces[space as usize].focused_col = None;
        if let Some(idx) = self.windows.iter().position(|w| w.id == target_id) {
            let win = self.windows.remove(idx);
            self.windows.push(win);
            self.pointer_focus_dirty = true;
            self.needs_redraw = true;
            self.refresh_focus();
        }
    }

    /// `Super + Shift + стрелки` on a `Tile` or `Tape` desktop: reorder/swap or rotate split.
    pub fn space_layout_swap_dir(&mut self, dir: Dir) {
        if !self.current_space_managed() {
            return;
        }
        let space = self.spaces.current;
        let ids = self.space_eligible_windows(space);
        if ids.len() < 2 {
            return;
        }
        let Some(active_id) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == space && w.is_visible() && ids.contains(&w.id))
            .map(|w| w.id)
        else {
            return;
        };
        let Some(from) = ids.iter().position(|&id| id == active_id) else {
            return;
        };

        match self.space_layouts.mode(space) {
            SpaceMode::Free => return,
            SpaceMode::Tile => {
                if self.space_layouts.spaces[space as usize].zoomed.take().is_some() {
                    self.apply_space_visibility();
                }
                let area = self.desktop_work_area();
                self.space_layouts.spaces[space as usize].ensure_splits(ids.len());
                let splits = self.space_layouts.spaces[space as usize].splits.clone();
                let slots = tile_slots(ids.len(), area, &splits);
                let ps = &mut self.space_layouts.spaces[space as usize];
                if let Some(to) = neighbor_slot(&slots, from, dir) {
                    let other_id = ids[to];
                    if let (Some(a), Some(b)) = (
                        ps.order.iter().position(|&id| id == active_id),
                        ps.order.iter().position(|&id| id == other_id),
                    ) {
                        ps.order.swap(a, b);
                    }
                } else {
                    let sp_idx = from.min(ids.len() - 2);
                    let want_side = matches!(dir, Dir::Left | Dir::Right);
                    ps.splits[sp_idx].side_by_side = Some(want_side);
                    let want_first = matches!(dir, Dir::Left | Dir::Up);
                    if want_first != (from == sp_idx) && sp_idx + 1 < ids.len() {
                        let (id_a, id_b) = (ids[sp_idx], ids[sp_idx + 1]);
                        if let (Some(a), Some(b)) = (
                            ps.order.iter().position(|&id| id == id_a),
                            ps.order.iter().position(|&id| id == id_b),
                        ) {
                            ps.order.swap(a, b);
                        }
                    }
                }
                ps.invalidate();
            }
            SpaceMode::Tape => {
                let target = match dir {
                    Dir::Left if from > 0 => Some(from - 1),
                    Dir::Right if from + 1 < ids.len() => Some(from + 1),
                    _ => None,
                };
                let Some(to) = target else { return };
                let other_id = ids[to];
                let ps = &mut self.space_layouts.spaces[space as usize];
                if let (Some(a), Some(b)) = (
                    ps.order.iter().position(|&id| id == active_id),
                    ps.order.iter().position(|&id| id == other_id),
                ) {
                    ps.order.swap(a, b);
                }
                ps.focused_col = None;
                ps.invalidate();
            }
        }
        self.space_layout_sync();
    }

    /// `Ctrl/Alt + Super + стрелки`: move split divider (`Tile`) or resize active column (`Tape`).
    pub fn space_layout_resize_step(&mut self, dir: Dir) {
        if !self.current_space_managed() {
            return;
        }
        let space = self.spaces.current;
        let ids = self.space_eligible_windows(space);
        if ids.is_empty() {
            return;
        }
        let Some(active_id) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == space && w.is_visible() && ids.contains(&w.id))
            .map(|w| w.id)
        else {
            return;
        };
        let area = self.desktop_work_area();

        match self.space_layouts.mode(space) {
            SpaceMode::Free => return,
            SpaceMode::Tile => {
                if ids.len() < 2 {
                    return;
                }
                if self.space_layouts.spaces[space as usize].zoomed.take().is_some() {
                    self.apply_space_visibility();
                }
                let Some(slot_idx) = ids.iter().position(|&id| id == active_id) else {
                    return;
                };
                self.space_layouts.spaces[space as usize].ensure_splits(ids.len());
                let splits = self.space_layouts.spaces[space as usize].splits.clone();
                let slots = tile_slots(ids.len(), area, &splits);
                let slot = slots[slot_idx];
                let ps = &mut self.space_layouts.spaces[space as usize];
                match dir {
                    Dir::Left | Dir::Right => {
                        let Some(sp_idx) = slot.splits.1.or(slot.splits.0) else {
                            return;
                        };
                        let usable = (slots[sp_idx].split_parent.w - TILE_GAP).max(2);
                        let min_w = MIN_WINDOW_W.min(usable / 2).max(1);
                        let step = ((usable as f32 * KEY_RESIZE_STEP).round() as i32).max(24);
                        let delta = if dir == Dir::Right { step } else { -step };
                        let w = (slots[sp_idx].rect.w + delta).clamp(min_w, (usable - min_w).max(min_w));
                        ps.splits[sp_idx].ratio = w as f32 / usable as f32;
                    }
                    Dir::Up | Dir::Down => {
                        let Some(sp_idx) = slot.splits.3.or(slot.splits.2) else {
                            return;
                        };
                        let usable = (slots[sp_idx].split_parent.h - TILE_GAP).max(2);
                        let min_h = MIN_WINDOW_H.min(usable / 2).max(1);
                        let step = ((usable as f32 * KEY_RESIZE_STEP).round() as i32).max(24);
                        let delta = if dir == Dir::Down { step } else { -step };
                        let h = (slots[sp_idx].rect.h + delta).clamp(min_h, (usable - min_h).max(min_h));
                        ps.splits[sp_idx].ratio = h as f32 / usable as f32;
                    }
                }
                ps.invalidate();
            }
            SpaceMode::Tape => {
                if !matches!(dir, Dir::Left | Dir::Right) {
                    return;
                }
                let inner = tape_inner(area);
                let cur_frac = self.space_layouts.spaces[space as usize].width_frac(active_id);
                let delta = if dir == Dir::Right {
                    KEY_RESIZE_STEP
                } else {
                    -KEY_RESIZE_STEP
                };
                let (min, max) = self
                    .windows
                    .iter()
                    .find(|w| w.id == active_id)
                    .map_or(((MIN_WINDOW_W, MIN_WINDOW_H), (i32::MAX, i32::MAX)), |w| {
                        w.size_limits()
                    });
                let min_frac = tape_frac_for_width(min.0.min(inner.w), inner.w);
                let max_frac = tape_frac_for_width(max.0.min(inner.w), inner.w);
                let ps = &mut self.space_layouts.spaces[space as usize];
                ps.saved_before_full = None;
                ps.set_width_frac(active_id, (cur_frac + delta).clamp(min_frac, max_frac));
                ps.focused_col = None;
                ps.invalidate();
            }
        }
        self.space_layout_sync();
    }

    /// `Super + 0`: equalize all splits (`Tile`) or reset all column widths to 50% (`Tape`).
    pub fn space_layout_equalize(&mut self) {
        if !self.current_space_managed() {
            return;
        }
        let space = self.spaces.current;
        let ps = &mut self.space_layouts.spaces[space as usize];
        if ps.zoomed.take().is_some() {
            self.apply_space_visibility();
        }
        let ps = &mut self.space_layouts.spaces[space as usize];
        for sp in &mut ps.splits {
            sp.ratio = 0.5;
        }
        ps.widths.clear();
        ps.saved_before_full = None;
        ps.focused_col = None;
        ps.invalidate();
        self.space_layout_sync();
    }

    /// `Super + R` in `Tape` mode: cycle active column preset width (`1/2 → 2/3 → Full → 1/3`).
    pub fn space_layout_cycle_tape_width(&mut self) {
        if self.current_space_mode() != SpaceMode::Tape || self.shell.scratch_open {
            return;
        }
        let space = self.spaces.current;
        let ids = self.space_eligible_windows(space);
        let Some(active_id) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == space && w.is_visible() && ids.contains(&w.id))
            .map(|w| w.id)
        else {
            return;
        };
        let cur = self.space_layouts.spaces[space as usize].width_frac(active_id);
        let idx = TAPE_PRESETS
            .iter()
            .position(|&p| (p - cur).abs() < 0.04)
            .map_or(0, |i| (i + 1) % TAPE_PRESETS.len());
        let ps = &mut self.space_layouts.spaces[space as usize];
        ps.saved_before_full = None;
        ps.set_width_frac(active_id, TAPE_PRESETS[idx]);
        ps.focused_col = None;
        ps.invalidate();
        self.space_layout_sync();
    }

    pub fn space_layout_set_zoom(&mut self, idx: usize, on: bool) {
        let space = self.windows[idx].space;
        if space >= MAX_SPACES {
            return;
        }
        let id = self.windows[idx].id;
        let ids = self.space_eligible_windows(space);
        if !ids.contains(&id) {
            return;
        }
        match self.space_layouts.mode(space) {
            SpaceMode::Free => return,
            SpaceMode::Tile => {
                let ps = &mut self.space_layouts.spaces[space as usize];
                ps.zoomed = (on && ids.len() >= 2).then_some(id);
                ps.invalidate();
                self.apply_space_visibility();
                self.space_layout_sync();
                self.refresh_focus();
            }
            SpaceMode::Tape => {
                let ps = &mut self.space_layouts.spaces[space as usize];
                let cur = ps.width_frac(id);
                if on {
                    if (cur - 1.0).abs() >= 0.02 {
                        ps.saved_before_full = Some((id, cur));
                        ps.set_width_frac(id, 1.0);
                    }
                } else if (cur - 1.0).abs() < 0.02 {
                    let prev = ps
                        .saved_before_full
                        .filter(|&(wid, _)| wid == id)
                        .map_or(0.5, |(_, f)| f);
                    ps.saved_before_full = None;
                    ps.set_width_frac(id, prev);
                }
                ps.focused_col = None;
                ps.invalidate();
                self.space_layout_sync();
            }
        }
    }

    /// `Super + F`, green button, or double-clicking a titlebar on a `Tile` or `Tape` desktop.
    pub fn space_layout_toggle_zoom(&mut self) {
        if !self.current_space_managed() {
            return;
        }
        let space = self.spaces.current;
        let ids = self.space_eligible_windows(space);
        let Some(active_id) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.space == space && w.is_visible() && ids.contains(&w.id))
            .map(|w| w.id)
        else {
            return;
        };
        match self.space_layouts.mode(space) {
            SpaceMode::Free => return,
            SpaceMode::Tile => {
                if ids.len() < 2 {
                    return;
                }
                let ps = &mut self.space_layouts.spaces[space as usize];
                if ps.zoomed == Some(active_id) {
                    ps.zoomed = None;
                } else {
                    ps.zoomed = Some(active_id);
                }
                ps.invalidate();
                self.apply_space_visibility();
                self.space_layout_sync();
                self.refresh_focus();
            }
            SpaceMode::Tape => {
                let ps = &mut self.space_layouts.spaces[space as usize];
                let cur = ps.width_frac(active_id);
                if (cur - 1.0).abs() < 0.02 {
                    let prev = ps
                        .saved_before_full
                        .filter(|&(id, _)| id == active_id)
                        .map_or(0.5, |(_, f)| f);
                    ps.saved_before_full = None;
                    ps.set_width_frac(active_id, prev);
                } else {
                    ps.saved_before_full = Some((active_id, cur));
                    ps.set_width_frac(active_id, 1.0);
                }
                ps.focused_col = None;
                ps.invalidate();
                self.space_layout_sync();
            }
        }
    }

    /// `Super + колесо мыши / прокрутка трекпадом` in `Tape` mode: glide horizontally along the ribbon.
    pub fn space_layout_scroll_by(&mut self, px_delta: i32) -> bool {
        if self.current_space_mode() != SpaceMode::Tape || self.shell.scratch_open {
            return false;
        }
        let space = self.spaces.current;
        let ids = self.space_eligible_windows(space);
        if ids.is_empty() {
            return false;
        }
        let area = self.desktop_work_area();
        let inner = tape_inner(area);
        let widths: Vec<i32> = self
            .tape_column_widths(space, &ids, inner.w)
            .into_iter()
            .map(|(_, w)| w)
            .collect();
        let (_, total_w) = tape_spans(&widths);
        if total_w <= inner.w {
            return true;
        }
        let old = self.space_layouts.spaces[space as usize].scroll;
        let next = tape_clamp_scroll(total_w, inner.w, old + px_delta);
        if next != old {
            self.space_layouts.spaces[space as usize].scroll = next;
            self.space_layouts.spaces[space as usize].invalidate();
            self.space_layout_sync();
        }
        true
    }

    /// Inner resize edges for `Super + RMB` anywhere inside a window on a `Tile` or `Tape` desktop.
    pub fn space_layout_super_resize_edges(&self, idx: usize) -> Option<Edges> {
        let win = &self.windows[idx];
        if !win.managed_layout || win.space >= MAX_SPACES {
            return None;
        }
        match self.space_layouts.mode(win.space) {
            SpaceMode::Free => None,
            SpaceMode::Tile => {
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
            SpaceMode::Tape => Some(Edges {
                left: false,
                right: true,
                top: false,
                bottom: false,
            }),
        }
    }

    pub fn space_layout_begin_resize(
        &mut self,
        win_id: u64,
        edges: Edges,
        allow_double_click: bool,
    ) -> bool {
        let Some(space) = self
            .windows
            .iter()
            .find(|w| w.id == win_id && w.managed_layout && w.space < MAX_SPACES)
            .map(|w| w.space)
        else {
            return false;
        };
        let mode = self.space_layouts.mode(space);
        let ids = self.space_eligible_windows(space);
        let area = self.desktop_work_area();
        let now = Instant::now();

        match mode {
            SpaceMode::Free => false,
            SpaceMode::Tile => {
                self.space_layouts.spaces[space as usize].ensure_splits(ids.len());
                let splits = self.space_layouts.spaces[space as usize].splits.clone();
                let slots = tile_slots(ids.len(), area, &splits);
                if let Some(slot_idx) = ids.iter().position(|&id| id == win_id) {
                    let pair = splits_for_edges(&slots[slot_idx], edges);
                    let target = DividerTarget::Tile(pair.0, pair.1);
                    let ps = &mut self.space_layouts.spaces[space as usize];
                    if allow_double_click
                        && (pair.0.is_some() || pair.1.is_some())
                        && ps.last_divider_click.is_some_and(|(t, prev)| {
                            prev == target && now.duration_since(t).as_millis() < DOUBLE_CLICK_MS
                        })
                    {
                        ps.last_divider_click = None;
                        if let Some(i) = pair.0 {
                            ps.splits[i].ratio = 0.5;
                        }
                        if let Some(i) = pair.1 {
                            ps.splits[i].ratio = 0.5;
                        }
                        ps.invalidate();
                        self.space_layout_sync();
                        return true;
                    }
                    if allow_double_click {
                        ps.last_divider_click = Some((now, target));
                    }
                }
                let ps = &mut self.space_layouts.spaces[space as usize];
                ps.drag_splits = Some(ps.splits.clone());
                false
            }
            SpaceMode::Tape => {
                let target = DividerTarget::Tape(win_id);
                let inner = tape_inner(area);
                let cur_px = self
                    .tape_column_widths(space, &[win_id], inner.w)
                    .first()
                    .map_or(inner.w / 2, |&(_, w)| w);
                let ps = &mut self.space_layouts.spaces[space as usize];
                if allow_double_click
                    && ps.last_divider_click.is_some_and(|(t, prev)| {
                        prev == target && now.duration_since(t).as_millis() < DOUBLE_CLICK_MS
                    })
                {
                    ps.last_divider_click = None;
                    ps.saved_before_full = None;
                    ps.set_width_frac(win_id, 0.5);
                    ps.invalidate();
                    self.space_layout_sync();
                    return true;
                }
                if allow_double_click {
                    ps.last_divider_click = Some((now, target));
                }
                ps.drag_width = Some((win_id, cur_px, ps.scroll));
                false
            }
        }
    }

    pub fn space_layout_resize_drag(
        &mut self,
        win_id: u64,
        dx: i32,
        dy: i32,
        edges: Edges,
    ) -> bool {
        let Some(space) = self
            .windows
            .iter()
            .find(|w| w.id == win_id && w.managed_layout && w.space < MAX_SPACES)
            .map(|w| w.space)
        else {
            return false;
        };
        let ids = self.space_eligible_windows(space);
        let Some(slot_idx) = ids.iter().position(|&id| id == win_id) else {
            return true;
        };
        let area = self.desktop_work_area();
        match self.space_layouts.mode(space) {
            SpaceMode::Free => false,
            SpaceMode::Tile => {
                self.space_layouts.spaces[space as usize].ensure_splits(ids.len());
                let base_splits = self.space_layouts.spaces[space as usize]
                    .drag_splits
                    .clone()
                    .unwrap_or_else(|| self.space_layouts.spaces[space as usize].splits.clone());
                let base = tile_slots(ids.len(), area, &base_splits);
                let (sx, sy) = splits_for_edges(&base[slot_idx], edges);
                let ps = &mut self.space_layouts.spaces[space as usize];
                if let Some(i) = sx {
                    let usable = (base[i].split_parent.w - TILE_GAP).max(2);
                    let min_w = MIN_WINDOW_W.min(usable / 2).max(1);
                    let w = (base[i].rect.w + dx).clamp(min_w, (usable - min_w).max(min_w));
                    ps.splits[i].ratio = w as f32 / usable as f32;
                }
                if let Some(i) = sy {
                    let usable = (base[i].split_parent.h - TILE_GAP).max(2);
                    let min_h = MIN_WINDOW_H.min(usable / 2).max(1);
                    let h = (base[i].rect.h + dy).clamp(min_h, (usable - min_h).max(min_h));
                    ps.splits[i].ratio = h as f32 / usable as f32;
                }
                ps.invalidate();
                self.space_layout_sync();
                true
            }
            SpaceMode::Tape => {
                let inner = tape_inner(area);
                let (min, max) = self
                    .windows
                    .iter()
                    .find(|w| w.id == win_id)
                    .map_or(((MIN_WINDOW_W, MIN_WINDOW_H), (i32::MAX, i32::MAX)), |w| {
                        w.size_limits()
                    });
                let (start_w, start_scroll) = self.space_layouts.spaces[space as usize]
                    .drag_width
                    .filter(|&(id, _, _)| id == win_id)
                    .map_or((inner.w / 2, self.space_layouts.spaces[space as usize].scroll), |(_, w, s)| {
                        (w, s)
                    });
                let raw_w = if edges.right {
                    start_w + dx
                } else if edges.left {
                    start_w - dx
                } else {
                    start_w
                };
                let lo = min.0.max(MIN_WINDOW_W).min(inner.w);
                let hi = max.0.min(inner.w).max(lo);
                let new_w = raw_w.clamp(lo, hi);
                let frac = tape_frac_for_width(new_w, inner.w);
                let ps = &mut self.space_layouts.spaces[space as usize];
                ps.saved_before_full = None;
                ps.set_width_frac(win_id, frac);
                if edges.left && !edges.right {
                    // Anchor the right edge on screen while dragging the left border!
                    ps.scroll = start_scroll + (new_w - start_w);
                }
                ps.invalidate();
                self.space_layout_sync();
                true
            }
        }
    }

    pub fn space_layout_end_resize(&mut self) {
        for ps in &mut self.space_layouts.spaces {
            ps.drag_splits = None;
            ps.drag_width = None;
        }
    }

    pub fn space_layout_drag_preview(&mut self, win_id: u64, px: f64, py: f64) {
        let Some(space) = self
            .windows
            .iter()
            .find(|w| w.id == win_id && w.managed_layout && w.space < MAX_SPACES)
            .map(|w| w.space)
        else {
            return;
        };
        let ids = self.space_eligible_windows(space);
        let area = self.desktop_work_area();
        let target = if ids.len() >= 2 {
            match self.space_layouts.mode(space) {
                SpaceMode::Free => None,
                SpaceMode::Tile if self.space_layouts.spaces[space as usize].zoomed.is_none() => {
                    self.space_layouts.spaces[space as usize].ensure_splits(ids.len());
                    let splits = &self.space_layouts.spaces[space as usize].splits;
                    let slots = tile_slots(ids.len(), area, splits);
                    slots
                        .iter()
                        .position(|s| s.rect.contains(px, py))
                        .filter(|&slot| ids[slot] != win_id)
                        .map(|slot| (win_id, slots[slot].rect, Step::Fill))
                }
                SpaceMode::Tape => {
                    let inner = tape_inner(area);
                    let widths: Vec<i32> = self
                        .tape_column_widths(space, &ids, inner.w)
                        .into_iter()
                        .map(|(_, w)| w)
                        .collect();
                    let (spans, _) = tape_spans(&widths);
                    let scroll = self.space_layouts.spaces[space as usize].scroll;
                    let rects = tape_rects(&spans, inner, scroll);
                    rects
                        .iter()
                        .position(|r| px >= r.x as f64 && px < (r.x + r.w) as f64)
                        .filter(|&slot| ids[slot] != win_id)
                        .map(|slot| (win_id, rects[slot], Step::Fill))
                }
                _ => None,
            }
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

    pub fn space_layout_drop(&mut self, win_id: u64) {
        let Some(space) = self
            .windows
            .iter()
            .find(|w| w.id == win_id && w.managed_layout && w.space < MAX_SPACES)
            .map(|w| w.space)
        else {
            return;
        };
        let ids = self.space_eligible_windows(space);
        let area = self.desktop_work_area();
        let (px, py) = (self.pointer_location.x, self.pointer_location.y);

        let target_id = match self.space_layouts.mode(space) {
            SpaceMode::Free => return,
            SpaceMode::Tile if self.space_layouts.spaces[space as usize].zoomed.is_none() => {
                self.space_layouts.spaces[space as usize].ensure_splits(ids.len());
                let splits = &self.space_layouts.spaces[space as usize].splits;
                let slots = tile_slots(ids.len(), area, splits);
                slots
                    .iter()
                    .position(|s| s.rect.contains(px, py))
                    .and_then(|slot| ids.get(slot).copied())
            }
            SpaceMode::Tape => {
                let inner = tape_inner(area);
                let widths: Vec<i32> = self
                    .tape_column_widths(space, &ids, inner.w)
                    .into_iter()
                    .map(|(_, w)| w)
                    .collect();
                let (spans, _) = tape_spans(&widths);
                let scroll = self.space_layouts.spaces[space as usize].scroll;
                let rects = tape_rects(&spans, inner, scroll);
                rects
                    .iter()
                    .position(|r| px >= r.x as f64 && px < (r.x + r.w) as f64)
                    .and_then(|slot| ids.get(slot).copied())
            }
            _ => None,
        };

        let ps = &mut self.space_layouts.spaces[space as usize];
        if let (Some(target), Some(from)) = (
            target_id.filter(|&t| t != win_id),
            ps.order.iter().position(|&o| o == win_id),
        ) {
            if let Some(to) = ps.order.iter().position(|&o| o == target) {
                ps.order.swap(from, to);
            }
        }
        ps.focused_col = None;
        ps.invalidate();
        self.shell.tile_preview.clear();
        self.space_layout_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_roundtrip_and_cycle() {
        for m in SpaceMode::ALL {
            assert_eq!(SpaceMode::parse(m.id()), Some(m));
        }
        assert_eq!(SpaceMode::Free.next(), SpaceMode::Tile);
        assert_eq!(SpaceMode::Tile.next(), SpaceMode::Tape);
        assert_eq!(SpaceMode::Tape.next(), SpaceMode::Free);
        assert_eq!(SpaceMode::parse("niri"), Some(SpaceMode::Tape));
        assert_eq!(SpaceMode::parse("bsp"), Some(SpaceMode::Tile));
    }

    #[test]
    fn tile_slots_pad_single_and_multiple_windows_without_overlap() {
        let area = Rect { x: 0, y: 28, w: 1440, h: 800 };
        let one = tile_slots(1, area, &[]);
        assert_eq!(one.len(), 1);
        assert_eq!(
            one[0].rect,
            Rect {
                x: TILE_GAP,
                y: 28 + TILE_GAP,
                w: 1440 - 2 * TILE_GAP,
                h: 800 - 2 * TILE_GAP,
            }
        );
        assert!(!one[0].inner_edges().any());

        let three = tile_slots(3, area, &[]);
        assert_eq!(three.len(), 3);
        assert!(three[0].inner_edges().right);
        assert!(three[1].inner_edges().left && three[1].inner_edges().bottom);
        assert!(three[2].inner_edges().left && three[2].inner_edges().top);
        for i in 0..three.len() {
            for j in i + 1..three.len() {
                assert_eq!(three[i].rect.intersection(three[j].rect), 0);
            }
        }
    }

    #[test]
    fn tape_proportional_columns_fit_exact_fractions_and_scroll_with_peek() {
        let area = Rect { x: 0, y: 28, w: 1440, h: 800 };
        let inner = tape_inner(area);
        assert_eq!(inner.w, 1428);

        // Two 1/2 columns fit inner.w pixel-perfectly with TAPE_GAP between them
        let half = tape_col_width(0.5, inner.w, MIN_WINDOW_W, i32::MAX);
        assert_eq!(half + TAPE_GAP + half, inner.w);

        // 2/3 + 1/3 also fit inner.w pixel-perfectly
        let two_thirds = tape_col_width(2.0 / 3.0, inner.w, MIN_WINDOW_W, i32::MAX);
        let one_third = tape_col_width(1.0 / 3.0, inner.w, MIN_WINDOW_W, i32::MAX);
        assert_eq!(two_thirds + TAPE_GAP + one_third, inner.w);

        // Full width column is exact inner.w
        let full = tape_col_width(1.0, inner.w, MIN_WINDOW_W, i32::MAX);
        assert_eq!(full, inner.w);

        // A single 1/2 column is centered horizontally
        let (spans_1, total_1) = tape_spans(&[half]);
        let centered = tape_ensure_visible(&spans_1, total_1, 0, inner.w, 0);
        let r1 = tape_rects(&spans_1, inner, centered);
        assert_eq!((r1[0].x - inner.x) - ((inner.x + inner.w) - (r1[0].x + r1[0].w)), 0);

        // Three 1/2 columns scroll cleanly with neighbor peek on interior column
        let (spans_3, total_3) = tape_spans(&[half, half, half]);
        let s0 = tape_ensure_visible(&spans_3, total_3, 0, inner.w, 0);
        assert_eq!(s0, 0);
        let s1 = tape_ensure_visible(&spans_3, total_3, 1, inner.w, s0);
        assert_eq!(s1, TAPE_PEEK);
        let s2 = tape_ensure_visible(&spans_3, total_3, 2, inner.w, s1);
        assert_eq!(s2, total_3 - inner.w);
        let s_back = tape_ensure_visible(&spans_3, total_3, 0, inner.w, s2);
        assert_eq!(s_back, 0);
    }

    #[test]
    fn removing_space_shifts_later_modes_left() {
        let mut layouts = SpaceLayouts::default();
        layouts.load_modes(&[(0, SpaceMode::Free), (1, SpaceMode::Tile), (2, SpaceMode::Tape)]);
        layouts.on_space_removed(1);
        assert_eq!(layouts.mode(0), SpaceMode::Free);
        assert_eq!(layouts.mode(1), SpaceMode::Tape);
        assert_eq!(layouts.mode(2), SpaceMode::Free);
    }
}
