//! Control Center view model: service snapshots, detail pages and local slider preview.
use crate::{
    controls::{
        bluetooth::Bluetooth,
        network::{Network, Security, Wifi},
        Audio, Control,
    },
    shell::{ShellClickAction, R},
    text::FontSystem,
    theme::*,
};
use tiny_skia::{Color, PixmapMut};
use zeroize::Zeroize;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Main,
    Wifi,
    Bluetooth,
    Sound,
    Password,
}
#[derive(Default)]
pub struct Panel {
    pub wifi: Wifi,
    pub bluetooth: Bluetooth,
    pub audio: Audio,
    pub page: Page,
    pub scroll: usize,
    pub stream_scroll: usize,
    pub error: Option<String>,
    password: feather_ui::input::TextInput,
    selected: Option<Network>,
    drag: Option<(Slider, R, f32)>,
    /// Pointer (global logical) while it is over the panel
    pub hover: Option<(f32, f32)>,
    /// Display brightness 0…1; None when no display can be controlled
    pub brightness: Option<f32>,
    /// The «Сейчас исполняется» card: the current MPRIS player
    pub media: Option<crate::mpris::Player>,
    /// Opened from the menu bar speaker: just the Sound page, no way back to the tiles
    pub sound_only: bool,
    /// Volume set by the last drag, shown until the sound server reports it (no jump back)
    pending_volume: Option<(String, f32, u8)>,
    /// Per-app stream volume set by the last drag
    pending_stream: Option<(u32, f32, u8)>,
    /// Last volume sent while dragging (sent again only after a 2 % change)
    sent_volume: Option<f32>,
    /// Output rows that fit the Sound page as last painted: the full Control Center and the
    /// speaker's own panel have different heights
    sound_fit: std::cell::Cell<usize>,
    stream_fit: std::cell::Cell<usize>,
    streams_top: std::cell::Cell<f32>,
}

/// Row pitch of the output list on the Sound page
const OUTPUT_ROW: f32 = 34.0;
/// Most outputs the speaker's own panel grows to before it scrolls
const SPEAKER_ROWS: usize = 6;
/// Header height before per-app playback streams («Программы»)
const STREAM_HEADER: f32 = 28.0;
/// Row pitch of each active playback stream in the Sound mixer
const STREAM_ROW: f32 = 50.0;

#[derive(Clone, Debug, PartialEq)]
enum Slider {
    Volume(String),
    StreamVolume(u32),
    Brightness,
}
impl Panel {
    pub fn close(&mut self) {
        self.page = Page::Main;
        self.scroll = 0;
        self.stream_scroll = 0;
        self.password.clear();
        self.selected = None;
        self.drag = None;
        self.error = None;
        self.sound_only = false;
    }
    /// Height of the speaker's panel: the slider, outputs, active app streams and an error line
    pub fn sound_height(&self) -> f32 {
        if self.audio.streams.is_empty() {
            let rows = self.audio.sinks.len().clamp(1, SPEAKER_ROWS) as f32;
            151.0 + rows * OUTPUT_ROW + self.bottom_gap()
        } else {
            let out_rows = self.audio.sinks.len().clamp(1, 3) as f32;
            let stream_rows = self.audio.streams.len().min(3) as f32;
            151.0 + out_rows * OUTPUT_ROW + STREAM_HEADER + stream_rows * STREAM_ROW + self.bottom_gap()
        }
    }
    /// Output rows that fit a Sound page of this height
    fn output_rows(&self, h: f32) -> usize {
        (((h - self.bottom_gap() - 151.0 + 2.0) / OUTPUT_ROW).floor() as usize).max(1)
    }
    /// `(output_rows, stream_rows)` that fit a Sound page of height `h`
    fn sound_sections(&self, h: f32) -> (usize, usize) {
        if self.audio.streams.is_empty() {
            return (self.output_rows(h), 0);
        }
        let avail = (h - self.bottom_gap() - 151.0 + 2.0).max(0.0);
        let max_sinks = self.audio.sinks.len().max(1);
        let cap_sinks = if self.sound_only { 3 } else { 2 };
        let mut out_rows = max_sinks.min(cap_sinks);
        while out_rows > 1 && avail - out_rows as f32 * OUTPUT_ROW < STREAM_HEADER + STREAM_ROW {
            out_rows -= 1;
        }
        let rem = avail - out_rows as f32 * OUTPUT_ROW - STREAM_HEADER;
        if rem < STREAM_ROW * 0.8 {
            return (self.output_rows(h), 0);
        }
        let stream_rows = ((rem / STREAM_ROW).floor() as usize)
            .max(1)
            .min(self.audio.streams.len());
        let leftover =
            avail - out_rows as f32 * OUTPUT_ROW - STREAM_HEADER - stream_rows as f32 * STREAM_ROW;
        let extra_out = (leftover / OUTPUT_ROW).floor().max(0.0) as usize;
        out_rows = (out_rows + extra_out).min(max_sinks);
        (out_rows, stream_rows)
    }
    /// Rows visible now (before the first paint: as many as the speaker's panel shows)
    fn visible_outputs(&self) -> usize {
        match self.sound_fit.get() {
            0 => SPEAKER_ROWS,
            n => n,
        }
    }
    fn visible_streams(&self) -> usize {
        match self.stream_fit.get() {
            0 => 3,
            n => n,
        }
    }
    /// Room under the lists: the footer button and error line, or only the error line in the
    /// speaker's panel
    fn bottom_gap(&self) -> f32 {
        if !self.sound_only {
            90.0
        } else if self.error.is_some() || self.audio.error.is_some() {
            40.0
        } else {
            14.0
        }
    }
    pub fn open(&mut self, page: Page) {
        self.password.clear();
        self.selected = None;
        self.page = page;
        self.scroll = 0;
        self.stream_scroll = 0;
        self.error = None;
    }
    pub fn volume(&self) -> f32 {
        let pending = self.pending_volume.as_ref().filter(|(sink, _, _)| self.audio.current().is_some_and(|s| &s.name == sink)).map(|p| p.1);
        self.drag.as_ref().filter(|d| matches!(d.0, Slider::Volume(_))).map(|d| d.2).or(pending).unwrap_or_else(|| {
            self.audio
                .current()
                .map_or(0.0, |s| s.volume().clamp(0.0, 1.0))
        })
    }
    pub fn stream_volume(&self, stream: &crate::controls::AudioStream) -> f32 {
        let pending = self
            .pending_stream
            .as_ref()
            .filter(|(idx, _, _)| *idx == stream.index)
            .map(|p| p.1);
        self.drag
            .as_ref()
            .filter(|d| d.0 == Slider::StreamVolume(stream.index))
            .map(|d| d.2)
            .or(pending)
            .unwrap_or_else(|| stream.volume().clamp(0.0, 1.0))
    }
    fn next_stream_sink(&self, stream: &crate::controls::AudioStream) -> Option<String> {
        if self.audio.sinks.len() < 2 {
            return None;
        }
        let pos = self
            .audio
            .sinks
            .iter()
            .position(|s| (!stream.sink_name.is_empty() && s.name == stream.sink_name) || s.index == stream.sink_index)
            .unwrap_or(0);
        Some(self.audio.sinks[(pos + 1) % self.audio.sinks.len()].name.clone())
    }
    fn stream_sink_label<'a>(&'a self, stream: &'a crate::controls::AudioStream) -> &'a str {
        self.audio
            .sinks
            .iter()
            .find(|s| (!stream.sink_name.is_empty() && s.name == stream.sink_name) || s.index == stream.sink_index)
            .or_else(|| self.audio.current())
            .map_or("Вывод", |s| s.label.as_str())
    }
    pub fn start_slider(&mut self, rect: R, x: f32) {
        if let Some(sink) = self.audio.current() {
            self.drag = Some((
                Slider::Volume(sink.name.clone()),
                rect,
                ((x - rect.x) / rect.w).clamp(0.0, 1.0),
            ));
        }
    }
    pub fn start_stream_slider(&mut self, index: u32, rect: R, x: f32) {
        self.drag = Some((
            Slider::StreamVolume(index),
            rect,
            ((x - rect.x) / rect.w).clamp(0.0, 1.0),
        ));
    }

    /// Brightness shown: the dragged value while a drag is on
    pub fn shown_brightness(&self) -> Option<f32> {
        self.drag.as_ref().filter(|d| d.0 == Slider::Brightness).map(|d| d.2).or(self.brightness)
    }

    /// Starts a brightness drag; the value applies live (the worker keeps only the latest)
    pub fn start_brightness(&mut self, rect: R, x: f32) -> Option<f32> {
        self.brightness?;
        let value = ((x - rect.x) / rect.w).clamp(0.0, 1.0);
        self.drag = Some((Slider::Brightness, rect, value));
        Some(value)
    }

    /// While dragging the volume: a command to send now, at most every 2 %
    pub fn live_volume(&mut self) -> Option<ShellClickAction> {
        let (target, _, value) = self.drag.as_ref()?;
        match target {
            Slider::Volume(sink) => {
                if self.sent_volume.is_some_and(|v| (v - value).abs() < 0.02) {
                    return None;
                }
                self.sent_volume = Some(*value);
                Some(ShellClickAction::Control(Control::Volume {
                    sink: sink.clone(),
                    value: *value,
                }))
            }
            Slider::StreamVolume(index) => {
                if self.sent_volume.is_some_and(|v| (v - value).abs() < 0.02) {
                    return None;
                }
                self.sent_volume = Some(*value);
                Some(ShellClickAction::Control(Control::StreamVolume {
                    index: *index,
                    value: *value,
                }))
            }
            Slider::Brightness => None,
        }
    }

