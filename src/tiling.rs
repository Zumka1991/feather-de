//! Window tiling, like macOS Sequoia/Tahoe: drag a window to a screen edge or corner, use the
//! green button's menu or Ctrl+Super+arrows. There is no background work: the preview exists
//! only during a drag and the green button's timer only while the pointer rests on it.
use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement}, Kind},
            gles::GlesRenderer,
        },
    },
    reexports::calloop::{timer::{TimeoutAction, Timer}, RegistrationToken},
    utils::{Rectangle, Transform},
};
use tiny_skia::Pixmap;

use crate::monitors::Rect;
use crate::shell::DockMenuAction;
use crate::theme::{appearance, fill_rounded_rect, palette, rgba, stroke_rounded_rect, ui_scale};
use crate::FeatherCompositor;

/// Margin around and between tiled windows, as in macOS
const GAP: i32 = 6;
/// How close to a screen edge the pointer must be
const EDGE: f64 = 3.0;
/// Along an edge, how far from a corner still means the corner
const CORNER: f64 = 72.0;
/// Pointer travel before a tiled or maximized window lets go: a click must not resize it
pub const DETACH_DISTANCE: i32 = 6;
const GREEN_MENU_DELAY: Duration = Duration::from_millis(450);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Where a window goes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Tile(Tile),
    Fill,
    /// Back to the size it had before
    Free,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Tile {
    /// The window's frame inside the work area
    pub fn frame(self, a: Rect) -> Rect {
        let inner_w = a.w - 2 * GAP;
        let inner_h = a.h - 2 * GAP;
        let half_w = (inner_w - GAP) / 2;
        let half_h = (inner_h - GAP) / 2;
        let (x0, y0) = (a.x + GAP, a.y + GAP);
        let (x1, y1) = (a.x + a.w - GAP - half_w, a.y + a.h - GAP - half_h);
        let (x, y, w, h) = match self {
            Tile::Left => (x0, y0, half_w, inner_h),
            Tile::Right => (x1, y0, half_w, inner_h),
            Tile::Top => (x0, y0, inner_w, half_h),
            Tile::Bottom => (x0, y1, inner_w, half_h),
            Tile::TopLeft => (x0, y0, half_w, half_h),
            Tile::TopRight => (x1, y0, half_w, half_h),
            Tile::BottomLeft => (x0, y1, half_w, half_h),
            Tile::BottomRight => (x1, y1, half_w, half_h),
        };
        Rect { x, y, w, h }
    }

    /// Screen edges the window touches: (left, right, top, bottom)
    pub fn edges(self) -> (bool, bool, bool, bool) {
        match self {
            Tile::Left => (true, false, true, true),
            Tile::Right => (false, true, true, true),
            Tile::Top => (true, true, true, false),
            Tile::Bottom => (true, true, false, true),
            Tile::TopLeft => (true, false, true, false),
            Tile::TopRight => (false, true, true, false),
            Tile::BottomLeft => (true, false, false, true),
            Tile::BottomRight => (false, true, false, true),
        }
    }
}

/// The monitor's area for windows: below the menu bar and clear of the Dock on the primary one
pub fn work_area(r: Rect) -> Rect {
    if r.x == 0 && r.y == 0 {
        let (x, y, w, h) = crate::theme::maximized_area(r.w, r.h);
        Rect { x, y, w, h }
    } else {
        r
    }
}

/// What dropping a window here would do. `open` is (left, right, top): false where the edge
/// borders another monitor, so that crossing to it is not a request to tile.
pub fn zone_at(x: f64, y: f64, r: Rect, open: (bool, bool, bool)) -> Option<Step> {
    let (rx, ry, rw, rh) = (r.x as f64, r.y as f64, r.w as f64, r.h as f64);
    let left = open.0 && x <= rx + EDGE;
    let right = open.1 && x >= rx + rw - 1.0 - EDGE;
    let top = open.2 && y <= ry + EDGE;
    let (near_top, near_bottom) = (y <= ry + CORNER, y >= ry + rh - CORNER);
    Some(match (left, right) {
        (true, _) if near_top => Step::Tile(Tile::TopLeft),
        (true, _) if near_bottom => Step::Tile(Tile::BottomLeft),
        (true, _) => Step::Tile(Tile::Left),
        (_, true) if near_top => Step::Tile(Tile::TopRight),
        (_, true) if near_bottom => Step::Tile(Tile::BottomRight),
        (_, true) => Step::Tile(Tile::Right),
        _ if top => Step::Fill,
        _ => return None,
    })
}

