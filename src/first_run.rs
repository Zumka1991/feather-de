//! First-run Welcome & Setup Assistant («Добро пожаловать в Пёрышко») in macOS Tahoe style.
//!
//! Guides the user through 4 steps on first launch (or when opened from the Feather menu):
//! 1. Language & Region («Язык и регион»)
//! 2. Appearance, Accent Color & Text Size («Оформление»)
//! 3. Keyboard Layouts & Switching Shortcut («Клавиатура и ввод»)
//! 4. Ready to Work («Готово к работе»)

use tiny_skia::PixmapMut;

use crate::{
    settings::{Shortcut, SOURCES},
    text::FontSystem,
    theme::{
        accent_color, appearance, draw_feather_logo, fill_circle, fill_rounded_rect,
        fill_squircle_gradient, palette, rgba, stroke_circle, stroke_line, stroke_rounded_rect,
        text_size, AccentColor, Appearance, Lang, Region, TextSize,
    },
};

pub const STEP_COUNT: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Step(usize),
    Next,
    Back,
    Finish,
    Language(Lang),
    Region(Region),
    Toggle24h,
    Appearance(Appearance),
    Accent(AccentColor),
    TextSize(TextSize),
    Shortcut(Shortcut),
    ToggleLayout(&'static str),
}

#[derive(Clone, Debug, Default)]
pub struct Wizard {
    pub step: usize,
}

const OFFERED_LAYOUTS: [&str; 8] = ["us", "ru", "de", "fr", "es", "gb", "ua", "by"];

const fn lang_badge(l: Lang) -> &'static str {
    match l {
        Lang::Ru => "RU",
        Lang::En => "EN",
        Lang::Es => "ES",
        Lang::De => "DE",
        Lang::Fr => "FR",
        Lang::Zh => "ZH",
    }
}

