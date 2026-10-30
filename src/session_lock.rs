//! Экран входа, экран блокировки (ext-session-lock-v1 + встроенная PAM-панель) и
//! заставка загрузки в стиле Пёрышка (macOS Tahoe Login Window & Boot Splash).
//! Потеря внешнего клиента блокировки никогда не раскрывает рабочий стол.
use std::time::{Duration, Instant};

use crate::{
    render::{FeatherElement, SceneElement},
    text::FontSystem,
    theme::*,
    FeatherCompositor,
};
use feather_ui::i18n::tr;
use feather_ui::input::TextInput;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    reexports::{
        calloop::channel,
        wayland_protocols::ext::session_lock::v1::server::{
            ext_session_lock_manager_v1::ExtSessionLockManagerV1,
            ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
            ext_session_lock_v1::{self, ExtSessionLockV1},
        },
        wayland_server::{
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
            Client, DataInit, Dispatch, DisplayHandle, Resource,
        },
    },
    utils::{Rectangle, Transform},
    wayland::session_lock::{
        ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
        SessionLockManagerState, SessionLockState, SessionLocker,
    },
};

const SPLASH_W: f32 = 360.0;
const SPLASH_H: f32 = 250.0;
const SPLASH_TRACK_W: f32 = 220.0;
const SPLASH_TRACK_H: f32 = 6.0;
const SPLASH_TRACK_Y: f32 = 194.0;
const SPLASH_DURATION: Duration = Duration::from_millis(1150);

/// Заставка загрузки в стиле Пёрышка (macOS Tahoe Boot Splash):
/// статическая эмблема и подпись рисуются на CPU один раз, а заполнение полосы
/// загрузки и финальное растворение выполняются исключительно GPU-элементами.
pub struct BootSplash {
    pub active: bool,
    pub paused_progress: Option<f32>,
    started_at: Instant,
    duration: Duration,
    dirty: bool,
    scale: f64,
    phys: (i32, i32),
    card_buffer: MemoryRenderBuffer,
    fill_buffer: MemoryRenderBuffer,
    bg_buffer: MemoryRenderBuffer,
}

impl Default for BootSplash {
    fn default() -> Self {
        let solid_buf = |rgba_px: [u8; 4]| {
            let mut b = MemoryRenderBuffer::new(Fourcc::Abgr8888, (4, 4), 1, Transform::Normal, None);
            let _ = b.render().draw(|buf| {
                buf.chunks_exact_mut(4).for_each(|px| px.copy_from_slice(&rgba_px));
                Result::<_, ()>::Ok(vec![Rectangle::from_size((4, 4).into())])
            });
            b
        };
        Self {
            active: false,
            paused_progress: None,
            started_at: Instant::now(),
            duration: SPLASH_DURATION,
            dirty: true,
            scale: 0.0,
            phys: (1, 1),
            card_buffer: MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None),
            fill_buffer: solid_buf([242, 248, 255, 255]),
            bg_buffer: solid_buf([11, 14, 24, 255]),
        }
    }
}

impl BootSplash {
    pub fn start(&mut self, paused_progress: Option<f32>) {
        self.active = true;
        self.paused_progress = paused_progress.map(|p| p.clamp(0.05, 1.0));
        self.started_at = Instant::now();
        self.dirty = true;
    }

    pub fn progress(&self, now: Instant) -> f32 {
        if let Some(p) = self.paused_progress {
            return p;
        }
        let t = (now.saturating_duration_since(self.started_at).as_secs_f32()
            / self.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        // Smooth cubic ease-out
        1.0 - (1.0 - t).powi(3)
    }

    pub fn alpha(&self, now: Instant) -> f32 {
        if self.paused_progress.is_some() {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(self.started_at).as_secs_f32();
        let total = self.duration.as_secs_f32();
        let fade_window = 0.20;
        if elapsed <= total - fade_window {
            1.0
        } else {
            ((total - elapsed) / fade_window).clamp(0.0, 1.0)
        }
    }

    pub fn is_opaque(&self, now: Instant) -> bool {
        self.active && self.alpha(now) >= 0.999
    }

    pub fn advance(&mut self, now: Instant) -> bool {
        if !self.active {
            return false;
        }
        if self.paused_progress.is_some() {
            return false;
        }
        if now.saturating_duration_since(self.started_at) >= self.duration {
            self.active = false;
            self.card_buffer =
                MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None);
            self.phys = (1, 1);
            self.scale = 0.0;
            return false;
        }
        true
    }

    fn card_origin(screen: (i32, i32)) -> (f32, f32) {
        (
            (screen.0 as f32 - SPLASH_W) * 0.5,
            (screen.1 as f32 - SPLASH_H) * 0.46,
        )
    }

    pub fn paint(&mut self, fonts: &mut FontSystem, scale: f64) {
        if !self.active {
            return;
        }
        let rescaled = (self.scale - scale).abs() > 1e-6;
        if !self.dirty && !rescaled {
            return;
        }
        self.scale = scale;
        self.dirty = false;
        let size = (
            (SPLASH_W as f64 * scale).round().max(1.0) as i32,
            (SPLASH_H as f64 * scale).round().max(1.0) as i32,
        );
        if self.phys != size {
            self.phys = size;
            self.card_buffer =
                MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        }
        let mut ctx = self.card_buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            let mut pm =
                tiny_skia::PixmapMut::from_bytes(buf, size.0 as u32, size.1 as u32).unwrap();
            let cx = SPLASH_W * 0.5;

            // 1. Emblem squircle with deep sky-blue -> indigo Tahoe gradient
            let icon_sz = 92.0;
            let ix = cx - icon_sz * 0.5;
            let iy = 12.0;
            // Subtle outer glow
            fill_rounded_rect(
                &mut pm,
                ix - 4.0,
                iy - 2.0,
                icon_sz + 8.0,
                icon_sz + 8.0,
                26.0,
                rgba(56, 148, 255, 38),
            );
            fill_squircle_gradient(
                &mut pm,
                ix,
                iy,
                icon_sz,
                rgba(92, 188, 255, 255),
                rgba(18, 96, 232, 255),
            );
            stroke_squircle(
                &mut pm,
                ix + 0.5,
                iy + 0.5,
                icon_sz - 1.0,
                1.0,
                rgba(255, 255, 255, 95),
            );

            // 2. Crisp white Feather quill silhouette inside the emblem
            draw_feather_emblem(&mut pm, ix, iy, icon_sz);

            // 3. Title & subtitle
            fonts.draw_text_centered(
                &mut pm,
                "Пёрышко",
                cx,
                118.0,
                25.0,
                rgba(255, 255, 255, 250),
                true,
            );
            fonts.draw_text_centered(
                &mut pm,
                "Feather DE · Tahoe",
                cx,
                150.0,
                12.5,
                rgba(186, 210, 245, 210),
                false,
            );

            // 4. Capsule progress track
            let tx = (SPLASH_W - SPLASH_TRACK_W) * 0.5;
            fill_rounded_rect(
                &mut pm,
                tx,
                SPLASH_TRACK_Y,
                SPLASH_TRACK_W,
                SPLASH_TRACK_H,
                SPLASH_TRACK_H * 0.5,
                rgba(255, 255, 255, 34),
            );
            stroke_rounded_rect(
                &mut pm,
                tx + 0.5,
                SPLASH_TRACK_Y + 0.5,
                SPLASH_TRACK_W - 1.0,
                SPLASH_TRACK_H - 1.0,
                (SPLASH_TRACK_H - 1.0) * 0.5,
                1.0,
                rgba(255, 255, 255, 52),
            );

            // 5. Status caption
            let status = tr("Запуск графической среды…");
            fonts.draw_text_centered(
                &mut pm,
                &status,
                cx,
                SPLASH_TRACK_Y + 18.0,
                11.5,
                rgba(175, 194, 224, 185),
                false,
            );

            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
    }

    pub fn elements(
        &self,
        renderer: &mut GlesRenderer,
        scale: f64,
        screen: (i32, i32),
        now: Instant,
    ) -> Vec<FeatherElement> {
        if !self.active {
            return Vec::new();
        }
        let alpha = self.alpha(now);
        if alpha <= 0.001 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(3);
        let (ox, oy) = Self::card_origin(screen);

        // 1. Progress fill bar (front-most)
        let prog = self.progress(now);
        let fill_w_log = (SPLASH_TRACK_W * prog).max(SPLASH_TRACK_H);
        let fill_x = ox + (SPLASH_W - SPLASH_TRACK_W) * 0.5;
        let fill_y = oy + SPLASH_TRACK_Y;
        let fill_loc = (
            (fill_x as f64 * scale).round(),
            (fill_y as f64 * scale).round(),
        );
        let fill_dst = (
            (fill_w_log as f64 * scale).round().max(2.0) as i32,
            (SPLASH_TRACK_H as f64 * scale).round().max(2.0) as i32,
        );
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            fill_loc,
            &self.fill_buffer,
            Some(alpha),
            Some(Rectangle::from_size((4.0, 4.0).into())),
            Some(fill_dst.into()),
            Kind::Unspecified,
        ) {
            out.push(SceneElement::Memory(e).into());
        }

        // 2. Emblem + title + progress track card
        let card_loc = ((ox as f64 * scale).round(), (oy as f64 * scale).round());
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            card_loc,
            &self.card_buffer,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        ) {
            out.push(SceneElement::Memory(e).into());
        }