/// Ctrl+Super+arrow from the current placement. Arrows compose: left then up is the top left
/// quarter, and down from a full window gives the old size back.
pub fn next_step(current: Option<Tile>, maximized: bool, dir: Dir) -> Step {
    use Tile::*;
    if maximized {
        return match dir {
            Dir::Left => Step::Tile(Left),
            Dir::Right => Step::Tile(Right),
            Dir::Up => Step::Fill,
            Dir::Down => Step::Free,
        };
    }
    match (dir, current) {
        (Dir::Left, Some(TopRight | TopLeft | Top)) => Step::Tile(TopLeft),
        (Dir::Left, Some(BottomRight | BottomLeft | Bottom)) => Step::Tile(BottomLeft),
        (Dir::Left, _) => Step::Tile(Left),
        (Dir::Right, Some(TopLeft | TopRight | Top)) => Step::Tile(TopRight),
        (Dir::Right, Some(BottomLeft | BottomRight | Bottom)) => Step::Tile(BottomRight),
        (Dir::Right, _) => Step::Tile(Right),
        (Dir::Up, Some(Left)) => Step::Tile(TopLeft),
        (Dir::Up, Some(Right)) => Step::Tile(TopRight),
        (Dir::Up, Some(BottomLeft)) => Step::Tile(Left),
        (Dir::Up, Some(BottomRight)) => Step::Tile(Right),
        (Dir::Up, Some(Bottom)) => Step::Tile(Top),
        (Dir::Up, _) => Step::Fill,
        (Dir::Down, Some(Left)) => Step::Tile(BottomLeft),
        (Dir::Down, Some(Right)) => Step::Tile(BottomRight),
        (Dir::Down, Some(TopLeft)) => Step::Tile(Left),
        (Dir::Down, Some(TopRight)) => Step::Tile(Right),
        (Dir::Down, Some(Top)) => Step::Tile(Bottom),
        (Dir::Down, Some(_)) => Step::Free,
        (Dir::Down, None) => Step::Tile(Bottom),
    }
}