    /// A hardware key supersedes the slider's local preview.
    pub fn keyboard_audio_updated(&mut self, audio: Audio) {
        self.pending_volume = None;
        self.sent_volume = None;
        if matches!(self.drag, Some((Slider::Volume(_), _, _))) { self.drag = None; }
        self.audio = audio;
    }

    /// A sound server snapshot: the pending value goes once it is confirmed (or after a
    /// few snapshots that disagree, so a clamped volume cannot stick).
    pub fn audio_updated(&mut self) {
        if let Some((sink, value, seen)) = self.pending_volume.as_mut() {
            let actual = self.audio.sinks.iter().find(|s| &s.name == sink).map(|s| s.volume());
            *seen += 1;
            if actual.is_none_or(|a| (a - *value).abs() < 0.02) || *seen >= 4 {
                self.pending_volume = None;
            }
        }
        if let Some((idx, value, seen)) = self.pending_stream.as_mut() {
            let actual = self.audio.streams.iter().find(|s| s.index == *idx).map(|s| s.volume());
            *seen += 1;
            if actual.is_none_or(|a| (a - *value).abs() < 0.02) || *seen >= 4 {
                self.pending_stream = None;
            }
        }
        if matches!(self.drag, Some((Slider::StreamVolume(idx), _, _)) if !self.audio.streams.iter().any(|s| s.index == idx)) {
            self.drag = None;
            self.sent_volume = None;
        }
        let max_s = self.audio.streams.len().saturating_sub(self.visible_streams());
        if self.stream_scroll > max_s {
            self.stream_scroll = max_s;
        }
    }