impl Wizard {
    pub fn paint(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        (cx, cy, cw, ch): (f32, f32, f32, f32),
        ctx: &crate::system_settings::Context<'_>,
    ) {
        let p = palette();
        let dark = appearance() == Appearance::Dark;

        // Top hero header with Feather squircle emblem & step dots
        let emblem = 52.0;
        let ex = cx + cw * 0.5 - emblem * 0.5;
        let ey = cy + 14.0;
        fill_squircle_gradient(
            pm,
            ex,
            ey,
            emblem,
            rgba(120, 205, 255, 255),
            rgba(20, 105, 235, 255),
        );
        draw_feather_logo(
            pm,
            cx + cw * 0.5,
            ey + emblem * 0.5,
            2.1,
            rgba(255, 255, 255, 255),
        );

        let (title, subtitle) = match self.step {
            0 => (
                "Добро пожаловать в Пёрышко",
                "Выберите язык интерфейса и региональные форматы.",
            ),
            1 => (
                "Выберите тему оформления",
                "Настройте светлый или тёмный вид, цвет акцента и размер текста.",
            ),
            2 => (
                "Раскладка клавиатуры",
                "Выберите используемые языки ввода и клавиши переключения.",
            ),
            _ => (
                "Пёрышко готово к работе",
                "Ваше окружение настроено — быстрый обзор главных возможностей.",
            ),
        };

        fonts.draw_text_centered(pm, title, cx + cw * 0.5, cy + 74.0, 20.0, p.text, true);
        fonts.draw_text_centered(
            pm,
            subtitle,
            cx + cw * 0.5,
            cy + 99.0,
            12.5,
            p.text_secondary,
            false,
        );

        // 4 step dots (clickable)
        let (ar, ag, ab) = accent_color().rgb(appearance());
        let dots_w = (STEP_COUNT as f32) * 18.0;
        let dots_x0 = cx + cw * 0.5 - dots_w * 0.5 + 9.0;
        let dots_y = cy + 124.0;
        for i in 0..STEP_COUNT {
            let dx = dots_x0 + i as f32 * 18.0;
            if i == self.step {
                fill_rounded_rect(pm, dx - 7.0, dots_y - 3.5, 14.0, 7.0, 3.5, p.accent);
            } else {
                let col = if i < self.step {
                    rgba(ar, ag, ab, 140)
                } else {
                    p.separator
                };
                fill_circle(pm, dx, dots_y, 3.5, col);
            }
        }

        let body_x = cx + 24.0;
        let body_y = cy + 140.0;
        let body_w = cw - 48.0;
        let body_h = ch - 196.0;

        match self.step {
            0 => self.paint_step_language(pm, fonts, (body_x, body_y, body_w, body_h), dark),
            1 => self.paint_step_appearance(pm, fonts, (body_x, body_y, body_w, body_h)),
            2 => self.paint_step_keyboard(pm, fonts, (body_x, body_y, body_w, body_h), ctx, dark),
            _ => self.paint_step_ready(pm, fonts, (body_x, body_y, body_w, body_h), ctx, dark),
        }

        // Bottom navigation bar
        let foot_y = cy + ch - 46.0;
        stroke_line(
            pm,
            cx + 20.0,
            foot_y - 8.0,
            cx + cw - 20.0,
            foot_y - 8.0,
            1.0,
            p.separator,
        );

        if self.step > 0 {
            let (bx, by, bw, bh) = (cx + 24.0, foot_y, 96.0, 30.0);
            fill_rounded_rect(pm, bx, by, bw, bh, bh * 0.5, p.control_fill);
            stroke_rounded_rect(pm, bx, by, bw, bh, bh * 0.5, 1.0, p.glass_outline);
            fonts.draw_text_centered(pm, "Назад", bx + bw * 0.5, by + 7.0, 13.0, p.text, false);
        } else {
            let step_lbl = format!(
                "{} 1 / {}",
                feather_ui::i18n::tr("Шаг"),
                STEP_COUNT
            );
            fonts.draw_text(
                pm,
                &step_lbl,
                cx + 24.0,
                foot_y + 7.0,
                12.0,
                p.text_tertiary,
                false,
            );
        }

        if self.step + 1 < STEP_COUNT {
            // Skip button + Continue primary button
            let (sx, sy, sw, sh) = (cx + cw - 266.0, foot_y, 104.0, 30.0);
            fill_rounded_rect(pm, sx, sy, sw, sh, sh * 0.5, p.control_fill);
            stroke_rounded_rect(pm, sx, sy, sw, sh, sh * 0.5, 1.0, p.glass_outline);
            fonts.draw_text_centered(
                pm,
                "Пропустить",
                sx + sw * 0.5,
                sy + 7.0,
                12.5,
                p.text_secondary,
                false,
            );

            let (nx, ny, nw, nh) = (cx + cw - 152.0, foot_y, 128.0, 30.0);
            fill_rounded_rect(pm, nx, ny, nw, nh, nh * 0.5, p.accent);
            fonts.draw_text_centered(
                pm,
                "Продолжить",
                nx + nw * 0.5,
                ny + 7.0,
                13.0,
                p.on_accent,
                true,
            );
        } else {
            let (nx, ny, nw, nh) = (cx + cw - 172.0, foot_y, 148.0, 30.0);
            fill_rounded_rect(pm, nx, ny, nw, nh, nh * 0.5, p.accent);
            fonts.draw_text_centered(
                pm,
                "Начать работу",
                nx + nw * 0.5,
                ny + 7.0,
                13.0,
                p.on_accent,
                true,
            );
        }
    }

