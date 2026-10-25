//! Pure state & tiny-skia rendering for «Просмотр» (`feather-preview`) in macOS Tahoe style.

use crate::doc::{self, Document};
use crate::theme::{
    colors, fill_circle, fill_rect, fill_rrect, fill_rrect_gradient, line, polyline, rgba,
    rounded_rect_path, stroke_circle, stroke_rrect, ui_scale,
};
use feather_ui::text::FontSystem;
use std::path::PathBuf;
use tiny_skia::{Color, FilterQuality, PixmapMut, PixmapPaint, Transform};

const BTN_LEFT: u32 = 0x110;
const CORNER_R: f32 = 12.0;
const RESIZE_BORDER: f32 = 6.0;
pub const TOOLBAR_H: f32 = 48.0;
pub const SIDEBAR_W: f32 = 164.0;

const ZOOM_STEPS: &[f32] = &[0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];

#[derive(Clone, Copy, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Default,
    Grab,
    Grabbing,
    Resize(u32),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    Close,
    Minimize,
    ToggleMaximize,
    Move,
    Resize(u32),
    OpenDialog,
    LoadPath(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoomMode {
    Fit,
    Manual(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Traffic( usize),
    SidebarToggle,
    PrevPage,
    NextPage,
    ZoomOut,
    ZoomReset,
    ZoomIn,
    RotateCcw,
    RotateCw,
    InfoToggle,
    OpenButton,
    EmptyOpenButton,
    Thumb(usize),
    Canvas,
    ToolbarBg,
}

#[derive(Clone, Copy)]
struct RectBox {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl RectBox {
    fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

pub struct App {
    pub font: FontSystem,
    pub w: f32,
    pub h: f32,
    pub focused: bool,
    pub maximized: bool,
    pub dirty: bool,

    pub doc: Option<Document>,
    pub loading: bool,
    pub error: Option<String>,
    pub page_idx: usize,
    pub sidebar_open: bool,
    pub sidebar_scroll: f32,
    pub info_open: bool,
    pub zoom: ZoomMode,
    pub pan_x: f32,
    pub pan_y: f32,
    pub drop_hover: bool,

    hover: Option<Hit>,
    traffic_hover: bool,
    pan_drag: Option<(f32, f32, f32, f32)>,
    last_click: Option<(std::time::Instant, Hit)>,
}

impl App {
    pub fn new(font: FontSystem) -> Self {
        Self {
            font,
            w: 940.0,
            h: 660.0,
            focused: true,
            maximized: false,
            dirty: true,
            doc: None,
            loading: false,
            error: None,
            page_idx: 0,
            sidebar_open: false,
            sidebar_scroll: 0.0,
            info_open: false,
            zoom: ZoomMode::Fit,
            pan_x: 0.0,
            pan_y: 0.0,
            drop_hover: false,
            hover: None,
            traffic_hover: false,
            pan_drag: None,
            last_click: None,
        }
    }

    pub fn invalidate_layout(&mut self) {
        self.dirty = true;
    }

    pub fn set_loading(&mut self, path: & std::path::Path) {
        self.loading = true;
        self.error = None;
        let _ = path;
        self.dirty = true;
    }

    pub fn set_document(&mut self, result: Result<Document, String>) {
        self.loading = false;
        match result {
            Ok(doc) => {
                self.sidebar_open = doc.pages.len() > 1;
                self.sidebar_scroll = 0.0;
                self.page_idx = 0;
                self.zoom = ZoomMode::Fit;
                self.pan_x = 0.0;
                self.pan_y = 0.0;
                self.error = None;
                self.doc = Some(doc);
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
        self.dirty = true;
    }

    pub fn resize(&mut self, w: f32, h: f32) {
        let (w, h) = (w.max(520.0), h.max(360.0));
        if (self.w - w).abs() > 0.5 || (self.h - h).abs() > 0.5 {
            self.w = w;
            self.h = h;
            self.clamp_pan();
            self.clamp_sidebar_scroll();
            self.dirty = true;
        }
    }

    pub fn drag_over(&mut self) {
        if !self.drop_hover {
            self.drop_hover = true;
            self.dirty = true;
        }
    }

    pub fn drag_leave(&mut self) {
        if self.drop_hover {
            self.drop_hover = false;
            self.dirty = true;
        }
    }

    pub fn dropped(&mut self, paths: Vec<PathBuf>) -> Vec<Effect> {
        self.drop_hover = false;
        self.dirty = true;
        if let Some(first) = paths.into_iter().find(|p| p.is_file() || is_supported_ext(p)) {
            vec![Effect::LoadPath(first)]
        } else {
            Vec::new()
        }
    }

    fn sidebar_visible(&self) -> bool {
        self.sidebar_open && self.doc.is_some()
    }

    fn canvas_rect(&self) -> RectBox {
        let sx = if self.sidebar_visible() { SIDEBAR_W } else { 0.0 };
        RectBox {
            x: sx,
            y: TOOLBAR_H,
            w: (self.w - sx).max(64.0),
            h: (self.h - TOOLBAR_H).max(64.0),
        }
    }

    fn fit_scale(&self) -> f32 {
        let Some(doc) = &self.doc else { return 1.0 };
        let Some(page) = doc.pages.get(self.page_idx) else { return 1.0 };
        let c = self.canvas_rect();
        let pad = 36.0;
        let avail_w = (c.w - pad * 2.0).max(48.0);
        let avail_h = (c.h - pad * 2.0).max(48.0);
        let pw = page.pixmap.width().max(1) as f32;
        let ph = page.pixmap.height().max(1) as f32;
        let s = (avail_w / pw).min(avail_h / ph);
        if doc.format.is_pdf() {
            s.clamp(0.15, 2.5)
        } else {
            s.min(1.0).max(0.1)
        }
    }

    pub fn effective_zoom(&self) -> f32 {
        match self.zoom {
            ZoomMode::Fit => self.fit_scale(),
            ZoomMode::Manual(z) => z.clamp(0.25, 4.0),
        }
    }

    fn clamp_pan(&mut self) {
        if matches!(self.zoom, ZoomMode::Fit) {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
            return;
        }
        let Some(doc) = &self.doc else {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
            return;
        };
        let Some(page) = doc.pages.get(self.page_idx) else { return };
        let c = self.canvas_rect();
        let z = self.effective_zoom();
        let dw = page.pixmap.width() as f32 * z;
        let dh = page.pixmap.height() as f32 * z;
        let max_x = ((dw - c.w * 0.65) * 0.5).max(0.0);
        let max_y = ((dh - c.h * 0.65) * 0.5).max(0.0);
        self.pan_x = self.pan_x.clamp(-max_x, max_x);
        self.pan_y = self.pan_y.clamp(-max_y, max_y);
    }

    fn clamp_sidebar_scroll(&mut self) {
        let count = self.doc.as_ref().map_or(0, |d| d.pages.len());
        let content_h = count as f32 * 180.0 + 16.0;
        let view_h = (self.h - TOOLBAR_H).max(1.0);
        let max_scroll = (content_h - view_h).max(0.0);
        self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, max_scroll);
    }

    fn step_zoom(&mut self, dir: i32) {
        if self.doc.is_none() {
            return;
        }
        let cur = self.effective_zoom();
        let next = if dir > 0 {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|&z| z > cur + 0.03)
                .unwrap_or(4.0)
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|&z| z < cur - 0.03)
                .unwrap_or(0.25)
        };
        self.zoom = ZoomMode::Manual(next);
        self.clamp_pan();
        self.dirty = true;
    }

    fn toggle_fit_or_100(&mut self) {
        if self.doc.is_none() {
            return;
        }
        self.zoom = match self.zoom {
            ZoomMode::Fit => ZoomMode::Manual(1.0),
            ZoomMode::Manual(_) => ZoomMode::Fit,
        };
        self.clamp_pan();
        self.dirty = true;
    }

    fn select_page(&mut self, idx: usize) {
        let Some(doc) = &self.doc else { return };
        if idx < doc.pages.len() && idx != self.page_idx {
            self.page_idx = idx;
            self.pan_x = 0.0;
            self.pan_y = 0.0;
            // Ensure active thumbnail is visible in sidebar
            let item_top = idx as f32 * 180.0;
            let item_bottom = item_top + 180.0;
            let view_h = (self.h - TOOLBAR_H).max(1.0);
            if item_top < self.sidebar_scroll {
                self.sidebar_scroll = item_top;
            } else if item_bottom > self.sidebar_scroll + view_h {
                self.sidebar_scroll = item_bottom - view_h;
            }
            self.clamp_sidebar_scroll();
            self.dirty = true;
        }
    }

    fn rotate(&mut self, clockwise: bool) {
        let Some(doc) = &mut self.doc else { return };
        doc.rotate_page(self.page_idx, clockwise);
        self.clamp_pan();
        self.dirty = true;
    }

    // --- Layout & Hit-testing ---

    fn sidebar_toggle_box(&self) -> RectBox {
        RectBox { x: 84.0, y: 10.0, w: 28.0, h: 28.0 }
    }

    fn prev_page_box(&self) -> RectBox {
        RectBox { x: 118.0, y: 10.0, w: 28.0, h: 28.0 }
    }

    fn next_page_box(&self) -> RectBox {
        RectBox { x: 148.0, y: 10.0, w: 28.0, h: 28.0 }
    }

    fn open_btn_box(&self) -> RectBox {
        RectBox { x: self.w - 94.0, y: 10.0, w: 80.0, h: 28.0 }
    }

    fn info_btn_box(&self) -> RectBox {
        RectBox { x: self.w - 128.0, y: 10.0, w: 28.0, h: 28.0 }
    }

    fn rotate_cw_box(&self) -> RectBox {
        RectBox { x: self.w - 162.0, y: 10.0, w: 28.0, h: 28.0 }
    }

    fn rotate_ccw_box(&self) -> RectBox {
        RectBox { x: self.w - 192.0, y: 10.0, w: 28.0, h: 28.0 }
    }

    fn zoom_in_box(&self) -> RectBox {
        RectBox { x: self.w - 228.0, y: 10.0, w: 28.0, h: 28.0 }
    }

    fn zoom_reset_box(&self) -> RectBox {
        RectBox { x: self.w - 294.0, y: 10.0, w: 64.0, h: 28.0 }
    }

    fn zoom_out_box(&self) -> RectBox {
        RectBox { x: self.w - 324.0, y: 10.0, w: 28.0, h: 28.0 }
    }

    fn empty_open_btn_box(&self) -> RectBox {
        let c = self.canvas_rect();
        let bw = 168.0;
        let bh = 36.0;
        RectBox {
            x: c.x + (c.w - bw) * 0.5,
            y: c.y + c.h * 0.5 + 72.0,
            w: bw,
            h: bh,
        }
    }

    fn resize_edges(&self, x: f32, y: f32) -> u32 {
        if self.maximized {
            return 0;
        }
        let mut e = 0;
        if y < RESIZE_BORDER {
            e |= 1;
        } else if y > self.h - RESIZE_BORDER {
            e |= 2;
        }
        if x < RESIZE_BORDER {
            e |= 4;
        } else if x > self.w - RESIZE_BORDER {
            e |= 8;
        }
        e
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<Hit> {
        if y < TOOLBAR_H {
            for (i, cx) in [18.0f32, 38.0, 58.0].into_iter().enumerate() {
                if (x - cx).hypot(y - 24.0) <= 8.0 {
                    return Some(Hit::Traffic(i));
                }
            }
            if self.doc.is_some() {
                if self.sidebar_toggle_box().contains(x, y) {
                    return Some(Hit::SidebarToggle);
                }
                if self.doc.as_ref().is_some_and(|d| d.pages.len() > 1) {
                    if self.prev_page_box().contains(x, y) {
                        return Some(Hit::PrevPage);
                    }
                    if self.next_page_box().contains(x, y) {
                        return Some(Hit::NextPage);
                    }
                }
                if self.zoom_out_box().contains(x, y) {
                    return Some(Hit::ZoomOut);
                }
                if self.zoom_reset_box().contains(x, y) {
                    return Some(Hit::ZoomReset);
                }
                if self.zoom_in_box().contains(x, y) {
                    return Some(Hit::ZoomIn);
                }
                if self.rotate_ccw_box().contains(x, y) {
                    return Some(Hit::RotateCcw);
                }
                if self.rotate_cw_box().contains(x, y) {
                    return Some(Hit::RotateCw);
                }
                if self.info_btn_box().contains(x, y) {
                    return Some(Hit::InfoToggle);
                }
            }
            if self.open_btn_box().contains(x, y) {
                return Some(Hit::OpenButton);
            }
            return Some(Hit::ToolbarBg);
        }

        if self.sidebar_visible() && x < SIDEBAR_W {
            if let Some(doc) = &self.doc {
                let rel_y = y - TOOLBAR_H + self.sidebar_scroll - 12.0;
                if rel_y >= 0.0 {
                    let idx = (rel_y / 180.0).floor() as usize;
                    if idx < doc.pages.len() {
                        return Some(Hit::Thumb(idx));
                    }
                }
            }
            return None;
        }

        if self.doc.is_none() && self.empty_open_btn_box().contains(x, y) {
            return Some(Hit::EmptyOpenButton);
        }

        Some(Hit::Canvas)
    }

    pub fn cursor_at(&self, x: f32, y: f32) -> Cursor {
        if self.pan_drag.is_some() {
            return Cursor::Grabbing;
        }
        let edges = self.resize_edges(x, y);
        if edges != 0 {
            return Cursor::Resize(edges);
        }
        if matches!(self.hit_test(x, y), Some(Hit::Canvas))
            && self.doc.is_some()
            && matches!(self.zoom, ZoomMode::Manual(_))
        {
            return Cursor::Grab;
        }
        Cursor::Default
    }

    pub fn motion(&mut self, x: f32, y: f32) {
        if let Some((sx, sy, ox, oy)) = self.pan_drag {
            self.pan_x = ox + (x - sx);
            self.pan_y = oy + (y - sy);
            self.clamp_pan();
            self.dirty = true;
            return;
        }
        let th = (8.0..=68.0).contains(&x) && (12.0..=36.0).contains(&y);
        let h = self.hit_test(x, y);
        if th != self.traffic_hover || h != self.hover {
            self.traffic_hover = th;
            self.hover = h;
            self.dirty = true;
        }
    }

    pub fn leave(&mut self) {
        if self.hover.is_some() || self.traffic_hover || self.pan_drag.is_some() {
            self.hover = None;
            self.traffic_hover = false;
            self.pan_drag = None;
            self.dirty = true;
        }
    }

    pub fn press(&mut self, button: u32, x: f32, y: f32, _mods: Mods) -> Vec<Effect> {
        if button != BTN_LEFT {
            return Vec::new();
        }
        let edges = self.resize_edges(x, y);
        if edges != 0 {
            return vec![Effect::Resize(edges)];
        }
        let Some(hit) = self.hit_test(x, y) else {
            return Vec::new();
        };

        let now = std::time::Instant::now();
        let double_click = self
            .last_click
            .is_some_and(|(t, prev)| prev == hit && now.duration_since(t).as_millis() < 360);
        self.last_click = Some((now, hit));

        match hit {
            Hit::Traffic(0) => vec![Effect::Close],
            Hit::Traffic(1) => vec![Effect::Minimize],
            Hit::Traffic(2) => vec![Effect::ToggleMaximize],
            Hit::Traffic(_) => Vec::new(),
            Hit::SidebarToggle => {
                if self.doc.is_some() {
                    self.sidebar_open = !self.sidebar_open;
                    self.clamp_pan();
                    self.dirty = true;
                }
                Vec::new()
            }
            Hit::PrevPage => {
                if self.page_idx > 0 {
                    self.select_page(self.page_idx - 1);
                }
                Vec::new()
            }
            Hit::NextPage => {
                let count = self.doc.as_ref().map_or(0, |d| d.pages.len());
                if self.page_idx + 1 < count {
                    self.select_page(self.page_idx + 1);
                }
                Vec::new()
            }
            Hit::ZoomOut => {
                self.step_zoom(-1);
                Vec::new()
            }
            Hit::ZoomReset => {
                self.toggle_fit_or_100();
                Vec::new()
            }
            Hit::ZoomIn => {
                self.step_zoom(1);
                Vec::new()
            }
            Hit::RotateCcw => {
                self.rotate(false);
                Vec::new()
            }
            Hit::RotateCw => {
                self.rotate(true);
                Vec::new()
            }
            Hit::InfoToggle => {
                self.info_open = !self.info_open;
                self.dirty = true;
                Vec::new()
            }
            Hit::OpenButton | Hit::EmptyOpenButton => vec![Effect::OpenDialog],
            Hit::Thumb(idx) => {
                self.select_page(idx);
                Vec::new()
            }
            Hit::ToolbarBg => {
                if double_click {
                    vec![Effect::ToggleMaximize]
                } else {
                    vec![Effect::Move]
                }
            }
            Hit::Canvas => {
                if double_click && self.doc.is_some() {
                    self.toggle_fit_or_100();
                    Vec::new()
                } else if self.doc.is_some() && matches!(self.zoom, ZoomMode::Manual(_)) {
                    self.pan_drag = Some((x, y, self.pan_x, self.pan_y));
                    self.dirty = true;
                    Vec::new()
                } else if self.doc.is_none() {
                    vec![Effect::Move]
                } else {
                    Vec::new()
                }
            }
        }
    }

    pub fn release(&mut self, button: u32) {
        if button == BTN_LEFT && self.pan_drag.take().is_some() {
            self.dirty = true;
        }
    }

    pub fn scroll(&mut self, dy: f32, x: f32, mods: Mods) {
        if self.doc.is_none() {
            return;
        }
        if mods.ctrl {
            if dy < 0.0 {
                self.step_zoom(1);
            } else if dy > 0.0 {
                self.step_zoom(-1);
            }
            return;
        }
        if self.sidebar_visible() && x < SIDEBAR_W {
            self.sidebar_scroll += dy;
            self.clamp_sidebar_scroll();
            self.dirty = true;
            return;
        }
        if matches!(self.zoom, ZoomMode::Manual(_)) {
            self.pan_y -= dy;
            self.clamp_pan();
            self.dirty = true;
        } else if let Some(doc) = &self.doc {
            if doc.pages.len() > 1 {
                if dy > 20.0 && self.page_idx + 1 < doc.pages.len() {
                    self.select_page(self.page_idx + 1);
                } else if dy < -20.0 && self.page_idx > 0 {
                    self.select_page(self.page_idx - 1);
                }
            }
        }
    }

    pub fn key(&mut self, raw: u32, utf8: Option<&str>, mods: Mods) -> Vec<Effect> {
        // Keysyms (X11/xkbcommon standard values)
        const XK_ESCAPE: u32 = 0xff1b;
        const XK_LEFT: u32 = 0xff51;
        const XK_UP: u32 = 0xff52;
        const XK_RIGHT: u32 = 0xff53;
        const XK_DOWN: u32 = 0xff54;
        const XK_PAGE_UP: u32 = 0xff55;
        const XK_PAGE_DOWN: u32 = 0xff56;
        const XK_HOME: u32 = 0xff50;
        const XK_END: u32 = 0xff57;
        const XK_BACKSPACE: u32 = 0xff08;
        const XK_SPACE: u32 = 0x0020;

        let ch = utf8.and_then(|s| s.chars().next()).map(|c| c.to_ascii_lowercase());
        if mods.ctrl {
            match ch {
                Some('o') => return vec![Effect::OpenDialog],
                Some('w') | Some('q') => return vec![Effect::Close],
                Some('+') | Some('=') => {
                    self.step_zoom(1);
                    return Vec::new();
                }
                Some('-') | Some('_') => {
                    self.step_zoom(-1);
                    return Vec::new();
                }
                Some('0') => {
                    self.zoom = ZoomMode::Fit;
                    self.clamp_pan();
                    self.dirty = true;
                    return Vec::new();
                }
                Some('1') => {
                    self.zoom = ZoomMode::Manual(1.0);
                    self.clamp_pan();
                    self.dirty = true;
                    return Vec::new();
                }
                Some('r') => {
                    self.rotate(!mods.shift);
                    return Vec::new();
                }
                Some('l') => {
                    self.rotate(false);
                    return Vec::new();
                }
                Some('i') => {
                    self.info_open = !self.info_open;
                    self.dirty = true;
                    return Vec::new();
                }
                _ => {}
            }
        }

        match raw {
            XK_ESCAPE => {
                if self.info_open {
                    self.info_open = false;
                    self.dirty = true;
                } else if matches!(self.zoom, ZoomMode::Manual(_)) {
                    self.zoom = ZoomMode::Fit;
                    self.clamp_pan();
                    self.dirty = true;
                }
                return Vec::new();
            }
            XK_LEFT | XK_UP | XK_PAGE_UP | XK_BACKSPACE => {
                if self.page_idx > 0 {
                    self.select_page(self.page_idx - 1);
                }
                return Vec::new();
            }
            XK_RIGHT | XK_DOWN | XK_PAGE_DOWN | XK_SPACE => {
                let count = self.doc.as_ref().map_or(0, |d| d.pages.len());
                if self.page_idx + 1 < count {
                    self.select_page(self.page_idx + 1);
                }
                return Vec::new();
            }
            XK_HOME => {
                self.select_page(0);
                return Vec::new();
            }
            XK_END => {
                let count = self.doc.as_ref().map_or(0, |d| d.pages.len());
                if count > 0 {
                    self.select_page(count - 1);
                }
                return Vec::new();
            }
            _ => {}
        }

        match ch {
            Some('+') | Some('=') => self.step_zoom(1),
            Some('-') => self.step_zoom(-1),
            Some('0') => {
                self.zoom = ZoomMode::Fit;
                self.clamp_pan();
                self.dirty = true;
            }
            Some('1') => {
                self.zoom = ZoomMode::Manual(1.0);
                self.clamp_pan();
                self.dirty = true;
            }
            Some('[') => self.rotate(false),
            Some(']') => self.rotate(true),
            Some('i') => {
                self.info_open = !self.info_open;
                self.dirty = true;
            }
            Some('s') => {
                if self.doc.is_some() {
                    self.sidebar_open = !self.sidebar_open;
                    self.clamp_pan();
                    self.dirty = true;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    // --- Painting ---

    pub fn paint(&mut self, pm: &mut PixmapMut) {
        self.dirty = false;
        pm.fill(Color::TRANSPARENT);
        let radius = if self.maximized { 0.0 } else { CORNER_R };
        let dark = feather_ui::scale::appearance() == feather_ui::scale::Appearance::Dark;

        // Window background
        fill_rrect(pm, 0.0, 0.0, self.w, self.h, radius, colors::window());

        // Main canvas area (slightly recessed stage behind document)
        let canvas_bg = if dark {
            rgba(22, 22, 25, 255)
        } else {
            rgba(232, 232, 237, 255)
        };
        let c = self.canvas_rect();
        fill_rect(pm, c.x, c.y, c.w, c.h, canvas_bg);

        if self.doc.is_some() {
            self.paint_document(pm, c);
        } else {
            self.paint_empty_state(pm, c);
        }

        if self.sidebar_visible() {
            self.paint_sidebar(pm);
        }

        if self.info_open && self.doc.is_some() {
            self.paint_info_popover(pm);
        }

        self.paint_toolbar(pm);

        if self.drop_hover {
            stroke_rrect(pm, 3.0, 3.0, self.w - 6.0, self.h - 6.0, (radius - 2.0).max(4.0), 2.5, colors::accent());
        }

        // Window outer border & rounded corner mask
        if !self.maximized {
            stroke_rrect(pm, 0.5, 0.5, self.w - 1.0, self.h - 1.0, radius, 1.0, colors::sidebar_edge());
            mask_rounded_corners(pm, self.w, self.h, radius);
        }
    }

    fn paint_toolbar(&mut self, pm: &mut PixmapMut) {
        fill_rect(pm, 0.0, 0.0, self.w, TOOLBAR_H, colors::glass());
        line(pm, 0.0, TOOLBAR_H - 0.5, self.w, TOOLBAR_H - 0.5, 1.0, colors::separator());

        // Traffic lights
        let lights = [
            (18.0f32, rgba(255, 95, 86, 255)),
            (38.0, rgba(255, 189, 46, 255)),
            (58.0, rgba(39, 201, 63, 255)),
        ];
        for (cx, col) in lights {
            let c = if self.focused || self.traffic_hover { col } else { colors::traffic_inactive() };
            fill_circle(pm, cx, 24.0, 6.0, c);
        }
        if self.traffic_hover {
            let g = rgba(60, 20, 15, 200);
            line(pm, 15.2, 21.2, 20.8, 26.8, 1.3, g);
            line(pm, 20.8, 21.2, 15.2, 26.8, 1.3, g);
            line(pm, 35.0, 24.0, 41.0, 24.0, 1.4, g);
            line(pm, 55.0, 24.0, 61.0, 24.0, 1.4, g);
            line(pm, 58.0, 21.0, 58.0, 27.0, 1.4, g);
        }

        let text_col = colors::text();
        let accent_col = colors::accent();
        let faint_col = colors::tertiary();

        let has_doc = self.doc.is_some();
        if has_doc {
            // Sidebar toggle button
            let sb = self.sidebar_toggle_box();
            self.paint_tool_btn_bg(pm, sb, self.hover == Some(Hit::SidebarToggle), self.sidebar_open);
            let ic = if self.sidebar_open { accent_col } else { text_col };
            stroke_rrect(pm, sb.x + 6.0, sb.y + 7.0, 16.0, 14.0, 2.5, 1.4, ic);
            line(pm, sb.x + 11.5, sb.y + 7.5, sb.x + 11.5, sb.y + 20.5, 1.3, ic);

            // Prev / Next page buttons (when multi-page)
            let page_count = self.doc.as_ref().map_or(0, |d| d.pages.len());
            if page_count > 1 {
                let pb = self.prev_page_box();
                let can_prev = self.page_idx > 0;
                self.paint_tool_btn_bg(pm, pb, can_prev && self.hover == Some(Hit::PrevPage), false);
                let pc = if can_prev { text_col } else { faint_col };
                polyline(
                    pm,
                    &[(pb.x + 16.0, pb.y + 9.0), (pb.x + 11.0, pb.y + 14.0), (pb.x + 16.0, pb.y + 19.0)],
                    1.6,
                    pc,
                );

                let nb = self.next_page_box();
                let can_next = self.page_idx + 1 < page_count;
                self.paint_tool_btn_bg(pm, nb, can_next && self.hover == Some(Hit::NextPage), false);
                let nc = if can_next { text_col } else { faint_col };
                polyline(
                    pm,
                    &[(nb.x + 12.0, nb.y + 9.0), (nb.x + 17.0, nb.y + 14.0), (nb.x + 12.0, nb.y + 19.0)],
                    1.6,
                    nc,
                );
            }

            // Zoom cluster: [-][По окну / 100%][+]
            let zo = self.zoom_out_box();
            self.paint_tool_btn_bg(pm, zo, self.hover == Some(Hit::ZoomOut), false);
            line(pm, zo.x + 9.0, zo.y + 14.0, zo.x + 19.0, zo.y + 14.0, 1.6, text_col);

            let zr = self.zoom_reset_box();
            self.paint_tool_btn_bg(pm, zr, self.hover == Some(Hit::ZoomReset), false);
            let zoom_label: std::borrow::Cow<'_, str> = match self.zoom {
                ZoomMode::Fit => feather_ui::i18n::tr("По окну"),
                ZoomMode::Manual(z) => format!("{:.0}%", z * 100.0).into(),
            };
            self.font.draw_text_centered(
                pm,
                &zoom_label,
                zr.x + zr.w * 0.5,
                zr.y + 7.5,
                11.5,
                text_col,
                true,
            );

            let zi = self.zoom_in_box();
            self.paint_tool_btn_bg(pm, zi, self.hover == Some(Hit::ZoomIn), false);
            line(pm, zi.x + 9.0, zi.y + 14.0, zi.x + 19.0, zi.y + 14.0, 1.6, text_col);
            line(pm, zi.x + 14.0, zi.y + 9.0, zi.x + 14.0, zi.y + 19.0, 1.6, text_col);

            // Rotate CCW & CW
            let rccw = self.rotate_ccw_box();
            self.paint_tool_btn_bg(pm, rccw, self.hover == Some(Hit::RotateCcw), false);
            draw_rotate_icon(pm, rccw.x + 14.0, rccw.y + 14.0, false, text_col);

            let rcw = self.rotate_cw_box();
            self.paint_tool_btn_bg(pm, rcw, self.hover == Some(Hit::RotateCw), false);
            draw_rotate_icon(pm, rcw.x + 14.0, rcw.y + 14.0, true, text_col);

            // Info button (ⓘ)
            let ib = self.info_btn_box();
            self.paint_tool_btn_bg(pm, ib, self.hover == Some(Hit::InfoToggle), self.info_open);
            let ic = if self.info_open { accent_col } else { text_col };
            stroke_circle(pm, ib.x + 14.0, ib.y + 14.0, 7.2, 1.4, ic);
            fill_circle(pm, ib.x + 14.0, ib.y + 10.7, 1.1, ic);
            line(pm, ib.x + 14.0, ib.y + 13.5, ib.x + 14.0, ib.y + 17.8, 1.5, ic);
        }

        // «Открыть…» button on the far right
        let ob = self.open_btn_box();
        self.paint_tool_btn_bg(pm, ob, self.hover == Some(Hit::OpenButton), false);
        self.font.draw_text_centered(
            pm,
            "Открыть…",
            ob.x + ob.w * 0.5,
            ob.y + 7.0,
            12.0,
            text_col,
            true,
        );

        // Centered title & subtitle
        if let Some(doc) = &self.doc {
            let left_limit = if doc.pages.len() > 1 { 184.0 } else { 122.0 };
            let right_limit = self.zoom_out_box().x - 12.0;
            let avail = (right_limit - left_limit).max(80.0);
            let cx = (left_limit + right_limit) * 0.5;
            let title = self.font.ellipsize(&doc.title, avail, 13.0, true);
            self.font.draw_text_centered(pm, &title, cx, 7.5, 13.0, text_col, true);

            if let Some(page) = doc.pages.get(self.page_idx) {
                let sub = if doc.pages.len() > 1 {
                    format!(
                        "{} · {} {} {} {} · {} × {}",
                        doc.format.label(),
                        feather_ui::i18n::tr("Стр."),
                        self.page_idx + 1,
                        feather_ui::i18n::tr("из"),
                        doc.pages.len(),
                        page.orig_w,
                        page.orig_h
                    )
                } else {
                    format!(
                        "{} · {} × {} · {}",
                        doc.format.label(),
                        page.orig_w,
                        page.orig_h,
                        doc::human_size(doc.file_size)
                    )
                };
                let sub_el = self.font.ellipsize(&sub, avail, 10.5, false);
                self.font.draw_text_centered(pm, &sub_el, cx, 25.0, 10.5, colors::secondary(), false);
            }
        } else {
            self.font.draw_text_centered(pm, "Просмотр", self.w * 0.5, 16.0, 13.5, text_col, true);
        }
    }

    fn paint_tool_btn_bg(&self, pm: &mut PixmapMut, b: RectBox, hovered: bool, active: bool) {
        let bg = if active {
            colors::selected_gray()
        } else if hovered {
            colors::hover()
        } else {
            colors::control()
        };
        fill_rrect(pm, b.x, b.y, b.w, b.h, 6.5, bg);
        stroke_rrect(
            pm,
            b.x + 0.5,
            b.y + 0.5,
            b.w - 1.0,
            b.h - 1.0,
            6.5,
            1.0,
            if active { colors::accent() } else { colors::separator() },
        );
    }

    fn paint_sidebar(&mut self, pm: &mut PixmapMut) {
        fill_rect(pm, 0.0, TOOLBAR_H, SIDEBAR_W, self.h - TOOLBAR_H, colors::sidebar());
        line(pm, SIDEBAR_W - 0.5, TOOLBAR_H, SIDEBAR_W - 0.5, self.h, 1.0, colors::sidebar_edge());

        let Some(doc) = &self.doc else { return };
        let s = ui_scale();
        let accent = colors::accent();
        for (i, page) in doc.pages.iter().enumerate() {
            let item_y = TOOLBAR_H + 14.0 + i as f32 * 180.0 - self.sidebar_scroll;
            if item_y + 176.0 < TOOLBAR_H || item_y > self.h {
                continue;
            }
            let selected = i == self.page_idx;
            let hovered = self.hover == Some(Hit::Thumb(i));

            let tw = page.thumb.width() as f32;
            let th = page.thumb.height() as f32;
            let tx = (SIDEBAR_W - tw) * 0.5;
            let ty = item_y + (148.0 - th) * 0.5;

            if selected {
                fill_rrect(pm, tx - 5.0, ty - 5.0, tw + 10.0, th + 10.0, 6.0, colors::selected_gray());
                stroke_rrect(pm, tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0, 5.5, 2.0, accent);
            } else if hovered {
                fill_rrect(pm, tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0, 5.0, colors::hover());
            }

            // Paper shadow & white backdrop
            fill_rrect(pm, tx + 1.0, ty + 2.0, tw, th, 2.0, rgba(0, 0, 0, 42));
            fill_rect(pm, tx, ty, tw, th, rgba(255, 255, 255, 255));

            let paint = PixmapPaint {
                quality: FilterQuality::Bilinear,
                ..Default::default()
            };
            pm.draw_pixmap(
                0,
                0,
                page.thumb.as_ref(),
                &paint,
                Transform::from_translate(tx * s, ty * s).pre_scale(s, s),
                None,
            );
            stroke_rrect(pm, tx, ty, tw, th, 1.5, 1.0, colors::separator());

            // Page number pill below thumbnail
            let num = (i + 1).to_string();
            let pill_y = ty + th + 7.0;
            if selected {
                let nw = (self.font.measure_raw(&num, 11.0, true) + 14.0).max(24.0);
                fill_rrect(pm, (SIDEBAR_W - nw) * 0.5, pill_y - 1.0, nw, 17.0, 8.0, accent);
                self.font.draw_text_centered(
                    pm,
                    &num,
                    SIDEBAR_W * 0.5,
                    pill_y + 1.5,
                    11.0,
                    rgba(255, 255, 255, 255),
                    true,
                );
            } else {
                self.font.draw_text_centered(
                    pm,
                    &num,
                    SIDEBAR_W * 0.5,
                    pill_y + 1.5,
                    11.0,
                    colors::secondary(),
                    false,
                );
            }
        }
    }

    fn paint_document(&mut self, pm: &mut PixmapMut, c: RectBox) {
        let Some(doc) = &self.doc else { return };
        let Some(page) = doc.pages.get(self.page_idx) else { return };

        let z = self.effective_zoom();
        let dw = page.pixmap.width() as f32 * z;
        let dh = page.pixmap.height() as f32 * z;
        let dx = (c.x + (c.w - dw) * 0.5 + self.pan_x).round();
        let dy = (c.y + (c.h - dh) * 0.5 + self.pan_y).round();

        // Soft paper/frame shadow
        fill_rrect(pm, dx - 2.0, dy + 4.0, dw + 4.0, dh + 6.0, 4.0, rgba(0, 0, 0, 52));
        fill_rrect(pm, dx - 1.0, dy + 1.5, dw + 2.0, dh + 2.0, 3.0, rgba(0, 0, 0, 30));

        if doc.format.is_pdf() || !page.has_alpha {
            fill_rect(pm, dx, dy, dw, dh, rgba(255, 255, 255, 255));
        } else {
            paint_checkerboard(pm, dx, dy, dw, dh, c);
        }

        let s = ui_scale();
        let paint = PixmapPaint {
            quality: if z < 2.0 { FilterQuality::Bilinear } else { FilterQuality::Nearest },
            ..Default::default()
        };
        pm.draw_pixmap(
            0,
            0,
            page.pixmap.as_ref(),
            &paint,
            Transform::from_translate(dx * s, dy * s).pre_scale(z * s, z * s),
            None,
        );

        stroke_rrect(pm, dx, dy, dw, dh, 1.5, 1.0, colors::separator());
    }

    fn paint_empty_state(&mut self, pm: &mut PixmapMut, c: RectBox) {
        let cx = c.x + c.w * 0.5;
        let cy = c.y + c.h * 0.5 - 24.0;
        let accent = colors::accent();

        // macOS Tahoe Preview artwork card: landscape picture + magnifier loupe
        let cw = 96.0;
        let ch = 76.0;
        let card_x = cx - cw * 0.5;
        let card_y = cy - 68.0;
        fill_rrect(pm, card_x + 2.0, card_y + 4.0, cw, ch, 14.0, rgba(0, 0, 0, 36));
        fill_rrect_gradient(
            pm,
            card_x,
            card_y,
            cw,
            ch,
            14.0,
            rgba(72, 162, 255, 255),
            rgba(26, 98, 224, 255),
        );
        stroke_rrect(pm, card_x + 3.0, card_y + 3.0, cw - 6.0, ch - 6.0, 11.0, 2.0, rgba(255, 255, 255, 215));
        // Sun & mountains inside the card
        fill_circle(pm, card_x + 28.0, card_y + 24.0, 7.5, rgba(255, 230, 128, 240));
        polyline(
            pm,
            &[
                (card_x + 12.0, card_y + 58.0),
                (card_x + 36.0, card_y + 36.0),
                (card_x + 54.0, card_y + 50.0),
                (card_x + 70.0, card_y + 32.0),
                (card_x + 84.0, card_y + 58.0),
            ],
            2.4,
            rgba(255, 255, 255, 230),
        );
        // Magnifier loupe on bottom-right corner of card
        let lx = card_x + cw - 10.0;
        let ly = card_y + ch - 10.0;
        fill_circle(pm, lx, ly, 16.0, colors::window());
        stroke_circle(pm, lx, ly, 15.0, 2.6, accent);
        line(pm, lx + 10.5, ly + 10.5, lx + 19.5, ly + 19.5, 3.6, accent);

        if let Some(err) = &self.error.clone() {
            self.font.draw_text_centered(
                pm,
                "Не удалось открыть файл",
                cx,
                cy + 20.0,
                15.0,
                rgba(255, 69, 58, 255),
                true,
            );
            let msg = self.font.ellipsize(err, (c.w - 64.0).max(120.0), 12.0, false);
            self.font.draw_text_centered(pm, &msg, cx, cy + 44.0, 12.0, colors::secondary(), false);
        } else if self.loading {
            self.font.draw_text_centered(pm, "Загрузка документа…", cx, cy + 28.0, 14.0, colors::text(), true);
        } else {
            self.font.draw_text_centered(
                pm,
                "Изображения PNG, JPEG, WebP, SVG и документы PDF",
                cx,
                cy + 20.0,
                14.0,
                colors::text(),
                true,
            );
            self.font.draw_text_centered(
                pm,
                "Перетащите файл сюда или нажмите «Открыть…»",
                cx,
                cy + 44.0,
                12.0,
                colors::secondary(),
                false,
            );
        }

        let btn = self.empty_open_btn_box();
        let bg = if self.hover == Some(Hit::EmptyOpenButton) {
            rgba(18, 132, 255, 255)
        } else {
            accent
        };
        fill_rrect(pm, btn.x, btn.y, btn.w, btn.h, 8.5, bg);
        self.font.draw_text_centered(
            pm,
            "Открыть файл…",
            btn.x + btn.w * 0.5,
            btn.y + 10.0,
            13.0,
            rgba(255, 255, 255, 255),
            true,
        );
    }

    fn paint_info_popover(&mut self, pm: &mut PixmapMut) {
        let Some(doc) = &self.doc else { return };
        let Some(page) = doc.pages.get(self.page_idx) else { return };

        let pw = 276.0;
        let ph = 178.0;
        let px = (self.w - pw - 16.0).max(16.0);
        let py = TOOLBAR_H + 12.0;

        fill_rrect(pm, px + 1.0, py + 4.0, pw, ph, 10.0, rgba(0, 0, 0, 48));
        fill_rrect(pm, px, py, pw, ph, 10.0, colors::glass());
        stroke_rrect(pm, px + 0.5, py + 0.5, pw - 1.0, ph - 1.0, 10.0, 1.0, colors::sidebar_edge());

        let text_col = colors::text();
        let muted_col = colors::secondary();
        self.font.draw_text(pm, "Сведения о документе", px + 14.0, py + 12.0, 12.5, text_col, true);
        line(pm, px + 12.0, py + 34.0, px + pw - 12.0, py + 34.0, 1.0, colors::separator());

        let rows = [
            ("Файл", doc.title.clone()),
            ("Формат", doc.format.label().to_string()),
            ("Размер", doc::human_size(doc.file_size)),
            ("Разрешение", format!("{} × {} пикс.", page.orig_w, page.orig_h)),
            ("Страниц", doc.pages.len().to_string()),
            ("Масштаб", format!("{:.0}%", self.effective_zoom() * 100.0)),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let ry = py + 42.0 + i as f32 * 21.5;
            self.font.draw_text(pm, k, px + 14.0, ry, 11.5, muted_col, false);
            let val = self.font.ellipsize(v, pw - 118.0, 11.5, true);
            self.font.draw_text_right(pm, &val, px + pw - 14.0, ry, 11.5, text_col, true);
        }
    }
}

fn is_supported_ext(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "svg" | "pdf"
    )
}

fn draw_rotate_icon(pm: &mut PixmapMut, cx: f32, cy: f32, clockwise: bool, color: Color) {
    stroke_rrect(pm, cx - 4.5, cy - 2.5, 9.0, 8.0, 1.5, 1.3, color);
    if clockwise {
        polyline(
            pm,
            &[(cx - 3.0, cy - 5.5), (cx + 2.5, cy - 5.5), (cx + 0.5, cy - 7.5)],
            1.4,
            color,
        );
    } else {
        polyline(
            pm,
            &[(cx + 3.0, cy - 5.5), (cx - 2.5, cy - 5.5), (cx - 0.5, cy - 7.5)],
            1.4,
            color,
        );
    }
}

fn paint_checkerboard(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, clip: RectBox) {
    let x0 = x.max(clip.x);
    let y0 = y.max(clip.y);
    let x1 = (x + w).min(clip.x + clip.w);
    let y1 = (y + h).min(clip.y + clip.h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    fill_rect(pm, x0, y0, x1 - x0, y1 - y0, rgba(250, 250, 252, 255));
    let cell = 12.0;
    let dark = rgba(224, 224, 230, 255);
    let cols = ((x1 - x0) / cell).ceil() as i32;
    let rows = ((y1 - y0) / cell).ceil() as i32;
    for r in 0..rows.min(180) {
        for c in 0..cols.min(240) {
            if (r + c) % 2 == 1 {
                let cx = x0 + c as f32 * cell;
                let cy = y0 + r as f32 * cell;
                let cw = (x1 - cx).min(cell);
                let ch = (y1 - cy).min(cell);
                fill_rect(pm, cx, cy, cw, ch, dark);
            }
        }
    }
}

fn mask_rounded_corners(pm: &mut PixmapMut, w: f32, h: f32, r: f32) {
    let Some(path) = rounded_rect_path(0.0, 0.0, w, h, r) else { return };
    let (pw, ph) = (pm.width(), pm.height());
    let Some(mut mask) = tiny_skia::Mask::new(pw, ph) else { return };
    let s = ui_scale();
    mask.fill_path(&path, tiny_skia::FillRule::Winding, true, Transform::from_scale(s, s));
    let m = mask.data();
    let d = pm.data_mut();
    let band = ((r * s).ceil() as u32 + 2).min(ph / 2);
    for y in (0..band).chain(ph.saturating_sub(band)..ph) {
        for x in 0..pw {
            let i = (y * pw + x) as usize;
            let a = m[i] as u32;
            if a < 255 {
                let p = i * 4;
                d[p] = (d[p] as u32 * a / 255) as u8;
                d[p + 1] = (d[p + 1] as u32 * a / 255) as u8;
                d[p + 2] = (d[p + 2] as u32 * a / 255) as u8;
                d[p + 3] = (d[p + 3] as u32 * a / 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::set_ui_scale;
    use feather_ui::scale::{set_appearance, Appearance};

    #[test]
    fn preview_ui_renders_empty_and_multipage_pdf_and_handles_controls() {
        let font = FontSystem::new();
        let mut app = App::new(font);

        // 1. Render empty state in Light & Dark themes
        for (appearance, scale) in [(Appearance::Light, 1.0f32), (Appearance::Dark, 1.25), (Appearance::Light, 2.0)] {
            set_appearance(appearance);
            set_ui_scale(scale);
            let pw = (app.w * scale).round() as u32;
            let ph = (app.h * scale).round() as u32;
            let mut pm = tiny_skia::Pixmap::new(pw, ph).unwrap();
            app.paint(&mut pm.as_mut());
            assert!(pm.data().iter().any(|&b| b != 0));
        }

        // 2. Load a multi-page PDF document
        let dir = std::env::temp_dir().join(format!("feather-preview-ui-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pdf_path = dir.join("guide.pdf");
        let pdf_bytes = doc::sample_pdf_bytes(&[
            ("Feather Preview", "Page 1: Images and PDF documents"),
            ("macOS Tahoe Style", "Page 2: Thumbnail sidebar, zoom, rotate, inspector"),
        ]);
        std::fs::write(&pdf_path, pdf_bytes).unwrap();
        let loaded = doc::load(&pdf_path).expect("sample pdf load");
        app.set_document(Ok(loaded));
        assert!(app.sidebar_open, "multi-page PDF automatically opens thumbnail sidebar");
        assert_eq!(app.page_idx, 0);

        // 3. Click Next page button -> page 1
        let nb = app.next_page_box();
        app.press(BTN_LEFT, nb.x + 14.0, nb.y + 14.0, Mods::default());
        assert_eq!(app.page_idx, 1);

        // 4. Click Zoom In -> manual zoom > fit_scale
        let zi = app.zoom_in_box();
        app.press(BTN_LEFT, zi.x + 14.0, zi.y + 14.0, Mods::default());
        assert!(matches!(app.zoom, ZoomMode::Manual(_)));

        // 5. Click Rotate CW -> swaps page dimensions
        let orig_dims = {
            let p = &app.doc.as_ref().unwrap().pages[1];
            (p.orig_w, p.orig_h)
        };
        let rcw = app.rotate_cw_box();
        app.press(BTN_LEFT, rcw.x + 14.0, rcw.y + 14.0, Mods::default());
        let new_dims = {
            let p = &app.doc.as_ref().unwrap().pages[1];
            (p.orig_w, p.orig_h)
        };
        assert_eq!(new_dims, (orig_dims.1, orig_dims.0));

        // 6. Toggle Info popover and render
        let ib = app.info_btn_box();
        app.press(BTN_LEFT, ib.x + 14.0, ib.y + 14.0, Mods::default());
        assert!(app.info_open);

        set_ui_scale(1.0);
        let mut pm = tiny_skia::Pixmap::new(app.w as u32, app.h as u32).unwrap();
        app.paint(&mut pm.as_mut());

        // 7. Click Open button -> emits Effect::OpenDialog
        let ob = app.open_btn_box();
        let effects = app.press(BTN_LEFT, ob.x + 20.0, ob.y + 14.0, Mods::default());
        assert_eq!(effects, vec![Effect::OpenDialog]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
