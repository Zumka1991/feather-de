//! macOS-style administrator password dialog, drawn by the compositor itself: no client
//! can draw over it, read it or receive keys while it is shown.
use crate::{
    polkit_agent::{Event, Request, Session},
    render::FeatherElement,
    shell::R,
    text::FontSystem,
    theme::*,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement}, Kind},
            gles::GlesRenderer,
        },
    },
    utils::{Rectangle, Transform},
};
use std::{collections::VecDeque, time::Instant};
use tiny_skia::PixmapMut;
use zeroize::Zeroizing;

const W: f32 = 320.0;
/// Transparent margin around the panel for its shadow.
const M: f32 = 28.0;
const SHAKE_SECS: f32 = 0.45;
const MAX_INPUT: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Cancel,
    Allow,
    User,
    Details,
}

pub enum Cmd {
    Submit,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmAction {
    EmptyTrash,
}

/// A macOS alert for a destructive action, shown in the same modal layer.
pub struct Confirm {
    pub title: String,
    pub message: String,
    pub ok: String,
    pub action: ConfirmAction,
}

struct Active {
    req: Request,
    user: usize,
    input: Zeroizing<String>,
    /// A further PAM question; answered through the running session.
    prompt: Option<(String, bool)>,
    message: Option<(String, bool)>,
    verifying: bool,
    session: Option<Session>,
    details: bool,
}

pub struct AuthUi {
    queue: VecDeque<Request>,
    active: Option<Active>,
    confirm: Option<Confirm>,
    buffer: MemoryRenderBuffer,
    dim: MemoryRenderBuffer,
    /// Logical panel height of the last paint.
    height: f32,
    phys: (i32, i32),
    dirty: bool,
    hits: Vec<(R, Hit)>,
    shake: Option<Instant>,
    caps: bool,
}

fn buffer(w: i32, h: i32) -> MemoryRenderBuffer {
    MemoryRenderBuffer::new(Fourcc::Abgr8888, (w.max(1), h.max(1)), 1, Transform::Normal, None)
}

impl Default for AuthUi {
    fn default() -> Self {
        let mut dim = buffer(1, 1);
        let mut ctx = dim.render();
        let _ = ctx.draw(|buf| {
            // Premultiplied black at ~30 %.
            buf.copy_from_slice(&[0, 0, 0, 76]);
            Result::<_, ()>::Ok(vec![Rectangle::from_size((1, 1).into())])
        });
        drop(ctx);
        Self {
            queue: VecDeque::new(),
            active: None,
            confirm: None,
            buffer: buffer(1, 1),
            dim,
            height: 0.0,
            phys: (0, 0),
            dirty: true,
            hits: Vec::new(),
            shake: None,
            caps: false,
        }
    }
}

fn title(req: &Request) -> String {
    let detail = |k: &str| req.details.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    match detail("program").and_then(|p| p.rsplit('/').next()).filter(|p| !p.is_empty()) {
        Some(program) => format!("«{program}» хочет внести изменения"),
        None => "Требуется подтверждение".into(),
    }
}

/// Greedy word wrap; the last line is ellipsized.
fn wrap(fonts: &mut FontSystem, text: &str, width: f32, size: f32, bold: bool, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
        if line.is_empty() || fonts.measure_text(&candidate, size, bold) <= width {
            line = candidate;
        } else {
            lines.push(std::mem::replace(&mut line, word.to_owned()));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max {
        let rest = lines[max - 1..].join(" ");
        lines.truncate(max - 1);
        lines.push(rest);
    }
    lines
        .into_iter()
        .map(|l| fonts.ellipsize(&l, width, size, bold))
        .collect()
}

/// Panel background with a layered soft shadow; returns the fill colour.
fn paint_panel(pm: &mut PixmapMut, ox: f32, oy: f32, h: f32, dark: bool) -> tiny_skia::Color {
    for i in 1..=10 {
        let e = i as f32 * 2.2;
        fill_rounded_rect(pm, ox - e, oy - e + 10.0, W + 2.0 * e, h + 2.0 * e, 18.0 + e, rgba(0, 0, 0, if dark { 9 } else { 5 }));
    }
    let bg = if dark { rgba(42, 42, 46, 255) } else { rgba(246, 246, 248, 255) };
    fill_rounded_rect(pm, ox, oy, W, h, 18.0, bg);
    stroke_rounded_rect(pm, ox + 0.25, oy + 0.25, W - 0.5, h - 0.5, 18.0, 0.5, palette().window_outline);
    bg
}

/// "Отменить" + the default (accent) button.
#[allow(clippy::too_many_arguments)]
fn paint_buttons(pm: &mut PixmapMut, fonts: &mut FontSystem, x: f32, y: f32, bw: f32, ok: &str, busy: bool, dark: bool) {
    let p = palette();
    let push = if dark { rgba(255, 255, 255, 30) } else { rgba(0, 0, 0, 16) };
    fill_rounded_rect(pm, x, y, bw, 30.0, 15.0, push);
    fonts.draw_text_centered(pm, "Отменить", x + bw / 2.0, y + 7.5, 13.0, p.text, false);
    let ax = x + bw + 10.0;
    fill_rounded_rect(pm, ax, y, bw, 30.0, 15.0, if busy { with_alpha(p.accent, 0.5) } else { p.accent });
    fonts.draw_text_clipped_centered(pm, ok, ax + bw / 2.0, y + 7.5, bw - 12.0, 13.0, p.on_accent, false);
}

fn draw_lock_icon(pm: &mut PixmapMut, x: f32, y: f32, s: f32) {
    let dark = appearance() == Appearance::Dark;
    let (top, bottom) = if dark {
        (rgba(96, 96, 104, 255), rgba(62, 62, 68, 255))
    } else {
        (rgba(252, 252, 254, 255), rgba(222, 222, 228, 255))
    };
    if let Some(path) = squircle_path(x, y, s) {
        fill_path_gradient(pm, &path, y, y + s, top, bottom);
    }
    stroke_squircle(pm, x, y, s, 0.5, rgba(0, 0, 0, if dark { 90 } else { 40 }));
    let k = s / 64.0;
    let cx = x + s / 2.0;
    // Shackle first; the body covers its lower ends.
    stroke_rounded_rect(pm, cx - 10.0 * k, y + 12.0 * k, 20.0 * k, 26.0 * k, 10.0 * k, 4.5 * k, rgba(150, 152, 160, 255));
    let (bx, by, bw, bh) = (cx - 16.0 * k, y + 27.0 * k, 32.0 * k, 25.0 * k);
    if let Some(path) = rounded_rect_path(bx, by, bw, bh, 5.0 * k) {
        fill_path_gradient(pm, &path, by, by + bh, rgba(255, 214, 10, 255), rgba(240, 160, 0, 255));
    }
    fill_circle(pm, cx, by + 10.0 * k, 3.2 * k, rgba(120, 72, 0, 255));
    fill_rounded_rect(pm, cx - 1.4 * k, by + 11.0 * k, 2.8 * k, 7.5 * k, 1.2 * k, rgba(120, 72, 0, 255));
}

impl AuthUi {
    pub fn is_open(&self) -> bool {
        self.active.is_some() || self.confirm.is_some()
    }
    pub fn confirm(&mut self, confirm: Confirm) {
        self.confirm = Some(confirm);
        self.dirty = true;
    }
    pub fn confirming(&self) -> Option<ConfirmAction> {
        self.confirm.as_ref().map(|c| c.action)
    }
    pub fn animating(&self) -> bool {
        self.shake.is_some_and(|t| t.elapsed().as_secs_f32() < SHAKE_SECS)
    }
    fn activate_next(&mut self) {
        self.active = self.queue.pop_front().map(|req| Active {
            user: req.default_user.min(req.users.len().saturating_sub(1)),
            req,
            input: Zeroizing::new(String::new()),
            prompt: None,
            message: None,
            verifying: false,
            session: None,
            details: false,
        });
        self.shake = None;
        self.dirty = true;
    }
    fn begin(&mut self, req: Request) {
        self.queue.push_back(req);
        if self.active.is_none() {
            self.activate_next();
        }
    }
    /// polkitd withdrew `cookie`; true when the visible dialog changed.
    fn withdraw(&mut self, cookie: &str) -> bool {
        self.queue.retain(|r| r.cookie != cookie);
        if self.active.as_ref().is_some_and(|a| a.req.cookie == cookie) {
            self.activate_next();
            return true;
        }
        false
    }
    fn active_for(&mut self, cookie: &str) -> Option<&mut Active> {
        self.active.as_mut().filter(|a| a.req.cookie == cookie)
    }

    pub fn key(&mut self, sym: u32, ch: Option<char>, ctrl: bool, caps: bool) -> Option<Cmd> {
        use smithay::input::keyboard::keysyms;
        self.dirty |= caps != self.caps;
        self.caps = caps;
        if self.active.is_none() && self.confirm.is_some() {
            return match sym {
                keysyms::KEY_Escape => Some(Cmd::Cancel),
                keysyms::KEY_Return | keysyms::KEY_KP_Enter => Some(Cmd::Submit),
                _ => None,
            };
        }
        let a = self.active.as_mut()?;
        if sym == keysyms::KEY_Escape {
            return Some(Cmd::Cancel);
        }
        if a.verifying {
            return None;
        }
        self.dirty = true;
        match sym {
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => return Some(Cmd::Submit),
            keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab if a.prompt.is_none() => {
                a.user = (a.user + 1) % a.req.users.len().max(1);
            }
            keysyms::KEY_BackSpace => {
                if ctrl {
                    a.input.clear();
                } else {
                    a.input.pop();
                }
            }
            _ if ctrl && matches!(sym, keysyms::KEY_a | keysyms::KEY_A | keysyms::KEY_u | keysyms::KEY_U) => {
                a.input.clear()
            }
            _ => {
                if let Some(c) = ch.filter(|c| !c.is_control() && !ctrl) {
                    if a.input.chars().count() < MAX_INPUT {
                        a.input.push(c);
                    }
                }
            }
        }
        None
    }

    /// `pos` is global logical. Every click is swallowed while the dialog is shown.
    pub fn click(&mut self, pos: (f64, f64), screen: (i32, i32)) -> Option<Cmd> {
        let (px, py) = self.panel_origin(screen);
        let (x, y) = (pos.0 as f32 - px, pos.1 as f32 - py);
        let hit = self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)?;
        if self.active.is_none() && self.confirm.is_some() {
            return match hit {
                Hit::Cancel => Some(Cmd::Cancel),
                Hit::Allow => Some(Cmd::Submit),
                _ => None,
            };
        }
        let a = self.active.as_mut()?;
        self.dirty = true;
        match hit {
            Hit::Cancel => Some(Cmd::Cancel),
            Hit::Allow => (!a.verifying).then_some(Cmd::Submit),
            Hit::User => {
                if !a.verifying && a.prompt.is_none() {
                    a.user = (a.user + 1) % a.req.users.len().max(1);
                }
                None
            }
            Hit::Details => {
                a.details = !a.details;
                None
            }
        }
    }

    fn panel_origin(&self, (sw, sh): (i32, i32)) -> (f32, f32) {
        let x = ((sw as f32 - W) / 2.0).round();
        let y = (sh as f32 * 0.22).max(TOP_BAR_HEIGHT as f32 + 24.0).min((sh as f32 - self.height - 12.0).max(0.0)).round();
        (x, y)
    }

    pub fn ensure_rendered(&mut self, fonts: &mut FontSystem) {
        if !self.dirty {
            return;
        }
        let Some(a) = self.active.as_ref() else {
            self.dirty = false;
            if self.confirm.is_some() {
                self.paint_confirm(fonts);
            } else {
                self.buffer = buffer(1, 1);
                self.phys = (0, 0);
            }
            return;
        };
        self.dirty = false;
        let p = palette();
        let dark = appearance() == Appearance::Dark;
        let inner = W - 48.0;
        // Layout pass (also used for hit testing).
        let title_lines = wrap(fonts, &title(&a.req), inner, 14.0, true, 2);
        let body_lines = wrap(fonts, &a.req.message, inner, 12.0, false, 4);
        let detail_lines: Vec<String> = if a.details {
            let mut text = vec![format!("Действие: {}", a.req.action_id)];
            for (k, v) in &a.req.details {
                if matches!(k.as_str(), "command_line" | "program") {
                    text.push(format!("{}: {v}", if k == "program" { "Программа" } else { "Команда" }));
                }
            }
            text.iter().flat_map(|t| wrap(fonts, t, inner, 11.0, false, 3)).take(8).collect()
        } else {
            Vec::new()
        };
        let mut y = 22.0;
        let icon_y = y;
        y += 64.0 + 14.0;
        let title_y = y;
        y += title_lines.len() as f32 * 19.0 + 4.0;
        let body_y = y;
        y += body_lines.len() as f32 * 16.0 + 16.0;
        let user_y = y;
        let show_user = a.prompt.is_none();
        if show_user {
            y += 30.0 + 8.0;
        }
        let field_y = y;
        y += 30.0;
        let message_y = y + 8.0;
        if a.message.is_some() {
            y += 24.0;
        }
        let details_y = y + 10.0;
        y += 30.0;
        let details_text_y = y;
        y += detail_lines.len() as f32 * 15.0;
        let buttons_y = y + 10.0;
        let h = buttons_y + 30.0 + 20.0;
        self.height = h;
        let bw = (inner - 10.0) / 2.0;
        self.hits = vec![
            (R::new(24.0, buttons_y, bw, 30.0), Hit::Cancel),
            (R::new(24.0 + bw + 10.0, buttons_y, bw, 30.0), Hit::Allow),
            (R::new(24.0, details_y - 4.0, 120.0, 22.0), Hit::Details),
        ];
        if show_user && a.req.users.len() > 1 {
            self.hits.push((R::new(24.0, user_y, inner, 30.0), Hit::User));
        }
        let phys = (to_phys((W + 2.0 * M) as i32), to_phys((h + 2.0 * M) as i32));
        if phys != self.phys {
            self.buffer = buffer(phys.0, phys.1);
            self.phys = phys;
        }
        let shake_failed = a.message.as_ref().is_some_and(|(_, e)| *e);
        let caps = self.caps;
        let a = self.active.as_ref().unwrap();
        let mut ctx = self.buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            let Some(mut pm) = PixmapMut::from_bytes(buf, phys.0 as u32, phys.1 as u32) else {
                return Result::<_, ()>::Ok(vec![]);
            };
            let pm = &mut pm;
            let (ox, oy) = (M, M);
            let bg = paint_panel(pm, ox, oy, h, dark);
            draw_lock_icon(pm, ox + (W - 64.0) / 2.0, oy + icon_y, 64.0);
            for (i, line) in title_lines.iter().enumerate() {
                fonts.draw_text_centered(pm, line, ox + W / 2.0, oy + title_y + i as f32 * 19.0, 14.0, p.text, true);
            }
            for (i, line) in body_lines.iter().enumerate() {
                fonts.draw_text_centered(pm, line, ox + W / 2.0, oy + body_y + i as f32 * 16.0, 12.0, p.text_secondary, false);
            }
            let field_bg = if dark { rgba(255, 255, 255, 16) } else { rgba(255, 255, 255, 255) };
            let field_edge = if dark { rgba(255, 255, 255, 34) } else { rgba(0, 0, 0, 40) };
            if show_user {
                let r = R::new(ox + 24.0, oy + user_y, inner, 30.0);
                fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 8.0, if dark { rgba(255, 255, 255, 8) } else { rgba(0, 0, 0, 10) });
                let user = &a.req.users[a.user];
                fonts.draw_text_clipped(pm, user.display(), r.x + 10.0, r.y + 8.0, r.w - 40.0, 13.0, p.text, false);
                if a.req.users.len() > 1 {
                    let (cx, cy) = (r.x + r.w - 16.0, r.y + 15.0);
                    let c = p.text_secondary;
                    stroke_line(pm, cx - 3.0, cy - 2.0, cx, cy - 5.0, 1.2, c);
                    stroke_line(pm, cx, cy - 5.0, cx + 3.0, cy - 2.0, 1.2, c);
                    stroke_line(pm, cx - 3.0, cy + 2.0, cx, cy + 5.0, 1.2, c);
                    stroke_line(pm, cx, cy + 5.0, cx + 3.0, cy + 2.0, 1.2, c);
                }
            }
            let r = R::new(ox + 24.0, oy + field_y, inner, 30.0);
            let focus = if shake_failed { rgba(255, 69, 58, 255) } else { p.accent };
            stroke_rounded_rect(pm, r.x - 1.5, r.y - 1.5, r.w + 3.0, r.h + 3.0, 9.5, 3.0, with_alpha(focus, 0.45));
            fill_rounded_rect(pm, r.x, r.y, r.w, r.h, 8.0, field_bg);
            stroke_rounded_rect(pm, r.x, r.y, r.w, r.h, 8.0, 0.5, field_edge);
            let echo = a.prompt.as_ref().is_some_and(|(_, e)| *e);
            let shown: String = if echo {
                a.input.as_str().to_owned()
            } else {
                "•".repeat(a.input.chars().count())
            };
            let reserve = if caps { 26.0 } else { 10.0 };
            let tx = r.x + 10.0;
            let ty = r.y + 8.0;
            if shown.is_empty() {
                let placeholder = a.prompt.as_ref().map_or("Пароль", |(t, _)| t.trim().trim_end_matches(':'));
                fonts.draw_text_clipped(pm, placeholder, tx + 3.0, ty, r.w - 10.0 - reserve, 13.0, p.text_tertiary, false);
            }
            // Keep the caret end visible: show the tail of long input.
            let mut visible = shown.as_str();
            while !visible.is_empty() && fonts.measure_text(visible, 13.0, false) > r.w - 14.0 - reserve {
                let mut it = visible.chars();
                it.next();
                visible = it.as_str();
            }
            let advance = fonts.draw_text(pm, visible, tx, ty, 13.0, p.text, false);
            if !a.verifying {
                fill_rounded_rect(pm, tx + advance + 1.0, r.y + 7.0, 1.5, 16.0, 0.75, p.accent);
            }
            if caps {
                // ⇪ Caps Lock warning, as macOS shows in password fields.
                let (cx, cy) = (r.x + r.w - 15.0, r.y + 15.0);
                fill_rounded_rect(pm, cx - 7.0, cy - 7.0, 14.0, 14.0, 3.5, p.text_secondary);
                fonts.draw_text_centered(pm, "⇪", cx, cy - 6.5, 11.0, bg, true);
            }
            if let Some((text, error)) = &a.message {
                let color = if *error { rgba(230, 60, 50, 255) } else { p.text_secondary };
                fonts.draw_text_clipped_centered(pm, text, ox + W / 2.0, oy + message_y, inner, 11.0, color, false);
            }
            fonts.draw_text(pm, if a.details { "Скрыть подробности" } else { "Подробнее…" }, ox + 24.0, oy + details_y, 11.0, p.accent, false);
            for (i, line) in detail_lines.iter().enumerate() {
                fonts.draw_text(pm, line, ox + 24.0, oy + details_text_y + i as f32 * 15.0, 11.0, p.text_secondary, false);
            }
            let label = if a.verifying {
                "Проверка…"
            } else if a.prompt.is_some() {
                "Продолжить"
            } else {
                "Разрешить"
            };
            paint_buttons(pm, fonts, ox + 24.0, oy + buttons_y, bw, label, a.verifying, dark);
            Result::<_, ()>::Ok(vec![Rectangle::from_size(phys.into())])
        });
    }

    fn paint_confirm(&mut self, fonts: &mut FontSystem) {
        let Some(c) = self.confirm.as_ref() else { return };
        let p = palette();
        let dark = appearance() == Appearance::Dark;
        let inner = W - 48.0;
        let title_lines = wrap(fonts, &c.title, inner, 14.0, true, 3);
        let body_lines = wrap(fonts, &c.message, inner, 12.0, false, 4);
        let title_y = 22.0 + 64.0 + 14.0;
        let body_y = title_y + title_lines.len() as f32 * 19.0 + 4.0;
        let buttons_y = body_y + body_lines.len() as f32 * 16.0 + 20.0;
        let h = buttons_y + 30.0 + 20.0;
        self.height = h;
        let bw = (inner - 10.0) / 2.0;
        self.hits = vec![
            (R::new(24.0, buttons_y, bw, 30.0), Hit::Cancel),
            (R::new(24.0 + bw + 10.0, buttons_y, bw, 30.0), Hit::Allow),
        ];
        let phys = (to_phys((W + 2.0 * M) as i32), to_phys((h + 2.0 * M) as i32));
        if phys != self.phys {
            self.buffer = buffer(phys.0, phys.1);
            self.phys = phys;
        }
        let mut ctx = self.buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            let Some(mut pm) = PixmapMut::from_bytes(buf, phys.0 as u32, phys.1 as u32) else {
                return Result::<_, ()>::Ok(vec![]);
            };
            let pm = &mut pm;
            let (ox, oy) = (M, M);
            paint_panel(pm, ox, oy, h, dark);
            crate::apps::draw_app_icon(pm, fonts, crate::apps::IconKind::TrashFull, "", ox + (W - 64.0) / 2.0, oy + 22.0, 64.0);
            for (i, line) in title_lines.iter().enumerate() {
                fonts.draw_text_centered(pm, line, ox + W / 2.0, oy + title_y + i as f32 * 19.0, 14.0, p.text, true);
            }
            for (i, line) in body_lines.iter().enumerate() {
                fonts.draw_text_centered(pm, line, ox + W / 2.0, oy + body_y + i as f32 * 16.0, 12.0, p.text_secondary, false);
            }
            paint_buttons(pm, fonts, ox + 24.0, oy + buttons_y, bw, &c.ok, false, dark);
            Result::<_, ()>::Ok(vec![Rectangle::from_size(phys.into())])
        });
    }

    /// Topmost elements (front first): the dialog, then the dimming layer.
    pub fn elements(&self, renderer: &mut GlesRenderer, scale: f64, screen: (i32, i32), phys: (i32, i32)) -> Vec<FeatherElement> {
        if !self.is_open() || self.phys == (0, 0) {
            return Vec::new();
        }
        let (px, py) = self.panel_origin(screen);
        let shake = self.shake.map_or(0.0, |t| {
            let t = t.elapsed().as_secs_f32();
            if t < SHAKE_SECS {
                (t * 42.0).sin() * 12.0 * (1.0 - t / SHAKE_SECS)
            } else {
                0.0
            }
        });
        let loc = (((px - M + shake) as f64 * scale).round(), ((py - M) as f64 * scale).round());
        let mut out = Vec::with_capacity(2);
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(renderer, loc, &self.buffer, None, None, None, Kind::Unspecified) {
            out.push(crate::render::SceneElement::Memory(e).into());
        }
        let src = Rectangle::new((0.0, 0.0).into(), (1.0, 1.0).into());
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(renderer, (0.0, 0.0), &self.dim, None, Some(src), Some(phys.into()), Kind::Unspecified) {
            out.push(crate::render::SceneElement::Memory(e).into());
        }
        out
    }
}

