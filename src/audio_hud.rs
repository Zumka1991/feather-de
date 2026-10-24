//! Small volume feedback, painted only on a changed value/theme/scale. One expiry timer,
//! no frame-rate animation and no polling while hidden.
use crate::{
    controls::{Audio, AudioKey, Control},
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

const W: i32 = 260;
const H: i32 = 68;
const M: i32 = 12;
const LIFETIME: Duration = Duration::from_millis(1400);
#[derive(Default)]
pub struct Hud {
    pub visible: bool,
    pub percent: u8,
    pub muted: bool,
    pub available: bool,
    timer: Option<RegistrationToken>,
    buffer: Option<MemoryRenderBuffer>,
    painted: Option<(u8, bool, bool, Appearance, u64)>,
}
impl Hud {
    fn origin(screen: (i32, i32)) -> (i32, i32) {
        (
            (screen.0 - W - 2 * M) / 2,
            (screen.1 - dock_reserved() - H - 2 * M - 18).max(TOP_BAR_HEIGHT),
        )
    }
    pub fn paint(&mut self, fonts: &mut crate::text::FontSystem, scale: f64) {
        if !self.visible {
            return;
        }
        let key = (
            self.percent,
            self.muted,
            self.available,
            appearance(),
            scale.to_bits(),
        );
        if self.painted == Some(key) {
            return;
        }
        let size = (
            (((W + 2 * M) as f64 * scale).round() as i32),
            (((H + 2 * M) as f64 * scale).round() as i32),
        );
        let buffer = self.buffer.get_or_insert_with(|| {
            MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None)
        });
        let mut ctx = buffer.render();
        ctx.resize(size);
        let _ = ctx.draw(|bytes| {
            bytes.fill(0);
            let mut pm =
                tiny_skia::PixmapMut::from_bytes(bytes, size.0 as u32, size.1 as u32).unwrap();
            let p = palette();
            let m = M as f32;
            for i in (1..=6).rev() {
                let e = i as f32 * 1.5;
                fill_rounded_rect(
                    &mut pm,
                    m - e,
                    m - e + 3.0,
                    W as f32 + 2.0 * e,
                    H as f32 + 2.0 * e,
                    20.0 + e,
                    rgba(0, 0, 0, 5),
                );
            }
            fill_rounded_rect(&mut pm, m, m, W as f32, H as f32, 20.0, p.window_bg);
            stroke_rounded_rect(
                &mut pm,
                m + 0.5,
                m + 0.5,
                W as f32 - 1.0,
                H as f32 - 1.0,
                19.5,
                0.5,
                p.window_outline,
            );
            crate::control_panel::icon(
                &mut pm,
                crate::control_panel::Icon::Speaker,
                m + 29.0,
                m + 34.0,
                p.text,
                self.muted || !self.available,
            );
            let title = if !self.available {
                "Звук недоступен"
            } else if self.muted {
                "Без звука"
            } else {
                "Громкость"
            };
            fonts.draw_text(&mut pm, title, m + 54.0, m + 14.0, 13.0, p.text, true);
            if self.available {
                fonts.draw_text_right(
                    &mut pm,
                    &format!("{}%", self.percent),
                    m + W as f32 - 18.0,
                    m + 14.0,
                    12.0,
                    p.text_secondary,
                    false,
                );
            }
            fill_rounded_rect(&mut pm, m + 54.0, m + 43.0, 188.0, 5.0, 2.5, p.control_fill);
            let value = if self.muted || !self.available {
                0.0
            } else {
                self.percent as f32 / 100.0
            };
            if value > 0.0 {
                fill_rounded_rect(
                    &mut pm,
                    m + 54.0,
                    m + 43.0,
                    188.0 * value,
                    5.0,
                    2.5,
                    p.accent,
                );
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
    ) -> Vec<FeatherElement> {
        if !self.visible {
            return Vec::new();
        }
        let Some(buffer) = self.buffer.as_ref() else {
            return Vec::new();
        };
        let (x, y) = Self::origin(screen);
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            ((x as f64 * scale).round(), (y as f64 * scale).round()),
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
        .map(|e| vec![SceneElement::Memory(e).into()])
        .unwrap_or_default()
    }
}
impl FeatherCompositor {
    pub fn handle_audio_key(&mut self, key: AudioKey) {
        if self.lock.active {
            return;
        }
        if let Err(error) = self
            .controls
            .as_ref()
            .ok_or("Служба звука недоступна")
            .and_then(|c| c.send(Control::AudioKey(key)))
        {
            self.shell.cc.error = Some(error.into());
            self.show_audio_feedback(&Audio::default());
        }
    }
    pub fn show_audio_feedback(&mut self, audio: &Audio) {
        if self.lock.active {
            return;
        }
        if let Some(token) = self.audio_hud.timer.take() {
            self.loop_handle.remove(token);
        }
        let sink = audio.current();
        if sink.is_some_and(|s| !s.muted && s.volume()>0.0) && audio.error.is_none() {
            self.play_effect(crate::sounds::Effect::Volume,false);
        }
        self.audio_hud.visible = true;
        self.audio_hud.available = sink.is_some();
        self.audio_hud.percent =
            sink.map_or(0, |s| (s.volume().clamp(0.0, 1.0) * 100.0).round() as u8);
        self.audio_hud.muted = sink.is_some_and(|s| s.muted);
        self.needs_redraw = true;
        if let Ok(token) =
            self.loop_handle
                .insert_source(Timer::from_duration(LIFETIME), |_, _, st| {
                    st.audio_hud.visible = false;
                    st.audio_hud.buffer = None;
                    st.audio_hud.painted = None;
                    st.audio_hud.timer = None;
                    st.needs_redraw = true;
                    TimeoutAction::Drop
                })
        {
            self.audio_hud.timer = Some(token);
        }
    }
}