    pub fn brightness_drag(&self) -> Option<f32> {
        self.drag.as_ref().filter(|d| d.0 == Slider::Brightness).map(|d| d.2)
    }
    pub fn motion(&mut self, x: f32) -> bool {
        let Some((_, r, value)) = &mut self.drag else {
            return false;
        };
        *value = ((x - r.x) / r.w).clamp(0.0, 1.0);
        true
    }
    pub fn release(&mut self) -> Option<ShellClickAction> {
        self.sent_volume = None;
        self.drag.take().map(|(target, _, value)| match target {
            Slider::Volume(sink) => {
                self.pending_volume = Some((sink.clone(), value, 0));
                ShellClickAction::Control(Control::Volume { sink, value })
            }
            Slider::StreamVolume(index) => {
                self.pending_stream = Some((index, value, 0));
                ShellClickAction::Control(Control::StreamVolume { index, value })
            }
            Slider::Brightness => ShellClickAction::SetBrightness(value),
        })
    }
    pub fn scroll(&mut self, delta: f64) {
        if self.page == Page::Sound && !self.audio.streams.is_empty() {
            let top = self.streams_top.get();
            let over_streams = top > 0.0 && self.hover.is_some_and(|(_, gy)| gy >= top);
            let out_max = self.audio.sinks.len().saturating_sub(self.visible_outputs());
            let stream_max = self.audio.streams.len().saturating_sub(self.visible_streams());
            if over_streams || (out_max == 0 && stream_max > 0) {
                self.stream_scroll = if delta > 0.0 {
                    (self.stream_scroll + 1).min(stream_max)
                } else {
                    self.stream_scroll.saturating_sub(1)
                };
                return;
            }
        }
        let max = match self.page {
            Page::Wifi => self.wifi.networks.len().saturating_sub(5),
            Page::Bluetooth => self.bluetooth.devices.len().saturating_sub(5),
            Page::Sound => self.audio.sinks.len().saturating_sub(self.visible_outputs()),
            _ => 0,
        };
        self.scroll = if delta > 0.0 {
            (self.scroll + 1).min(max)
        } else {
            self.scroll.saturating_sub(1)
        };
    }
    pub fn mute(&self) -> ShellClickAction {
        self.audio.current().map_or(ShellClickAction::None, |s| {
            ShellClickAction::Control(Control::Mute {
                sink: s.name.clone(),
                muted: !s.muted,
            })
        })
    }
    pub fn key(&mut self, sym: u32, ch: Option<char>, ctrl: bool, shift: bool) -> Option<ShellClickAction> {
        use smithay::input::keyboard::keysyms;
        match sym {
            keysyms::KEY_Escape => {
                self.open(Page::Wifi);
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => return self.submit_password(),
            _ => {
                let mut utf8 = [0; 4];
                let text = ch.map(|c| c.encode_utf8(&mut utf8) as &str);
                self.password = std::mem::take(&mut self.password).with_byte_limit(64);
                self.password.key(sym, text, ctrl, shift);
                utf8.zeroize();
            }
        }
        None
    }
    pub fn click_password(&mut self, p: R, x: f32, y: f32, fonts: &mut FontSystem, shift: bool) {
        if self.page == Page::Password && R::new(p.x + 18.0, p.y + 129.0, p.w - 36.0, 36.0).contains(x, y) {
            self.password.click_masked(fonts, x - p.x - 28.0, p.w - 56.0, 13.0, shift);
        }
    }
    fn submit_password(&mut self) -> Option<ShellClickAction> {
        if !crate::controls::network::valid_password(
            self.selected.as_ref()?.security,
            self.password.text(),
        ) {
            self.error = Some(
                if self.selected.as_ref()?.security == Security::Sae {
                    "Пароль SAE: 1–63 байта"
                } else {
                    "Пароль: 8–63 байта или 64 hex-символа"
                }
                .into(),
            );
            return None;
        }
        let n = self.selected.take()?;
        let password = self.password.take_text();
        self.page = Page::Wifi;
        Some(ShellClickAction::Control(Control::Connect {
            network: n,
            password: Some(password),
        }))
    }
    /// Clickable region under the pointer, as `click` would decide it; with its rect.
    pub fn hover_target(&self, p: R) -> Option<(u32, R)> {
        let (gx, gy) = self.hover?;
        let (x, y) = (gx - p.x, gy - p.y);
        let row = |y0: f32, h: f32| R::new(p.x + 10.0, p.y + y0, p.w - 20.0, h);
        if y < 48.0 && x < 55.0 && !self.sound_only {
            return Some((1, R::new(p.x + 8.0, p.y + 9.0, 36.0, 32.0)));
        }
        match self.page {
            Page::Password => (y > p.h - 65.0).then(|| (5, row(p.h - 60.0, 40.0))),
            Page::Wifi | Page::Bluetooth | Page::Sound if !self.sound_only && y > p.h - 43.0 => {
                Some((4, row(p.h - 42.0, 34.0)))
            }
            Page::Wifi => {
                if (50.0..85.0).contains(&y) && self.wifi.enabled {
                    return Some((3, row(50.0, 34.0)));
                }
                if (88.0..278.0).contains(&y) && self.wifi.enabled {
                    let i = ((y - 88.0) / 38.0) as usize;
                    if self.wifi.networks.len() > i + self.scroll {
                        return Some((100 + (i + self.scroll) as u32, row(88.0 + i as f32 * 38.0, 34.0)));
                    }
                }
                None
            }
            Page::Bluetooth => {
                if (50.0..85.0).contains(&y) && self.bluetooth.powered {
                    return Some((3, row(50.0, 34.0)));
                }
                if (88.0..278.0).contains(&y) && self.bluetooth.powered {
                    let i = ((y - 88.0) / 38.0) as usize;
                    if self.bluetooth.devices.len() > i + self.scroll {
                        return Some((300 + (i + self.scroll) as u32, row(88.0 + i as f32 * 38.0, 34.0)));
                    }
                }
                None
            }
            Page::Sound => {
                if (50.0..85.0).contains(&y) {
                    return Some((3, row(50.0, 34.0)));
                }
                let (rows, stream_rows) = self.sound_sections(p.h);
                let out_scroll = self.scroll.min(self.audio.sinks.len().saturating_sub(rows));
                if (151.0..151.0 + rows as f32 * OUTPUT_ROW).contains(&y) {
                    let i = ((y - 151.0) / OUTPUT_ROW) as usize;
                    if self.audio.sinks.len() > i + out_scroll {
                        return Some((200 + (i + out_scroll) as u32, row(150.0 + i as f32 * OUTPUT_ROW, 32.0)));
                    }
                }
                if stream_rows > 0 {
                    let streams_y0 = 151.0 + rows as f32 * OUTPUT_ROW + STREAM_HEADER;
                    if (streams_y0..streams_y0 + stream_rows as f32 * STREAM_ROW).contains(&y) {
                        let row_i = ((y - streams_y0) / STREAM_ROW) as usize;
                        let s_scroll = self
                            .stream_scroll
                            .min(self.audio.streams.len().saturating_sub(stream_rows));
                        if let Some(stream) = self.audio.streams.get(s_scroll + row_i) {
                            let ry = streams_y0 + row_i as f32 * STREAM_ROW;
                            if y < ry + 24.0 {
                                if x < 42.0 {
                                    return Some((
                                        400 + stream.index,
                                        R::new(p.x + 12.0, p.y + ry, 28.0, 22.0),
                                    ));
                                }
                                if self.audio.sinks.len() > 1 && x >= p.w - 114.0 {
                                    return Some((
                                        500 + stream.index,
                                        R::new(p.x + p.w - 112.0, p.y + ry + 1.0, 96.0, 20.0),
                                    ));
                                }
                            }
                        }
                    }
                }
                None
            }
            Page::Main => None,
        }
    }

    pub fn click(&mut self, p: R, x: f32, y: f32) -> ShellClickAction {
        let x = x - p.x;
        let y = y - p.y;
        if y < 48.0 && x < 55.0 && !self.sound_only {
            self.open(if self.page == Page::Password {
                Page::Wifi
            } else {
                Page::Main
            });
            return ShellClickAction::None;
        }
        if self.page == Page::Password {
            if y > p.h - 65.0 {
                return self.submit_password().unwrap_or(ShellClickAction::None);
            }
            return ShellClickAction::None;
        }
        if !self.sound_only && y > p.h - 43.0 && matches!(self.page, Page::Wifi | Page::Bluetooth | Page::Sound) {
            return match self.page {
                Page::Wifi => ShellClickAction::NetworkSettings,
                Page::Bluetooth => ShellClickAction::BluetoothSettings,
                Page::Sound => ShellClickAction::SoundSettings,
                _ => ShellClickAction::None,
            };
        }
        if self.page == Page::Wifi {
            if y < 48.0 && x > p.w - 65.0 && self.wifi.available && self.wifi.hardware {
                return ShellClickAction::Control(Control::Wifi(!self.wifi.enabled));
            }
            if (50.0..85.0).contains(&y) {
                return ShellClickAction::Control(Control::Scan);
            }
            if (88.0..278.0).contains(&y) && self.wifi.enabled {
                let i = ((y - 88.0) / 38.0) as usize + self.scroll;
                if let Some(n) = self.wifi.networks.get(i).cloned() {
                    if n.active {
                        return ShellClickAction::Control(Control::Disconnect(n.device));
                    }
                    if n.saved.is_some() || matches!(n.security, Security::Open | Security::Owe) {
                        return ShellClickAction::Control(Control::Connect {
                            network: n,
                            password: None,
                        });
                    }
                    if n.security.password() {
                        self.selected = Some(n);
                        self.password.clear();
                        self.page = Page::Password;
                        return ShellClickAction::None;
                    }
                    return ShellClickAction::NetworkSettings;
                }
            }
        } else if self.page == Page::Bluetooth {
            if y < 48.0 && x > p.w - 65.0 && self.bluetooth.available {
                return ShellClickAction::Control(Control::Bluetooth(!self.bluetooth.powered));
            }
            if (50.0..85.0).contains(&y) && self.bluetooth.powered {
                return ShellClickAction::Control(Control::ScanBluetooth);
            }
            if (88.0..278.0).contains(&y) && self.bluetooth.powered {
                let i = ((y - 88.0) / 38.0) as usize + self.scroll;
                if let Some(d) = self.bluetooth.devices.get(i).cloned() {
                    let busy = self.bluetooth.connecting.as_deref() == Some(d.path.as_str());
                    if d.connected || busy {
                        // A second click on «Подключение…» cancels the attempt.
                        self.bluetooth.connecting = None;
                        return ShellClickAction::Control(Control::DisconnectBluetooth(d.path));
                    }
                    self.bluetooth.connecting = Some(d.path.clone());
                    return ShellClickAction::Control(Control::ConnectBluetooth(d.path));
                }
            }
        } else if self.page == Page::Sound {
            if (50.0..85.0).contains(&y) {
                return self.mute();
            }
            let slider = R::new(p.x + 18.0, p.y + 93.0, p.w - 36.0, 22.0);
            if slider.contains(x + p.x, y + p.y) {
                self.start_slider(slider, x + p.x);
                return ShellClickAction::None;
            }
            let (rows, stream_rows) = self.sound_sections(p.h);
            let out_scroll = self.scroll.min(self.audio.sinks.len().saturating_sub(rows));
            if (151.0..151.0 + rows as f32 * OUTPUT_ROW).contains(&y) {
                let i = ((y - 151.0) / OUTPUT_ROW) as usize + out_scroll;
                if let Some(sink) = self.audio.sinks.get(i) {
                    return ShellClickAction::Control(Control::Output(sink.name.clone()));
                }
            }
            if stream_rows > 0 {
                let streams_y0 = 151.0 + rows as f32 * OUTPUT_ROW + STREAM_HEADER;
                if (streams_y0..streams_y0 + stream_rows as f32 * STREAM_ROW).contains(&y) {
                    let row_i = ((y - streams_y0) / STREAM_ROW) as usize;
                    let s_scroll = self
                        .stream_scroll
                        .min(self.audio.streams.len().saturating_sub(stream_rows));
                    if let Some(stream) = self.audio.streams.get(s_scroll + row_i).cloned() {
                        let ry = streams_y0 + row_i as f32 * STREAM_ROW;
                        if y < ry + 24.0 {
                            if x < 42.0 {
                                return ShellClickAction::Control(Control::StreamMute {
                                    index: stream.index,
                                    muted: !stream.muted,
                                });
                            }
                            if let Some(next_sink) = self
                                .next_stream_sink(&stream)
                                .filter(|_| x >= p.w - 114.0)
                            {
                                return ShellClickAction::Control(Control::StreamOutput {
                                    index: stream.index,
                                    sink: next_sink,
                                });
                            }
                        } else {
                            let s_rect = R::new(p.x + 18.0, p.y + ry + 26.0, p.w - 64.0, 18.0);
                            if s_rect.contains(x + p.x, y + p.y) {
                                self.start_stream_slider(stream.index, s_rect, x + p.x);
                            }
                        }
                    }
                }
            }
        }
        ShellClickAction::None
    }
    pub fn paint(&self, pm: &mut PixmapMut, fonts: &mut FontSystem, p: R) {
        let pal = palette();
        let heading = pal.text_secondary;
        draw_glass_panel(pm, p.x, p.y, p.w, p.h, PANEL_RADIUS, false);
        let hover = self.hover_target(p);
        if let Some((_, r)) = hover.filter(|(key, _)| *key != 1) {
            let tint = if appearance() == Appearance::Dark { rgba(255, 255, 255, 22) } else { rgba(0, 0, 0, 14) };
            fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 10.0, tint);
        }
        if !self.sound_only {
            back_button(pm, p.x + 28.0, p.y + 26.0, hover.is_some_and(|(key, _)| key == 1));
        }
        let title = match self.page {
            Page::Wifi => "Wi‑Fi",
            Page::Bluetooth => "Bluetooth",
            Page::Sound => "Звук",
            Page::Password => "Подключение",
            _ => "",
        };
        fonts.draw_text(pm, title, p.x + if self.sound_only { 18.0 } else { 52.0 }, p.y + 17.0, 15.0, pal.text, true);
        // A hairline under the page's own action and one above the link to Settings
        let rule = |pm: &mut PixmapMut, y: f32| fill_rounded_rect(pm, p.x + 18.0, p.y + y, p.w - 36.0, 0.75, 0.35, pal.separator);
        // One row of a list: a round plate with the glyph, two lines of text
        let row_text = |pm: &mut PixmapMut, fonts: &mut FontSystem, y: f32, name: &str, sub: &str, active: bool, right: f32| {
            fonts.draw_text_clipped(pm, name, p.x + 52.0, y + 2.5, p.w - 70.0 - right, 13.0, pal.text, active);
            fonts.draw_text_clipped(pm, sub, p.x + 52.0, y + 19.0, p.w - 70.0 - right, 10.5, pal.text_secondary, false);
        };
        if self.page == Page::Wifi {
            let can = self.wifi.available && self.wifi.hardware;
            switch(pm, p.x + p.w - 59.0, p.y + 15.0, can && self.wifi.enabled);
            fonts.draw_text_clipped(
                pm,
                if self.wifi.enabled && can { "Обновить сети ↻" } else { self.wifi.subtitle() },
                p.x + 18.0,
                p.y + 58.0,
                p.w - 36.0,
                12.0,
                heading,
                false,
            );
            rule(pm, 85.0);
            if self.wifi.enabled && self.wifi.available {
                for (row, n) in self.wifi.networks.iter().skip(self.scroll).take(5).enumerate() {
                    let y = p.y + 88.0 + row as f32 * 38.0;
                    if y + 38.0 > p.y + p.h - 90.0 {
                        break;
                    }
                    if n.active {
                        fill_rounded_rect(pm, p.x + 10.0, y, p.w - 20.0, 34.0, 10.0, with_alpha(pal.accent, 0.13));
                    }
                    disc(pm, p.x + 29.0, y + 17.0, 13.0, n.active);
                    wifi_signal(pm, p.x + 29.0, y + 17.6, if n.active { pal.on_accent } else { pal.text }, n.strength);
                    let sub = if n.active {
                        "Подключено"
                    } else if n.saved.is_some() {
                        "Сохранённая сеть"
                    } else if n.security == Security::Open {
                        "Открытая сеть"
                    } else {
                        "Защищённая сеть"
                    };
                    let locked = n.security != Security::Open;
                    row_text(pm, fonts, y, &n.label, sub, n.active, if locked && n.active { 44.0 } else { 22.0 });
                    let mut right = p.x + p.w - 28.0;
                    if n.active {
                        checkmark(pm, right, y + 17.0, pal.accent);
                        right -= 22.0;
                    }
                    if locked {
                        lock(pm, right, y + 16.0, pal.text_tertiary);
                    }
                }
                if self.wifi.networks.is_empty() {
                    fonts.draw_text(pm, "Сети пока не найдены", p.x + 18.0, p.y + 100.0, 12.0, pal.text_secondary, false);
                }
            }
            rule(pm, p.h - 45.0);
            fonts.draw_text(pm, "Настройки сети…", p.x + 18.0, p.y + p.h - 32.0, 12.5, pal.text, false);
        } else if self.page == Page::Bluetooth {
            switch(pm, p.x + p.w - 59.0, p.y + 15.0, self.bluetooth.available && self.bluetooth.powered);
            fonts.draw_text_clipped(
                pm,
                if self.bluetooth.powered && self.bluetooth.available {
                    if self.bluetooth.discovering {
                        "Поиск устройств… (нажмите, чтобы остановить)"
                    } else {
                        "Поиск устройств ↻"
                    }
                } else {
                    self.bluetooth.subtitle()
                },
                p.x + 18.0,
                p.y + 58.0,
                p.w - 36.0,
                12.0,
                heading,
                false,
            );
            rule(pm, 85.0);
            if self.bluetooth.powered && self.bluetooth.available {
                for (row, d) in self.bluetooth.devices.iter().skip(self.scroll).take(5).enumerate() {
                    let y = p.y + 88.0 + row as f32 * 38.0;
                    if y + 38.0 > p.y + p.h - 90.0 {
                        break;
                    }
                    if d.connected {
                        fill_rounded_rect(pm, p.x + 10.0, y, p.w - 20.0, 34.0, 10.0, with_alpha(pal.accent, 0.13));
                    }
                    disc(pm, p.x + 29.0, y + 17.0, 13.0, d.connected);
                    device_glyph(pm, &d.icon, p.x + 29.0, y + 17.0, if d.connected { pal.on_accent } else { pal.text });
                    let sub = if d.connected {
                        "Подключено"
                    } else if self.bluetooth.connecting.as_deref() == Some(d.path.as_str()) {
                        "Подключение… · нажмите, чтобы отменить"
                    } else if d.paired {
                        "Сопряжено"
                    } else {
                        d.kind_label()
                    };
                    row_text(pm, fonts, y, &d.name, sub, d.connected, if d.battery.is_some() { 64.0 } else { 0.0 });
                    if let Some(battery) = d.battery {
                        draw_battery_icon(pm, p.x + p.w - 31.0, y + 17.0, pal.text_secondary, battery, false, battery <= 15);
                        fonts.draw_text_right(pm, &format!("{battery}%"), p.x + p.w - 47.0, y + 10.5, 11.0, pal.text_secondary, false);
                    }
                }
                if self.bluetooth.devices.is_empty() {
                    fonts.draw_text(pm, "Устройства пока не найдены", p.x + 18.0, p.y + 100.0, 12.0, pal.text_secondary, false);
                }
            }
            rule(pm, p.h - 45.0);
            fonts.draw_text(pm, "Настройки Bluetooth…", p.x + 18.0, p.y + p.h - 32.0, 12.5, pal.text, false);
        } else if self.page == Page::Sound {
            let muted = self.audio.current().is_some_and(|s| s.muted);
            disc(pm, p.x + 29.0, p.y + 67.0, 13.0, !muted && self.audio.current().is_some());
            speaker(pm, p.x + 29.5, p.y + 67.0, 0.72, if muted || self.audio.current().is_none() { pal.text } else { pal.on_accent }, muted);
            fonts.draw_text(
                pm,
                if muted {
                    "Включить звук"
                } else {
                    "Выключить звук"
                },
                p.x + 52.0,
                p.y + 58.0,
                13.0,
                pal.text,
                false,
            );
            fonts.draw_text_right(
                pm,
                &format!("{}%", (self.volume() * 100.0).round() as u32),
                p.x + p.w - 18.0,
                p.y + 58.0,
                12.0,
                pal.text_secondary,
                false,
            );
            slider(
                pm,
                R::new(p.x + 18.0, p.y + 93.0, p.w - 36.0, 22.0),
                self.volume(),
                self.audio.current().is_some(),
            );
            fonts.draw_text(
                pm,
                "Устройство вывода",
                p.x + 18.0,
                p.y + 130.0,
                11.5,
                heading,
                true,
            );
            let (rows, stream_rows) = self.sound_sections(p.h);
            self.sound_fit.set(rows);
            self.stream_fit.set(stream_rows);
            let total = self.audio.sinks.len();
            let out_scroll = self.scroll.min(total.saturating_sub(rows));
            if total > rows {
                // More outputs than fit: a thin scroll bar says the list goes on
                let track = rows as f32 * OUTPUT_ROW - 4.0;
                let thumb = (track * rows as f32 / total as f32).max(16.0);
                let at = track - thumb;
                let ty = p.y + 152.0 + at * out_scroll as f32 / (total - rows) as f32;
                fill_rounded_rect(pm, p.x + p.w - 7.0, ty, 3.0, thumb, 1.5, with_alpha(pal.text_secondary, 0.55));
            }
            for (row, sink) in self.audio.sinks.iter().skip(out_scroll).take(rows).enumerate() {
                let y = p.y + 151.0 + row as f32 * OUTPUT_ROW;
                let active = sink.name == self.audio.default;
                if active {
                    fill_rounded_rect(
                        pm,
                        p.x + 10.0,
                        y,
                        p.w - 20.0,
                        32.0,
                        10.0,
                        with_alpha(pal.accent, 0.13),
                    );
                }
                fonts.draw_text_clipped(
                    pm,
                    &sink.label,
                    p.x + 18.0,
                    y + 7.0,
                    p.w - 52.0,
                    12.0,
                    pal.text,
                    active,
                );
                if active {
                    checkmark(pm, p.x + p.w - 28.0, y + 16.0, pal.accent);
                }
            }
            if stream_rows > 0 {
                let hdr_y = p.y + 151.0 + rows as f32 * OUTPUT_ROW + 7.0;
                self.streams_top.set(hdr_y);
                fill_rounded_rect(pm, p.x + 18.0, hdr_y - 3.0, p.w - 36.0, 0.75, 0.35, pal.separator);
                fonts.draw_text(
                    pm,
                    "Программы",
                    p.x + 18.0,
                    hdr_y + 3.0,
                    11.5,
                    heading,
                    true,
                );
                let streams_y0 = p.y + 151.0 + rows as f32 * OUTPUT_ROW + STREAM_HEADER;
                let s_total = self.audio.streams.len();
                let s_scroll = self.stream_scroll.min(s_total.saturating_sub(stream_rows));
                if s_total > stream_rows {
                    let track = stream_rows as f32 * STREAM_ROW - 6.0;
                    let thumb = (track * stream_rows as f32 / s_total as f32).max(16.0);
                    let at = track - thumb;
                    let ty = streams_y0 + 2.0 + at * s_scroll as f32 / (s_total - stream_rows) as f32;
                    fill_rounded_rect(pm, p.x + p.w - 7.0, ty, 3.0, thumb, 1.5, with_alpha(pal.text_secondary, 0.55));
                }
                let multi_sink = self.audio.sinks.len() > 1;
                for (row, stream) in self
                    .audio
                    .streams
                    .iter()
                    .skip(s_scroll)
                    .take(stream_rows)
                    .enumerate()
                {
                    let ry = streams_y0 + row as f32 * STREAM_ROW;
                    speaker(pm, p.x + 26.0, ry + 11.0, 0.72, if stream.muted { pal.text_tertiary } else { pal.text }, stream.muted);
                    let title_max = if multi_sink { p.w - 160.0 } else { p.w - 60.0 };
                    fonts.draw_text_clipped(
                        pm,
                        &stream.name,
                        p.x + 41.0,
                        ry + 2.5,
                        title_max.max(40.0),
                        12.0,
                        if stream.muted { pal.text_secondary } else { pal.text },
                        false,
                    );
                    if multi_sink {
                        let pill = R::new(p.x + p.w - 112.0, ry + 1.0, 96.0, 20.0);
                        fill_rounded_rect(pm, pill.x, pill.y, pill.w, pill.h, 10.0, quiet_fill());
                        let route = format!("↦ {}", self.stream_sink_label(stream));
                        fonts.draw_text_clipped(
                            pm,
                            &route,
                            pill.x + 6.0,
                            pill.y + 3.0,
                            pill.w - 12.0,
                            10.5,
                            pal.text_secondary,
                            false,
                        );
                    }
                    let vol = self.stream_volume(stream);
                    slider(
                        pm,
                        R::new(p.x + 18.0, ry + 27.0, p.w - 64.0, 16.0),
                        vol,
                        !stream.muted,
                    );
                    fonts.draw_text_right(
                        pm,
                        &format!("{}%", (vol * 100.0).round() as u32),
                        p.x + p.w - 18.0,
                        ry + 27.5,
                        11.0,
                        pal.text_secondary,
                        false,
                    );
                }
            } else {
                self.streams_top.set(0.0);
            }
            if !self.sound_only {
                fill_rounded_rect(pm, p.x + 18.0, p.y + p.h - 45.0, p.w - 36.0, 0.75, 0.35, pal.separator);
                fonts.draw_text(pm, "Настройки звука…", p.x + 18.0, p.y + p.h - 32.0, 12.5, pal.text, false);
            }
        } else if self.page == Page::Password {
            let name = self
                .selected
                .as_ref()
                .map(|n| n.label.as_str())
                .unwrap_or("");
            fonts.draw_text_clipped(
                pm,
                name,
                p.x + 18.0,
                p.y + 67.0,
                p.w - 36.0,
                15.0,
                pal.text,
                true,
            );
            fonts.draw_text(
                pm,
                "Введите пароль сети",
                p.x + 18.0,
                p.y + 99.0,
                12.0,
                pal.text_secondary,
                false,
            );
            fill_rounded_rect(
                pm,
                p.x + 18.0,
                p.y + 129.0,
                p.w - 36.0,
                36.0,
                10.0,
                quiet_fill(),
            );
            self.password.draw_masked(pm, fonts, p.x + 28.0, p.y + 139.0, p.w - 56.0, 13.0, pal.text, pal.accent);
            fonts.draw_text(
                pm,
                "Enter — подключить · Esc — отмена",
                p.x + 18.0,
                p.y + 183.0,
                11.0,
                pal.text_secondary,
                false,
            );
            if let Some(error) = &self.error {
                fonts.draw_text_clipped(
                    pm,
                    error,
                    p.x + 18.0,
                    p.y + 215.0,
                    p.w - 36.0,
                    10.5,
                    pal.text_secondary,
                    false,
                );
            }
            fill_rounded_rect(
                pm,
                p.x + 18.0,
                p.y + p.h - 61.0,
                p.w - 36.0,
                37.0,
                12.0,
                pal.accent,
            );
            fonts.draw_text_centered(
                pm,
                "Подключить",
                p.x + p.w / 2.0,
                p.y + p.h - 50.0,
                13.0,
                pal.on_accent,
                true,
            );
        }
        if self.page != Page::Password {
            let err = self.error.as_deref().or(match self.page {
                Page::Wifi => self.wifi.error.as_deref(),
                Page::Bluetooth => self.bluetooth.error.as_deref(),
                _ => self.audio.error.as_deref(),
            });
            if let Some(err) = err {
                fonts.draw_text_clipped(
                    pm,
                    err,
                    p.x + 18.0,
                    p.y + p.h - if self.sound_only { 30.0 } else { 74.0 },
                    p.w - 36.0,
                    11.0,
                    pal.text_secondary,
                    false,
                );
            } else if self.page == Page::Sound && self.audio.sinks.is_empty() {
                fonts.draw_text(
                    pm,
                    "Нет устройств вывода",
                    p.x + 18.0,
                    p.y + 180.0,
                    12.0,
                    pal.text_secondary,
                    false,
                );
            }
        }
    }
}
/// Corner radii of Control Center: the panel and the cards inside it
pub const PANEL_RADIUS: f32 = 26.0;
pub const CARD_RADIUS: f32 = 16.0;