impl crate::FeatherCompositor {
    pub fn polkit_event(&mut self, event: Event) {
        match event {
            Event::Unavailable(e) => {
                tracing::warn!("polkit: собственный агент недоступен ({e}), запускаю резервный");
                self.polkit = None;
                crate::apps::spawn_polkit_fallback(&self.wayland_socket_name);
            }
            Event::Begin(req) => {
                self.auth.begin(req);
                self.shell.close_menus();
                self.refresh_focus();
            }
            Event::Cancel(cookie) => {
                if self.auth.withdraw(&cookie) {
                    self.refresh_focus();
                }
            }
            Event::Prompt { cookie, text, echo } => {
                if let Some(a) = self.auth.active_for(&cookie) {
                    a.prompt = Some((text, echo));
                    a.verifying = false;
                    a.input.clear();
                    self.auth.dirty = true;
                }
            }
            Event::Info { cookie, text, error } => {
                if let Some(a) = self.auth.active_for(&cookie) {
                    a.message = Some((text, error));
                    self.auth.dirty = true;
                }
            }
            Event::Done { cookie, ok } => {
                if let Some(a) = self.auth.active_for(&cookie) {
                    if ok {
                        self.auth.activate_next();
                        self.refresh_focus();
                    } else {
                        a.session = None;
                        a.verifying = false;
                        a.prompt = None;
                        a.input.clear();
                        if !a.message.as_ref().is_some_and(|(_, e)| *e) {
                            a.message = Some(("Неверный пароль. Попробуйте ещё раз.".into(), true));
                        }
                        self.auth.shake = Some(Instant::now());
                        self.auth.dirty = true;
                    }
                }
            }
        }
        self.needs_redraw = true;
    }