        // 3. Fullscreen deep obsidian backdrop
        let full = (
            (screen.0 as f64 * scale).round().max(1.0) as i32,
            (screen.1 as f64 * scale).round().max(1.0) as i32,
        );
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (0.0, 0.0),
            &self.bg_buffer,
            Some(alpha),
            Some(Rectangle::from_size((4.0, 4.0).into())),
            Some(full.into()),
            Kind::Unspecified,
        ) {
            out.push(SceneElement::Memory(e).into());
        }

        out
    }
}

fn draw_feather_emblem(pm: &mut tiny_skia::PixmapMut, ix: f32, iy: f32, sz: f32) {
    let mut pb = tiny_skia::PathBuilder::new();
    // Graceful diagonal feather vane from bottom-left (0.24, 0.78) to top-right (0.78, 0.20)
    pb.move_to(ix + sz * 0.26, iy + sz * 0.74);
    pb.cubic_to(
        ix + sz * 0.24,
        iy + sz * 0.44,
        ix + sz * 0.48,
        iy + sz * 0.22,
        ix + sz * 0.78,
        iy + sz * 0.20,
    );
    pb.cubic_to(
        ix + sz * 0.76,
        iy + sz * 0.50,
        ix + sz * 0.54,
        iy + sz * 0.72,
        ix + sz * 0.26,
        iy + sz * 0.74,
    );
    pb.close();
    if let Some(path) = pb.finish() {
        fill_path_gradient(
            pm,
            &path,
            iy + sz * 0.20,
            iy + sz * 0.76,
            rgba(255, 255, 255, 252),
            rgba(218, 238, 255, 240),
        );
    }
    // Central shaft (rachis) + quill stem extending to bottom-left
    stroke_line(
        pm,
        ix + sz * 0.20,
        iy + sz * 0.80,
        ix + sz * 0.72,
        iy + sz * 0.26,
        sz * 0.038,
        rgba(255, 255, 255, 255),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockClick {
    None,
    Submit,
    NextLayout,
    Sleep,
    Restart,
    PowerOff,
}

pub struct Lock {
    pub active: bool,
    pub login_mode: bool,
    pub splash: BootSplash,
    pub owner: Option<ExtSessionLockV1>,
    pub surface: Option<LockSurface>,
    pub extra_surfaces: Vec<(String, LockSurface)>,
    pending: Option<SessionLocker>,
    confirmed: bool,
    generation: u64,
    input: TextInput,
    verifying: bool,
    pub(crate) message: &'static str,
    caps: bool,
    layout: String,
    hover_action: Option<u8>,
    pub(crate) dirty: bool,
    status_dirty: bool,
    /// Account & power actions panel at the bottom: avatar, name, password field, Sleep/Restart/Shut Down
    buffer: MemoryRenderBuffer,
    /// Date and large time at the top; repainted when the minute changes
    clock_buffer: MemoryRenderBuffer,
    /// Top-right status pill (session badge, keyboard layout switcher, Wi-Fi & battery icons)
    status_buffer: MemoryRenderBuffer,
    clock: String,
    /// 4×4 black, stretched over the wallpaper so white text reads on light ones
    dim: MemoryRenderBuffer,
    scale: f64,
    phys: (i32, i32),
    status_phys: (i32, i32),
    pub protected_frame: bool,
    pub display_protected: bool,
    was_external: bool,
    retired_owner: Option<ExtSessionLockV1>,
}

impl Default for Lock {
    fn default() -> Self {
        Self {
            active: false,
            login_mode: false,
            splash: BootSplash::default(),
            owner: None,
            surface: None,
            extra_surfaces: Vec::new(),
            pending: None,
            confirmed: false,
            generation: 0,
            input: TextInput::default().with_byte_limit(1024),
            verifying: false,
            message: "Введите пароль вашей учётной записи",
            caps: false,
            layout: String::new(),
            hover_action: None,
            dirty: true,
            status_dirty: true,
            buffer: MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None),
            clock_buffer: MemoryRenderBuffer::new(
                Fourcc::Abgr8888,
                (1, 1),
                1,
                Transform::Normal,
                None,
            ),
            status_buffer: MemoryRenderBuffer::new(
                Fourcc::Abgr8888,
                (1, 1),
                1,
                Transform::Normal,
                None,
            ),
            clock: String::new(),
            dim: {
                let mut dim =
                    MemoryRenderBuffer::new(Fourcc::Abgr8888, (4, 4), 1, Transform::Normal, None);
                let _ = dim.render().draw(|buf| {
                    buf.chunks_exact_mut(4)
                        .for_each(|px| px.copy_from_slice(&[0, 0, 0, 255]));
                    Result::<_, ()>::Ok(vec![Rectangle::from_size((4, 4).into())])
                });
                dim
            },
            scale: 0.0,
            phys: (1, 1),
            status_phys: (1, 1),
            protected_frame: false,
            display_protected: false,
            was_external: false,
            retired_owner: None,
        }
    }
}

impl Lock {
    pub fn external(&self) -> bool {
        self.owner.as_ref().is_some_and(Resource::is_alive)
    }

    pub fn focus(&self) -> Option<WlSurface> {
        if !self.external() {
            return None;
        }
        self.surface
            .as_ref()
            .filter(|s| s.alive())
            .map(|s| s.wl_surface().clone())
            .filter(|s| {
                smithay::backend::renderer::utils::with_renderer_surface_state(s, |r| {
                    r.buffer().is_some()
                })
                .unwrap_or(false)
            })
    }

    pub fn key(&mut self, sym: u32, ch: Option<char>, ctrl: bool, shift: bool, caps: bool) -> bool {
        if self.verifying {
            return false;
        }
        self.caps = caps;
        self.dirty = true;
        if sym == 0xff0d || sym == 0xff8d {
            return true;
        }
        if sym == 0xff1b {
            self.input.clear();
            return false;
        }
        let mut bytes = [0u8; 4];
        let utf8 = ch.map(|c| c.encode_utf8(&mut bytes) as &str).unwrap_or("");
        self.input.key(sym, Some(utf8), ctrl, shift);
        zeroize::Zeroize::zeroize(&mut bytes);
        false
    }

    /// Account panel origin (logical): bottom centre, like the macOS Tahoe login window
    fn panel_origin(screen: (i32, i32)) -> (f32, f32) {
        (
            (screen.0 as f32 - PANEL_W) * 0.5,
            (screen.1 as f32 - PANEL_H - (screen.1 as f32 * 0.05).max(28.0)).max(CLOCK_H + 20.0),
        )
    }

    fn clock_origin(screen: (i32, i32)) -> (f32, f32) {
        (
            (screen.0 as f32 - CLOCK_W) * 0.5,
            (screen.1 as f32 * 0.07).clamp(24.0, 90.0),
        )
    }

    fn status_origin(screen: (i32, i32)) -> (f32, f32) {
        ((screen.0 as f32 - STATUS_W - 18.0).max(12.0), 12.0)
    }

    fn action_buttons_centers() -> [(f32, f32); 3] {
        let cx = PANEL_W * 0.5;
        let cy = 216.0;
        [(cx - 104.0, cy), (cx, cy), (cx + 104.0, cy)]
    }

    pub fn motion(&mut self, x: f64, y: f64, screen: (i32, i32)) -> bool {
        if !self.active || self.external() {
            return false;
        }
        let mut new_hover = None;
        let (sx, sy) = Self::status_origin(screen);
        if (sx..=sx + STATUS_W).contains(&(x as f32))
            && (sy..=sy + STATUS_H).contains(&(y as f32))
        {
            new_hover = Some(3);
        } else {
            let (ox, oy) = Self::panel_origin(screen);
            let (lx, ly) = (x as f32 - ox, y as f32 - oy);
            for (idx, (bx, by)) in Self::action_buttons_centers().into_iter().enumerate() {
                if (lx - bx).hypot(ly - by) <= 22.0 {
                    new_hover = Some(idx as u8);
                    break;
                }
            }
        }
        if self.hover_action != new_hover {
            if self.hover_action == Some(3) || new_hover == Some(3) {
                self.status_dirty = true;
            }
            self.hover_action = new_hover;
            self.dirty = true;
            return true;
        }
        false
    }

    /// Handles a click on the built-in login/lock screen.
    pub fn click_action(
        &mut self,
        fonts: &mut FontSystem,
        x: f64,
        y: f64,
        screen: (i32, i32),
        extend: bool,
    ) -> LockClick {
        if self.verifying || self.external() {
            return LockClick::None;
        }
        // 1. Top-right status pill (keyboard layout switcher)
        let (sx, sy) = Self::status_origin(screen);
        if (sx..=sx + STATUS_W).contains(&(x as f32))
            && (sy..=sy + STATUS_H).contains(&(y as f32))
        {
            return LockClick::NextLayout;
        }

        let (ox, oy) = Self::panel_origin(screen);
        let (x, y) = (x as f32 - ox, y as f32 - oy);

        // 2. Bottom power/session action buttons (Sleep / Restart / Shut Down)
        for (idx, (bx, by)) in Self::action_buttons_centers().into_iter().enumerate() {
            if (x - bx).hypot(y - by) <= 22.0 {
                return match idx {
                    0 => LockClick::Sleep,
                    1 => LockClick::Restart,
                    _ => LockClick::PowerOff,
                };
            }
        }

        // 3. Password field & submit arrow
        let (fx, fy, fw, fh) = FIELD;
        if !(fy..fy + fh).contains(&y) || !(fx..fx + fw).contains(&x) {
            return LockClick::None;
        }
        if x >= fx + fw - fh {
            return if !self.input.is_empty() {
                LockClick::Submit
            } else {
                LockClick::None
            };
        }
        self.input
            .click_masked(fonts, x - (fx + 16.0), fw - 16.0 - fh, 14.0, extend);
        self.dirty = true;
        LockClick::None
    }

    /// Backwards-compatible helper returning true when password submission is requested
    pub fn click(
        &mut self,
        fonts: &mut FontSystem,
        x: f64,
        y: f64,
        screen: (i32, i32),
        extend: bool,
    ) -> bool {
        self.click_action(fonts, x, y, screen, extend) == LockClick::Submit
    }

    /// Wallpaper or glass changed under the built-in panel
    pub fn repaint(&mut self) {
        if self.active {
            self.dirty = true;
            self.status_dirty = true;
            self.clock.clear();
        }
    }

    /// The clock shows a different minute: the next frame repaints it
    pub fn clock_stale(&self) -> bool {
        self.active && !self.external() && self.clock != clock_text().1
    }

    pub fn paint(&mut self, fonts: &mut FontSystem, scale: f64, layout: &str, screen: (i32, i32)) {
        if !self.active || self.external() {
            return;
        }
        if self.layout != layout {
            self.layout.clear();
            self.layout.push_str(layout);
            self.dirty = true;
            self.status_dirty = true;
        }
        let rescaled = self.scale != scale;
        let (date, time) = clock_text();
        if rescaled || self.clock != time {
            self.paint_clock(fonts, scale, screen, &date, &time);
            self.clock = time;
        }
        if rescaled || self.status_dirty {
            self.paint_status(fonts, scale, screen);
            self.status_dirty = false;
        }
        if !self.dirty && !rescaled {
            return;
        }
        self.scale = scale;
        self.dirty = false;
        let size = (
            (PANEL_W as f64 * scale).round() as i32,
            (PANEL_H as f64 * scale).round() as i32,
        );
        if self.phys != size {
            self.phys = size;
            self.buffer =
                MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        }
        let (ox, oy) = Self::panel_origin(screen);
        set_glass_origin(ox, oy);
        let name = account_name();
        let mut ctx = self.buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            let mut pm =
                tiny_skia::PixmapMut::from_bytes(buf, size.0 as u32, size.1 as u32).unwrap();
            let white = rgba(255, 255, 255, 255);
            let soft = rgba(255, 255, 255, 205);
            let accent = rgba(255, 255, 255, 255);
            let cx = PANEL_W * 0.5;

            // 1. Avatar: frosted disc with luminous ring & account initial
            let r = 34.0;
            fill_frosted(&mut pm, cx - r, 6.0, r * 2.0, r * 2.0, r);
            fill_rounded_rect(
                &mut pm,
                cx - r,
                6.0,
                r * 2.0,
                r * 2.0,
                r,
                rgba(255, 255, 255, 70),
            );
            stroke_rounded_rect(
                &mut pm,
                cx - r + 0.5,
                6.5,
                r * 2.0 - 1.0,
                r * 2.0 - 1.0,
                r - 0.5,
                1.2,
                rgba(255, 255, 255, 130),
            );
            let initial: String = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().collect())
                .unwrap_or_default();
            shadowed(&mut pm, fonts, &initial, cx, 6.0 + r - 17.0, 30.0, white, true);

            // 2. Account name + session subtitle
            shadowed(&mut pm, fonts, &name, cx, 82.0, 16.5, white, true);
            let sub = if self.login_mode {
                tr("Сеанс Пёрышко · Wayland")
            } else {
                tr("Пёрышко DE")
            };
            shadowed(
                &mut pm,
                fonts,
                &sub,
                cx,
                102.0,
                11.0,
                rgba(255, 255, 255, 180),
                false,
            );

            // 3. Password field: frosted pill with submit arrow inside
            let (fx, fy, fw, fh) = FIELD;
            fill_frosted(&mut pm, fx, fy, fw, fh, fh * 0.5);
            fill_rounded_rect(
                &mut pm,
                fx,
                fy,
                fw,
                fh,
                fh * 0.5,
                rgba(255, 255, 255, 64),
            );
            stroke_rounded_rect(
                &mut pm,
                fx + 0.5,
                fy + 0.5,
                fw - 1.0,
                fh - 1.0,
                fh * 0.5 - 0.5,
                1.0,
                rgba(255, 255, 255, 95),
            );
            let text_w = fw - 16.0 - fh;
            if self.input.is_empty() && !self.verifying {
                let ph = tr("Введите пароль");
                fonts.draw_text_clipped(
                    &mut pm,
                    &ph,
                    fx + 19.0,
                    fy + 8.5,
                    text_w,
                    13.0,
                    rgba(255, 255, 255, 155),
                    false,
                );
            }
            self.input
                .draw_masked(&mut pm, fonts, fx + 16.0, fy + 9.0, text_w, 14.0, white, accent);
            let (bx, by, bd) = (fx + fw - fh + 4.0, fy + 4.0, fh - 8.0);
            let ready = !self.input.is_empty() && !self.verifying;
            fill_rounded_rect(
                &mut pm,
                bx,
                by,
                bd,
                bd,
                bd * 0.5,
                rgba(255, 255, 255, if ready { 210 } else { 72 }),
            );
            fonts.draw_text_centered(
                &mut pm,
                "→",
                bx + bd * 0.5,
                by + 3.5,
                14.0,
                if ready {
                    rgba(30, 30, 40, 255)
                } else {
                    rgba(255, 255, 255, 175)
                },
                true,
            );

            // Input source badge to the right of the password pill
            if !self.layout.is_empty() {
                let lx = fx + fw + 10.0;
                stroke_rounded_rect(&mut pm, lx, fy + 8.0, 30.0, 18.0, 5.0, 1.0, soft);
                fonts.draw_text_centered(
                    &mut pm,
                    &self.layout,
                    lx + 15.0,
                    fy + 10.5,
                    10.0,
                    soft,
                    true,
                );
            }

            let raw_msg = if self.verifying {
                tr("Проверка…")
            } else if self.caps {
                tr("Включён Caps Lock")
            } else {
                tr(self.message)
            };
            let w = PANEL_W - 20.0;
            let fitted = fonts.ellipsize(&raw_msg, w, 12.0, false);
            shadowed(
                &mut pm,
                fonts,
                &fitted,
                cx,
                fy + fh + 10.0,
                12.0,
                soft,
                false,
            );

            // 4. Bottom macOS Tahoe Login Action Bar: Сон / Перезагрузить / Выключить
            let actions = [
                (0u8, tr("Сон")),
                (1u8, tr("Перезагрузить")),
                (2u8, tr("Выключить")),
            ];
            let centers = Self::action_buttons_centers();
            for ((idx, label), (acx, acy)) in actions.iter().zip(centers.iter()) {
                let br = 16.0;
                let hovered = self.hover_action == Some(*idx);
                fill_frosted(&mut pm, acx - br, acy - br, br * 2.0, br * 2.0, br);
                fill_rounded_rect(
                    &mut pm,
                    acx - br,
                    acy - br,
                    br * 2.0,
                    br * 2.0,
                    br,
                    rgba(255, 255, 255, if hovered { 105 } else { 58 }),
                );
                stroke_rounded_rect(
                    &mut pm,
                    acx - br + 0.5,
                    acy - br + 0.5,
                    br * 2.0 - 1.0,
                    br * 2.0 - 1.0,
                    br - 0.5,
                    1.0,
                    rgba(255, 255, 255, if hovered { 150 } else { 90 }),
                );
                draw_login_action_icon(&mut pm, *idx, *acx, *acy, white);
                shadowed(&mut pm, fonts, label, *acx, acy + br + 5.0, 11.0, soft, false);
            }

            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
    }

    fn paint_clock(
        &mut self,
        fonts: &mut FontSystem,
        scale: f64,
        screen: (i32, i32),
        date: &str,
        time: &str,
    ) {
        let size = (
            (CLOCK_W as f64 * scale).round() as i32,
            (CLOCK_H as f64 * scale).round() as i32,
        );
        self.clock_buffer =
            MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        let (ox, oy) = Self::clock_origin(screen);
        set_glass_origin(ox, oy);
        let mut ctx = self.clock_buffer.render();
        let _ = ctx.draw(|buf| {
            let mut pm =
                tiny_skia::PixmapMut::from_bytes(buf, size.0 as u32, size.1 as u32).unwrap();
            let cx = CLOCK_W * 0.5;
            shadowed(
                &mut pm,
                fonts,
                date,
                cx,
                6.0,
                22.0,
                rgba(255, 255, 255, 235),
                true,
            );
            shadowed(
                &mut pm,
                fonts,
                time,
                cx,
                34.0,
                96.0,
                rgba(255, 255, 255, 245),
                true,
            );
            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
    }

    fn paint_status(&mut self, fonts: &mut FontSystem, scale: f64, screen: (i32, i32)) {
        let size = (
            (STATUS_W as f64 * scale).round().max(1.0) as i32,
            (STATUS_H as f64 * scale).round().max(1.0) as i32,
        );
        if self.status_phys != size {
            self.status_phys = size;
            self.status_buffer =
                MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        }
        let (ox, oy) = Self::status_origin(screen);
        set_glass_origin(ox, oy);
        let layout_str = if self.layout.is_empty() {
            "RU"
        } else {
            &self.layout
        };
        let hovered = self.hover_action == Some(3);
        let mut ctx = self.status_buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            let mut pm =
                tiny_skia::PixmapMut::from_bytes(buf, size.0 as u32, size.1 as u32).unwrap();
            let r = STATUS_H * 0.5;
            fill_frosted(&mut pm, 0.0, 0.0, STATUS_W, STATUS_H, r);
            fill_rounded_rect(
                &mut pm,
                0.0,
                0.0,
                STATUS_W,
                STATUS_H,
                r,
                rgba(255, 255, 255, if hovered { 78 } else { 52 }),
            );
            stroke_rounded_rect(
                &mut pm,
                0.5,
                0.5,
                STATUS_W - 1.0,
                STATUS_H - 1.0,
                r - 0.5,
                1.0,
                rgba(255, 255, 255, 92),
            );

            // Mini feather badge + session title on the left
            fill_circle(&mut pm, 16.0, r, 6.5, rgba(56, 152, 255, 235));
            stroke_line(
                &mut pm,
                13.0,
                r + 2.5,
                19.0,
                r - 2.5,
                1.5,
                rgba(255, 255, 255, 250),
            );
            fonts.draw_text(
                &mut pm,
                "Пёрышко",
                28.0,
                7.5,
                11.5,
                rgba(255, 255, 255, 240),
                true,
            );

            // Keyboard layout pill
            let lx = 104.0;
            fill_rounded_rect(
                &mut pm,
                lx,
                6.0,
                32.0,
                18.0,
                5.0,
                rgba(255, 255, 255, 45),
            );
            stroke_rounded_rect(
                &mut pm,
                lx + 0.5,
                6.5,
                31.0,
                17.0,
                4.5,
                1.0,
                rgba(255, 255, 255, 160),
            );
            fonts.draw_text_centered(
                &mut pm,
                layout_str,
                lx + 16.0,
                8.5,
                10.0,
                rgba(255, 255, 255, 245),
                true,
            );

            // Wi-Fi waves icon + battery icon on the right
            let white = rgba(255, 255, 255, 235);
            let wx = 154.0;
            let wy = r + 3.5;
            fill_circle(&mut pm, wx, wy, 1.5, white);
            stroke_line(&mut pm, wx - 4.0, wy - 3.5, wx, wy - 5.5, 1.4, white);
            stroke_line(&mut pm, wx, wy - 5.5, wx + 4.0, wy - 3.5, 1.4, white);
            stroke_line(&mut pm, wx - 6.5, wy - 6.5, wx, wy - 9.0, 1.4, white);
            stroke_line(&mut pm, wx, wy - 9.0, wx + 6.5, wy - 6.5, 1.4, white);

            let bx = 174.0;
            let by = 9.5;
            stroke_rounded_rect(&mut pm, bx, by, 22.0, 11.0, 2.5, 1.2, white);
            fill_rounded_rect(&mut pm, bx + 2.2, by + 2.2, 16.0, 6.6, 1.2, white);
            fill_rounded_rect(&mut pm, bx + 23.0, by + 3.2, 2.0, 4.6, 1.0, white);

            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
    }

    pub fn monitor_elements(
        &self,
        renderer: &mut GlesRenderer,
        scale: f64,
        info: &crate::monitors::Info,
        wallpaper: Option<(
            &smithay::backend::renderer::element::texture::TextureBuffer<
                smithay::backend::renderer::gles::GlesTexture,
            >,
            (i32, i32),
        )>,
    ) -> Vec<FeatherElement> {
        if self.external() {
            let mut out = Vec::new();
            if let Some((_, s)) = self
                .extra_surfaces
                .iter()
                .find(|(name, _)| *name == info.name)
            {
                if s.alive() {
                    crate::render::push_surface_tree(
                        &mut out,
                        renderer,
                        s.wl_surface(),
                        (0, 0).into(),
                        scale,
                        Default::default(),
                    );
                }
            }
            return out;
        }
        if info.primary {
            self.elements(renderer, scale, (info.rect.w, info.rect.h), wallpaper)
        } else {
            Vec::new()
        }
    }

    pub fn elements(
        &self,
        renderer: &mut GlesRenderer,
        scale: f64,
        screen: (i32, i32),
        wallpaper: Option<(
            &smithay::backend::renderer::element::texture::TextureBuffer<
                smithay::backend::renderer::gles::GlesTexture,
            >,
            (i32, i32),
        )>,
    ) -> Vec<FeatherElement> {
        if self.external() {
            let mut out = Vec::new();
            if let Some(surface) = self.focus() {
                crate::render::push_surface_tree(
                    &mut out,
                    renderer,
                    &surface,
                    (0, 0).into(),
                    scale,
                    Default::default(),
                );
            }
            return out;
        }
        let at = |(x, y): (f32, f32)| ((x as f64 * scale).round(), (y as f64 * scale).round());
        let mut out: Vec<FeatherElement> = [
            (&self.buffer, Self::panel_origin(screen)),
            (&self.clock_buffer, Self::clock_origin(screen)),
            (&self.status_buffer, Self::status_origin(screen)),
        ]
        .into_iter()
        .filter_map(|(buffer, origin)| {
            MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                at(origin),
                buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .ok()
        })
        .map(|e| SceneElement::Memory(e).into())
        .collect();
        // The desktop wallpaper only: no window content is ever drawn while locked
        if let Some((texture, (w, h))) = wallpaper {
            let full = (
                crate::theme::to_phys(screen.0),
                crate::theme::to_phys(screen.1),
            );
            if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (0.0, 0.0),
                &self.dim,
                Some(0.2),
                Some(Rectangle::from_size((4.0, 4.0).into())),
                Some(full.into()),
                Kind::Unspecified,
            ) {
                out.push(SceneElement::Memory(e).into());
            }
            out.push(
                SceneElement::Texture(
                    smithay::backend::renderer::element::texture::TextureRenderElement::from_texture_buffer(
                        (0.0, 0.0),
                        texture,
                        None,
                        Some(Rectangle::from_size((w as f64, h as f64).into())),
                        Some(full.into()),
                        Kind::Unspecified,
                    ),
                )
                .into(),
            );
        }
        out
    }
}

fn draw_login_action_icon(
    pm: &mut tiny_skia::PixmapMut,
    idx: u8,
    cx: f32,
    cy: f32,
    color: tiny_skia::Color,
) {
    match idx {
        0 => {
            // Sleep (crescent moon / standby arc)
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(cx + 1.5, cy - 6.0);
            pb.cubic_to(cx - 5.5, cy - 5.5, cx - 7.0, cy + 3.5, cx - 1.0, cy + 6.2);
            pb.cubic_to(cx + 3.5, cy + 7.2, cx + 6.5, cy + 4.5, cx + 6.5, cy + 2.0);
            pb.cubic_to(cx + 2.0, cy + 3.5, cx - 2.0, cy - 1.0, cx + 1.5, cy - 6.0);
            pb.close();
            if let Some(path) = pb.finish() {
                let mut p = tiny_skia::Paint::default();
                p.set_color(color);
                p.anti_alias = true;
                pm.fill_path(
                    &path,
                    &p,
                    tiny_skia::FillRule::Winding,
                    crate::theme::scale_transform(),
                    None,
                );
            }
        }
        1 => {
            // Restart (circular arrow)
            feather_ui::draw::stroke_circle(pm, cx, cy, 5.8, 1.5, color);
            stroke_line(pm, cx + 2.5, cy - 6.8, cx + 6.2, cy - 5.2, 1.5, color);
            stroke_line(pm, cx + 6.2, cy - 5.2, cx + 4.2, cy - 1.8, 1.5, color);
        }
        _ => {
            // PowerOff (power circle + top stem)
            feather_ui::draw::stroke_circle(pm, cx, cy + 1.0, 5.6, 1.5, color);
            stroke_line(pm, cx, cy - 6.5, cx, cy + 0.5, 1.8, color);
        }
    }
}

const PANEL_W: f32 = 420.0;
const PANEL_H: f32 = 264.0;
const CLOCK_W: f32 = 640.0;
const CLOCK_H: f32 = 150.0;
const STATUS_W: f32 = 212.0;
const STATUS_H: f32 = 30.0;
/// Password field in panel coordinates: x, y, width, height
const FIELD: (f32, f32, f32, f32) = (90.0, 124.0, 240.0, 34.0);

/// Centred white text with a soft shadow, readable on any wallpaper
#[allow(clippy::too_many_arguments)]
fn shadowed(
    pm: &mut tiny_skia::PixmapMut,
    fonts: &mut FontSystem,
    text: &str,
    cx: f32,
    y: f32,
    size: f32,
    color: tiny_skia::Color,
    bold: bool,
) {
    let spread = (size * 0.02).max(0.6);
    for (dx, dy) in [(-spread, spread), (spread, spread), (0.0, spread * 2.0)] {
        fonts.draw_text_centered(pm, text, cx + dx, y + dy, size, rgba(0, 0, 0, 22), bold);
    }
    fonts.draw_text_centered(pm, text, cx, y, size, color, bold);
}

/// («суббота, 24 октября», «8:36»)
fn clock_text() -> (String, String) {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    feather_ui::i18n::format_lock_clock(
        now.weekday().num_days_from_monday() as usize,
        now.day(),
        now.month0() as usize,
        now.hour(),
        now.minute(),
    )
}

/// Full name from the account (GECOS), else the login; read once
fn account_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        let mut buf = vec![0u8; 16384];
        if libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        ) != 0
            || result.is_null()
        {
            return String::new();
        }
        let text = |p: *const libc::c_char| {
            if p.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        let gecos = text(pwd.pw_gecos);
        let full = gecos.split(',').next().unwrap_or("").trim().to_string();
        if full.is_empty() {
            text(pwd.pw_name)
        } else {
            full
        }
    })
    .clone()
}