fn scaled() -> tiny_skia::Transform {
    tiny_skia::Transform::from_scale(ui_scale(), ui_scale())
}

/// A stroke with round caps and joins, so glyphs read as one drawn line
fn stroke_path(pm: &mut PixmapMut, path: Option<tiny_skia::Path>, width: f32, color: Color) {
    let Some(path) = path else { return };
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = tiny_skia::Stroke {
        width,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };
    pm.stroke_path(&path, &paint, &stroke, scaled(), None);
}

fn fill_shape(pm: &mut PixmapMut, path: Option<tiny_skia::Path>, color: Color) {
    let Some(path) = path else { return };
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pm.fill_path(&path, &paint, tiny_skia::FillRule::Winding, scaled(), None);
}

fn polyline(points: &[(f32, f32)], close: bool) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for (i, (x, y)) in points.iter().enumerate() {
        if i == 0 { pb.move_to(*x, *y) } else { pb.line_to(*x, *y) }
    }
    if close { pb.close(); }
    pb.finish()
}

fn arc_points(out: &mut Vec<(f32, f32)>, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) {
    let steps = 18;
    for k in 0..=steps {
        let t = a0 + (a1 - a0) * k as f32 / steps as f32;
        out.push((cx + r * t.cos(), cy + r * t.sin()));
    }
}