/// The items of the green button's menu and of the «Окно» menu
pub fn menu_steps() -> [(&'static str, Step); 9] {
    [
        ("Заполнить экран", Step::Fill),
        ("Левая половина", Step::Tile(Tile::Left)),
        ("Правая половина", Step::Tile(Tile::Right)),
        ("Верхняя половина", Step::Tile(Tile::Top)),
        ("Нижняя половина", Step::Tile(Tile::Bottom)),
        ("Левая верхняя четверть", Step::Tile(Tile::TopLeft)),
        ("Правая верхняя четверть", Step::Tile(Tile::TopRight)),
        ("Левая нижняя четверть", Step::Tile(Tile::BottomLeft)),
        ("Правая нижняя четверть", Step::Tile(Tile::BottomRight)),
    ]
}

/// The translucent place a dropped window will take: nine small pieces stretched on the GPU,
/// so its memory does not depend on the size of the screen.
#[derive(Default)]
pub struct Preview {
    /// Dragged window (drawn right behind it) and the frame it would get
    pub target: Option<(u64, Rect, Step)>,
    pieces: Vec<MemoryRenderBuffer>,
    built_for: Option<(u32, bool, (u8, u8, u8))>,
    corner: i32,
}

impl Preview {
    /// Keeps the pieces only while they are needed: dropped at the end of every drag
    pub fn clear(&mut self) {
        self.target = None;
        self.pieces = Vec::new();
        self.built_for = None;
    }

    pub(crate) fn ensure(&mut self) {
        let scale = ui_scale();
        let accent = palette().accent;
        let channel = |v: f32| (v * 255.0) as u8;
        let key = (scale.to_bits(), appearance() == crate::theme::Appearance::Dark, (channel(accent.red()), channel(accent.green()), channel(accent.blue())));
        if self.built_for == Some(key) && self.pieces.len() == 9 {
            return;
        }
        let radius = 14.0;
        let k = ((radius + 2.0) * scale).ceil() as i32;
        let side = (2 * k + 1) as u32;
        let Some(mut pixmap) = Pixmap::new(side, side) else { return };
        let logical = side as f32 / scale;
        {
            let mut view = pixmap.as_mut();
            let mut fill = accent;
            fill.set_alpha(0.28);
            fill_rounded_rect(&mut view, 0.0, 0.0, logical, logical, radius, fill);
            stroke_rounded_rect(&mut view, 0.5, 0.5, logical - 1.0, logical - 1.0, radius, 1.0, rgba(255, 255, 255, 110));
        }
        let data = pixmap.data();
        let stride = side as usize * 4;
        let crop = |x: i32, y: i32, w: i32, h: i32| {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for row in y..y + h {
                let start = row as usize * stride + x as usize * 4;
                out.extend_from_slice(&data[start..start + w as usize * 4]);
            }
            MemoryRenderBuffer::from_slice(&out, Fourcc::Abgr8888, (w, h), 1, Transform::Normal, None)
        };
        self.pieces = vec![
            crop(0, 0, k, k),
            crop(k + 1, 0, k, k),
            crop(0, k + 1, k, k),
            crop(k + 1, k + 1, k, k),
            crop(k, 0, 1, k),
            crop(k, k + 1, 1, k),
            crop(0, k, k, 1),
            crop(k + 1, k, k, 1),
            crop(k, k, 1, 1),
        ];
        self.corner = k;
        self.built_for = Some(key);
    }

    pub fn elements(&self, renderer: &mut GlesRenderer, frame: Rect, scale: f64) -> Vec<MemoryRenderBufferRenderElement<GlesRenderer>> {
        if self.pieces.len() != 9 {
            return Vec::new();
        }
        let k = self.corner;
        let x = (frame.x as f64 * scale).round() as i32;
        let y = (frame.y as f64 * scale).round() as i32;
        let w = (frame.w as f64 * scale).round() as i32;
        let h = (frame.h as f64 * scale).round() as i32;
        if w <= 2 * k || h <= 2 * k {
            return Vec::new();
        }
        let (mid_w, mid_h) = (w - 2 * k, h - 2 * k);
        let places = [
            (x, y, k, k, k, k),
            (x + w - k, y, k, k, k, k),
            (x, y + h - k, k, k, k, k),
            (x + w - k, y + h - k, k, k, k, k),
            (x + k, y, mid_w, k, 1, k),
            (x + k, y + h - k, mid_w, k, 1, k),
            (x, y + k, k, mid_h, k, 1),
            (x + w - k, y + k, k, mid_h, k, 1),
            (x + k, y + k, mid_w, mid_h, 1, 1),
        ];
        let mut out = Vec::with_capacity(9);
        for (piece, (px, py, pw, ph, sw, sh)) in self.pieces.iter().zip(places) {
            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (px as f64, py as f64),
                piece,
                None,
                Some(Rectangle::from_size((sw as f64, sh as f64).into())),
                Some((pw, ph).into()),
                Kind::Unspecified,
            ) {
                out.push(element);
            }
        }
        out
    }
}

impl FeatherCompositor {
    /// Sends a window where `step` says. Fullscreen windows and Larvis's desktop stay put.
    pub fn apply_tile(&mut self, idx: usize, step: Step) {
        let r = self.window_monitor(idx);
        let win = &mut self.windows[idx];
        if win.fullscreen || win.larvis || !win.mapped || win.space == crate::scratch::SCRATCH_SPACE || win.managed_layout {
            return;
        }
        win.resize_anchor = None;
        match step {
            Step::Fill => win.maximize_on(true, r),
            Step::Free if win.maximized => win.maximize_on(false, r),
            Step::Free => win.untile_on(r),
            Step::Tile(tile) => win.tile_to(tile, tile.frame(work_area(r))),
        }
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
        self.dock_model_dirty = true;
        self.window_memory_changed();
    }

    /// Ctrl+Super+arrow on the front window
    pub fn tile_front_step(&mut self, dir: Dir) {
        let Some(idx) = self.windows.iter().rposition(|w| w.is_visible()) else { return };
        if self.windows[idx].space == crate::scratch::SCRATCH_SPACE {
            self.scratch_resize_step(dir);
            return;
        }
        if self.windows[idx].managed_layout && self.current_space_managed() {
            self.space_layout_resize_step(dir);
            return;
        }
        let win = &self.windows[idx];
        let step = next_step(win.tiled, win.maximized, dir);
        self.apply_tile(idx, step);
    }