impl FeatherCompositor {
    pub fn start_boot_splash(&mut self, paused_progress: Option<f32>) {
        self.lock.splash.start(paused_progress);
        self.needs_redraw = true;
    }

    pub fn open_login_screen(&mut self) {
        self.lock_session();
        self.lock.login_mode = true;
        self.lock.message = "Вход в систему Пёрышко";
        self.lock.dirty = true;
        self.lock.status_dirty = true;
        self.needs_redraw = true;
    }

    pub fn handle_lock_click_action(&mut self, action: LockClick) {
        match action {
            LockClick::None => {}
            LockClick::Submit => self.lock_submit(),
            LockClick::NextLayout => {
                let count = self.keyboard_config.layouts.len().max(1);
                self.select_input_source((self.active_layout_index + 1) % count);
                self.lock.status_dirty = true;
                self.lock.dirty = true;
            }
            LockClick::Sleep => self.sleep_system(),
            LockClick::Restart => self.restart_system(),
            LockClick::PowerOff => self.poweroff_system(),
        }
    }

    pub fn lock_session(&mut self) {
        if self.lock.active {
            return;
        }
        self.lock.active = true;
        self.lock.login_mode = false;
        self.lock.display_protected = false;
        self.lock.protected_frame = false;
        self.lock.generation += 1;
        self.lock.dirty = true;
        self.lock.status_dirty = true;
        self.power.locked_hint(true);
        self.lock.input.clear();
        self.lock.message = "Введите пароль вашей учётной записи";
        self.cancel_scene_input();
        self.shell.close_menus();
        self.shell.switcher = None;
        self.icon_drag = None;
        self.screenshot_select = None;
        self.apply_cursor(smithay::input::pointer::CursorImageStatus::default_named());
        self.refresh_focus();
        self.refresh_pointer_focus();
        tracing::info!("Сессия заблокирована");
    }

