use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{memory::MemoryRenderBuffer, memory::MemoryRenderBufferRenderElement, Kind},
            utils::with_renderer_surface_state,
            ImportMem, Renderer,
        },
    },
    desktop::utils::bbox_from_surface_tree,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, Transform},
    wayland::{
        compositor::with_states,
        shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData},
    },
};
use tiny_skia::PixmapMut;

use crate::apps::BuiltinWindowKind;
use crate::text::FontSystem;
use crate::theme::{
    appearance, draw_feather_logo, fill_circle, fill_rounded_rect, fill_squircle_gradient, palette,
    render_shadow_tile, rgba, stroke_circle, stroke_line, stroke_rounded_rect, to_phys, ui_scale,
    Appearance, SHADOW_PAD, TITLEBAR_HEIGHT, TOP_BAR_HEIGHT, TRAFFIC_CENTERS,
    TRAFFIC_RADIUS, WINDOW_CORNER_RADIUS,
};

/// Width of the invisible grab band around decorated windows used for edge resizing
pub const RESIZE_BAND: i32 = 6;
pub const MIN_WINDOW_W: i32 = 280;
pub const MIN_WINDOW_H: i32 = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficButton {
    Close,
    Minimize,
    Maximize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Edges {
    pub const BOTTOM_RIGHT: Edges = Edges {
        left: false,
        right: true,
        top: false,
        bottom: true,
    };

    pub fn from_xdg(edge: xdg_toplevel::ResizeEdge) -> Self {
        use xdg_toplevel::ResizeEdge as E;
        Self {
            left: matches!(edge, E::Left | E::TopLeft | E::BottomLeft),
            right: matches!(edge, E::Right | E::TopRight | E::BottomRight),
            top: matches!(edge, E::Top | E::TopLeft | E::TopRight),
            bottom: matches!(edge, E::Bottom | E::BottomLeft | E::BottomRight),
        }
    }

    pub fn any(&self) -> bool {
        self.left || self.right || self.top || self.bottom
    }
}

/// Keeps the opposite edge of a window in place while it is resized from the left/top
#[derive(Debug, Clone, Copy)]
pub struct ResizeAnchor {
    pub edges: Edges,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone)]
pub enum WindowBackend {
    Wayland(ToplevelSurface),
    Builtin(BuiltinWindowKind),
}

/// 9-slice drop shadow: the shadow image is rasterized once per focus state / scale
/// and then stretched on the GPU, so resizing a window never re-rasterizes it on the CPU.
pub struct ShadowSet {
    pieces: Vec<MemoryRenderBuffer>,
    built_for: Option<(bool, u32, bool)>,
    corner_phys: i32,
}

impl ShadowSet {
    fn new() -> Self {
        Self {
            pieces: Vec::new(),
            built_for: None,
            corner_phys: 0,
        }
    }

    fn ensure(&mut self, focused: bool) {
        let s = ui_scale();
        let key = (focused, s.to_bits(), appearance() == Appearance::Dark);
        if self.built_for == Some(key) {
            return;
        }
        self.built_for = Some(key);

        let corner_logical = SHADOW_PAD as f32 + WINDOW_CORNER_RADIUS + 6.0;
        let kp = (corner_logical * s).ceil() as i32;
        let np = (2 * kp + 1) as u32;
        let Some(pixmap) = render_shadow_tile(np, focused) else {
            return;
        };
        let data = pixmap.data();
        let stride = np as usize * 4;
        let crop = |x: i32, y: i32, w: i32, h: i32| -> MemoryRenderBuffer {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for row in y..y + h {
                let start = row as usize * stride + x as usize * 4;
                out.extend_from_slice(&data[start..start + w as usize * 4]);
            }
            MemoryRenderBuffer::from_slice(
                &out,
                Fourcc::Abgr8888,
                (w, h),
                1,
                Transform::Normal,
                None,
            )
        };
        // Order: TL, TR, BL, BR, Top, Bottom, Left, Right
        self.pieces = vec![
            crop(0, 0, kp, kp),
            crop(kp + 1, 0, kp, kp),
            crop(0, kp + 1, kp, kp),
            crop(kp + 1, kp + 1, kp, kp),
            crop(kp, 0, 1, kp),
            crop(kp, kp + 1, 1, kp),
            crop(0, kp, kp, 1),
            crop(kp + 1, kp, kp, 1),
        ];
        self.corner_phys = kp;
    }

    /// Builds shadow elements around a frame given in logical coordinates.
    pub fn elements<R>(
        &self,
        renderer: &mut R,
        frame: Rectangle<i32, Logical>,
        scale: f64,
        alpha: f32,
    ) -> Vec<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        if self.pieces.len() != 8 {
            return Vec::new();
        }
        let pad = SHADOW_PAD as f64 * scale;
        let rx = (frame.loc.x as f64 * scale - pad).round() as i32;
        let ry = (frame.loc.y as f64 * scale - pad).round() as i32;
        let rw = (frame.size.w as f64 * scale + pad * 2.0).round() as i32;
        let rh = (frame.size.h as f64 * scale + pad * 2.0).round() as i32;
        let k = self.corner_phys;
        if rw <= 2 * k || rh <= 2 * k {
            return Vec::new();
        }
        let mid_w = rw - 2 * k;
        let mid_h = rh - 2 * k;
        let placements = [
            (rx, ry, k, k),
            (rx + rw - k, ry, k, k),
            (rx, ry + rh - k, k, k),
            (rx + rw - k, ry + rh - k, k, k),
            (rx + k, ry, mid_w, k),
            (rx + k, ry + rh - k, mid_w, k),
            (rx, ry + k, k, mid_h),
            (rx + rw - k, ry + k, k, mid_h),
        ];
        let mut out = Vec::with_capacity(8);
        for (index, (buf, (x, y, w, h))) in self.pieces.iter().zip(placements).enumerate() {
            if let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (x as f64, y as f64),
                buf,
                Some(alpha),
                Some(Rectangle::from_size(match index {
                    0..=3 => (k as f64, k as f64).into(),
                    4..=5 => (1.0, k as f64).into(),
                    _ => (k as f64, 1.0).into(),
                })),
                Some((w, h).into()),
                Kind::Unspecified,
            ) {
                out.push(elem);
            }
        }
        out
    }
}