fn arc(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Option<tiny_skia::Path> {
    let mut points = Vec::with_capacity(19);
    arc_points(&mut points, cx, cy, r, a0, a1);
    polyline(&points, false)
}

/// The colour moved towards white: the lit top of a gradient
fn lift(c: Color, t: f32) -> Color {
    Color::from_rgba(c.red() + (1.0 - c.red()) * t, c.green() + (1.0 - c.green()) * t, c.blue() + (1.0 - c.blue()) * t, c.alpha()).unwrap_or(c)
}

fn quiet_fill() -> Color {
    if appearance() == Appearance::Dark { rgba(255, 255, 255, 30) } else { rgba(0, 0, 0, 22) }
}

/// A card of Control Center: translucent surface with a rim lit from above. The prepared
/// backdrop and the live paint both come here, so the two always agree.
pub fn card(pm: &mut PixmapMut, r: R) {
    let dark = appearance() == Appearance::Dark;
    let hc = high_contrast();
    let fill = if dark { rgba(255, 255, 255, if hc { 38 } else { 22 }) } else { rgba(255, 255, 255, if hc { 215 } else { 150 }) };
    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, CARD_RADIUS, fill);
    let (top, bottom) = if dark { (rgba(255, 255, 255, 38), rgba(255, 255, 255, 8)) } else { (rgba(255, 255, 255, 215), rgba(255, 255, 255, 70)) };
    if let Some(path) = rounded_rect_path(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, CARD_RADIUS - 0.5) {
        let mut paint = tiny_skia::Paint { anti_alias: true, ..Default::default() };
        match tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(0.0, r.y),
            tiny_skia::Point::from_xy(0.0, r.y + r.h),
            vec![tiny_skia::GradientStop::new(0.0, top), tiny_skia::GradientStop::new(1.0, bottom)],
            tiny_skia::SpreadMode::Pad,
            tiny_skia::Transform::identity(),
        ) {
            Some(shader) => paint.shader = shader,
            None => paint.set_color(top),
        }
        pm.stroke_path(&path, &paint, &tiny_skia::Stroke { width: 1.0, ..Default::default() }, scaled(), None);
    }
    if hc {
        stroke_rounded_rect(pm, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, CARD_RADIUS - 0.5, 0.75, palette().separator);
    }
}

/// The pointer over a card
pub fn card_hover(pm: &mut PixmapMut, r: R) {
    let tint = if appearance() == Appearance::Dark { rgba(255, 255, 255, 16) } else { rgba(255, 255, 255, 105) };
    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, CARD_RADIUS, tint);
}

/// Round plate under a glyph: the accent, lit from above, when the thing is on
pub fn disc(pm: &mut PixmapMut, cx: f32, cy: f32, r: f32, on: bool) {
    if on {
        let accent = palette().accent;
        if let Some(path) = tiny_skia::PathBuilder::from_circle(cx, cy, r) {
            fill_path_gradient(pm, &path, cy - r, cy + r, lift(accent, 0.24), accent);
        }
    } else {
        fill_circle(pm, cx, cy, r, quiet_fill());
    }
}

/// Rounded-square plate under the glyph of a tile that is not a switch
pub fn chip(pm: &mut PixmapMut, cx: f32, cy: f32) {
    fill_rounded_rect(pm, cx - 15.0, cy - 15.0, 30.0, 30.0, 9.0, quiet_fill());
}

fn knob(pm: &mut PixmapMut, cx: f32, cy: f32, r: f32) {
    fill_circle(pm, cx, cy + 0.8, r + 0.6, rgba(0, 0, 0, 34));
    fill_circle(pm, cx, cy, r, rgba(255, 255, 255, 255));
}

/// A 40×22 switch
pub fn switch(pm: &mut PixmapMut, x: f32, y: f32, on: bool) {
    if on {
        let accent = palette().accent;
        if let Some(path) = rounded_rect_path(x, y, 40.0, 22.0, 11.0) {
            fill_path_gradient(pm, &path, y, y + 22.0, lift(accent, 0.16), accent);
        }
    } else {
        fill_rounded_rect(pm, x, y, 40.0, 22.0, 11.0, quiet_fill());
    }
    knob(pm, x + if on { 29.0 } else { 11.0 }, y + 11.0, 9.0);
}

/// The round «back» button of a detail page
fn back_button(pm: &mut PixmapMut, cx: f32, cy: f32, hot: bool) {
    let pal = palette();
    let fill = if hot { with_alpha(pal.text, 0.16) } else { quiet_fill() };
    fill_circle(pm, cx, cy, 14.0, fill);
    stroke_path(pm, polyline(&[(cx + 2.0, cy - 5.0), (cx - 3.0, cy), (cx + 2.0, cy + 5.0)], false), 1.8, pal.text);
}

fn checkmark(pm: &mut PixmapMut, cx: f32, cy: f32, color: Color) {
    stroke_path(pm, polyline(&[(cx - 4.6, cy + 0.4), (cx - 1.4, cy + 3.6), (cx + 4.8, cy - 3.8)], false), 1.8, color);
}

fn lock(pm: &mut PixmapMut, cx: f32, cy: f32, color: Color) {
    fill_rounded_rect(pm, cx - 4.0, cy - 0.8, 8.0, 6.2, 1.6, color);
    stroke_path(pm, arc(cx, cy - 1.2, 2.5, std::f32::consts::PI, std::f32::consts::TAU), 1.3, color);
}