    pub fn auth_command(&mut self, cmd: Cmd) {
        // A password request takes precedence over an alert; the alert waits below it.
        if self.auth.active.is_none() {
            if let Some(confirm) = self.auth.confirm.take() {
                if matches!(cmd, Cmd::Submit) {
                    match confirm.action {
                        ConfirmAction::EmptyTrash => self.empty_trash_now(),
                    }
                }
                self.auth.dirty = true;
                self.refresh_focus();
                self.needs_redraw = true;
                return;
            }
        }
        match cmd {
            Cmd::Submit => {
                let Some(agent) = self.polkit.clone() else { return };
                let Some(a) = self.auth.active.as_mut() else { return };
                if a.verifying {
                    return;
                }
                let answer = Zeroizing::new(std::mem::take(&mut *a.input));
                a.verifying = true;
                a.message = None;
                match (&a.session, a.prompt.take()) {
                    (Some(session), Some(_)) => session.respond(answer),
                    _ => {
                        let user = a.req.users[a.user].name.clone();
                        a.session = Some(agent.authenticate(a.req.cookie.clone(), user, answer));
                    }
                }
            }
            Cmd::Cancel => {
                if let Some(a) = self.auth.active.take() {
                    drop(a.session);
                    if let Some(agent) = &self.polkit {
                        agent.finish(&a.req.cookie, false);
                    }
                }
                self.auth.activate_next();
                self.refresh_focus();
            }
        }
        self.auth.dirty = true;
        self.needs_redraw = true;
    }
}