pub struct ManagedWindow {
    pub settings_ticket: Option<std::sync::Arc<()>>,
    pub id: u64,
    /// Desktop (Space) the window lives on
    pub space: u32,
    /// On another desktop than the current one: hidden, no input, no focus
    pub off_space: bool,
    pub title: String,
    pub app_id: String,
    /// Logical top-left of the visible frame (titlebar top for SSD, content top for CSD)
    pub x: i32,
    pub y: i32,
    /// Logical content size (xdg window geometry for Wayland clients)
    pub width: i32,
    pub height: i32,
    /// Offset of the xdg window geometry inside the client's surface (CSD shadows etc.)
    pub geo_loc: Point<i32, Logical>,
    /// Server-side decorations: we draw the titlebar with traffic lights
    pub ssd: bool,
    /// Client has attached a buffer; unmapped windows are invisible and never take input
    pub mapped: bool,
    /// Client draws its own shadow outside of its window geometry
    pub client_shadow: bool,
    pub activated: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub saved_rect: Option<(i32, i32, i32, i32)>,
    pub resize_anchor: Option<ResizeAnchor>,
    pub hovered_traffic: Option<TrafficButton>,
    pub traffic_group_hovered: bool,
    pub backend: WindowBackend,
    pub decoration_buffer: MemoryRenderBuffer,
    pub phys_buf_w: i32,
    pub phys_buf_h: i32,
    pub shadow: ShadowSet,
    pub dirty: bool,
}

pub struct BuiltinContext<'a> {
    pub settings: &'a crate::system_settings::Panel,
    pub settings_context: crate::system_settings::Context<'a>,
    pub notes_text: &'a str,
    pub rss_mb: f32,
    pub window_count: usize,
    pub wayland_socket: &'a str,
    pub active_layout: &'a str,
    pub input_shortcut: &'static str,
    pub scale_factor: f32,
    pub show_about: bool,
}

/// What changed in a Wayland window after a client commit
#[derive(Default)]
pub struct CommitChanges {
    pub newly_mapped: bool,
    pub unmapped: bool,
    pub title_changed: bool,
}

fn empty_buffer() -> MemoryRenderBuffer {
    MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None)
}