/// Wi‑Fi fan whose arcs say how strong the signal is
fn wifi_signal(pm: &mut PixmapMut, cx: f32, cy: f32, color: Color, strength: u8) {
    let bars = match strength { 0..=34 => 1, 35..=64 => 2, _ => 3 };
    let (a0, a1) = (-std::f32::consts::FRAC_PI_4 * 3.0, -std::f32::consts::FRAC_PI_4);
    for (i, r) in [4.2f32, 7.6].into_iter().enumerate() {
        let ink = if i + 2 <= bars { color } else { with_alpha(color, 0.3) };
        stroke_path(pm, arc(cx, cy + 3.6, r, a0, a1), 1.5, ink);
    }
    fill_circle(pm, cx, cy + 3.4, 1.4, color);
}

/// What kind of thing a Bluetooth device is, from its BlueZ icon name
fn device_glyph(pm: &mut PixmapMut, name: &str, x: f32, y: f32, color: Color) {
    use std::f32::consts::PI;
    if name.starts_with("audio-head") {
        stroke_path(pm, arc(x, y + 0.5, 5.6, PI, PI * 2.0), 1.5, color);
        fill_rounded_rect(pm, x - 7.0, y + 0.2, 3.4, 5.8, 1.5, color);
        fill_rounded_rect(pm, x + 3.6, y + 0.2, 3.4, 5.8, 1.5, color);
    } else if name.starts_with("audio-") {
        speaker(pm, x + 0.5, y, 0.72, color, false);
    } else if name == "input-keyboard" {
        stroke_rounded_rect(pm, x - 7.5, y - 4.5, 15.0, 9.0, 2.2, 1.3, color);
        for i in -1..=1 {
            fill_circle(pm, x + i as f32 * 3.4, y - 1.4, 0.8, color);
        }
        stroke_path(pm, polyline(&[(x - 3.4, y + 1.8), (x + 3.4, y + 1.8)], false), 1.3, color);
    } else if name == "input-mouse" || name == "input-tablet" {
        stroke_rounded_rect(pm, x - 4.2, y - 6.5, 8.4, 13.0, 4.2, 1.4, color);
        stroke_path(pm, polyline(&[(x, y - 4.0), (x, y - 1.4)], false), 1.4, color);
    } else if name.starts_with("phone") {
        stroke_rounded_rect(pm, x - 4.2, y - 6.8, 8.4, 13.6, 2.2, 1.4, color);
        fill_circle(pm, x, y + 4.2, 0.9, color);
    } else {
        bluetooth_rune(pm, x, y, 0.68, color);
    }
}

fn bluetooth_rune(pm: &mut PixmapMut, x: f32, y: f32, s: f32, color: Color) {
    let at = |a: f32, b: f32| (x + a * s, y + b * s);
    stroke_path(pm, polyline(&[at(-5.0, -5.0), at(5.0, 5.0), at(0.0, 10.0), at(0.0, -10.0), at(5.0, -5.0), at(-5.0, 5.0)], false), 1.5, color);
}

/// Crescent: a disc with a smaller one bitten out of its upper right
pub fn moon(pm: &mut PixmapMut, x: f32, y: f32, s: f32, color: Color) {
    let (big, small, apart, turn) = (7.6 * s, 6.4 * s, 4.4 * s, -0.62f32);
    let alpha = ((apart * apart + big * big - small * small) / (2.0 * apart * big)).acos();
    let gamma = (big * alpha.sin()).atan2(big * alpha.cos() - apart);
    let (bx, by) = (x - 0.9 * s, y + 0.6 * s);
    let (sx, sy) = (bx + apart * turn.cos(), by + apart * turn.sin());
    let mut points = Vec::with_capacity(40);
    arc_points(&mut points, bx, by, big, turn + alpha, turn + std::f32::consts::TAU - alpha);
    arc_points(&mut points, sx, sy, small, turn + std::f32::consts::TAU - gamma, turn + gamma);
    fill_shape(pm, polyline(&points, true), color);
}

/// A capsule with the accent up to the value and a white knob at its end. The geometry of
/// `r` is also the hit area, so the knob stays inside it.
pub fn slider(pm: &mut PixmapMut, r: R, value: f32, enabled: bool) {
    let half = r.h / 2.0;
    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, half, quiet_fill());
    if !enabled {
        return;
    }
    let accent = palette().accent;
    let w = (r.w * value.clamp(0.0, 1.0)).max(r.h);
    if let Some(path) = rounded_rect_path(r.x, r.y, w, r.h, half) {
        fill_path_gradient(pm, &path, r.y, r.y + r.h, lift(accent, 0.16), accent);
    }
    knob(pm, r.x + w - half, r.y + half, half - 1.5);
}

/// Centres of ⏮ ⏯ ⏭ on the «Сейчас исполняется» card
fn media_buttons(r: R) -> [f32; 3] {
    [r.x + r.w - 92.0, r.x + r.w - 60.0, r.x + r.w - 28.0]
}

/// A click on the card: a button, else the title brings the player's window back
pub fn media_hit(player: &crate::mpris::Player, r: R, x: f32) -> crate::mpris::Command {
    use crate::mpris::Command;
    let [prev, play, next] = media_buttons(r);
    let near = |cx: f32| (x - cx).abs() <= 15.0;
    if near(prev) {
        Command::Previous
    } else if near(play) {
        Command::PlayPause
    } else if near(next) {
        Command::Next
    } else if player.can_raise {
        Command::Raise
    } else {
        Command::PlayPause
    }
}

/// The cover's place on the media card: a note on a warm plate while something plays
fn media_cover(pm: &mut PixmapMut, x: f32, y: f32, size: f32, live: bool) {
    let ink = if live {
        if let Some(path) = rounded_rect_path(x, y, size, size, 9.0) {
            fill_path_gradient(pm, &path, y, y + size, rgba(255, 106, 112, 255), rgba(246, 48, 84, 255));
        }
        rgba(255, 255, 255, 255)
    } else {
        fill_rounded_rect(pm, x, y, size, size, 9.0, quiet_fill());
        palette().text_tertiary
    };
    let (cx, cy) = (x + size * 0.5 - 0.6, y + size * 0.5 + 0.4);
    fill_circle(pm, cx - 3.4, cy + 4.4, 2.3, ink);
    fill_circle(pm, cx + 4.4, cy + 2.8, 2.3, ink);
    stroke_path(pm, polyline(&[(cx - 1.8, cy + 4.4), (cx - 1.8, cy - 5.0), (cx + 6.0, cy - 6.6), (cx + 6.0, cy + 2.8)], false), 1.5, ink);
    stroke_path(pm, polyline(&[(cx - 1.8, cy - 4.0), (cx + 6.0, cy - 5.6)], false), 2.2, ink);
}

pub fn paint_media(pm: &mut PixmapMut, fonts: &mut FontSystem, player: Option<&crate::mpris::Player>, r: R) {
    let pal = palette();
    let cover = 38.0;
    let cy = r.y + r.h * 0.5;
    media_cover(pm, r.x + 11.0, cy - cover * 0.5, cover, player.is_some());
    let text_x = r.x + 11.0 + cover + 11.0;
    let Some(player) = player else {
        fonts.draw_text(pm, "Не исполняется", text_x, cy - 8.5, 13.0, pal.text_secondary, false);
        return;
    };
    let [prev, play, next] = media_buttons(r);
    let width = prev - 18.0 - text_x;
    fonts.draw_text_clipped(pm, &player.title, text_x, cy - 16.0, width, 13.0, pal.text, true);
    fonts.draw_text_clipped(pm, player.subtitle(), text_x, cy + 1.5, width, 11.5, pal.text_secondary, false);
    let ink = |on: bool| if on { pal.text } else { pal.text_tertiary };
    media_glyph(pm, MediaGlyph::Previous, prev, cy, ink(player.can_previous));
    media_glyph(pm, if player.playing { MediaGlyph::Pause } else { MediaGlyph::Play }, play, cy, pal.text);
    media_glyph(pm, MediaGlyph::Next, next, cy, ink(player.can_next));
}

#[derive(Clone, Copy)]
enum MediaGlyph {
    Previous,
    Play,
    Pause,
    Next,
}

fn media_glyph(pm: &mut PixmapMut, kind: MediaGlyph, x: f32, y: f32, color: Color) {
    // Filled triangles with a hair of round stroke, so the tips are not needle-sharp
    let triangle = |pm: &mut PixmapMut, tip: f32, base: f32, h: f32| {
        let path = polyline(&[(x + base, y - h), (x + tip, y), (x + base, y + h)], true);
        fill_shape(pm, path.clone(), color);
        stroke_path(pm, path, 1.2, color);
    };
    match kind {
        MediaGlyph::Play => triangle(pm, 6.5, -4.5, 6.2),
        MediaGlyph::Pause => {
            fill_rounded_rect(pm, x - 5.5, y - 6.5, 4.0, 13.0, 1.4, color);
            fill_rounded_rect(pm, x + 1.5, y - 6.5, 4.0, 13.0, 1.4, color);
        }
        MediaGlyph::Next => {
            triangle(pm, 0.5, -7.5, 5.0);
            triangle(pm, 8.5, 0.5, 5.0);
        }
        MediaGlyph::Previous => {
            triangle(pm, -0.5, 7.5, 5.0);
            triangle(pm, -8.5, -0.5, 5.0);
        }
    }
}

