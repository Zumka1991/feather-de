//! Control Center view model: service snapshots, detail pages and local slider preview.
use crate::{
    controls::{
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
    Sound,
    Password,
}
#[derive(Default)]
pub struct Panel {
    pub wifi: Wifi,
    pub audio: Audio,
    pub page: Page,
    pub scroll: usize,
    pub error: Option<String>,
    password: crate::controls::Secret,
    selected: Option<Network>,
    drag: Option<(String, R, f32)>,
}
impl Panel {
    pub fn close(&mut self) {
        self.page = Page::Main;
        self.scroll = 0;
        self.password.zeroize();
        self.selected = None;
        self.drag = None;
        self.error = None;
    }
    pub fn open(&mut self, page: Page) {
        self.password.zeroize();
        self.selected = None;
        self.page = page;
        self.scroll = 0;
        self.error = None;
    }
    pub fn volume(&self) -> f32 {
        self.drag.as_ref().map(|d| d.2).unwrap_or_else(|| {
            self.audio
                .current()
                .map_or(0.0, |s| s.volume().clamp(0.0, 1.0))
        })
    }
    pub fn start_slider(&mut self, rect: R, x: f32) {
        if let Some(sink) = self.audio.current() {
            self.drag = Some((
                sink.name.clone(),
                rect,
                ((x - rect.x) / rect.w).clamp(0.0, 1.0),
            ));
        }
    }
    pub fn motion(&mut self, x: f32) -> bool {
        let Some((_, r, value)) = &mut self.drag else {
            return false;
        };
        *value = ((x - r.x) / r.w).clamp(0.0, 1.0);
        true
    }
    pub fn release(&mut self) -> Option<ShellClickAction> {
        self.drag
            .take()
            .map(|(sink, _, value)| ShellClickAction::Control(Control::Volume { sink, value }))
    }
    pub fn scroll(&mut self, delta: f64) {
        let max = match self.page {
            Page::Wifi => self.wifi.networks.len().saturating_sub(5),
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
    pub fn key(&mut self, sym: u32, ch: Option<char>) -> Option<ShellClickAction> {
        use smithay::input::keyboard::keysyms;
        match sym {
            keysyms::KEY_Escape => {
                self.open(Page::Wifi);
            }
            keysyms::KEY_BackSpace => {
                self.password.pop();
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => return self.submit_password(),
            _ => {
                if let Some(c) = ch.filter(|c| !c.is_control()) {
                    if self.password.len() + c.len_utf8() <= 64 {
                        self.password.push(c);
                    }
                }
            }
        }
        None
    }
    fn submit_password(&mut self) -> Option<ShellClickAction> {
        if !crate::controls::network::valid_password(
            self.selected.as_ref()?.security,
            &self.password,
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
        let password = std::mem::take(&mut self.password);
        self.page = Page::Wifi;
        Some(ShellClickAction::Control(Control::Connect {
            network: n,
            password: Some(password),
        }))
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
            return if self.page == Page::Wifi {
                ShellClickAction::NetworkSettings
            } else {
                ShellClickAction::None
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
                        self.password.zeroize();
                        self.page = Page::Password;
                        return ShellClickAction::None;
                    }
                    return ShellClickAction::NetworkSettings;
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
        fonts.draw_text(pm, "‹", p.x + 16.0, p.y + 10.0, 24.0, pal.text, false);
        let title = match self.page {
            Page::Wifi => "Wi‑Fi",
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
            fonts.draw_text_clipped(
                pm,
                &"•".repeat(self.password.chars().count()),
                p.x + 28.0,
                p.y + 139.0,
                p.w - 56.0,
                13.0,
                pal.text,
                false,
            );
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
            let err = self.error.as_deref().or(if self.page == Page::Wifi {
                self.wifi.error.as_deref()
            } else {
                self.audio.error.as_deref()
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
        panel.password.push_str("short");
        assert!(panel.submit_password().is_none());
        assert!(panel.page == Page::Password);
        assert!(panel.selected.is_some());
        panel.password.zeroize();
        panel.password.push_str("valid password");
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
            error: None,
        };
        panel.start_slider(R::new(10., 10., 200., 20.), 20.);
        panel.motion(160.);
        assert_eq!(panel.audio.current().unwrap().volume(), 0.5);
        assert_eq!(panel.volume(), 0.75);
        assert!(
            matches!(panel.release(),Some(ShellClickAction::Control(Control::Volume {value,..})) if value==0.75)
        );
        assert!(panel.release().is_none());
        panel.close();
        assert!(panel.selected.is_none());
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
            panel.audio = Audio {
                default: "test".into(),
                sinks: vec![crate::controls::Sink {
                    name: "test".into(),
                    label: "USB · Наушники".into(),
                    channels: vec![40000, 40000],
                    ..Default::default()
                }],
                error: None,
            };
            let mut fonts = FontSystem::new();
            for (name, page) in [
                ("wifi", Page::Wifi),
                ("sound", Page::Sound),
                ("password", Page::Password),
            ] {
                panel.page = page;
                panel.selected = Some(network());
                panel.password.push_str("example");
                std::fs::create_dir_all("/tmp/feather-controls").unwrap();
                let mut image = tiny_skia::Pixmap::new(648, 768).unwrap();
                panel.paint(&mut image.as_mut(), &mut fonts, R::new(0., 0., 324., 384.));
                image
                    .save_png(format!("/tmp/feather-controls/fixture-{name}.png"))
                    .unwrap();
                panel.password.zeroize();
            }
        });
    }
}