impl ManagedWindow {
    pub fn settings_needs_shadow(&self) -> bool {
        self.shadow.built_for
            != Some((
                self.activated,
                ui_scale().to_bits(),
                appearance() == Appearance::Dark,
            ))
    }
    pub fn decoration_size(&self) -> (i32, i32) {
        (self.phys_buf_w, self.phys_buf_h)
    }
    /// Called on the raster worker's own built-in window. The returned buffers
    /// have never been imported by a renderer; no EGL objects leave the event loop.
    pub fn raster_settings(
        &mut self,
        fonts: &mut FontSystem,
        job: &crate::system_settings::FrameJob,
    ) -> SettingsFrame {
        self.width = job.width;
        self.height = job.height;
        self.activated = job.focused;
        self.traffic_group_hovered = job.group_hovered;
        self.hovered_traffic = job.traffic;
        self.dirty = true;
        self.shadow = ShadowSet::new();
        if !job.shadow {
            self.shadow.built_for = Some((
                job.focused,
                ui_scale().to_bits(),
                appearance() == Appearance::Dark,
            ));
        }
        let context = job.context();
        let ctx = BuiltinContext {
            settings: &job.panel,
            settings_context: context,
            notes_text: "",
            rss_mb: 0.0,
            window_count: 0,
            wayland_socket: "",
            active_layout: "",
            input_shortcut: "",
            scale_factor: job.scale as f32,
            show_about: false,
        };
        self.ensure_rendered(fonts, job.focused, &ctx);
        let buffer = std::mem::replace(&mut self.decoration_buffer, empty_buffer());
        let size = (self.phys_buf_w, self.phys_buf_h);
        self.phys_buf_w = 0;
        self.phys_buf_h = 0;
        let shadow = job
            .shadow
            .then(|| std::mem::replace(&mut self.shadow, ShadowSet::new()));
        SettingsFrame {
            buffer,
            shadow,
            size,
        }
    }
    pub fn install_settings_frame(
        &mut self,
        ticket: &std::sync::Weak<()>,
        frame: SettingsFrame,
    ) -> bool {
        if self.dirty
            || !self.settings_ticket.as_ref().is_some_and(|current| {
                std::sync::Weak::ptr_eq(ticket, &std::sync::Arc::downgrade(current))
            })
        {
            return false;
        }
        if (self.phys_buf_w, self.phys_buf_h) != frame.size {
            // A size change needs a new texture anyway. Transfer the worker's
            // pixels directly instead of allocating a third full-size CPU frame.
            self.decoration_buffer = frame.buffer;
        } else {
            let mut source = frame.buffer;
            let mut target = self.decoration_buffer.render();
            let _ = target.draw(|dest| {
                let mut src = source.render();
                let _ = src.draw(|data| {
                    dest.copy_from_slice(data);
                    Ok::<_, ()>(Vec::new())
                });
                Ok::<_, ()>(vec![Rectangle::from_size(frame.size.into())])
            });
        }
        if let Some(shadow) = frame.shadow {
            self.shadow = shadow;
        }
        self.phys_buf_w = frame.size.0;
        self.phys_buf_h = frame.size.1;
        self.settings_ticket = None;
        true
    }
    pub fn new_wayland(id: u64, surface: ToplevelSurface) -> Self {
        Self {
            settings_ticket: None,
            id,
            title: "Окно Wayland".to_string(),
            app_id: String::new(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            geo_loc: (0, 0).into(),
            // Clients that never bind xdg-decoration draw their own frame (GTK, etc.)
            ssd: false,
            mapped: false,
            client_shadow: false,
            activated: false,
            minimized: false,
            space: 0,
            off_space: false,
            maximized: false,
            fullscreen: false,
            saved_rect: None,
            resize_anchor: None,
            hovered_traffic: None,
            traffic_group_hovered: false,
            backend: WindowBackend::Wayland(surface),
            decoration_buffer: empty_buffer(),
            phys_buf_w: 1,
            phys_buf_h: 1,
            shadow: ShadowSet::new(),
            dirty: true,
        }
    }

    pub fn new_builtin(id: u64, kind: BuiltinWindowKind, x: i32, y: i32) -> Self {
        let (title, app_id, width, height) = match kind {
            BuiltinWindowKind::AboutFeather => {
                ("О системе Пёрышко (Feather DE)", "builtin.about", 540, 340)
            }
            BuiltinWindowKind::ActivityMonitor => {
                ("Монитор системы — Пёрышко", "builtin.monitor", 620, 380)
            }
            BuiltinWindowKind::QuickNotes => {
                ("Заметки (Alt+Shift: RU/EN)", "builtin.notes", 540, 360)
            }
            BuiltinWindowKind::SystemSettings => {
                ("Системные настройки", "builtin.settings", 800, 540)
            }
        };

        Self {
            settings_ticket: None,
            id,
            title: title.to_string(),
            app_id: app_id.to_string(),
            x,
            y,
            width,
            height,
            geo_loc: (0, 0).into(),
            ssd: true,
            mapped: true,
            client_shadow: false,
            activated: false,
            minimized: false,
            space: 0,
            off_space: false,
            maximized: false,
            fullscreen: false,
            saved_rect: None,
            resize_anchor: None,
            hovered_traffic: None,
            traffic_group_hovered: false,
            backend: WindowBackend::Builtin(kind),
            decoration_buffer: empty_buffer(),
            phys_buf_w: 1,
            phys_buf_h: 1,
            shadow: ShadowSet::new(),
            dirty: true,
        }
    }

    pub fn toplevel(&self) -> Option<&ToplevelSurface> {
        match self.backend {
            WindowBackend::Wayland(ref t) => Some(t),
            WindowBackend::Builtin(_) => None,
        }
    }

    pub fn wl_surface(&self) -> Option<&WlSurface> {
        self.toplevel().map(|t| t.wl_surface())
    }

    pub fn is_builtin(&self) -> bool {
        matches!(self.backend, WindowBackend::Builtin(_))
    }

    /// Visible and able to receive input
    pub fn is_visible(&self) -> bool {
        self.mapped && !self.minimized && !self.off_space
    }

    pub fn has_titlebar(&self) -> bool {
        self.ssd && !self.fullscreen
    }

    pub fn titlebar_h(&self) -> i32 {
        if self.has_titlebar() {
            TITLEBAR_HEIGHT
        } else {
            0
        }
    }

    pub fn draws_shadow(&self) -> bool {
        !self.fullscreen && !self.maximized && (self.has_titlebar() || !self.client_shadow)
    }

    pub fn total_height(&self) -> i32 {
        self.titlebar_h() + self.height
    }

    pub fn frame_rect(&self) -> Rectangle<i32, Logical> {
        Rectangle::new(
            (self.x, self.y).into(),
            (self.width, self.total_height()).into(),
        )
    }

    /// Global logical position of the client's window-geometry origin
    pub fn client_origin(&self) -> (i32, i32) {
        (self.x, self.y + self.titlebar_h())
    }

    /// Global logical position of the client's root wl_surface
    pub fn surface_origin(&self) -> Point<i32, Logical> {
        let (cx, cy) = self.client_origin();
        Point::from((cx, cy)) - self.geo_loc
    }

    pub fn contains_point(&self, px: f64, py: f64) -> bool {
        if !self.is_visible() {
            return false;
        }
        px >= self.x as f64
            && px < (self.x + self.width) as f64
            && py >= self.y as f64
            && py < (self.y + self.total_height()) as f64
    }

    /// Resize edges under the pointer in the invisible band around a decorated window
    pub fn resize_edges_at(&self, px: f64, py: f64) -> Option<Edges> {
        if !self.is_visible() || !self.has_titlebar() || self.maximized {
            return None;
        }
        let b = RESIZE_BAND as f64;
        let (x0, y0) = (self.x as f64, self.y as f64);
        let (x1, y1) = (x0 + self.width as f64, y0 + self.total_height() as f64);
        if px < x0 - b || px >= x1 + b || py < y0 - b || py >= y1 + b {
            return None;
        }
        let edges = Edges {
            left: px < x0,
            right: px >= x1,
            top: py < y0,
            bottom: py >= y1,
        };
        edges.any().then_some(edges)
    }

    /// System Settings draws a unified toolbar: its traffic lights sit inside the inset sidebar.
    pub fn traffic_shift(&self) -> (f32, f32) {
        match self.backend {
            WindowBackend::Builtin(BuiltinWindowKind::SystemSettings) => (6.0, 6.0),
            _ => (0.0, 0.0),
        }
    }

    pub fn in_titlebar(&self, px: f64, py: f64) -> bool {
        self.has_titlebar()
            && px >= self.x as f64
            && px < (self.x + self.width) as f64
            && py >= self.y as f64
            && py < (self.y + TITLEBAR_HEIGHT) as f64
    }

    pub fn hit_test_traffic(&self, px: f64, py: f64) -> Option<TrafficButton> {
        if !self.has_titlebar() {
            return None;
        }
        let (dx, dy) = self.traffic_shift();
        let lx = (px as f32) - (self.x as f32) - dx;
        let ly = (py as f32) - (self.y as f32) - dy;
        if !(-dy..=TITLEBAR_HEIGHT as f32).contains(&ly) {
            return None;
        }
        let cy = TITLEBAR_HEIGHT as f32 * 0.5;
        let centers = [
            (TRAFFIC_CENTERS[0], TrafficButton::Close),
            (TRAFFIC_CENTERS[1], TrafficButton::Minimize),
            (TRAFFIC_CENTERS[2], TrafficButton::Maximize),
        ];
        centers
            .into_iter()
            .find(|(cx, _)| (lx - cx).powi(2) + (ly - cy).powi(2) <= 8.0 * 8.0)
            .map(|(_, btn)| btn)
    }

    pub fn update_hover(&mut self, px: f64, py: f64) {
        if !self.has_titlebar() {
            return;
        }
        let (dx, dy) = self.traffic_shift();
        let lx = (px as f32) - (self.x as f32) - dx;
        let ly = (py as f32) - (self.y as f32) - dy;
        let group_hovered = (TRAFFIC_CENTERS[0] - 10.0..=TRAFFIC_CENTERS[2] + 10.0).contains(&lx)
            && (2.0..=(TITLEBAR_HEIGHT as f32 - 2.0)).contains(&ly);
        let btn = self.hit_test_traffic(px, py);
        if group_hovered != self.traffic_group_hovered || btn != self.hovered_traffic {
            self.traffic_group_hovered = group_hovered;
            self.hovered_traffic = btn;
            self.dirty = true;
        }
    }

    fn translate_monitor(&mut self,dx:i32,dy:i32){
        self.x+=dx;self.y+=dy;
        if let Some((x,y,_,_))=&mut self.saved_rect{*x+=dx;*y+=dy;}
    }
    pub fn toggle_maximize_on(&mut self,r:crate::monitors::Rect){self.maximize_on(!self.maximized,r);}
    pub fn maximize_on(&mut self,on:bool,r:crate::monitors::Rect){
        self.translate_monitor(-r.x,-r.y);self.set_maximized(on,r.w,r.h);self.translate_monitor(r.x,r.y);
        if on && (r.x!=0||r.y!=0){self.x=r.x;self.y=r.y;self.resize_client(r.w,r.h-self.titlebar_h());}
    }
    pub fn fullscreen_on(&mut self,on:bool,r:crate::monitors::Rect){
        self.translate_monitor(-r.x,-r.y);self.set_fullscreen(on,r.w,r.h);self.translate_monitor(r.x,r.y);
        if !on && self.maximized && (r.x!=0||r.y!=0){self.x=r.x;self.y=r.y;self.resize_client(r.w,r.h-self.titlebar_h());}
    }
    pub fn unmaximize_on(&mut self,p:(f64,f64),r:crate::monitors::Rect){
        self.translate_monitor(-r.x,-r.y);self.unmaximize_under((p.0-r.x as f64,p.1-r.y as f64),r.w,r.h);self.translate_monitor(r.x,r.y);
    }

    pub fn set_maximized(&mut self, maximize: bool, screen_w: i32, screen_h: i32) {
        if maximize == self.maximized {
            return;
        }
        if maximize {
            if !self.fullscreen {
                self.remember_rect();
            }
            let (ax, ay, aw, ah) = crate::theme::maximized_area(screen_w, screen_h);
            self.x = ax;
            self.y = ay;
            self.maximized = true;
            let max_w = aw.max(MIN_WINDOW_W);
            let max_h = (ah - self.titlebar_h()).max(MIN_WINDOW_H);
            self.set_xdg_state(xdg_toplevel::State::Maximized, true);
            self.resize_client(max_w, max_h);
        } else {
            self.maximized = false;
            self.set_xdg_state(xdg_toplevel::State::Maximized, false);
            self.restore_saved_rect((screen_w, screen_h));
        }
        self.dirty = true;
    }

    /// Size and place to come back to. A window maximized before its first frame (Electron
    /// and Chromium restore their maximized state that way) has no size yet: remembering
    /// 0×0 shrank it to the minimum on the first drag, and the app then kept that size.
    fn remember_rect(&mut self) {
        self.saved_rect = (self.mapped && self.width > 0 && self.height > 0).then_some((self.x, self.y, self.width, self.height));
    }

    /// Restored size when nothing was remembered: a comfortable part of the screen
    pub fn fallback_size(screen_w: i32, screen_h: i32) -> (i32, i32) {
        let usable = screen_h - TOP_BAR_HEIGHT - crate::theme::dock_reserved();
        (((screen_w as f32 * 0.6) as i32).max(MIN_WINDOW_W), ((usable as f32 * 0.75) as i32).max(MIN_WINDOW_H))
    }

    /// Width the window will have once un-maximized
    pub fn restored_width(&self, screen_w: i32, screen_h: i32) -> i32 {
        self.saved_rect.map_or_else(|| Self::fallback_size(screen_w, screen_h).0, |r| r.2)
    }

    /// Dragging a maximized window: restore it under the pointer, like macOS, keeping the
    /// grab at the same relative spot of the titlebar
    pub fn unmaximize_under(&mut self, pointer: (f64, f64), screen_w: i32, screen_h: i32) {
        if !self.maximized {
            return;
        }
        let rel = if self.width > 0 { ((pointer.0 - self.x as f64) / self.width as f64).clamp(0.0, 1.0) } else { 0.5 };
        let width = self.restored_width(screen_w, screen_h);
        self.set_maximized(false, screen_w, screen_h);
        self.x = ((pointer.0 - rel * width as f64) as i32).clamp(0, (screen_w - 40).max(0));
        self.y = (pointer.1 as i32 - 10).max(TOP_BAR_HEIGHT);
    }

    pub fn toggle_maximize(&mut self, screen_w: i32, screen_h: i32) {
        let target = !self.maximized;
        self.set_maximized(target, screen_w, screen_h);
    }

    pub fn set_fullscreen(&mut self, fullscreen: bool, screen_w: i32, screen_h: i32) {
        if fullscreen == self.fullscreen || self.is_builtin() {
            return;
        }
        if fullscreen {
            if !self.maximized {
                self.remember_rect();
            }
            self.fullscreen = true;
            self.x = 0;
            self.y = 0;
            self.set_xdg_state(xdg_toplevel::State::Fullscreen, true);
            self.resize_client(screen_w, screen_h);
        } else {
            self.fullscreen = false;
            self.set_xdg_state(xdg_toplevel::State::Fullscreen, false);
            if self.maximized {
                self.maximized = false;
                self.set_maximized(true, screen_w, screen_h);
            } else {
                self.restore_saved_rect((screen_w, screen_h));
            }
        }
        self.dirty = true;
    }

    fn restore_saved_rect(&mut self, (screen_w, screen_h): (i32, i32)) {
        if let Some((sx, sy, sw, sh)) = self.saved_rect.take() {
            self.x = sx;
            self.y = sy;
            self.resize_client(sw, sh);
        } else if self.mapped {
            // Shown already but never had a size of its own: a sane one, centred. The
            // client's own idea may be a tiny size it saved earlier.
            let (w, h) = Self::fallback_size(screen_w, screen_h);
            self.x = (screen_w - w) / 2;
            self.y = TOP_BAR_HEIGHT + ((screen_h - TOP_BAR_HEIGHT - crate::theme::dock_reserved() - h) / 2).max(0);
            self.resize_client(w, h);
        } else if let Some(t) = self.toplevel() {
            // Let the client pick its preferred size again
            t.with_pending_state(|s| s.size = None);
            if t.is_initial_configure_sent() {
                t.send_pending_configure();
            }
        }
    }

    fn set_xdg_state(&self, st: xdg_toplevel::State, on: bool) {
        if let Some(t) = self.toplevel() {
            t.with_pending_state(|s| {
                if on {
                    s.states.set(st);
                } else {
                    s.states.unset(st);
                }
            });
        }
    }

    /// Requests a new content size. Builtin windows resize immediately, Wayland clients
    /// get a configure and the real size is picked up on their next commit.
    pub fn resize_client(&mut self, new_w: i32, new_h: i32) {
        let w = new_w.max(MIN_WINDOW_W);
        let h = new_h.max(MIN_WINDOW_H);
        match self.backend {
            WindowBackend::Builtin(_) => {
                if self.width != w || self.height != h {
                    self.width = w;
                    self.height = h;
                    self.dirty = true;
                }
            }
            WindowBackend::Wayland(ref toplevel) => {
                toplevel.with_pending_state(|state| {
                    state.size = Some((w, h).into());
                });
                // Before the initial commit the size simply rides along with the first configure
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
        }
    }

    pub fn set_activated(&mut self, active: bool) {
        if self.activated == active {
            return;
        }
        self.activated = active;
        self.dirty = true;
        if let Some(t) = self.toplevel() {
            t.with_pending_state(|state| {
                if active {
                    state.states.set(xdg_toplevel::State::Activated);
                } else {
                    state.states.unset(xdg_toplevel::State::Activated);
                }
            });
            if t.is_initial_configure_sent() {
                t.send_pending_configure();
            }
        }
    }

    /// Pulls title, app_id, window geometry and mapped state from the client after a commit
    pub fn sync_wayland_metadata(&mut self) -> CommitChanges {
        let mut changes = CommitChanges::default();
        let Some(toplevel) = self.toplevel().cloned() else {
            return changes;
        };
        let wl_surface = toplevel.wl_surface();

        let (new_title, new_app_id, geometry) = with_states(wl_surface, |states| {
            let (t, a) = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|data| {
                    let role = data.lock().unwrap();
                    (role.title.clone(), role.app_id.clone())
                })
                .unwrap_or((None, None));
            let geo = states
                .cached_state
                .get::<SurfaceCachedState>()
                .current()
                .geometry;
            (t, a, geo)
        });

        if let Some(t) = new_title {
            if !t.is_empty() && t != self.title {
                self.title = t;
                self.dirty = true;
                changes.title_changed = true;
            }
        }
        if let Some(a) = new_app_id {
            if !a.is_empty() && a != self.app_id {
                self.app_id = a;
                changes.title_changed = true;
            }
        }

        let has_buffer =
            with_renderer_surface_state(wl_surface, |s| s.buffer().is_some()).unwrap_or(false);
        if has_buffer && !self.mapped {
            self.mapped = true;
            changes.newly_mapped = true;
        } else if !has_buffer && self.mapped {
            self.mapped = false;
            changes.unmapped = true;
        }
        if !self.mapped {
            return changes;
        }

        let bbox = bbox_from_surface_tree(wl_surface, (0, 0));
        let geo = geometry.and_then(|g| g.intersection(bbox)).unwrap_or(bbox);
        self.client_shadow =
            geometry.is_some() && (bbox.size.w > geo.size.w + 2 || bbox.size.h > geo.size.h + 2);
        self.geo_loc = geo.loc;

        if geo.size.w > 0
            && geo.size.h > 0
            && (geo.size.w != self.width || geo.size.h != self.height)
        {
            self.width = geo.size.w;
            self.height = geo.size.h;
            self.dirty = true;
            if let Some(anchor) = self.resize_anchor {
                if anchor.edges.left {
                    self.x = anchor.right - self.width;
                }
                if anchor.edges.top {
                    self.y = anchor.bottom - self.total_height();
                }
            }
        }
        changes
    }

    pub fn ensure_shadow(&mut self) {
        if self.draws_shadow() {
            let focused = self.activated;
            self.shadow.ensure(focused);
        }
    }

    pub fn ensure_rendered(
        &mut self,
        fonts: &mut FontSystem,
        focused: bool,
        ctx: &BuiltinContext<'_>,
    ) {
        self.ensure_shadow();

        // Builtin windows own their whole frame; Wayland SSD windows only need a titlebar strip
        let (lw, lh) = match self.backend {
            WindowBackend::Builtin(_) => (self.width, TITLEBAR_HEIGHT + self.height),
            WindowBackend::Wayland(_) if self.has_titlebar() => (self.width, TITLEBAR_HEIGHT),
            WindowBackend::Wayland(_) => return,
        };
        let target_phys_w = to_phys(lw);
        let target_phys_h = to_phys(lh);
        if self.phys_buf_w != target_phys_w || self.phys_buf_h != target_phys_h {
            self.decoration_buffer = MemoryRenderBuffer::new(
                Fourcc::Abgr8888,
                (target_phys_w, target_phys_h),
                1,
                Transform::Normal,
                None,
            );
            self.phys_buf_w = target_phys_w;
            self.phys_buf_h = target_phys_h;
            self.dirty = true;
        }

        if !self.dirty {
            return;
        }
        self.dirty = false;

        let bw = self.phys_buf_w;
        let bh = self.phys_buf_h;
        let win_w = lw as f32;
        let win_h = lh as f32;
        let title = self.title.clone();
        let group_hovered = self.traffic_group_hovered;
        let builtin_kind = match self.backend {
            WindowBackend::Wayland(_) => None,
            WindowBackend::Builtin(k) => Some(k),
        };
        let r = WINDOW_CORNER_RADIUS;
        let tb = TITLEBAR_HEIGHT as f32;
        let p = palette();
        let unified = builtin_kind == Some(BuiltinWindowKind::SystemSettings);
        let (tdx, tdy) = self.traffic_shift();

        let mut render_ctx = self.decoration_buffer.render();
        let _ = render_ctx.draw(|buf| {
            buf.fill(0);
            if let Some(mut pm) = PixmapMut::from_bytes(buf, bw as u32, bh as u32) {
                // A Wayland titlebar is just the top of a rounded rect: extend it past the strip
                let body_h = if builtin_kind.is_some() {
                    win_h
                } else {
                    win_h + r * 2.0
                };
                fill_rounded_rect(&mut pm, 0.0, 0.0, win_w, body_h, r, p.window_bg);
                if unified {
                    ctx.settings.paint(&mut pm, fonts, (0.0, tb), (win_w, win_h - tb), tb, &ctx.settings_context);
                } else {
                    let tb_col = if focused {
                        p.titlebar_active
                    } else {
                        p.titlebar_inactive
                    };
                    fill_rounded_rect(&mut pm, 0.0, 0.0, win_w, tb + r * 2.0, r, tb_col);
                    if builtin_kind.is_some() {
                        fill_rounded_rect(&mut pm, 0.0, tb, win_w, r * 2.0, 0.0, p.window_bg);
                    }
                    stroke_line(&mut pm, 0.0, tb - 0.5, win_w, tb - 0.5, 1.0, p.separator);
                }

                // Traffic lights (12 pt), coloured when focused or hovered
                let cy = tb * 0.5 + tdy;
                let colors = [
                    (rgba(255, 95, 87, 255), rgba(224, 68, 62, 255)),
                    (rgba(254, 188, 46, 255), rgba(222, 161, 35, 255)),
                    (rgba(40, 200, 64, 255), rgba(26, 171, 41, 255)),
                ];
                let centers = TRAFFIC_CENTERS.map(|x| x + tdx);
                for (i, cx) in centers.iter().enumerate() {
                    let (fill, edge) = if focused || group_hovered {
                        colors[i]
                    } else {
                        (p.traffic_inactive, p.traffic_inactive_edge)
                    };
                    fill_circle(&mut pm, *cx, cy, TRAFFIC_RADIUS, fill);
                    stroke_circle(&mut pm, *cx, cy, TRAFFIC_RADIUS - 0.25, 0.5, edge);
                }
                if group_hovered {
                    let ink = rgba(0, 0, 0, 150);
                    let [c0, c1, c2] = centers;
                    stroke_line(&mut pm, c0 - 2.4, cy - 2.4, c0 + 2.4, cy + 2.4, 1.2, ink);
                    stroke_line(&mut pm, c0 + 2.4, cy - 2.4, c0 - 2.4, cy + 2.4, 1.2, ink);
                    stroke_line(&mut pm, c1 - 3.0, cy, c1 + 3.0, cy, 1.3, ink);
                    // Green: two diagonal arrowheads (zoom), drawn as small triangles
                    fill_rounded_rect(&mut pm, c2 - 3.0, cy - 3.0, 3.6, 3.6, 0.6, ink);
                    fill_rounded_rect(&mut pm, c2 - 0.6, cy - 0.6, 3.6, 3.6, 0.6, ink);
                    fill_circle(&mut pm, c2, cy, 1.0, colors[2].0);
                }

                let title_col = if focused {
                    p.title_active
                } else {
                    p.title_inactive
                };
                if !unified { fonts.draw_text_clipped_centered(
                    &mut pm,
                    &title,
                    win_w * 0.5,
                    (tb - 13.0) * 0.5 - 1.5,
                    (win_w - 2.0 * (TRAFFIC_CENTERS[2] + 24.0)).max(40.0),
                    13.0,
                    title_col,
                    true,
                );

                }
                if let Some(kind) = builtin_kind.filter(|_| !unified) {
                    Self::draw_builtin_content(
                        &mut pm,
                        fonts,
                        kind,
                        0.0,
                        tb,
                        win_w,
                        win_h - tb,
                        ctx,
                    );
                }

                stroke_rounded_rect(
                    &mut pm,
                    0.25,
                    0.25,
                    win_w - 0.5,
                    body_h - 0.5,
                    r,
                    0.5,
                    p.window_outline,
                );
            }
            Result::<_, ()>::Ok(vec![Rectangle::from_size((bw, bh).into())])
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_builtin_content(
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        kind: BuiltinWindowKind,
        cx: f32,
        cy: f32,
        cw: f32,
        ch: f32,
        ctx: &BuiltinContext<'_>,
    ) {
        let p = palette();
        let row = |pm: &mut PixmapMut, fonts: &mut FontSystem, y: f32, k: &str, v: &str| {
            fonts.draw_text_right(pm, k, cx + cw * 0.5 - 8.0, y, 13.0, p.text_secondary, false);
            fonts.draw_text_clipped(
                pm,
                v,
                cx + cw * 0.5 + 8.0,
                y,
                cw * 0.5 - 30.0,
                13.0,
                p.text,
                false,
            );
        };
        match kind {
            BuiltinWindowKind::AboutFeather => {
                let size = 92.0;
                fill_squircle_gradient(
                    pm,
                    cx + cw * 0.5 - size * 0.5,
                    cy + 22.0,
                    size,
                    rgba(120, 205, 255, 255),
                    rgba(20, 105, 235, 255),
                );
                draw_feather_logo(
                    pm,
                    cx + cw * 0.5,
                    cy + 22.0 + size * 0.5,
                    3.6,
                    rgba(255, 255, 255, 255),
                );
                fonts.draw_text_centered(
                    pm,
                    "Пёрышко",
                    cx + cw * 0.5,
                    cy + 128.0,
                    24.0,
                    p.text,
                    true,
                );
                fonts.draw_text_centered(
                    pm,
                    "Версия 0.2 • Tahoe",
                    cx + cw * 0.5,
                    cy + 160.0,
                    12.0,
                    p.text_secondary,
                    false,
                );
                let scale = format!(
                    "{}% ({})",
                    (ctx.scale_factor * 100.0).round() as i32,
                    ctx.wayland_socket
                );
                let mem = format!("{:.1} МБ", ctx.rss_mb);
                let lines = [
                    ("Оформление", appearance().name().to_string()),
                    ("Масштаб", scale),
                    ("Память оболочки", mem),
                    (
                        "Раскладка",
                        format!("{} ({})", ctx.active_layout, ctx.input_shortcut),
                    ),
                ];
                for (i, (k, v)) in lines.iter().enumerate() {
                    row(pm, fonts, cy + 190.0 + i as f32 * 21.0, k, v);
                }
                let (bw, bh) = (150.0, 26.0);
                let bx = cx + cw - bw - 20.0;
                let by = cy + ch - 42.0;
                // «Показывать при входе» checkbox, macOS style
                let (kx, ky) = (cx + 20.0, by + 5.0);
                if ctx.show_about {
                    fill_rounded_rect(pm, kx, ky, 16.0, 16.0, 4.0, p.accent);
                    stroke_line(pm, kx + 4.0, ky + 8.5, kx + 7.0, ky + 11.5, 1.8, p.on_accent);
                    stroke_line(pm, kx + 7.0, ky + 11.5, kx + 12.5, ky + 4.5, 1.8, p.on_accent);
                } else {
                    fill_rounded_rect(pm, kx, ky, 16.0, 16.0, 4.0, p.window_bg);
                    stroke_rounded_rect(pm, kx, ky, 16.0, 16.0, 4.0, 1.0, p.glass_outline);
                }
                fonts.draw_text(pm, "Показывать при входе", kx + 24.0, ky + 1.0, 13.0, p.text, false);
                fill_rounded_rect(pm, bx, by, bw, bh, bh * 0.5, p.control_fill);
                stroke_rounded_rect(pm, bx, by, bw, bh, bh * 0.5, 0.5, p.glass_outline);
                fonts.draw_text_centered(
                    pm,
                    "Сменить обои…",
                    bx + bw * 0.5,
                    by + 5.5,
                    13.0,
                    p.text,
                    false,
                );
            }
            BuiltinWindowKind::ActivityMonitor => {
                let card_w = (cw - 56.0) / 3.0;
                let metrics = [
                    ("Память оболочки", format!("{:.1} МБ", ctx.rss_mb)),
                    ("Окон", ctx.window_count.to_string()),
                    (
                        "Масштаб",
                        format!("{}%", (ctx.scale_factor * 100.0).round() as i32),
                    ),
                ];
                for (i, (label, val)) in metrics.iter().enumerate() {
                    let mx = cx + 20.0 + i as f32 * (card_w + 8.0);
                    fill_rounded_rect(pm, mx, cy + 16.0, card_w, 66.0, 12.0, p.card);
                    fonts.draw_text(
                        pm,
                        label,
                        mx + 14.0,
                        cy + 26.0,
                        12.0,
                        p.text_secondary,
                        false,
                    );
                    fonts.draw_text(pm, val, mx + 14.0, cy + 46.0, 20.0, p.text, true);
                }
                let ty = cy + 98.0;
                fill_rounded_rect(pm, cx + 20.0, ty, cw - 40.0, ch - 116.0, 12.0, p.card);
                let rows = [
                    ("Композитор", "Smithay • calloop", "событийный цикл"),
                    ("Рендер", "OpenGL ES", "damage tracking"),
                    ("Темп кадров", "по vblank хоста", "неблокирующий swap"),
                    ("Дробный масштаб", "wp_fractional_scale_v1", "HiDPI"),
                    ("Раскладка", ctx.active_layout, ctx.input_shortcut),
                ];
                for (i, (a, b, c)) in rows.iter().enumerate() {
                    let ry = ty + 12.0 + i as f32 * 28.0;
                    if i > 0 {
                        stroke_line(
                            pm,
                            cx + 34.0,
                            ry - 6.0,
                            cx + cw - 34.0,
                            ry - 6.0,
                            1.0,
                            p.separator,
                        );
                    }
                    fonts.draw_text(pm, a, cx + 34.0, ry, 13.0, p.text, false);
                    fonts.draw_text(pm, b, cx + 200.0, ry, 13.0, p.text_secondary, false);
                    fonts.draw_text_right(pm, c, cx + cw - 34.0, ry, 12.0, p.text_tertiary, false);
                }
            }
            BuiltinWindowKind::QuickNotes => {
                let max_lines = (((ch - 36.0) / 22.0).floor() as usize).max(1);
                let all_lines: Vec<&str> = ctx.notes_text.split('\n').collect();
                let skip = all_lines.len().saturating_sub(max_lines);
                let visible = &all_lines[skip..];
                for (i, line) in visible.iter().enumerate() {
                    let ly = cy + 18.0 + i as f32 * 22.0;
                    let bold = i == 0 && skip == 0;
                    fonts.draw_text_clipped(
                        pm,
                        line,
                        cx + 24.0,
                        ly,
                        cw - 48.0,
                        if bold { 17.0 } else { 14.0 },
                        p.text,
                        bold,
                    );
                }
                let last_line = visible.last().copied().unwrap_or("");
                let last_bold = visible.len() == 1 && skip == 0;
                let size = if last_bold { 17.0 } else { 14.0 };
                let cursor_x = cx
                    + 24.0
                    + fonts
                        .measure_text(last_line, size, last_bold)
                        .min(cw - 48.0);
                let cursor_y = cy + 18.0 + (visible.len().saturating_sub(1) as f32) * 22.0;
                fill_rounded_rect(
                    pm,
                    cursor_x + 1.5,
                    cursor_y,
                    2.0,
                    18.0,
                    1.0,
                    rgba(255, 179, 0, 255),
                );
            }
            BuiltinWindowKind::SystemSettings => {
                ctx.settings
                    .paint(pm, fonts, (cx, cy), (cw, ch), cy, &ctx.settings_context);
            }
        }
    }
}

pub struct SettingsFrame {
    buffer: MemoryRenderBuffer,
    shadow: Option<ShadowSet>,
    size: (i32, i32),
}

#[cfg(test)]
mod maximize_tests {
    use super::*;
    use crate::apps::BuiltinWindowKind;

    #[test]
    fn maximize_on_negative_monitor_preserves_global_restore_rectangle(){
        let r=crate::monitors::Rect{x:-1280,y:-100,w:1280,h:720};
        let mut w=ManagedWindow::new_builtin(1,BuiltinWindowKind::SystemSettings,-1100,40);w.resize_client(700,440);
        let before=(w.x,w.y,w.width,w.height);w.maximize_on(true,r);
        assert_eq!((w.x,w.y,w.width,w.total_height()),(-1280,-100,1280,720));
        w.maximize_on(false,r);assert_eq!((w.x,w.y,w.width,w.height),before);
    }
    #[test]
    fn unmaximize_restores_the_real_size_or_a_sane_one_never_the_minimum() {
        let (sw, sh) = (1440, 900);
        let mut w = ManagedWindow::new_builtin(1, BuiltinWindowKind::SystemSettings, 100, 120);
        w.resize_client(800, 500);
        w.set_maximized(true, sw, sh);
        w.set_maximized(false, sw, sh);
        assert_eq!((w.x, w.y, w.width, w.height), (100, 120, 800, 500));
        // Maximized before it ever had a size (Electron restoring its state)
        let mut early = ManagedWindow::new_builtin(2, BuiltinWindowKind::SystemSettings, 0, 0);
        early.width = 0;
        early.height = 0;
        early.set_maximized(true, sw, sh);
        assert!(early.saved_rect.is_none(), "0×0 is not remembered");
        early.set_maximized(false, sw, sh);
        let (fw, fh) = ManagedWindow::fallback_size(sw, sh);
        assert_eq!((early.width, early.height), (fw, fh));
        assert!(fw > MIN_WINDOW_W * 2 && fh > MIN_WINDOW_H, "a usable window, not the minimum");
        // Dragging a maximized window keeps the grab at the same relative spot
        let mut drag = ManagedWindow::new_builtin(3, BuiltinWindowKind::SystemSettings, 0, 0);
        drag.resize_client(600, 400);
        drag.set_maximized(true, sw, sh);
        drag.unmaximize_under((sw as f64 * 0.75, 40.0), sw, sh);
        assert!(!drag.maximized && drag.width == 600);
        assert_eq!(drag.x, (sw as f64 * 0.75 - 0.75 * 600.0) as i32);
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    fn bytes(buffer: &mut MemoryRenderBuffer) -> Vec<u8> {
        let mut out = Vec::new();
        let mut context = buffer.render();
        let _ = context.draw(|pixels| {
            out.extend_from_slice(pixels);
            Ok::<_, ()>(Vec::new())
        });
        out
    }
    #[test]
    fn worker_settings_frame_matches_direct_paint_and_rejects_late_or_superseded_frames() {
        fn send<T: Send>() {}
        send::<SettingsFrame>();
        send::<crate::system_settings::FrameJob>();
        let mut fonts = FontSystem::new();
        let mut worker_fonts = fonts.for_worker();
        let panel = crate::system_settings::Panel::default();
        let keyboard = crate::settings::Keyboard::default();
        let mode = crate::system_settings::Mode {
            width: 1920,
            height: 1080,
            refresh: 60000,
        };
        crate::theme::with_render_settings(1.25, Appearance::Light, || {
            let context = crate::system_settings::Context {
                system: Default::default(),
                pointer: Default::default(),
                power:Default::default(),power_ready:false,
                pointer_managed: true,
                keyboard: &keyboard,
                active: 0,
                animations: true,
                dock_zoom: true,
                scale: 1.25,
                display_name: "Window",
                monitors: Vec::new(),
                modes: &[mode],
                current: mode,
                direct: false,
                wallpaper: crate::theme::WallpaperTheme::for_appearance(Appearance::Light),
                wallpaper_image: None,
                dnd: false,
                dnd_schedule: None,
                notify_sounds: true,
                sounds: Default::default(),
                audio: Default::default(),
                hot_corners: Default::default(),
                night: Default::default(),
                night_active: false,
                notify_apps: &[],
            };
            let mut direct = ManagedWindow::new_builtin(1, BuiltinWindowKind::SystemSettings, 0, 0);
            direct.activated = true;
            let ctx = BuiltinContext {
                settings: &panel,
                settings_context: context,
                notes_text: "",
                rss_mb: 0.0,
                window_count: 0,
                wayland_socket: "",
                active_layout: "",
                input_shortcut: "",
                scale_factor: 1.25,
                show_about: false,
            };
            direct.ensure_rendered(&mut fonts, true, &ctx);
            let expected = bytes(&mut direct.decoration_buffer);
            let mut target = ManagedWindow::new_builtin(2, BuiltinWindowKind::SystemSettings, 0, 0);
            target.activated = true;
            let ticket = std::sync::Arc::new(());
            target.settings_ticket = Some(ticket.clone());
            target.dirty = false;
            let job = crate::system_settings::FrameJob::new(
                std::sync::Arc::downgrade(&ticket),
                &panel,
                &ctx.settings_context,
                &target,
            );
            let mut worker = ManagedWindow::new_builtin(0, BuiltinWindowKind::SystemSettings, 0, 0);
            let wrong = std::sync::Arc::new(());
            assert!(!target.install_settings_frame(
                &std::sync::Arc::downgrade(&wrong),
                worker.raster_settings(&mut worker_fonts, &job)
            ));
            assert!(target.install_settings_frame(
                &job.ticket,
                worker.raster_settings(&mut worker_fonts, &job)
            ));
            assert_eq!(bytes(&mut target.decoration_buffer), expected);
            target.settings_ticket = Some(ticket);
            target.dirty = true;
            assert!(
                !target.install_settings_frame(
                    &job.ticket,
                    worker.raster_settings(&mut worker_fonts, &job)
                ),
                "late frame painted over a newer edit"
            );
        });
    }
}