    fn paint_step_language(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        (x, y, w, _h): (f32, f32, f32, f32),
        dark: bool,
    ) {
        let p = palette();
        let (ar, ag, ab) = accent_color().rgb(appearance());
        let active_lang = crate::theme::lang();
        let active_region = crate::theme::region();
        let use_24h = crate::theme::use_24h_time();

        fonts.draw_text(
            pm,
            "Язык интерфейса",
            x + 4.0,
            y,
            12.5,
            p.text_secondary,
            true,
        );

        let cols = 2usize;
        let gap = 10.0;
        let card_w = (w - gap) / 2.0;
        let card_h = 38.0;
        let grid_y = y + 20.0;

        for (i, &lang) in Lang::ALL.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let cx = x + col as f32 * (card_w + gap);
            let cy = grid_y + row as f32 * (card_h + 8.0);
            let selected = lang == active_lang;
            let fill = if selected {
                rgba(ar, ag, ab, if dark { 58 } else { 34 })
            } else {
                p.card
            };
            fill_rounded_rect(pm, cx, cy, card_w, card_h, 10.0, fill);
            stroke_rounded_rect(
                pm,
                cx,
                cy,
                card_w,
                card_h,
                10.0,
                if selected { 1.5 } else { 1.0 },
                if selected { p.accent } else { p.glass_outline },
            );

            // Badge pill
            let badge_col = if selected { p.accent } else { p.control_fill };
            fill_rounded_rect(pm, cx + 10.0, cy + 8.0, 30.0, 22.0, 6.0, badge_col);
            fonts.draw_text_centered(
                pm,
                lang_badge(lang),
                cx + 25.0,
                cy + 12.5,
                11.0,
                if selected { p.on_accent } else { p.text },
                true,
            );

            fonts.draw_text_clipped(
                pm,
                lang.full_label(),
                cx + 48.0,
                cy + 11.0,
                card_w - 58.0,
                13.0,
                p.text,
                selected,
            );
        }

        // Region row
        let reg_y = grid_y + 3.0 * (card_h + 8.0) + 6.0;
        fonts.draw_text(
            pm,
            "Регион и формат времени",
            x + 4.0,
            reg_y,
            12.5,
            p.text_secondary,
            true,
        );

        let pill_y = reg_y + 20.0;
        let rcols = 4usize;
        let r_w = (w - 3.0 * 8.0) / 4.0;
        let r_h = 28.0;
        for (i, &reg) in Region::ALL.iter().enumerate() {
            let c = i % rcols;
            let r = i / rcols;
            let rx = x + c as f32 * (r_w + 8.0);
            let ry = pill_y + r as f32 * (r_h + 6.0);
            let sel = reg == active_region;
            fill_rounded_rect(
                pm,
                rx,
                ry,
                r_w,
                r_h,
                8.0,
                if sel { p.accent } else { p.card },
            );
            if !sel {
                stroke_rounded_rect(pm, rx, ry, r_w, r_h, 8.0, 1.0, p.glass_outline);
            }
            fonts.draw_text_clipped_centered(
                pm,
                reg.label(),
                rx + r_w * 0.5,
                ry + 6.5,
                r_w - 10.0,
                12.0,
                if sel { p.on_accent } else { p.text },
                sel,
            );
        }

        // 24-hour time toggle row
        let t_y = pill_y + 2.0 * (r_h + 6.0) + 6.0;
        let sample = feather_ui::i18n::format_topbar_clock(6, 25, 9, 14, 30);
        fonts.draw_text(pm, &sample, x + 4.0, t_y + 4.0, 12.0, p.text_secondary, false);