    pub(crate) fn cancel_scene_input(&mut self) {
        self.shell.desktop.cancel_interaction();
        let keyboard = self.keyboard.clone();
        keyboard.unset_grab(self);
        self.refresh_focus();
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            None,
            &smithay::input::pointer::MotionEvent {
                location: self.pointer_location,
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: self.start_time.elapsed().as_millis() as u32,
            },
        );
        pointer.unset_grab(
            self,
            smithay::utils::SERIAL_COUNTER.next_serial(),
            self.start_time.elapsed().as_millis() as u32,
        );
        pointer.motion(
            self,
            None,
            &smithay::input::pointer::MotionEvent {
                location: self.pointer_location,
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: self.start_time.elapsed().as_millis() as u32,
            },
        );
        pointer.frame(self);
        self.release_all_input();
    }

    pub(crate) fn unlock_session(&mut self) {
        if self.power.sleeping {
            return;
        }
        self.cancel_scene_input();
        self.lock.active = false;
        self.lock.login_mode = false;
        self.lock.generation += 1;
        self.lock.retired_owner = self.lock.owner.take();
        self.lock.was_external = false;
        self.lock.surface = None;
        self.lock.extra_surfaces.clear();
        self.lock.pending = None;
        self.lock.confirmed = false;
        self.lock.input.clear();
        self.lock.verifying = false;
        self.lock.hover_action = None;
        self.lock.buffer =
            MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None);
        self.lock.status_buffer =
            MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None);
        self.lock.phys = (1, 1);
        self.lock.status_phys = (1, 1);
        self.lock.scale = 0.0;
        self.lock.protected_frame = false;
        self.lock.display_protected = false;
        self.power.locked_hint(false);
        self.activity();
        self.refresh_focus();
        self.refresh_pointer_focus();
        tracing::info!("Сессия разблокирована");
        self.maybe_launch_autostart();
    }

    pub fn lock_submit(&mut self) {
        if !self.lock.active || self.lock.external() || self.lock.verifying || self.power.sleeping {
            return;
        }
        self.lock.verifying = true;
        self.lock.dirty = true;
        self.needs_redraw = true;
        let generation = self.lock.generation;
        let password = self.lock.input.take_text();
        let (tx, rx) = channel::channel();
        if self
            .loop_handle
            .insert_source(rx, move |event, _, st| {
                if let channel::Event::Msg(result) = event {
                    if st.lock.active && st.lock.generation == generation && !st.lock.external() {
                        st.lock.verifying = false;
                        st.lock.dirty = true;
                        st.needs_redraw = true;
                        match result {
                            Ok(true) => st.unlock_session(),
                            Ok(false) => st.lock.message = "Неверный пароль или вход недоступен",
                            Err(message) => st.lock.message = message,
                        }
                    }
                }
            })
            .is_err()
        {
            self.lock.verifying = false;
            self.lock.message = "Не удалось начать проверку";
            return;
        }
        if std::thread::Builder::new()
            .name("feather-unlock".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                let _ = tx.send(crate::lock_auth::authenticate(password));
            })
            .is_err()
        {
            self.lock.verifying = false;
            self.lock.message = "Не удалось начать проверку";
        }
    }

    /// Called only after the protected frame reached the display (DRM vblank/host frame).
    pub fn lock_presented(&mut self) {
        if !self.lock.protected_frame {
            return;
        }
        self.lock.protected_frame = false;
        self.lock.display_protected = true;
        if self.power.sleeping {
            self.power.protected();
        }
        if let Some(confirmation) = self.lock.pending.take() {
            self.lock.confirmed = true;
            confirmation.lock();
        }
    }

    pub fn lock_sleep_barrier(&mut self) {
        self.lock.generation += 1;
        self.lock.verifying = false;
        self.lock.input.clear();
        self.lock.dirty = true;
        self.lock_session();
        if self.power.blanked || self.lock.display_protected {
            self.power.protected();
        }
        self.needs_redraw = true;
    }

    pub fn refresh_lock(&mut self) {
        if self.lock.active && self.lock.was_external && !self.lock.external() {
            self.lock.was_external = false;
            self.lock.dirty = true;
            self.lock.surface = None;
            self.lock.extra_surfaces.clear();
            self.refresh_focus();
            self.refresh_pointer_focus();
            tracing::warn!("Приложение блокировки завершилось; сессия остаётся заблокированной");
        }
    }

    pub fn lock_surface_commit(&mut self, surface: &WlSurface) {
        if self.lock.active
            && self
                .lock
                .extra_surfaces
                .iter()
                .any(|(_, s)| s.wl_surface() == surface)
        {
            self.refresh_focus();
        }
    }

    pub fn lock_focus(&self) -> Option<WlSurface> {
        if !self.lock.external() {
            return None;
        }
        let r = self.monitor_at(self.pointer_location.x, self.pointer_location.y);
        let m = self.monitors.iter().find(|m| m.info.rect == r)?;
        let (_, s) = self
            .lock
            .extra_surfaces
            .iter()
            .find(|(name, _)| *name == m.info.name)?;
        s.alive()
            .then(|| s.wl_surface().clone())
            .filter(|s| {
                smithay::backend::renderer::utils::with_renderer_surface_state(s, |r| {
                    r.buffer().is_some()
                })
                .unwrap_or(false)
            })
    }

    pub fn lock_resized(&mut self) {
        self.lock.surface = None;
        for (name, surface) in &self.lock.extra_surfaces {
            if let Some(m) = self.monitors.iter().find(|m| m.info.name == *name) {
                surface.with_pending_state(|s| {
                    s.size = Some((m.info.rect.w as u32, m.info.rect.h as u32).into())
                });
                surface.send_configure();
                if m.info.primary {
                    self.lock.surface = Some(surface.clone());
                }
            }
        }
    }
}

