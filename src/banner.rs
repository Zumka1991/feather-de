//! A short message in the middle of the screen: painted once, gone after one timer, no work in
//! between. Says what a toggle without a window of its own has just done.
use crate::{
    render::{FeatherElement, SceneElement},
    theme::*,
    FeatherCompositor,
};
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
    reexports::calloop::{
        timer::{TimeoutAction, Timer},
        RegistrationToken,
    },
    utils::{Rectangle, Transform},
};
use std::time::Duration;

const W: i32 = 420;
const H: i32 = 84;
const M: i32 = 14;
const LIFETIME: Duration = Duration::from_millis(2000);
const SPACE_LIFETIME: Duration = Duration::from_millis(1200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerIcon {
    Caffeine(bool),
    Scratch,
    SpaceMode(crate::space_layout::SpaceMode),
    DropShelf,
    Pip(bool),
    Space { current: u8, count: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub icon: BannerIcon,
    pub title: &'static str,
    pub detail: &'static str,
}

#[derive(Default)]
pub struct Banner {
    pub message: Option<Message>,
    timer: Option<RegistrationToken>,
    buffer: Option<MemoryRenderBuffer>,
    painted: Option<(Message, Appearance, u64, bool, bool)>,
}

impl Banner {
    pub fn glass_spec(&self, screen: (i32, i32)) -> Option<crate::glass::GlassSpec> {
        self.message?;
        let (x, y) = ((screen.0 - W - 2 * M) / 2 + M, (screen.1 - H - 2 * M) / 2 + M);
        Some(crate::glass::GlassSpec {
            key: crate::glass::KEY_BANNER,
            rect: crate::shell::R::new(x as f32, y as f32, W as f32, H as f32),
            radius: 22.0,
            strength: 0.95,
            visual: crate::animation::Visual::default(),
        })
    }

    pub fn paint(&mut self, fonts: &mut crate::text::FontSystem, scale: f64, screen: (i32, i32)) {
        let Some(message) = self.message else { return };
        let key = (message, appearance(), scale.to_bits(), reduce_transparency(), crate::glass::live_glass_active());
        if self.painted == Some(key) {
            return;
        }
        let (ox, oy) = ((screen.0 - W - 2 * M) / 2, (screen.1 - H - 2 * M) / 2);
        set_glass_origin(ox as f32, oy as f32);
        let size = (((W + 2 * M) as f64 * scale).round() as i32, ((H + 2 * M) as f64 * scale).round() as i32);
        let buffer = self.buffer.get_or_insert_with(|| MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None));
        let mut ctx = buffer.render();
        ctx.resize(size);
        let _ = ctx.draw(|bytes| {
            bytes.fill(0);
            let mut pm = tiny_skia::PixmapMut::from_bytes(bytes, size.0 as u32, size.1 as u32).unwrap();
            let p = palette();
            let m = M as f32;
            for i in (1..=7).rev() {
                let e = i as f32 * 1.6;
                fill_rounded_rect(&mut pm, m - e, m - e + 4.0, W as f32 + 2.0 * e, H as f32 + 2.0 * e, 22.0 + e, rgba(0, 0, 0, 5));
            }
            draw_glass_panel(&mut pm, m, m, W as f32, H as f32, 22.0, true);
            let (cx, cy) = (m + 44.0, m + H as f32 * 0.5);
            match message.icon {
                BannerIcon::Caffeine(on) => draw_caffeine_icon(&mut pm, cx, cy + 1.0, 2.3, if on { p.accent } else { p.text_secondary }, on),
                BannerIcon::Scratch => draw_scratch_icon(&mut pm, cx, cy, 2.3, p.accent),
                BannerIcon::SpaceMode(mode) => draw_space_mode_icon(&mut pm, cx, cy, 2.3, p.accent, mode),
                BannerIcon::DropShelf => draw_drop_shelf_icon(&mut pm, cx, cy, 2.2, p.accent),
                BannerIcon::Pip(ghost) => draw_pip_icon(&mut pm, cx, cy, 2.25, p.accent, ghost),
                BannerIcon::Space { current, count } => {
                    let (bw, bh) = (38.0, 26.0);
                    fill_rounded_rect(&mut pm, cx - bw * 0.5, cy - 18.0, bw, bh, 7.5, with_alpha(p.accent, 0.16));
                    stroke_rounded_rect(&mut pm, cx - bw * 0.5, cy - 18.0, bw, bh, 7.5, 1.5, p.accent);
                    fonts.draw_text_centered(&mut pm, &(current as u32 + 1).to_string(), cx, cy - 13.0, 14.5, p.accent, true);
                    let dots = (count as usize).clamp(1, 9);
                    let active_dot = (current as usize).min(dots - 1);
                    let total_w: f32 = (0..dots).map(|i| if i == active_dot { 7.5 } else { 4.0 }).sum::<f32>()
                        + (dots.saturating_sub(1) as f32) * 3.0;
                    let mut dx = cx - total_w * 0.5;
                    for i in 0..dots {
                        let w_dot = if i == active_dot { 7.5 } else { 4.0 };
                        let col = if i == active_dot { p.accent } else { with_alpha(p.text_secondary, 0.42) };
                        fill_rounded_rect(&mut pm, dx, cy + 13.5, w_dot, 4.0, 2.0, col);
                        dx += w_dot + 3.0;
                    }
                }
            }
            if let BannerIcon::Space { current, count } = message.icon {
                let space_title = format!("Рабочий стол {}", current as u32 + 1);
                fonts.draw_text(&mut pm, &space_title, m + 82.0, m + 21.0, 15.0, p.text, true);
                let counter = format!("{} / {}", current as u32 + 1, count.max(1));
                fonts.draw_text_right(&mut pm, &counter, m + W as f32 - 20.0, m + 22.5, 12.0, p.text_tertiary, true);
                fonts.draw_text(&mut pm, message.detail, m + 82.0, m + 46.0, 12.5, p.text_secondary, false);
            } else {
                fonts.draw_text(&mut pm, message.title, m + 80.0, m + 21.0, 15.0, p.text, true);
                fonts.draw_text(&mut pm, message.detail, m + 80.0, m + 46.0, 12.5, p.text_secondary, false);
            }
            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
        self.painted = Some(key);
    }

    pub fn elements(
        &self,
        renderer: &mut GlesRenderer,
        scale: f64,
        screen: (i32, i32),
        mut glass: Option<(&mut crate::glass::GlassState, &str)>,
    ) -> Vec<FeatherElement> {
        if self.message.is_none() {
            return Vec::new();
        }
        let Some(buffer) = self.buffer.as_ref() else { return Vec::new() };
        let (x, y) = ((screen.0 - W - 2 * M) / 2, (screen.1 - H - 2 * M) / 2);
        let mut out = Vec::with_capacity(2);
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(renderer, ((x as f64 * scale).round(), (y as f64 * scale).round()), buffer, None, None, None, Kind::Unspecified) {
            out.push(SceneElement::Memory(e).into());
        }
        if let (Some((g, head)), Some(spec)) = (glass.as_mut(), self.glass_spec(screen)) {
            if let Some(ge) = g.element(head, &spec, scale) {
                out.push(SceneElement::Glass(ge).into());
            }
        }
        out
    }
}

impl FeatherCompositor {
    pub fn show_banner(&mut self, message: Message) {
        if self.lock.active {
            return;
        }
        if let Some(token) = self.banner.timer.take() {
            self.loop_handle.remove(token);
        }
        self.banner.message = Some(message);
        self.needs_redraw = true;
        let ttl = if matches!(message.icon, BannerIcon::Space { .. }) {
            SPACE_LIFETIME
        } else {
            LIFETIME
        };
        if let Ok(token) = self.loop_handle.insert_source(Timer::from_duration(ttl), |_, _, st| {
            st.banner = Banner::default();
            st.needs_redraw = true;
            TimeoutAction::Drop
        }) {
            self.banner.timer = Some(token);
        }
    }

    pub fn show_space_banner(&mut self, carried: bool) {
        if self.larvis_shown || self.mission_open() {
            return;
        }
        let current = self.spaces.current.min(u8::MAX as u32) as u8;
        let count = self.spaces.count.max(1).min(u8::MAX as u32) as u8;
        let detail = if carried {
            "Окно перенесено на этот стол"
        } else {
            self.space_layouts.mode(self.spaces.current).label()
        };
        self.show_banner(Message {
            icon: BannerIcon::Space { current, count },
            title: "",
            detail,
        });
    }

    pub fn clear_space_banner(&mut self) {
        if matches!(self.banner.message.map(|m| m.icon), Some(BannerIcon::Space { .. })) {
            if let Some(token) = self.banner.timer.take() {
                self.loop_handle.remove(token);
            }
            self.banner = Banner::default();
            self.needs_redraw = true;
        }
    }

    /// The cup in the menu bar was pressed
    pub fn toggle_caffeine(&mut self) {
        let on = !self.shell.caffeine;
        self.set_caffeine(on);
        self.show_banner(if on {
            Message { icon: BannerIcon::Caffeine(true), title: "Режим повышенной бодрости включён", detail: "Компьютер не уснёт, экран не погаснет" }
        } else {
            Message { icon: BannerIcon::Caffeine(false), title: "Режим повышенной бодрости выключен", detail: "Сон и гашение экрана снова по расписанию" }
        });
    }
}
