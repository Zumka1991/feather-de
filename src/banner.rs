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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerIcon {
    Caffeine(bool),
    Scratch,
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
    painted: Option<(Message, Appearance, u64)>,
}

impl Banner {
    pub fn paint(&mut self, fonts: &mut crate::text::FontSystem, scale: f64) {
        let Some(message) = self.message else { return };
        let key = (message, appearance(), scale.to_bits());
        if self.painted == Some(key) {
            return;
        }
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
            fill_rounded_rect(&mut pm, m, m, W as f32, H as f32, 22.0, p.window_bg);
            stroke_rounded_rect(&mut pm, m + 0.5, m + 0.5, W as f32 - 1.0, H as f32 - 1.0, 21.5, 0.5, p.window_outline);
            let (cx, cy) = (m + 44.0, m + H as f32 * 0.5);
            match message.icon {
                BannerIcon::Caffeine(on) => draw_caffeine_icon(&mut pm, cx, cy + 1.0, 2.3, if on { p.accent } else { p.text_secondary }, on),
                BannerIcon::Scratch => draw_scratch_icon(&mut pm, cx, cy, 2.3, p.accent),
            }
            fonts.draw_text(&mut pm, message.title, m + 80.0, m + 21.0, 15.0, p.text, true);
            fonts.draw_text(&mut pm, message.detail, m + 80.0, m + 46.0, 12.5, p.text_secondary, false);
            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
        self.painted = Some(key);
    }

    pub fn elements(&self, renderer: &mut GlesRenderer, scale: f64, screen: (i32, i32)) -> Vec<FeatherElement> {
        if self.message.is_none() {
            return Vec::new();
        }
        let Some(buffer) = self.buffer.as_ref() else { return Vec::new() };
        let (x, y) = ((screen.0 - W - 2 * M) / 2, (screen.1 - H - 2 * M) / 2);
        MemoryRenderBufferRenderElement::from_buffer(renderer, ((x as f64 * scale).round(), (y as f64 * scale).round()), buffer, None, None, None, Kind::Unspecified)
            .ok()
            .map(|e| vec![SceneElement::Memory(e).into()])
            .unwrap_or_default()
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
        if let Ok(token) = self.loop_handle.insert_source(Timer::from_duration(LIFETIME), |_, _, st| {
            st.banner = Banner::default();
            st.needs_redraw = true;
            TimeoutAction::Drop
        }) {
            self.banner.timer = Some(token);
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