/// Speaker: a solid cone with two waves, or a cross when muted; `s` scales it (the menu
/// bar one is smaller)
pub fn speaker(pm: &mut PixmapMut, x: f32, y: f32, s: f32, color: Color, muted: bool) {
    let at = |a: f32, b: f32| (x + a * s, y + b * s);
    let width = if s < 0.9 { 1.35 } else { 1.5 };
    let cone = polyline(&[at(-9.0, -2.6), at(-5.8, -2.6), at(-1.2, -6.6), at(-1.2, 6.6), at(-5.8, 2.6), at(-9.0, 2.6)], true);
    fill_shape(pm, cone.clone(), color);
    stroke_path(pm, cone, 1.3, color);
    if muted {
        stroke_path(pm, polyline(&[at(3.4, -3.4), at(9.6, 3.4)], false), width, color);
        stroke_path(pm, polyline(&[at(3.4, 3.4), at(9.6, -3.4)], false), width, color);
    } else {
        let (cx, cy) = at(0.2, 0.0);
        stroke_path(pm, arc(cx, cy, 4.4 * s, -0.72, 0.72), width, color);
        stroke_path(pm, arc(cx, cy, 8.2 * s, -0.78, 0.78), width, color);
    }
}

#[derive(Clone, Copy)]
pub enum Icon {
    Wifi,
    Bluetooth,
    Moon,
    Contrast,
    Sun,
    Speaker,
    Scale,
    Wallpaper,
    Keyboard,
}
pub fn icon(pm: &mut PixmapMut, kind: Icon, x: f32, y: f32, color: Color, muted: bool) {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
    match kind {
        Icon::Wifi => wifi_signal(pm, x, y + 0.6, color, 100),
        Icon::Bluetooth => bluetooth_rune(pm, x, y, 0.78, color),
        Icon::Moon => moon(pm, x, y, 1.0, color),
        Icon::Contrast => {
            stroke_circle(pm, x, y, 7.2, 1.5, color);
            let mut half = Vec::with_capacity(20);
            arc_points(&mut half, x, y, 4.6, FRAC_PI_2, FRAC_PI_2 * 3.0);
            fill_shape(pm, polyline(&half, true), color);
        }
        Icon::Sun => {
            stroke_circle(pm, x, y, 3.6, 1.5, color);
            for i in 0..8 {
                let (sin, cos) = (i as f32 * FRAC_PI_4).sin_cos();
                stroke_path(pm, polyline(&[(x + cos * 6.6, y + sin * 6.6), (x + cos * 8.8, y + sin * 8.8)], false), 1.5, color);
            }
        }
        Icon::Speaker => speaker(pm, x, y, 0.92, color, muted),
        Icon::Scale => {
            stroke_rounded_rect(pm, x - 7.5, y - 6.5, 15.0, 13.0, 3.0, 1.4, color);
            stroke_path(pm, polyline(&[(x - 3.2, y + 2.8), (x + 3.2, y - 2.8)], false), 1.4, color);
            stroke_path(pm, polyline(&[(x - 0.6, y - 2.8), (x + 3.2, y - 2.8), (x + 3.2, y + 1.0)], false), 1.4, color);
        }
        Icon::Wallpaper => {
            stroke_rounded_rect(pm, x - 8.0, y - 6.5, 16.0, 13.0, 3.0, 1.4, color);
            stroke_path(pm, polyline(&[(x - 6.0, y + 4.2), (x - 1.6, y - 0.4), (x + 1.2, y + 2.2), (x + 3.4, y + 0.2), (x + 6.0, y + 3.2)], false), 1.4, color);
            fill_circle(pm, x + 3.4, y - 2.8, 1.4, color);
        }
        Icon::Keyboard => {
            stroke_rounded_rect(pm, x - 9.0, y - 5.5, 18.0, 11.0, 2.8, 1.4, color);
            for i in -2..=2 {
                fill_circle(pm, x + i as f32 * 3.0, y - 1.8, 0.8, color);
            }
            stroke_path(pm, polyline(&[(x - 4.2, y + 2.0), (x + 4.2, y + 2.0)], false), 1.4, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn network() -> Network {
        Network {
            ssid: b"Test Wi-Fi".to_vec(),
            label: "Test Wi-Fi".into(),
            device: "/test/device".into(),
            ap: "/test/ap".into(),
            saved: None,
            security: Security::Psk,
            strength: 82,
            active: false,
        }
    }
    #[test]
    fn password_validation_keeps_form_and_slider_commits_only_on_release() {
        let mut panel = Panel::default();
        panel.page = Page::Password;
        panel.selected = Some(network());
        panel.password.insert("short");
        assert!(panel.submit_password().is_none());
        assert!(panel.page == Page::Password);
        assert!(panel.selected.is_some());
        panel.password.clear();
        panel.password.insert("valid password");
        let action = panel.submit_password().unwrap();
        assert!(panel.page == Page::Wifi);
        assert!(panel.password.is_empty());
        assert!(!format!("{action:?}").contains("valid password"));
        panel.audio = Audio {
            default: "test".into(),
            sinks: vec![crate::controls::Sink {
                name: "test".into(),
                channels: vec![32768],
                ..Default::default()
            }],
            ..Default::default()
        };
        panel.start_slider(R::new(10., 10., 200., 20.), 20.);
        panel.motion(160.);
        assert_eq!(panel.audio.current().unwrap().volume(), 0.5);
        assert_eq!(panel.volume(), 0.75);
        assert!(
            matches!(panel.release(),Some(ShellClickAction::Control(Control::Volume {value,..})) if value==0.75)
        );
        assert!(panel.release().is_none());
        // After the release the new value stays shown until the server confirms it
        assert_eq!(panel.volume(), 0.75, "no jump back to the old volume");
        panel.audio_updated();
        assert_eq!(panel.volume(), 0.75, "a stale snapshot does not move it");
        panel.audio.sinks[0].channels = vec![49152];
        panel.audio_updated();
        assert_eq!(panel.volume(), 0.75);
        assert!(panel.pending_volume.is_none(), "confirmed");
        // Live volume while dragging: sent in 2 % steps, not on every pixel
        panel.start_slider(R::new(10., 10., 200., 20.), 20.);
        assert!(panel.live_volume().is_some());
        panel.motion(21.);
        assert!(panel.live_volume().is_none(), "half a percent: nothing sent");
        panel.motion(30.);
        assert!(panel.live_volume().is_some());
        panel.release();
        panel.close();
        assert!(panel.selected.is_none());
        assert!(panel.password.is_empty());
    }

    #[test]
    fn hardware_keys_supersede_slider_preview_but_keep_brightness_drag() {
        let mut panel=Panel::default();
        let actual=Audio {default:"out".into(),sinks:vec![crate::controls::Sink {name:"out".into(),channels:vec![32768],..Default::default()}],..Default::default()};
        panel.audio=actual.clone();panel.start_slider(R::new(0.0,0.0,100.0,20.0),80.0);panel.release();
        assert_eq!(panel.volume(),0.8);
        panel.keyboard_audio_updated(actual.clone());assert_eq!(panel.volume(),0.5);
        panel.brightness=Some(0.3);panel.start_brightness(R::new(0.0,0.0,100.0,20.0),60.0);
        panel.keyboard_audio_updated(actual);assert_eq!(panel.shown_brightness(),Some(0.6));
    }

    #[test]
    fn password_editing_uses_masked_positions_selection_and_byte_limit() {
        use smithay::input::keyboard::keysyms as K;
        let mut panel = Panel::default();
        panel.page = Page::Password;
        panel.selected = Some(network());
        for ch in "старый пароль".chars() { panel.key(0, Some(ch), false, false); }
        panel.key(K::KEY_a, Some('a'), true, false);
        assert_eq!(panel.password.selected_text(), Some("старый пароль"));
        for ch in "new-password".chars() { panel.key(0, Some(ch), false, false); }
        let rect = R::new(0.0, 0.0, 324.0, 384.0);
        panel.click_password(rect, 28.0, 140.0, &mut FontSystem::new(), false);
        assert_eq!(panel.password.caret(), 0);
        panel.key(K::KEY_Right, None, false, true);
        assert_eq!(panel.password.selected_text(), Some("n"));
        panel.key(0, Some('N'), false, false);
        let action = panel.submit_password().unwrap();
        assert!(panel.password.is_empty());
        assert!(!format!("{action:?}").contains("New-password"));
        assert!(matches!(action, ShellClickAction::Control(Control::Connect { password: Some(p), .. }) if &*p == "New-password"));
        panel.open(Page::Password);
        for _ in 0..40 { panel.key(0, Some('я'), false, false); }
        assert_eq!(panel.password.len(), 64);
        panel.close();
        assert!(panel.password.is_empty());
    }
    #[test]
    fn every_output_can_be_scrolled_into_visible_hit_region() {
        let mut panel = Panel::default();
        panel.page = Page::Sound;
        panel.audio.sinks = (0..8)
            .map(|i| crate::controls::Sink {
                name: format!("sink-{i}"),
                ..Default::default()
            })
            .collect();
        // The full Control Center fits four outputs (headphones included next to three
        // wired ones), the painted height decides how far the list scrolls
        assert_eq!(panel.output_rows(384.0), 4);
        panel.sound_fit.set(panel.output_rows(384.0));
        for _ in 0..8 {
            panel.scroll(1.);
        }
        assert_eq!(panel.scroll, 4);
        assert!(
            matches!(panel.click(R::new(0.,0.,324.,384.),30.,260.),ShellClickAction::Control(Control::Output(name)) if name=="sink-7")
        );
        assert!(matches!(
            panel.click(R::new(0., 0., 324., 384.), 30., 300.),
            ShellClickAction::None
        ));
    }
    /// The menu bar speaker's panel: only Sound, no way back to the tiles, sized to its outputs
    #[test]
    fn speaker_panel_has_no_back_and_fits_outputs() {
        let mut panel = Panel::default();
        panel.open(Page::Sound);
        panel.sound_only = true;
        let sink = |name: &str| crate::controls::Sink { name: name.into(), label: name.into(), channels: vec![32768, 32768], ..Default::default() };
        panel.audio = Audio { sinks: vec![sink("a"), sink("b")], default: "a".into(), ..Default::default() };
        assert_eq!(panel.sound_height(), 151.0 + 2.0 * OUTPUT_ROW + 14.0);
        let p = R::new(100.0, 40.0, 300.0, panel.sound_height());
        assert!(matches!(panel.click(p, p.x + 20.0, p.y + 20.0), ShellClickAction::None));
        assert!(panel.page == Page::Sound && panel.sound_only);
        assert!(panel.hover_target(p).is_none());
        // The second output is still in reach
        assert!(matches!(panel.click(p, p.x + 60.0, p.y + 151.0 + OUTPUT_ROW + 10.0), ShellClickAction::Control(Control::Output(ref s)) if s == "b"));
        panel.close();
        assert!(!panel.sound_only && panel.page == Page::Main);
    }

    #[test]
    fn media_card_buttons_and_title() {
        use crate::mpris::{Command, Player};
        let r = R::new(0.0, 0.0, 300.0, 58.0);
        let mut player = Player { title: "Песня".into(), can_raise: true, ..Default::default() };
        assert_eq!(media_hit(&player, r, 300.0 - 92.0), Command::Previous);
        assert_eq!(media_hit(&player, r, 300.0 - 60.0), Command::PlayPause);
        assert_eq!(media_hit(&player, r, 300.0 - 28.0), Command::Next);
        assert_eq!(media_hit(&player, r, 40.0), Command::Raise);
        player.can_raise = false;
        assert_eq!(media_hit(&player, r, 40.0), Command::PlayPause);
    }

    #[test]
    fn bluetooth_page_toggles_scans_and_connects_devices() {
        use crate::controls::bluetooth::BtDevice;
        let mut panel = Panel::default();
        panel.page = Page::Bluetooth;
        panel.bluetooth = Bluetooth {
            available: true,
            powered: true,
            discovering: false,
            devices: vec![
                BtDevice {
                    path: "/org/bluez/hci0/dev_1".into(),
                    name: "AirPods Pro".into(),
                    paired: true,
                    connected: true,
                    battery: Some(85),
                    ..Default::default()
                },
                BtDevice {
                    path: "/org/bluez/hci0/dev_2".into(),
                    name: "Magic Keyboard".into(),
                    paired: true,
                    connected: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let rect = R::new(0., 0., 324., 384.);
        assert!(matches!(
            panel.click(rect, 300., 25.),
            ShellClickAction::Control(Control::Bluetooth(false))
        ));
        assert!(matches!(
            panel.click(rect, 50., 65.),
            ShellClickAction::Control(Control::ScanBluetooth)
        ));
        assert!(matches!(
            panel.click(rect, 50., 105.),
            ShellClickAction::Control(Control::DisconnectBluetooth(p)) if p == "/org/bluez/hci0/dev_1"
        ));
        assert!(matches!(
            panel.click(rect, 50., 143.),
            ShellClickAction::Control(Control::ConnectBluetooth(p)) if p == "/org/bluez/hci0/dev_2"
        ));
        assert_eq!(panel.bluetooth.connecting.as_deref(), Some("/org/bluez/hci0/dev_2"));
        assert!(matches!(
            panel.click(rect, 50., 143.),
            ShellClickAction::Control(Control::DisconnectBluetooth(p)) if p == "/org/bluez/hci0/dev_2"
        ));
        assert_eq!(panel.bluetooth.connecting, None);
        assert!(matches!(
            panel.click(rect, 50., 360.),
            ShellClickAction::BluetoothSettings
        ));
    }
    #[test]
    #[ignore = "Explicit fixture screenshots; no system controls are touched"]
    fn render_fixtures() {
        with_render_settings(2.0, Appearance::Light, || {
            let mut panel = Panel::default();
            panel.wifi = Wifi {
                available: true,
                enabled: true,
                hardware: true,
                networks: vec![network()],
                ..Default::default()
            };
            panel.bluetooth = Bluetooth {
                available: true,
                powered: true,
                discovering: false,
                connected: Some("AirPods Pro".into()),
                devices: vec![crate::controls::bluetooth::BtDevice {
                    path: "/org/bluez/hci0/dev_1".into(),
                    name: "AirPods Pro".into(),
                    icon: "audio-headphones".into(),
                    paired: true,
                    connected: true,
                    battery: Some(85),
                    ..Default::default()
                }],
                ..Default::default()
            };
            panel.audio = Audio {
                default: "test".into(),
                sinks: vec![crate::controls::Sink {
                    name: "test".into(),
                    label: "USB · Наушники".into(),
                    channels: vec![40000, 40000],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut fonts = FontSystem::new();
            for (name, page) in [
                ("wifi", Page::Wifi),
                ("bluetooth", Page::Bluetooth),
                ("sound", Page::Sound),
                ("password", Page::Password),
            ] {
                panel.page = page;
                panel.selected = Some(network());
                panel.password.insert("example");
                std::fs::create_dir_all("/tmp/feather-controls").unwrap();
                let mut image = tiny_skia::Pixmap::new(648, 768).unwrap();
                panel.paint(&mut image.as_mut(), &mut fonts, R::new(0., 0., 324., 384.));
                image
                    .save_png(format!("/tmp/feather-controls/fixture-{name}.png"))
                    .unwrap();
                panel.password.clear();
            }
            // The menu bar speaker's panel and the «Сейчас исполняется» card
            panel.page = Page::Sound;
            panel.sound_only = true;
            panel.audio.sinks.push(crate::controls::Sink { name: "hdmi".into(), label: "HDMI · Монитор".into(), channels: vec![30000, 30000], ..Default::default() });
            let h = panel.sound_height();
            let mut image = tiny_skia::Pixmap::new(600, (h * 2.0) as u32).unwrap();
            panel.paint(&mut image.as_mut(), &mut fonts, R::new(0., 0., 300., h));
            image.save_png("/tmp/feather-controls/fixture-speaker.png").unwrap();
            let player = crate::mpris::Player {
                identity: "Rhythmbox".into(),
                title: "Тяжелые Времена".into(),
                artist: "Цифей".into(),
                playing: true,
                can_next: true,
                can_previous: false,
                ..Default::default()
            };
            let mut image = tiny_skia::Pixmap::new(600, 260).unwrap();
            for (i, p) in [Some(&player), None].into_iter().enumerate() {
                let r = R::new(0., i as f32 * 66.0, 300., 58.);
                fill_rounded_rect(&mut image.as_mut(), r.x, r.y, r.w, r.h, 14.0, rgba(235, 235, 240, 255));
                paint_media(&mut image.as_mut(), &mut fonts, p, r);
            }
            image.save_png("/tmp/feather-controls/fixture-media.png").unwrap();
        });
    }

    #[test]
    fn per_app_mixer_mutes_routes_and_slides_streams() {
        let mut panel = Panel::default();
        panel.open(Page::Sound);
        panel.sound_only = true;
        let sink = |name: &str, index: u32| crate::controls::Sink {
            name: name.into(),
            label: name.into(),
            index,
            channels: vec![65536, 65536],
            ..Default::default()
        };
        panel.audio = Audio {
            sinks: vec![sink("speakers", 1), sink("usb", 2)],
            default: "speakers".into(),
            streams: vec![crate::controls::AudioStream {
                index: 42,
                name: "Google Chrome".into(),
                sink_index: 1,
                sink_name: "speakers".into(),
                channels: vec![32768, 32768],
                muted: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = R::new(50.0, 36.0, 300.0, panel.sound_height());
        let streams_y0 = p.y + 151.0 + 2.0 * OUTPUT_ROW + STREAM_HEADER;
        // Left speaker icon toggles stream mute
        assert!(matches!(
            panel.click(p, p.x + 24.0, streams_y0 + 10.0),
            ShellClickAction::Control(Control::StreamMute { index: 42, muted: true })
        ));
        // Right pill cycles output sink for this stream
        assert!(matches!(
            panel.click(p, p.x + p.w - 40.0, streams_y0 + 10.0),
            ShellClickAction::Control(Control::StreamOutput { index: 42, ref sink }) if sink == "usb"
        ));
        // Bottom slider drags and releases per-stream volume
        let slider_mid_x = p.x + 18.0 + (p.w - 64.0) * 0.75;
        assert!(matches!(panel.click(p, slider_mid_x, streams_y0 + 34.0), ShellClickAction::None));
        assert!(matches!(
            panel.release(),
            Some(ShellClickAction::Control(Control::StreamVolume { index: 42, value })) if (value - 0.75).abs() < 0.01
        ));
    }
}
