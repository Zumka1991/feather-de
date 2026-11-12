//! «Призрачный PiP» (Picture-in-Picture + Click-Through Ghost Mode):
//! - Pins any window above normal/fullscreen windows across all user desktops (`sticky`).
//! - Excludes it from automatic `Tile` / `Tape` layouts while preserving its restore slot.
//! - Magnetically snaps to the nearest screen corner with GPU spring motion.
//! - Optional «Сквозь курсор» (`pip_ghost`) renders the window at translucent GPU alpha (`0.68`)
//!   and passes all pointer clicks, hovers, and wheel scrolls straight through to the window
//!   underneath unless `Super` is held.

use crate::banner::{BannerIcon, Message};
use crate::monitors::Rect;
use crate::spaces::MAX_SPACES;
use crate::window::ManagedWindow;
use crate::FeatherCompositor;

pub const PIP_MARGIN: i32 = 16;
pub const PIP_GHOST_ALPHA: f32 = 0.68;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PipCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

impl PipCorner {
    pub const fn id(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "top-left" | "tl" | "nw" => Some(Self::TopLeft),
            "top-right" | "tr" | "ne" => Some(Self::TopRight),
            "bottom-left" | "bl" | "sw" => Some(Self::BottomLeft),
            "bottom-right" | "br" | "se" => Some(Self::BottomRight),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PipSaved {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub maximized: bool,
    pub tiled: Option<crate::tiling::Tile>,
}

/// Logical `(x, y)` of a PiP frame `(w, total_h)` anchored to `corner` inside `area`.
pub fn pip_corner_pos(area: Rect, w: i32, total_h: i32, corner: PipCorner) -> (i32, i32) {
    let left = area.x + PIP_MARGIN;
    let right = (area.x + area.w - w - PIP_MARGIN).max(left);
    let top = area.y + PIP_MARGIN;
    let bottom = (area.y + area.h - total_h - PIP_MARGIN).max(top);
    match corner {
        PipCorner::TopLeft => (left, top),
        PipCorner::TopRight => (right, top),
        PipCorner::BottomLeft => (left, bottom),
        PipCorner::BottomRight => (right, bottom),
    }
}

/// Nearest corner of `area` to a window's center `(cx, cy)`.
pub fn nearest_pip_corner(area: Rect, cx: i32, cy: i32) -> PipCorner {
    let left = cx < area.x + area.w / 2;
    let top = cy < area.y + area.h / 2;
    match (left, top) {
        (true, true) => PipCorner::TopLeft,
        (false, true) => PipCorner::TopRight,
        (true, false) => PipCorner::BottomLeft,
        (false, false) => PipCorner::BottomRight,
    }
}

/// Compact PiP content `(w, h)` preserving aspect ratio within client `size_limits`.
pub fn pip_initial_size(
    area: Rect,
    cur_w: i32,
    cur_h: i32,
    limits: ((i32, i32), (i32, i32)),
) -> (i32, i32) {
    let ((min_w, min_h), (max_w, max_h)) = limits;
    let target_w = ((area.w as f32 * 0.28).round() as i32)
        .clamp(320, 520)
        .clamp(min_w, max_w.max(min_w));
    let aspect = if cur_w > 0 && cur_h > 0 {
        (cur_w as f32 / cur_h as f32).clamp(1.25, 2.25)
    } else {
        16.0 / 9.0
    };
    let target_h = ((target_w as f32 / aspect).round() as i32)
        .clamp(180, 360)
        .clamp(min_h, max_h.max(min_h));
    (target_w, target_h)
}

impl FeatherCompositor {
    /// True when `win` is in click-through Ghost PiP mode right now (`Super` is not held).
    pub fn pip_is_ghosted(&self, win: &ManagedWindow) -> bool {
        win.pip && win.pip_ghost && !self.keyboard.modifier_state().logo
    }

    pub fn has_visible_ghost_pip(&self) -> bool {
        self.windows
            .iter()
            .any(|w| w.pip && w.pip_ghost && w.is_visible())
    }

    /// Topbar indicator state: `None` if no PiP window is active, `Some(any_ghost)` otherwise.
    pub fn pip_bar_state(&self) -> Option<bool> {
        let mut any_pip = false;
        let mut any_ghost = false;
        for w in &self.windows {
            if w.pip && w.mapped && !w.minimized && !w.larvis {
                any_pip = true;
                any_ghost |= w.pip_ghost;
            }
        }
        any_pip.then_some(any_ghost)
    }

    pub fn sync_pip_topbar(&mut self) {
        let state = self.pip_bar_state();
        if self.shell.pip_state != state {
            self.shell.pip_state = state;
            self.shell.topbar_dirty = true;
        }
    }

    /// Chooses the target window for `Super + P`: if the pointer is over a visible PiP window
    /// (even ghosted), act on that PiP window; otherwise act on the focused visible window, or
    /// fall back to the topmost visible PiP window.
    pub fn resolve_pip_target(&self) -> Option<u64> {
        let (px, py) = (self.pointer_location.x, self.pointer_location.y);
        if let Some(w) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.pip && w.is_visible() && w.contains_point(px, py))
        {
            return Some(w.id);
        }
        if let Some(w) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.activated && w.is_visible() && !w.larvis && w.space != crate::scratch::SCRATCH_SPACE)
        {
            return Some(w.id);
        }
        if let Some(w) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.pip && w.is_visible())
        {
            return Some(w.id);
        }
        self.windows
            .iter()
            .rev()
            .find(|w| w.is_visible() && !w.larvis && w.space != crate::scratch::SCRATCH_SPACE)
            .map(|w| w.id)
    }

    /// Chooses the target window for `Super + Alt + P` («Сквозь курсор»):
    /// 1. Pointer over a visible PiP window → toggle its ghost mode.
    /// 2. Focused window is PiP → toggle its ghost mode.
    /// 3. Any visible PiP window exists → toggle its ghost mode.
    /// 4. Focused normal window → enter PiP + Ghost in one step!
    pub fn toggle_focused_pip_ghost(&mut self) {
        if self.larvis_shown || self.shell.scratch_open {
            return;
        }
        let (px, py) = (self.pointer_location.x, self.pointer_location.y);
        let hovered_pip = self
            .windows
            .iter()
            .rev()
            .find(|w| w.pip && w.is_visible() && w.contains_point(px, py))
            .map(|w| (w.id, w.pip_ghost));
        if let Some((id, ghost)) = hovered_pip {
            let _ = self.set_window_pip_state(id, true, !ghost, true);
            return;
        }
        let active = self
            .windows
            .iter()
            .rev()
            .find(|w| w.activated && w.is_visible() && !w.larvis && w.space != crate::scratch::SCRATCH_SPACE)
            .or_else(|| {
                self.windows
                    .iter()
                    .rev()
                    .find(|w| !w.pip_ghost && w.is_visible() && !w.larvis && w.space != crate::scratch::SCRATCH_SPACE)
            })
            .map(|w| (w.id, w.pip, w.pip_ghost));
        if let Some((id, true, ghost)) = active {
            let _ = self.set_window_pip_state(id, true, !ghost, true);
            return;
        }
        if let Some((id, ghost)) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.pip && w.is_visible())
            .map(|w| (w.id, w.pip_ghost))
        {
            let _ = self.set_window_pip_state(id, true, !ghost, true);
            return;
        }
        if let Some((id, false, _)) = active {
            let _ = self.set_window_pip_state(id, true, true, true);
        }
    }

    pub fn toggle_focused_pip(&mut self) {
        if self.larvis_shown || self.shell.scratch_open {
            return;
        }
        let Some(id) = self.resolve_pip_target() else {
            return;
        };
        let Some(currently_pip) = self.windows.iter().find(|w| w.id == id).map(|w| w.pip) else {
            return;
        };
        let _ = self.set_window_pip_state(id, !currently_pip, false, true);
    }

    /// Topbar PiP pill click: left click toggles ghost (or focuses PiP), right click exits PiP.
    pub fn click_topbar_pip(&mut self, left_button: bool) {
        let Some((id, ghost)) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.pip && w.mapped && !w.minimized && !w.larvis)
            .map(|w| (w.id, w.pip_ghost))
        else {
            return;
        };
        if left_button {
            let _ = self.set_window_pip_state(id, true, !ghost, true);
        } else {
            let _ = self.set_window_pip_state(id, false, false, true);
        }
    }

    /// Sets `pip` and `pip_ghost` on window `id`. Returns `false` if no eligible window matches.
    pub fn set_window_pip_state(
        &mut self,
        id: u64,
        pip: bool,
        ghost: bool,
        show_banner: bool,
    ) -> bool {
        let Some(idx) = self
            .windows
            .iter()
            .position(|w| w.id == id && w.mapped && !w.larvis)
        else {
            return false;
        };
        if self.windows[idx].space == crate::scratch::SCRATCH_SPACE {
            return false;
        }

        let mon = self.window_monitor(idx);
        let area = self.desktop_work_area();
        let was_pip = self.windows[idx].pip;
        let was_ghost = self.windows[idx].pip_ghost;
        let old_pos = (self.windows[idx].x, self.windows[idx].y);

        if pip && !was_pip {
            if self.windows[idx].fullscreen {
                self.windows[idx].fullscreen_on(false, mon);
            }
            if self.windows[idx].managed_layout {
                self.windows[idx].leave_managed_layout(mon);
            }
            let saved = PipSaved {
                x: self.windows[idx].x,
                y: self.windows[idx].y,
                w: self.windows[idx].width,
                h: self.windows[idx].height,
                maximized: self.windows[idx].maximized,
                tiled: self.windows[idx].tiled,
            };
            let limits = self.windows[idx].size_limits();
            let (pw, ph) = pip_initial_size(
                area,
                self.windows[idx].width,
                self.windows[idx].height,
                limits,
            );
            let win = &mut self.windows[idx];
            win.pip_saved = Some(saved);
            win.maximized = false;
            win.tiled = None;
            win.minimized = false;
            win.clear_xdg_maximized_and_tiled();
            win.resize_client(pw, ph);
            if win.is_builtin() {
                win.width = pw;
                win.height = ph;
                win.dirty = true;
            }
            let est_total_h = ph + win.titlebar_h();
            let corner = win.pip_corner;
            let (nx, ny) = pip_corner_pos(area, pw, est_total_h, corner);
            win.x = nx;
            win.y = ny;
            win.resize_anchor = (!win.is_builtin()).then_some(crate::window::ResizeAnchor {
                edges: crate::window::Edges {
                    left: matches!(corner, PipCorner::TopRight | PipCorner::BottomRight),
                    right: false,
                    top: matches!(corner, PipCorner::BottomLeft | PipCorner::BottomRight),
                    bottom: false,
                },
                right: area.x + area.w - PIP_MARGIN,
                bottom: area.y + area.h - PIP_MARGIN,
            });
            win.pip = true;
            win.pip_ghost = ghost;
            if self.spaces.current < MAX_SPACES {
                win.space = self.spaces.current;
            }
            self.animations
                .glide_window(id, (old_pos.0 - nx) as f64, (old_pos.1 - ny) as f64);
        } else if !pip && was_pip {
            let saved = self.windows[idx].pip_saved.take();
            let space_mode = self.current_space_mode();
            let win = &mut self.windows[idx];
            win.pip = false;
            win.pip_ghost = false;
            win.resize_anchor = None;
            if self.spaces.current < MAX_SPACES {
                win.space = self.spaces.current;
            }
            let (nx, ny) = if let Some(s) = saved {
                win.resize_client(s.w, s.h);
                if win.is_builtin() {
                    win.width = s.w;
                    win.height = s.h;
                    win.dirty = true;
                }
                win.x = s.x;
                win.y = s.y;
                if space_mode != crate::space_layout::SpaceMode::Free && !win.has_parent() {
                    win.enter_managed_layout();
                } else if s.maximized && space_mode == crate::space_layout::SpaceMode::Free {
                    win.maximize_on(true, mon);
                }
                (win.x, win.y)
            } else {
                if space_mode != crate::space_layout::SpaceMode::Free && !win.has_parent() {
                    win.enter_managed_layout();
                }
                (win.x, win.y)
            };
            if space_mode != crate::space_layout::SpaceMode::Free {
                self.space_layouts.invalidate_space(self.spaces.current);
            }
            self.animations
                .glide_window(id, (old_pos.0 - nx) as f64, (old_pos.1 - ny) as f64);
        } else if pip {
            self.windows[idx].pip_ghost = ghost;
        } else {
            return true;
        }

        // Focus management:
        // - Turning Ghost ON hands keyboard focus back to the underlying window so the user
        //   can immediately keep typing/playing underneath.
        // - Entering interactive PiP or turning Ghost OFF brings keyboard focus to the PiP window.
        if pip && ghost {
            if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
                if pos + 1 == self.windows.len() && self.windows.len() > 1 {
                    let win = self.windows.remove(pos);
                    let insert_at = self
                        .windows
                        .iter()
                        .rposition(|w| w.is_visible() && !w.pip)
                        .unwrap_or(0);
                    self.windows.insert(insert_at, win);
                }
            }
        } else if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let win = self.windows.remove(pos);
            self.windows.push(win);
        }

        self.apply_space_visibility();
        self.space_layout_sync();
        self.sync_pip_topbar();
        self.dock_model_dirty = true;
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
        self.refresh_focus();

        if show_banner && (was_pip != pip || was_ghost != ghost) {
            let msg = match (pip, ghost) {
                (true, true) => Message {
                    icon: BannerIcon::Pip(true),
                    title: "Призрачный PiP · Сквозь курсор",
                    detail: "Клики проходят насквозь · Super — захват окна",
                },
                (true, false) => Message {
                    icon: BannerIcon::Pip(false),
                    title: "Картинка в картинке (PiP)",
                    detail: "Поверх всех столов · ⌥⌘P — сквозь курсор",
                },
                (false, _) => Message {
                    icon: BannerIcon::Pip(false),
                    title: "Обычное окно",
                    detail: "Закрепление поверх всех столов снято",
                },
            };
            self.show_banner(msg);
        }
        true
    }

    /// Snaps a dragged PiP window to the nearest corner of the work area on mouse release.
    pub fn snap_pip_after_move(&mut self, win_id: u64) -> bool {
        let Some(idx) = self
            .windows
            .iter()
            .position(|w| w.id == win_id && w.pip && w.mapped)
        else {
            return false;
        };
        let area = self.desktop_work_area();
        let win = &self.windows[idx];
        let cx = win.x + win.width / 2;
        let cy = win.y + win.total_height() / 2;
        let corner = nearest_pip_corner(area, cx, cy);
        let (nx, ny) = pip_corner_pos(area, win.width, win.total_height(), corner);
        let (ox, oy) = (win.x, win.y);
        let win = &mut self.windows[idx];
        win.pip_corner = corner;
        win.x = nx;
        win.y = ny;
        if ox != nx || oy != ny {
            self.animations
                .glide_window(win_id, (ox - nx) as f64, (oy - ny) as f64);
        }
        self.needs_redraw = true;
        true
    }

    /// Keeps PiP windows anchored to their chosen corners when screen resolution/Dock changes.
    pub fn relayout_pip_windows(&mut self) {
        if !self.windows.iter().any(|w| w.pip && w.mapped) {
            return;
        }
        let area = self.desktop_work_area();
        for win in &mut self.windows {
            if win.pip && win.mapped && !win.minimized {
                let (nx, ny) = pip_corner_pos(area, win.width, win.total_height(), win.pip_corner);
                win.x = nx;
                win.y = ny;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_corner_and_placement_stay_inside_work_area_margins() {
        let area = Rect {
            x: 0,
            y: 28,
            w: 1440,
            h: 800,
        };
        assert_eq!(nearest_pip_corner(area, 200, 120), PipCorner::TopLeft);
        assert_eq!(nearest_pip_corner(area, 1200, 120), PipCorner::TopRight);
        assert_eq!(nearest_pip_corner(area, 200, 700), PipCorner::BottomLeft);
        assert_eq!(nearest_pip_corner(area, 1200, 700), PipCorner::BottomRight);

        let (x, y) = pip_corner_pos(area, 380, 240, PipCorner::BottomRight);
        assert_eq!(x, 1440 - 380 - PIP_MARGIN);
        assert_eq!(y, 28 + 800 - 240 - PIP_MARGIN);

        let (pw, ph) = pip_initial_size(area, 1280, 720, ((200, 120), (4096, 4096)));
        assert!((320..=520).contains(&pw));
        assert!((180..=360).contains(&ph));
    }
}