    pub fn tile_window_id(&mut self, id: u64, step: Step) {
        if let Some(idx) = self.windows.iter().position(|w| w.id == id) {
            self.apply_tile(idx, step);
        }
    }

    /// During a window drag: shows or moves the preview when the pointer reaches a zone
    pub fn tile_drag_motion(&mut self, win_id: u64, x: f64, y: f64) {
        if self.windows.iter().any(|w| w.id == win_id && w.space == crate::scratch::SCRATCH_SPACE) {
            self.scratch_drag_preview(win_id, x, y);
            return;
        }
        if self.window_in_managed_space(win_id) {
            self.space_layout_drag_preview(win_id, x, y);
            return;
        }
        let r = self.monitor_at(x, y);
        let reach = 8.0;
        let open = (
            self.monitor_at(x - reach, y) == r,
            self.monitor_at(x + reach, y) == r,
            self.monitor_at(x, y - reach) == r,
        );
        let target = zone_at(x, y, r, open).map(|step| {
            let area = work_area(r);
            let frame = match step {
                Step::Tile(tile) => tile.frame(area),
                _ => area,
            };
            (win_id, frame, step)
        });
        if self.shell.tile_preview.target != target {
            if target.is_some() {
                self.shell.tile_preview.ensure();
            }
            self.shell.tile_preview.target = target;
            self.needs_redraw = true;
        }
    }

    /// The button went up: drop into the zone, if any
    pub fn tile_drag_finish(&mut self, win_id: u64) {
        if self.window_in_managed_space(win_id) {
            return;
        }
        let target = self.shell.tile_preview.target.take();
        self.shell.tile_preview.clear();
        if let Some((_, _, step)) = target.filter(|t| t.0 == win_id) {
            self.tile_window_id(win_id, step);
        }
        self.needs_redraw = true;
    }

    /// After pointer motion: waiting on the green button opens its menu, as in macOS.
    pub fn green_button_motion(&mut self, x: f64, y: f64) {
        let over = if self.pressed_buttons.is_empty() && !self.shell.any_menu_open() {
            self.windows
                .iter()
                .rev()
                .find(|w| w.is_visible() && w.contains_point(x, y))
                .filter(|w| {
                    w.space != crate::scratch::SCRATCH_SPACE
                        && !w.managed_layout
                        && w.hit_test_traffic(x, y) == Some(crate::window::TrafficButton::Maximize)
                })
                .map(|w| w.id)
        } else {
            None
        };
        if over == self.green_hover.map(|(id, _)| id) {
            return;
        }
        self.cancel_green_hover();
        let Some(id) = over else { return };
        let token: Option<RegistrationToken> = self
            .loop_handle
            .insert_source(Timer::from_duration(GREEN_MENU_DELAY), move |_, _, state| {
                state.green_hover = None;
                state.open_green_menu(id);
                TimeoutAction::Drop
            })
            .ok();
        self.green_hover = token.map(|t| (id, t));
    }

    pub fn cancel_green_hover(&mut self) {
        if let Some((_, token)) = self.green_hover.take() {
            self.loop_handle.remove(token);
        }
    }

    fn open_green_menu(&mut self, id: u64) {
        let Some(win) = self.windows.iter().find(|w| w.id == id && w.is_visible()) else { return };
        if !self.pressed_buttons.is_empty() || self.shell.any_menu_open() {
            return;
        }
        let (tiled, maximized) = (win.tiled, win.maximized);
        let x = win.x as f32 + crate::theme::TRAFFIC_CENTERS[2] - 10.0;
        let y = (win.y + crate::theme::TITLEBAR_HEIGHT) as f32;
        let mut items: Vec<(String, DockMenuAction)> = Vec::with_capacity(10);
        for (label, step) in menu_steps() {
            let on = match step {
                Step::Fill => maximized,
                Step::Tile(tile) => !maximized && tiled == Some(tile),
                Step::Free => false,
            };
            let mark = if on { "✓ " } else { "" };
            items.push((format!("{mark}{}", feather_ui::i18n::tr(label)), DockMenuAction::Tile(id, step)));
        }
        if tiled.is_some() || maximized {
            items.push((feather_ui::i18n::tr("Вернуть прежний размер").into_owned(), DockMenuAction::Tile(id, Step::Free)));
        }
        self.shell.open_context_menu(items, x, y, &mut self.fonts);
        self.needs_redraw = true;
    }