        let (tw, th) = (168.0, 24.0);
        let tx = x + w - tw;
        fill_rounded_rect(
            pm,
            tx,
            t_y,
            tw,
            th,
            th * 0.5,
            if use_24h { p.accent } else { p.control_fill },
        );
        fonts.draw_text_centered(
            pm,
            "24‑часовой формат",
            tx + tw * 0.5,
            t_y + 4.5,
            11.5,
            if use_24h { p.on_accent } else { p.text },
            use_24h,
        );
    }

    fn paint_step_appearance(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        (x, y, w, _h): (f32, f32, f32, f32),
    ) {
        let p = palette();
        let current_app = appearance();
        let current_accent = accent_color();
        let current_ts = text_size();

        // Two large miniature desktop preview cards: Light vs Dark
        let gap = 16.0;
        let card_w = (w - gap) * 0.5;
        let card_h = 128.0;
        for (i, (mode, label)) in [
            (Appearance::Light, "Светлое"),
            (Appearance::Dark, "Тёмное"),
        ]
        .iter()
        .enumerate()
        {
            let cx = x + i as f32 * (card_w + gap);
            let cy = y + 4.0;
            let selected = *mode == current_app;

            fill_rounded_rect(pm, cx, cy, card_w, card_h, 14.0, p.card);
            stroke_rounded_rect(
                pm,
                cx,
                cy,
                card_w,
                card_h,
                14.0,
                if selected { 2.0 } else { 1.0 },
                if selected { p.accent } else { p.glass_outline },
            );

            // Miniature screen preview inside card
            let sx = cx + 14.0;
            let sy = cy + 12.0;
            let sw = card_w - 28.0;
            let sh = 78.0;
            let sky = if *mode == Appearance::Dark {
                rgba(24, 32, 54, 255)
            } else {
                rgba(130, 178, 224, 255)
            };
            fill_rounded_rect(pm, sx, sy, sw, sh, 8.0, sky);

            // Mini topbar
            let tb_bg = if *mode == Appearance::Dark {
                rgba(18, 22, 32, 210)
            } else {
                rgba(255, 255, 255, 210)
            };
            fill_rounded_rect(pm, sx + 2.0, sy + 2.0, sw - 4.0, 10.0, 4.0, tb_bg);

            // Mini window
            let win_bg = if *mode == Appearance::Dark {
                rgba(34, 38, 48, 245)
            } else {
                rgba(248, 250, 254, 245)
            };
            fill_rounded_rect(pm, sx + 22.0, sy + 18.0, sw - 44.0, 42.0, 6.0, win_bg);
            fill_circle(pm, sx + 30.0, sy + 24.0, 2.2, rgba(255, 95, 87, 255));
            fill_circle(pm, sx + 37.0, sy + 24.0, 2.2, rgba(254, 188, 46, 255));
            fill_circle(pm, sx + 44.0, sy + 24.0, 2.2, rgba(40, 200, 64, 255));

            // Mini Dock
            fill_rounded_rect(
                pm,
                sx + sw * 0.5 - 34.0,
                sy + sh - 12.0,
                68.0,
                8.0,
                4.0,
                tb_bg,
            );

            fonts.draw_text_centered(
                pm,
                label,
                cx + card_w * 0.5,
                cy + card_h - 28.0,
                13.5,
                if selected { p.accent } else { p.text },
                selected,
            );
        }

        // Accent color swatches card
        let ac_y = y + card_h + 18.0;
        fill_rounded_rect(pm, x, ac_y, w, 58.0, 12.0, p.card);
        stroke_rounded_rect(pm, x, ac_y, w, 58.0, 12.0, 1.0, p.glass_outline);
        fonts.draw_text(
            pm,
            "Цвет акцента",
            x + 16.0,
            ac_y + 11.0,
            13.0,
            p.text,
            true,
        );
        fonts.draw_text(
            pm,
            current_accent.label(),
            x + 16.0,
            ac_y + 30.0,
            12.0,
            p.text_secondary,
            false,
        );

        let sw_start_x = x + w - 8.0 * 30.0 - 12.0;
        let sw_cy = ac_y + 29.0;
        for (i, &ac) in AccentColor::ALL.iter().enumerate() {
            let scx = sw_start_x + i as f32 * 30.0 + 12.0;
            let (r, g, b) = ac.rgb(current_app);
            fill_circle(pm, scx, sw_cy, 10.5, rgba(r, g, b, 255));
            if ac == current_accent {
                stroke_circle(pm, scx, sw_cy, 12.5, 1.8, p.text);
                fill_circle(pm, scx, sw_cy, 3.5, rgba(255, 255, 255, 255));
            }
        }

        // Text size pills card
        let ts_y = ac_y + 68.0;
        fill_rounded_rect(pm, x, ts_y, w, 56.0, 12.0, p.card);
        stroke_rounded_rect(pm, x, ts_y, w, 56.0, 12.0, 1.0, p.glass_outline);
        fonts.draw_text(
            pm,
            "Размер текста",
            x + 16.0,
            ts_y + 18.0,
            13.0,
            p.text,
            true,
        );

        let pills_w = 340.0;
        let px0 = x + w - pills_w - 14.0;
        let pw = (pills_w - 3.0 * 6.0) / 4.0;
        for (i, &ts) in TextSize::ALL.iter().enumerate() {
            let px = px0 + i as f32 * (pw + 6.0);
            let py = ts_y + 13.0;
            let sel = ts == current_ts;
            fill_rounded_rect(
                pm,
                px,
                py,
                pw,
                30.0,
                8.0,
                if sel { p.accent } else { p.control_fill },
            );
            fonts.draw_text_clipped_centered(
                pm,
                ts.short_label(),
                px + pw * 0.5,
                py + 7.5,
                pw - 6.0,
                11.5,
                if sel { p.on_accent } else { p.text },
                sel,
            );
        }
    }

    fn paint_step_keyboard(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        (x, y, w, _h): (f32, f32, f32, f32),
        ctx: &crate::system_settings::Context<'_>,
        dark: bool,
    ) {
        let p = palette();

        // Switching shortcut card
        fill_rounded_rect(pm, x, y, w, 52.0, 12.0, p.card);
        stroke_rounded_rect(pm, x, y, w, 52.0, 12.0, 1.0, p.glass_outline);
        fonts.draw_text(
            pm,
            "Сочетание клавиш",
            x + 16.0,
            y + 16.0,
            13.0,
            p.text,
            true,
        );

        let sw_w = 104.0;
        let sx0 = x + w - 3.0 * sw_w - 2.0 * 8.0 - 14.0;
        for (i, &sc) in Shortcut::ALL.iter().enumerate() {
            let sx = sx0 + i as f32 * (sw_w + 8.0);
            let sy = y + 11.0;
            let sel = ctx.keyboard.shortcut == sc;
            fill_rounded_rect(
                pm,
                sx,
                sy,
                sw_w,
                30.0,
                8.0,
                if sel { p.accent } else { p.control_fill },
            );
            fonts.draw_text_centered(
                pm,
                sc.label(),
                sx + sw_w * 0.5,
                sy + 7.5,
                12.0,
                if sel { p.on_accent } else { p.text },
                sel,
            );
        }

        // Offered input sources grid (2x4)
        let ly0 = y + 64.0;
        fonts.draw_text(
            pm,
            "Источники ввода",
            x + 4.0,
            ly0,
            12.5,
            p.text_secondary,
            true,
        );

        let (ar, ag, ab) = accent_color().rgb(appearance());
        let gap = 10.0;
        let card_w = (w - gap) * 0.5;
        let card_h = 36.0;
        let gy0 = ly0 + 20.0;
        for (i, &id) in OFFERED_LAYOUTS.iter().enumerate() {
            let Some(src) = SOURCES.iter().find(|s| s.id == id) else {
                continue;
            };
            let col = i % 2;
            let row = i / 2;
            let cx = x + col as f32 * (card_w + gap);
            let cy = gy0 + row as f32 * (card_h + 7.0);
            let enabled = ctx.keyboard.layouts.iter().any(|l| l == id);
            let fill = if enabled {
                rgba(ar, ag, ab, if dark { 58 } else { 34 })
            } else {
                p.card
            };
            fill_rounded_rect(pm, cx, cy, card_w, card_h, 10.0, fill);
            stroke_rounded_rect(
                pm,
                cx,
                cy,
                card_w,
                card_h,
                10.0,
                if enabled { 1.5 } else { 1.0 },
                if enabled { p.accent } else { p.glass_outline },
            );

            fill_rounded_rect(
                pm,
                cx + 10.0,
                cy + 7.0,
                28.0,
                22.0,
                6.0,
                if enabled { p.accent } else { p.control_fill },
            );
            fonts.draw_text_centered(
                pm,
                src.short,
                cx + 24.0,
                cy + 11.5,
                11.0,
                if enabled { p.on_accent } else { p.text },
                true,
            );
            fonts.draw_text_clipped(
                pm,
                src.label,
                cx + 46.0,
                cy + 9.5,
                card_w - 56.0,
                12.5,
                p.text,
                enabled,
            );
        }
    }

    fn paint_step_ready(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        (x, y, w, _h): (f32, f32, f32, f32),
        ctx: &crate::system_settings::Context<'_>,
        _dark: bool,
    ) {
        let p = palette();

        // Summary card
        fill_rounded_rect(pm, x, y, w, 108.0, 12.0, p.card);
        stroke_rounded_rect(pm, x, y, w, 108.0, 12.0, 1.0, p.glass_outline);

        let layouts = ctx
            .keyboard
            .sources()
            .into_iter()
            .map(|s| s.short)
            .collect::<Vec<_>>()
            .join(" / ");
        let rows = [
            ("Язык интерфейса", crate::theme::lang().full_label()),
            ("Оформление", appearance().name()),
            ("Источники ввода", layouts.as_str()),
            ("Переключение языка", ctx.keyboard.shortcut.label()),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let ry = y + 12.0 + i as f32 * 22.0;
            fonts.draw_text(pm, k, x + 18.0, ry, 12.5, p.text_secondary, false);
            fonts.draw_text_right(pm, v, x + w - 18.0, ry, 12.5, p.text, true);
        }

        // Quick shortcuts guide card
        let sy = y + 120.0;
        fill_rounded_rect(pm, x, sy, w, 134.0, 12.0, p.card);
        stroke_rounded_rect(pm, x, sy, w, 134.0, 12.0, 1.0, p.glass_outline);
        fonts.draw_text(
            pm,
            "Горячие клавиши Пёрышка",
            x + 18.0,
            sy + 12.0,
            13.0,
            p.text,
            true,
        );

        let shortcuts = [
            ("Super + Space", "Spotlight — мгновенный поиск программ и файлов"),
            ("Super + A", "Launchpad — сетка всех приложений"),
            ("Ctrl + Alt + ↑", "Mission Control — обзор окон и рабочих столов"),
            ("Super + Ctrl + Q", "Мгновенная блокировка экрана"),
        ];
        for (i, (chord, desc)) in shortcuts.iter().enumerate() {
            let ry = sy + 36.0 + i as f32 * 23.0;
            fill_rounded_rect(pm, x + 18.0, ry - 2.0, 118.0, 19.0, 5.0, p.control_fill);
            fonts.draw_text_centered(pm, chord, x + 77.0, ry + 1.0, 11.0, p.accent, true);
            fonts.draw_text_clipped(
                pm,
                desc,
                x + 148.0,
                ry,
                w - 164.0,
                12.0,
                p.text_secondary,
                false,
            );
        }
    }

    pub fn hit(
        &self,
        lx: f32,
        ly: f32,
        cw: f32,
        ch: f32,
        _ctx: &crate::system_settings::Context<'_>,
    ) -> Option<Action> {
        // Step dots
        let dots_w = (STEP_COUNT as f32) * 18.0;
        let dots_x0 = cw * 0.5 - dots_w * 0.5 + 9.0;
        let dots_y = 124.0;
        if (dots_y - 10.0..=dots_y + 10.0).contains(&ly) {
            for i in 0..STEP_COUNT {
                let dx = dots_x0 + i as f32 * 18.0;
                if (dx - 8.0..=dx + 8.0).contains(&lx) {
                    return Some(Action::Step(i));
                }
            }
        }

        // Bottom navigation bar
        let foot_y = ch - 46.0;
        if (foot_y..=foot_y + 32.0).contains(&ly) {
            if self.step > 0 && (24.0..=120.0).contains(&lx) {
                return Some(Action::Back);
            }
            if self.step + 1 < STEP_COUNT {
                if (cw - 266.0..=cw - 162.0).contains(&lx) {
                    return Some(Action::Finish);
                }
                if (cw - 152.0..=cw - 24.0).contains(&lx) {
                    return Some(Action::Next);
                }
            } else if (cw - 172.0..=cw - 24.0).contains(&lx) {
                return Some(Action::Finish);
            }
        }

        let x = 24.0;
        let y = 140.0;
        let w = cw - 48.0;

        match self.step {
            0 => {
                let gap = 10.0;
                let card_w = (w - gap) / 2.0;
                let card_h = 38.0;
                let grid_y = y + 20.0;
                for (i, &lang) in Lang::ALL.iter().enumerate() {
                    let col = i % 2;
                    let row = i / 2;
                    let cx = x + col as f32 * (card_w + gap);
                    let cy = grid_y + row as f32 * (card_h + 8.0);
                    if (cx..=cx + card_w).contains(&lx) && (cy..=cy + card_h).contains(&ly) {
                        return Some(Action::Language(lang));
                    }
                }
                let reg_y = grid_y + 3.0 * (card_h + 8.0) + 6.0;
                let pill_y = reg_y + 20.0;
                let r_w = (w - 3.0 * 8.0) / 4.0;
                let r_h = 28.0;
                for (i, &reg) in Region::ALL.iter().enumerate() {
                    let c = i % 4;
                    let r = i / 4;
                    let rx = x + c as f32 * (r_w + 8.0);
                    let ry = pill_y + r as f32 * (r_h + 6.0);
                    if (rx..=rx + r_w).contains(&lx) && (ry..=ry + r_h).contains(&ly) {
                        return Some(Action::Region(reg));
                    }
                }
                let t_y = pill_y + 2.0 * (r_h + 6.0) + 6.0;
                let (tw, th) = (168.0, 24.0);
                let tx = x + w - tw;
                if (tx..=tx + tw).contains(&lx) && (t_y..=t_y + th).contains(&ly) {
                    return Some(Action::Toggle24h);
                }
            }
            1 => {
                let gap = 16.0;
                let card_w = (w - gap) * 0.5;
                let card_h = 128.0;
                let cy = y + 4.0;
                if (cy..=cy + card_h).contains(&ly) {
                    if (x..=x + card_w).contains(&lx) {
                        return Some(Action::Appearance(Appearance::Light));
                    }
                    if (x + card_w + gap..=x + w).contains(&lx) {
                        return Some(Action::Appearance(Appearance::Dark));
                    }
                }
                let ac_y = y + card_h + 18.0;
                let sw_start_x = x + w - 8.0 * 30.0 - 12.0;
                let sw_cy = ac_y + 29.0;
                if (sw_cy - 14.0..=sw_cy + 14.0).contains(&ly) {
                    for (i, &ac) in AccentColor::ALL.iter().enumerate() {
                        let scx = sw_start_x + i as f32 * 30.0 + 12.0;
                        if (scx - 13.0..=scx + 13.0).contains(&lx) {
                            return Some(Action::Accent(ac));
                        }
                    }
                }
                let ts_y = ac_y + 68.0;
                let pills_w = 340.0;
                let px0 = x + w - pills_w - 14.0;
                let pw = (pills_w - 3.0 * 6.0) / 4.0;
                let py = ts_y + 13.0;
                if (py..=py + 30.0).contains(&ly) {
                    for (i, &ts) in TextSize::ALL.iter().enumerate() {
                        let px = px0 + i as f32 * (pw + 6.0);
                        if (px..=px + pw).contains(&lx) {
                            return Some(Action::TextSize(ts));
                        }
                    }
                }
            }
            2 => {
                let sw_w = 104.0;
                let sx0 = x + w - 3.0 * sw_w - 2.0 * 8.0 - 14.0;
                let sy = y + 11.0;
                if (sy..=sy + 30.0).contains(&ly) {
                    for (i, &sc) in Shortcut::ALL.iter().enumerate() {
                        let sx = sx0 + i as f32 * (sw_w + 8.0);
                        if (sx..=sx + sw_w).contains(&lx) {
                            return Some(Action::Shortcut(sc));
                        }
                    }
                }
                let ly0 = y + 64.0;
                let gap = 10.0;
                let card_w = (w - gap) * 0.5;
                let card_h = 36.0;
                let gy0 = ly0 + 20.0;
                for (i, &id) in OFFERED_LAYOUTS.iter().enumerate() {
                    let col = i % 2;
                    let row = i / 2;
                    let cx = x + col as f32 * (card_w + gap);
                    let cy = gy0 + row as f32 * (card_h + 7.0);
                    if (cx..=cx + card_w).contains(&lx) && (cy..=cy + card_h).contains(&ly) {
                        return Some(Action::ToggleLayout(id));
                    }
                }
            }
            _ => {}
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_wizard_paints_all_steps_and_hits_controls() {
        let mut fonts = FontSystem::new();
        let mut wiz = Wizard::default();
        let keyboard = crate::settings::Keyboard::default();
        let mode = crate::system_settings::Mode {
            width: 1920,
            height: 1080,
            refresh: 60_000,
        };
        let ctx = crate::system_settings::Context {
            power: Default::default(),
            power_ready: false,
            system: Default::default(),
            pointer: Default::default(),
            pointer_managed: true,
            keyboard: &keyboard,
            active: 0,
            animations: true,
            dock_zoom: true,
            scale: 1.0,
            display_name: "Feather-Display-1",
            monitors: Vec::new(),
            modes: &[mode],
            current: mode,
            direct: false,
            wallpaper: crate::theme::WallpaperTheme::TahoeDay,
            wallpaper_image: None,
            dnd: false,
            dnd_schedule: None,
            notify_sounds: true,
            sounds: Default::default(),
            audio: Default::default(),
            wifi: Default::default(),
            bluetooth: Default::default(),
            hot_corners: Default::default(),
            night: Default::default(),
            night_active: false,
            notify_apps: &[],
            screen_capture_policy: crate::file_portal::ScreenCapturePolicy::Ask,
            screen_capture_cursor: true,
            screen_capture_apps: &[],
            sticky_keys: false,
        };
        let mut pm = tiny_skia::Pixmap::new(640, 496).unwrap();
        for step in 0..STEP_COUNT {
            wiz.step = step;
            wiz.paint(&mut pm.as_mut(), &mut fonts, (0.0, 0.0, 640.0, 496.0), &ctx);
            assert!(pm.data().iter().any(|&b| b != 0));
        }

        wiz.step = 0;
        assert_eq!(
            wiz.hit(100.0, 175.0, 640.0, 496.0, &ctx),
            Some(Action::Language(Lang::Ru))
        );
        assert_eq!(
            wiz.hit(550.0, 464.0, 640.0, 496.0, &ctx),
            Some(Action::Next)
        );

        wiz.step = 1;
        assert_eq!(
            wiz.hit(450.0, 200.0, 640.0, 496.0, &ctx),
            Some(Action::Appearance(Appearance::Dark))
        );
        assert_eq!(
            wiz.hit(60.0, 464.0, 640.0, 496.0, &ctx),
            Some(Action::Back)
        );

        wiz.step = 3;
        assert_eq!(
            wiz.hit(550.0, 464.0, 640.0, 496.0, &ctx),
            Some(Action::Finish)
        );
    }
}