impl SessionLockHandler for FeatherCompositor {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }
    fn lock(&mut self, confirmation: SessionLocker) {
        if self.lock.active {
            return;
        }
        self.lock_session();
        self.lock.owner = Some(confirmation.ext_session_lock().clone());
        self.lock.was_external = true;
        self.lock.pending = Some(confirmation);
    }
    fn unlock(&mut self) {
        self.unlock_session();
        self.lock.retired_owner = None;
    }
    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(name) = self
            .monitors
            .iter()
            .find(|m| m.output.owns(&output))
            .map(|m| m.info.name.clone())
        else {
            return;
        };
        self.lock.extra_surfaces.retain(|(n, _)| *n != name);
        self.lock.extra_surfaces.push((name, surface));
        self.lock_resized();
    }
}

// Smithay 0.7 calls unlock() even after posting InvalidUnlock. Gate requests before
// delegation so a rejected/second lock object cannot unlock the accepted owner's lock.
impl Dispatch<ExtSessionLockV1, SessionLockState> for FeatherCompositor {
    fn request(
        st: &mut Self,
        client: &Client,
        resource: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        data: &SessionLockState,
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if !matches!(request, ext_session_lock_v1::Request::Destroy)
            && (st.lock.owner.as_ref() != Some(resource)
                || (matches!(request, ext_session_lock_v1::Request::UnlockAndDestroy)
                    && !st.lock.confirmed))
        {
            resource.post_error(
                ext_session_lock_v1::Error::InvalidUnlock,
                "Not the confirmed lock owner",
            );
            return;
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
            st, client, resource, request, data, dh, init,
        );
    }
}