    /// Menu bar «Окно» → placement of the front window
    pub fn tile_focused(&mut self, step: Step) {
        if let Some(idx) = self.windows.iter().rposition(|w| w.is_visible()) {
            self.apply_tile(idx, step);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };

    #[test]
    fn halves_and_quarters_fill_the_area_without_overlap() {
        let area = Rect { x: 0, y: 28, w: 1920, h: 980 };
        let (l, r) = (Tile::Left.frame(area), Tile::Right.frame(area));
        assert_eq!(l.y, area.y + GAP);
        assert_eq!(l.x + l.w + GAP, r.x);
        assert_eq!(r.x + r.w, area.x + area.w - GAP);
        assert_eq!(l.y + l.h, area.y + area.h - GAP);
        let (a, b) = (Tile::TopLeft.frame(area), Tile::BottomRight.frame(area));
        assert_eq!(a.intersection(b), 0);
        assert_eq!(b.x + b.w, area.x + area.w - GAP);
        assert_eq!(b.y + b.h, area.y + area.h - GAP);
        let (t, d) = (Tile::Top.frame(area), Tile::Bottom.frame(area));
        assert_eq!(t.y + t.h + GAP, d.y);
    }

    #[test]
    fn odd_sizes_keep_the_outer_margin() {
        let area = Rect { x: 100, y: 0, w: 1367, h: 769 };
        let r = Tile::BottomRight.frame(area);
        assert_eq!(r.x + r.w, 100 + 1367 - GAP);
        assert_eq!(r.y + r.h, 769 - GAP);
    }

    #[test]
    fn zones_follow_edges_and_corners() {
        let all = (true, true, true);
        assert_eq!(zone_at(0.0, 500.0, SCREEN, all), Some(Step::Tile(Tile::Left)));
        assert_eq!(zone_at(1919.0, 500.0, SCREEN, all), Some(Step::Tile(Tile::Right)));
        assert_eq!(zone_at(800.0, 0.0, SCREEN, all), Some(Step::Fill));
        assert_eq!(zone_at(0.0, 10.0, SCREEN, all), Some(Step::Tile(Tile::TopLeft)));
        assert_eq!(zone_at(1919.0, 1079.0, SCREEN, all), Some(Step::Tile(Tile::BottomRight)));
        assert_eq!(zone_at(0.0, 1079.0, SCREEN, all), Some(Step::Tile(Tile::BottomLeft)));
        assert_eq!(zone_at(800.0, 500.0, SCREEN, all), None);
        assert_eq!(zone_at(800.0, 1079.0, SCREEN, all), None, "the bottom edge belongs to the Dock");
    }

    #[test]
    fn edges_shared_with_another_monitor_are_not_zones() {
        assert_eq!(zone_at(1919.0, 500.0, SCREEN, (true, false, true)), None);
        assert_eq!(zone_at(0.0, 500.0, SCREEN, (false, true, true)), None);
        assert_eq!(zone_at(800.0, 0.0, SCREEN, (true, true, false)), None);
    }

    #[test]
    fn arrows_compose_into_quarters_and_back() {
        use Dir::*;
        assert_eq!(next_step(None, false, Left), Step::Tile(Tile::Left));
        assert_eq!(next_step(Some(Tile::Left), false, Up), Step::Tile(Tile::TopLeft));
        assert_eq!(next_step(Some(Tile::TopLeft), false, Right), Step::Tile(Tile::TopRight));
        assert_eq!(next_step(Some(Tile::TopLeft), false, Down), Step::Tile(Tile::Left));
        assert_eq!(next_step(None, false, Up), Step::Fill);
        assert_eq!(next_step(None, true, Down), Step::Free);
        assert_eq!(next_step(Some(Tile::Left), true, Down), Step::Free);
        assert_eq!(next_step(None, true, Left), Step::Tile(Tile::Left));
        assert_eq!(next_step(Some(Tile::BottomLeft), false, Down), Step::Free);
        assert_eq!(next_step(None, false, Down), Step::Tile(Tile::Bottom));
    }
}
