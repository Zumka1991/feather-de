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
    pub error: Option<String>,
    password: feather_ui::input::TextInput,
    selected: Option<Network>,
    drag: Option<(Slider, R, f32)>,
    /// Pointer (global logical) while it is over the panel
    pub hover: Option<(f32, f32)>,
    /// Display brightness 0…1; None when no display can be controlled
    pub brightness: Option<f32>,
    /// Volume set by the last drag, shown until the sound server reports it (no jump back)
    pending_volume: Option<(String, f32, u8)>,
    /// Last volume sent while dragging (sent again only after a 2 % change)
    sent_volume: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
enum Slider {
    Volume(String),
    Brightness,
}
impl Panel {
    pub fn close(&mut self) {
        self.page = Page::Main;
        self.scroll = 0;
        self.password.clear();
        self.selected = None;
        self.drag = None;
        self.error = None;
    }
    pub fn open(&mut self, page: Page) {
        self.password.clear();
        self.selected = None;
        self.page = page;
        self.scroll = 0;
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
    pub fn start_slider(&mut self, rect: R, x: f32) {
        if let Some(sink) = self.audio.current() {
            self.drag = Some((
                Slider::Volume(sink.name.clone()),
                rect,
                ((x - rect.x) / rect.w).clamp(0.0, 1.0),
            ));
        }
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
        let (Slider::Volume(sink), _, value) = self.drag.as_ref()? else { return None };
        if self.sent_volume.is_some_and(|v| (v - value).abs() < 0.02) {
            return None;
        }
        self.sent_volume = Some(*value);
        Some(ShellClickAction::Control(Control::Volume { sink: sink.clone(), value: *value }))
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
            Slider::Brightness => ShellClickAction::SetBrightness(value),
        })
    }
    pub fn scroll(&mut self, delta: f64) {
        let max = match self.page {
            Page::Wifi => self.wifi.networks.len().saturating_sub(5),
            Page::Bluetooth => self.bluetooth.devices.len().saturating_sub(5),
            Page::Sound => self.audio.sinks.len().saturating_sub(3),
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
        if y < 48.0 && x < 55.0 {
            return Some((1, R::new(p.x + 8.0, p.y + 9.0, 36.0, 32.0)));
        }
        match self.page {
            Page::Password => (y > p.h - 65.0).then(|| (5, row(p.h - 60.0, 40.0))),
            _ if y > p.h - 43.0 => matches!(self.page, Page::Wifi | Page::Bluetooth).then(|| (4, row(p.h - 42.0, 34.0))),
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
                if (151.0..259.0).contains(&y) {
                    let i = ((y - 151.0) / 36.0) as usize;
                    if self.audio.sinks.len() > i + self.scroll {
                        return Some((200 + (i + self.scroll) as u32, row(149.0 + i as f32 * 36.0, 34.0)));
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
        if y < 48.0 && x < 55.0 {
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
        if y > p.h - 43.0 {
            return match self.page {
                Page::Wifi => ShellClickAction::NetworkSettings,
                Page::Bluetooth => ShellClickAction::BluetoothSettings,
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
                    if d.connected {
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
            }
            if (151.0..259.0).contains(&y) {
                let i = ((y - 151.0) / 36.0) as usize + self.scroll;
                if let Some(sink) = self.audio.sinks.get(i) {
                    return ShellClickAction::Control(Control::Output(sink.name.clone()));
                }
            }
        }
        ShellClickAction::None
    }
    pub fn paint(&self, pm: &mut PixmapMut, fonts: &mut FontSystem, p: R) {
        let pal = palette();
        draw_glass_panel(pm, p.x, p.y, p.w, p.h, 22.0, false);
        if let Some((_, r)) = self.hover_target(p) {
            let tint = if appearance() == Appearance::Dark { rgba(255, 255, 255, 22) } else { rgba(0, 0, 0, 16) };
            fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 8.0, tint);
        }
        fonts.draw_text(pm, "‹", p.x + 16.0, p.y + 10.0, 24.0, pal.text, false);
        let title = match self.page {
            Page::Wifi => "Wi‑Fi",
            Page::Bluetooth => "Bluetooth",
            Page::Sound => "Звук",
            Page::Password => "Подключение",
            _ => "",
        };
        fonts.draw_text(pm, title, p.x + 48.0, p.y + 17.0, 15.0, pal.text, true);
        if self.page == Page::Wifi {
            let can = self.wifi.available && self.wifi.hardware;
            fill_rounded_rect(
                pm,
                p.x + p.w - 59.0,
                p.y + 15.0,
                40.0,
                22.0,
                11.0,
                if can && self.wifi.enabled {
                    pal.accent
                } else {
                    pal.control_fill
                },
            );
            fill_circle(
                pm,
                p.x + p.w - if self.wifi.enabled { 29.0 } else { 49.0 },
                p.y + 26.0,
                8.0,
                pal.on_accent,
            );
            fonts.draw_text_clipped(
                pm,
                if self.wifi.enabled && self.wifi.available && self.wifi.hardware {
                    "Обновить сети ↻"
                } else {
                    self.wifi.subtitle()
                },
                p.x + 18.0,
                p.y + 58.0,
                p.w - 36.0,
                12.0,
                pal.text_secondary,
                false,
            );
            if self.wifi.enabled && self.wifi.available {
                for (row, n) in self
                    .wifi
                    .networks
                    .iter()
                    .skip(self.scroll)
                    .take(5)
                    .enumerate()
                {
                    let y = p.y + 88.0 + row as f32 * 38.0;
                    if y + 38.0 > p.y + p.h - 90.0 {
                        break;
                    }
                    if n.active {
                        fill_rounded_rect(
                            pm,
                            p.x + 10.0,
                            y,
                            p.w - 20.0,
                            34.0,
                            8.0,
                            with_alpha(pal.accent, 0.14),
                        );
                    }
                    draw_wifi_icon(pm, p.x + 28.0, y + 17.0, pal.text, true);
                    fonts.draw_text_clipped(
                        pm,
                        &n.label,
                        p.x + 49.0,
                        y + 3.0,
                        p.w - 104.0,
                        13.0,
                        pal.text,
                        n.active,
                    );
                    let sub = if n.active {
                        "Подключено · нажмите, чтобы отключить"
                    } else if n.saved.is_some() {
                        "Сохранённая сеть"
                    } else if n.security == Security::Open {
                        "Открытая сеть"
                    } else {
                        "Защищённая сеть"
                    };
                    fonts.draw_text_clipped(
                        pm,
                        sub,
                        p.x + 49.0,
                        y + 19.0,
                        p.w - 69.0,
                        10.0,
                        pal.text_secondary,
                        false,
                    );
                    fonts.draw_text_right(
                        pm,
                        &format!("{}%", n.strength),
                        p.x + p.w - 18.0,
                        y + 4.0,
                        10.0,
                        pal.text_secondary,
                        false,
                    );
                }
                if self.wifi.networks.is_empty() {
                    fonts.draw_text(
                        pm,
                        "Сети пока не найдены",
                        p.x + 18.0,
                        p.y + 100.0,
                        12.0,
                        pal.text_secondary,
                        false,
                    );
                }
            }
            fonts.draw_text(
                pm,
                "Настройки сети…",
                p.x + 18.0,
                p.y + p.h - 29.0,
                12.0,
                pal.accent,
                false,
            );
        } else if self.page == Page::Bluetooth {
            let can = self.bluetooth.available;
            fill_rounded_rect(
                pm,
                p.x + p.w - 59.0,
                p.y + 15.0,
                40.0,
                22.0,
                11.0,
                if can && self.bluetooth.powered {
                    pal.accent
                } else {
                    pal.control_fill
                },
            );
            fill_circle(
                pm,
                p.x + p.w - if self.bluetooth.powered { 29.0 } else { 49.0 },
                p.y + 26.0,
                8.0,
                pal.on_accent,
            );
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
                pal.text_secondary,
                false,
            );
            if self.bluetooth.powered && self.bluetooth.available {
                for (row, d) in self
                    .bluetooth
                    .devices
                    .iter()
                    .skip(self.scroll)
                    .take(5)
                    .enumerate()
                {
                    let y = p.y + 88.0 + row as f32 * 38.0;
                    if y + 38.0 > p.y + p.h - 90.0 {
                        break;
                    }
                    if d.connected {
                        fill_rounded_rect(
                            pm,
                            p.x + 10.0,
                            y,
                            p.w - 20.0,
                            34.0,
                            8.0,
                            with_alpha(pal.accent, 0.14),
                        );
                    }
                    icon(pm, Icon::Bluetooth, p.x + 28.0, y + 17.0, pal.text, false);
                    fonts.draw_text_clipped(
                        pm,
                        &d.name,
                        p.x + 49.0,
                        y + 3.0,
                        p.w - 104.0,
                        13.0,
                        pal.text,
                        d.connected,
                    );
                    let sub = if d.connected {
                        "Подключено · нажмите, чтобы отключить"
                    } else if self.bluetooth.connecting.as_deref() == Some(d.path.as_str()) {
                        "Подключение…"
                    } else if d.paired {
                        "Сопряжено"
                    } else {
                        d.kind_label()
                    };
                    fonts.draw_text_clipped(
                        pm,
                        sub,
                        p.x + 49.0,
                        y + 19.0,
                        p.w - 69.0,
                        10.0,
                        pal.text_secondary,
                        false,
                    );
                    if let Some(battery) = d.battery {
                        fonts.draw_text_right(
                            pm,
                            &format!("{battery}%"),
                            p.x + p.w - 18.0,
                            y + 4.0,
                            10.0,
                            pal.text_secondary,
                            false,
                        );
                    }
                }
                if self.bluetooth.devices.is_empty() {
                    fonts.draw_text(
                        pm,
                        "Устройства пока не найдены",
                        p.x + 18.0,
                        p.y + 100.0,
                        12.0,
                        pal.text_secondary,
                        false,
                    );
                }
            }
            fonts.draw_text(
                pm,
                "Настройки Bluetooth…",
                p.x + 18.0,
                p.y + p.h - 29.0,
                12.0,
                pal.accent,
                false,
            );
        } else if self.page == Page::Sound {
            icon(
                pm,
                Icon::Speaker,
                p.x + 28.0,
                p.y + 68.0,
                pal.text,
                self.audio.current().is_some_and(|s| s.muted),
            );
            let muted = self.audio.current().is_some_and(|s| s.muted);
            fonts.draw_text(
                pm,
                if muted {
                    "Включить звук"
                } else {
                    "Выключить звук"
                },
                p.x + 50.0,
                p.y + 57.0,
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
                p.y + 129.0,
                12.0,
                pal.text_secondary,
                true,
            );
            for (row, sink) in self
                .audio
                .sinks
                .iter()
                .skip(self.scroll)
                .take(3)
                .enumerate()
            {
                let y = p.y + 151.0 + row as f32 * 36.0;
                if y + 36.0 > p.y + p.h - 90.0 {
                    break;
                }
                let active = sink.name == self.audio.default;
                if active {
                    fill_rounded_rect(
                        pm,
                        p.x + 10.0,
                        y,
                        p.w - 20.0,
                        32.0,
                        8.0,
                        with_alpha(pal.accent, 0.14),
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
                    fonts.draw_text(pm, "✓", p.x + p.w - 31.0, y + 7.0, 13.0, pal.accent, true);
                }
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
                9.0,
                pal.control_fill,
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
                10.0,
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
                    p.y + p.h - 74.0,
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
pub fn slider(pm: &mut PixmapMut, r: R, value: f32, enabled: bool) {
    let p = palette();
    fill_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, p.control_fill);
    if enabled {
        fill_rounded_rect(
            pm,
            r.x,
            r.y,
            (r.w * value).max(r.h),
            r.h,
            r.h / 2.0,
            p.accent,
        );
    }
    stroke_rounded_rect(pm, r.x, r.y, r.w, r.h, r.h / 2.0, 0.5, p.glass_outline);
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
    let line = |pm: &mut PixmapMut, a: f32, b: f32, c: f32, d: f32| {
        stroke_line(pm, x + a, y + b, x + c, y + d, 1.5, color)
    };
    match kind {
        Icon::Wifi => draw_wifi_icon(pm, x, y, color, true),
        Icon::Bluetooth => {
            for (a, b, c, d) in [
                (0., -10., 0., 10.),
                (0., -10., 6., -5.),
                (6., -5., -6., 5.),
                (-6., -5., 6., 5.),
                (6., 5., 0., 10.),
            ] {
                line(pm, a, b, c, d);
            }
        }
        Icon::Moon => {
            let mut path = tiny_skia::PathBuilder::new();
            path.move_to(x + 4.0, y - 7.0);
            path.cubic_to(x - 10.0, y - 10.0, x - 10.0, y + 10.0, x + 4.0, y + 7.0);
            path.cubic_to(x - 4.0, y + 5.0, x - 4.0, y - 5.0, x + 4.0, y - 7.0);
            path.close();
            if let Some(path) = path.finish() {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color(color);
                paint.anti_alias = true;
                pm.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::from_scale(ui_scale(), ui_scale()),
                    None,
                );
            }
        }
        Icon::Contrast => {
            stroke_rounded_rect(pm, x - 8., y - 8., 16., 16., 8., 1.5, color);
            fill_rounded_rect(pm, x - 6., y - 6., 6., 12., 2., color);
        }
        Icon::Sun => {
            stroke_rounded_rect(pm, x - 4., y - 4., 8., 8., 4., 1.5, color);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                line(pm, a.cos() * 7., a.sin() * 7., a.cos() * 10., a.sin() * 10.);
            }
        }
        Icon::Speaker => {
            for (a, b, c, d) in [
                (-9., -3., -5., -3.),
                (-5., -3., 1., -8.),
                (1., -8., 1., 8.),
                (1., 8., -5., 3.),
                (-5., 3., -9., 3.),
                (-9., 3., -9., -3.),
            ] {
                line(pm, a, b, c, d);
            }
            if muted {
                line(pm, 5., -4., 11., 4.);
                line(pm, 5., 4., 11., -4.);
            } else {
                line(pm, 5., -5., 8., 0.);
                line(pm, 8., 0., 5., 5.);
            }
        }
        Icon::Scale => {
            stroke_rounded_rect(pm, x - 8., y - 7., 16., 14., 2., 1.5, color);
            line(pm, -4., 3., 4., -3.);
            line(pm, 0., -3., 4., -3.);
            line(pm, 4., -3., 4., 1.);
        }
        Icon::Wallpaper => {
            stroke_rounded_rect(pm, x - 9., y - 7., 18., 14., 2., 1.5, color);
            line(pm, -7., 5., -1., -1.);
            line(pm, -1., -1., 2., 2.);
            line(pm, 2., 2., 6., -3.);
            fill_circle(pm, x - 4., y - 3., 1.5, color);
        }
        Icon::Keyboard => {
            stroke_rounded_rect(pm, x - 10., y - 6., 20., 12., 2., 1.5, color);
            for i in -2..=2 {
                fill_circle(pm, x + i as f32 * 3., y - 2., 0.8, color);
            }
            line(pm, -5., 2., 5., 2.);
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
        for _ in 0..8 {
            panel.scroll(1.);
        }
        assert!(
            matches!(panel.click(R::new(0.,0.,324.,384.),30.,240.),ShellClickAction::Control(Control::Output(name)) if name=="sink-7")
        );
        assert!(matches!(
            panel.click(R::new(0., 0., 324., 384.), 30., 280.),
            ShellClickAction::None
        ));
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
        });
    }
}