smithay::reexports::wayland_server::delegate_global_dispatch!(FeatherCompositor:[ExtSessionLockManagerV1:SessionLockManagerGlobalData]=>SessionLockManagerState);

impl Dispatch<ExtSessionLockManagerV1, ()> for FeatherCompositor {
    fn request(
        st: &mut Self,
        client: &Client,
        resource: &ExtSessionLockManagerV1,
        request: smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::Request,
        data: &(),
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::Request;
        if matches!(request, Request::Lock { .. }) && !st.lock.active {
            if let Some(owner) = st.lock.retired_owner.take() {
                if let Some(old) = owner.data::<SessionLockState>() {
                    <SessionLockManagerState as Dispatch<
                        ExtSessionLockV1,
                        SessionLockState,
                        Self,
                    >>::request(
                        st,
                        client,
                        &owner,
                        ext_session_lock_v1::Request::UnlockAndDestroy,
                        old,
                        dh,
                        init,
                    );
                }
            }
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockManagerV1, (), Self>>::request(
            st, client, resource, request, data, dh, init,
        );
    }
}

smithay::reexports::wayland_server::delegate_dispatch!(FeatherCompositor:[ExtSessionLockSurfaceV1:ExtLockSurfaceUserData]=>SessionLockManagerState);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_splash_and_login_screen_paint_and_handle_actions() {
        let mut fonts = FontSystem::new();
        let mut lock = Lock::default();

        // 1. BootSplash paints once and advances progress & alpha cleanly
        lock.splash.start(Some(0.68));
        assert!(lock.splash.active);
        assert!((lock.splash.progress(Instant::now()) - 0.68).abs() < 1e-5);
        lock.splash.paint(&mut fonts, 2.0);
        assert!(!lock.splash.dirty);

        // Live splash finishes after SPLASH_DURATION
        lock.splash.start(None);
        let future = lock.splash.started_at + SPLASH_DURATION + Duration::from_millis(50);
        assert!(!lock.splash.advance(future));
        assert!(!lock.splash.active);

        // 2. Login screen paints clock, status bar, account panel, and handles button clicks
        lock.active = true;
        lock.login_mode = true;
        lock.message = "Вход в систему Пёрышко";
        let screen = (1440, 900);
        lock.paint(&mut fonts, 1.0, "RU", screen);

        let (sx, sy) = Lock::status_origin(screen);
        assert_eq!(
            lock.click_action(&mut fonts, (sx + 120.0) as f64, (sy + 15.0) as f64, screen, false),
            LockClick::NextLayout
        );

        let (ox, oy) = Lock::panel_origin(screen);
        let centers = Lock::action_buttons_centers();
        assert_eq!(
            lock.click_action(
                &mut fonts,
                (ox + centers[0].0) as f64,
                (oy + centers[0].1) as f64,
                screen,
                false
            ),
            LockClick::Sleep
        );
        assert_eq!(
            lock.click_action(
                &mut fonts,
                (ox + centers[1].0) as f64,
                (oy + centers[1].1) as f64,
                screen,
                false
            ),
            LockClick::Restart
        );
        assert_eq!(
            lock.click_action(
                &mut fonts,
                (ox + centers[2].0) as f64,
                (oy + centers[2].1) as f64,
                screen,
                false
            ),
            LockClick::PowerOff
        );
    }
}
