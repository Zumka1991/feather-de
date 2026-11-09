//! Кольцо программ: короткое одиночное нажатие Cmd (Super) раскладывает открытые программы по
//! окружности вокруг курсора. Стрелки, Tab, колесо, буква названия, наведение или «жест» мышью
//! в сторону сектора только подсвечивают; переходят щелчок по сектору или Enter. Esc, второй тап
//! Cmd или щелчок мимо закрывают кольцо без перехода. Места программ постоянны
//! (порядок Дока), сразу подсвечена предыдущая программа.
//!
//! Триггер не мешает сочетаниям: кольцо открывается на *отпускании* Super, только если между
//! нажатием и отпусканием не было ни одной другой клавиши, кнопки мыши, прокрутки или жеста,
//! при нажатии не держалась другая клавиша, и нажатие было короче [`TAP_MAX_MS`]. Набор
//! Super+Shift+5, Super+Tab, Super+перетаскивание и раздумья с зажатым Super кольцо не трогают,
//! а до отпускания на экране ничего не появляется.
//!
//! Перерисовка: стекло и иконки рисуются один раз при открытии (и когда приходит растр
//! иконки), подсветка выбора — отдельный маленький буфер, который GPU просто сдвигает, подпись в
//! центре перерисовывается только при смене выбора.
use std::f32::consts::{FRAC_PI_2, PI, TAU};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Rectangle, Transform},
};
use tiny_skia::PixmapMut;

use crate::{
    shell::{draw_any_icon, AppIcon, AppKey, MacShell, R},
    text::FontSystem,
    theme::{draw_glass_panel, fill_circle, fill_rounded_rect, palette, rgba, stroke_circle, stroke_rounded_rect, to_phys, with_alpha, TOP_BAR_HEIGHT},
    FeatherCompositor,
};

/// Нажатие Super длиннее — это раздумья над сочетанием, а не вызов кольца.
pub const TAP_MAX_MS: u32 = 300;
/// Логический размер иконки на кольце.
pub const ICON: f32 = 56.0;
/// Больше программ не помещается на разумном кольце; остальные доступны через Alt+Tab.
pub const MAX_ITEMS: usize = 16;
/// Ближе к центру направление указателя ничего не выбирает.
pub const DEAD_ZONE: f32 = 26.0;
/// Сколько указатель должен сдвинуться после открытия, прежде чем мышь начнёт выбирать.
const MOUSE_ARM: f32 = 6.0;
/// Щелчок дальше края диска на столько — «мимо».
const OUTSIDE: f32 = 36.0;
const GAP: f32 = 26.0;
/// Иконка растрируется крупной и уменьшается на GPU: выбранная увеличивается без размытия
const ICON_BIG: f32 = 64.0;
const ICON_PAD: f32 = 10.0;
const ICON_BOX: f32 = ICON_BIG + ICON_PAD * 2.0;
const MARK: f32 = ICON_BIG + 12.0;
const PIP: f32 = 12.0;
/// Доля открытия, на которую последняя иконка отстаёт от первой
const STAGGER: f64 = 0.3;
const LABEL_H: f32 = 44.0;
/// Поле буфера вокруг диска под мягкую тень: кольцо висит прямо над окнами
const SHADOW: f32 = 14.0;

// ───────────────────────── Триггер ─────────────────────────

/// Распознаёт «чистый» короткий тап Super по событиям клавиатуры и указателя.
#[derive(Debug, Default)]
pub struct SuperTap {
    /// Код зажатой клавиши Super и время нажатия (мс события)
    down: Option<(u32, u32)>,
    spoiled: bool,
}

impl SuperTap {
    /// Учитывает событие клавиши. `others_held` — была ли зажата другая клавиша в момент этого
    /// события. Возвращает `true` на отпускании, которое завершает чистый тап.
    pub fn key(&mut self, code: u32, is_super: bool, pressed: bool, others_held: bool, time: u32) -> bool {
        match (is_super, pressed, self.down) {
            // Автоповтор зажатого Super не перезапускает отсчёт
            (true, true, Some((c, _))) if c == code => false,
            (true, true, None) => {
                self.down = Some((code, time));
                self.spoiled = others_held;
                false
            }
            (true, false, Some((c, t0))) if c == code => {
                self.down = None;
                !std::mem::take(&mut self.spoiled) && time.wrapping_sub(t0) <= TAP_MAX_MS
            }
            // Вторая клавиша Super или любая другая клавиша, пока Super зажат
            (_, _, Some(_)) => {
                self.spoiled = true;
                false
            }
            _ => false,
        }
    }
    /// Кнопка мыши, прокрутка или жест: Super был частью другого действия.
    pub fn spoil(&mut self) {
        if self.down.is_some() {
            self.spoiled = true;
        }
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// ───────────────────────── Геометрия ─────────────────────────

/// Радиус окружности, по которой стоят центры иконок.
pub fn radius(n: usize) -> f32 {
    ((ICON + GAP) * n as f32 / TAU).max(104.0)
}
/// Радиус стеклянного диска.
pub fn disc_radius(n: usize) -> f32 {
    radius(n) + ICON * 0.5 + 16.0
}
fn inner_radius(n: usize) -> f32 {
    radius(n) - ICON * 0.5 - 12.0
}

/// Угол позиции `i` из `n`: первая сверху, дальше по часовой стрелке. Две программы стоят
/// слева и справа, как в Alt+Tab.
pub fn angle(n: usize, i: usize) -> f32 {
    if n == 2 {
        if i == 0 { PI } else { 0.0 }
    } else {
        -FRAC_PI_2 + TAU * i as f32 / n.max(1) as f32
    }
}
/// Единичный вектор позиции (ось y вниз, как на экране).
pub fn slot(n: usize, i: usize) -> (f32, f32) {
    let a = angle(n, i);
    (a.cos(), a.sin())
}

/// Сектор в направлении `(dx, dy)` от центра; `None` в мёртвой зоне.
pub fn sector_at(n: usize, dx: f32, dy: f32) -> Option<usize> {
    if n == 0 || dx.hypot(dy) < DEAD_ZONE {
        return None;
    }
    let a = dy.atan2(dx);
    let dist = |i: usize| {
        let d = (a - angle(n, i)).rem_euclid(TAU);
        d.min(TAU - d)
    };
    (0..n).min_by(|&x, &y| dist(x).total_cmp(&dist(y)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Center,
    Item(usize),
    Outside,
}

pub fn hit(n: usize, dx: f32, dy: f32) -> Hit {
    let d = dx.hypot(dy);
    if d > disc_radius(n) + OUTSIDE {
        Hit::Outside
    } else {
        sector_at(n, dx, dy).map_or(Hit::Center, Hit::Item)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

pub fn step(n: usize, cur: usize, delta: i32) -> usize {
    if n == 0 {
        return 0;
    }
    (cur as i32 + delta).rem_euclid(n as i32) as usize
}

/// Стрелка ведёт к ближайшей иконке в её направлении (с поправкой на угол). Если в этом
/// направлении никого нет, подойдёт иконка сбоку, а с самого края выбор остаётся на месте.
pub fn navigate(n: usize, cur: usize, dir: Dir) -> usize {
    if n < 2 {
        return 0;
    }
    // Две программы: любая стрелка — к другой, даже «наружу» с края
    if n == 2 {
        return 1 - cur.min(1);
    }
    let (dx, dy) = match dir {
        Dir::Left => (-1.0, 0.0),
        Dir::Right => (1.0, 0.0),
        Dir::Up => (0.0, -1.0),
        Dir::Down => (0.0, 1.0),
    };
    let (cx, cy) = slot(n, cur);
    let cands: Vec<(usize, f32, f32)> = (0..n)
        .filter(|&i| i != cur)
        .map(|i| {
            let (x, y) = slot(n, i);
            let (vx, vy) = (x - cx, y - cy);
            let d = vx.hypot(vy).max(1e-4);
            (i, d, (vx * dx + vy * dy) / d)
        })
        .collect();
    if let Some(&(i, ..)) = cands
        .iter()
        .filter(|c| c.2 > 0.3)
        .min_by(|a, b| (a.1 / (a.2 * a.2)).total_cmp(&(b.1 / (b.2 * b.2))))
    {
        return i;
    }
    cands
        .iter()
        .filter(|c| c.2 > -0.1)
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map_or(cur, |c| c.0)
}

/// «1 окно», «3 окна», «5 окон»
pub fn windows_label(n: usize, hidden: bool) -> String {
    let word = match (n % 10, n % 100) {
        (1, r) if r != 11 => "окно",
        (2..=4, r) if !(12..=14).contains(&r) => "окна",
        _ => "окон",
    };
    if hidden {
        format!("{n} {word} · свёрнуто")
    } else {
        format!("{n} {word}")
    }
}

// ───────────────────────── Состояние ─────────────────────────

#[derive(Debug, Clone)]
pub struct RingItem {
    pub key: AppKey,
    /// Живое окно программы: по нему выбор находит программу, даже если ключи сдвинулись
    pub window_id: u64,
    pub label: String,
    pub icon: AppIcon,
    pub windows: usize,
    /// Все окна свёрнуты
    pub hidden: bool,
}

#[derive(Debug, Clone)]
pub struct Ring {
    pub items: Vec<RingItem>,
    pub selected: usize,
    /// Центр в логических координатах оболочки (основной монитор)
    pub center: (f32, f32),
    anchor: (f32, f32),
    mouse: bool,
    scroll: f64,
}

/// Что сделать с кольцом после клавиши; выполняется вне фильтра клавиатуры.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingKey {
    Nav(Dir),
    Step(i32),
    Letter(char),
    /// Enter: явное подтверждение с клавиатуры
    Commit,
    Close,
}

impl Ring {
    /// `pointer` — указатель в координатах оболочки; кольцо встаёт вокруг него, но целиком
    /// помещается на экране под строкой меню.
    pub fn new(items: Vec<RingItem>, pointer: (f32, f32), screen: (f32, f32)) -> Self {
        let n = items.len();
        // Целиком, вместе с тенью
        let r = disc_radius(n) + SHADOW;
        let top = TOP_BAR_HEIGHT as f32;
        let fit = |v: f32, lo: f32, hi: f32| if lo <= hi { v.clamp(lo, hi) } else { (lo + hi) * 0.5 };
        let center = (
            fit(pointer.0, r, screen.0 - r).round(),
            fit(pointer.1, top + r, screen.1 - r).round(),
        );
        Self { selected: usize::from(n > 1), items, center, anchor: pointer, mouse: false, scroll: 0.0 }
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    fn select(&mut self, i: usize) -> bool {
        let changed = i != self.selected && i < self.items.len();
        if changed {
            self.selected = i;
        }
        changed
    }
    pub fn key(&mut self, key: RingKey) -> bool {
        let n = self.len();
        match key {
            RingKey::Nav(dir) => self.select(navigate(n, self.selected, dir)),
            RingKey::Step(d) => self.select(step(n, self.selected, d)),
            RingKey::Letter(ch) => {
                let want: Vec<char> = ch.to_lowercase().collect();
                let next = (1..=n).map(|k| (self.selected + k) % n).find(|&i| {
                    let first: Vec<char> = self.items[i].label.chars().next().map(|c| c.to_lowercase().collect()).unwrap_or_default();
                    first == want
                });
                next.is_some_and(|i| self.select(i))
            }
            RingKey::Commit | RingKey::Close => false,
        }
    }
    /// Движение указателя: после первого заметного сдвига выбор следует за направлением.
    pub fn pointer(&mut self, x: f32, y: f32) -> bool {
        if !self.mouse {
            if (x - self.anchor.0).hypot(y - self.anchor.1) < MOUSE_ARM {
                return false;
            }
            self.mouse = true;
        }
        sector_at(self.len(), x - self.center.0, y - self.center.1).is_some_and(|i| self.select(i))
    }
    pub fn hit(&self, x: f32, y: f32) -> Hit {
        hit(self.len(), x - self.center.0, y - self.center.1)
    }
    /// Прокрутка: щелчок колеса — шаг, тачпад — шаг на каждые 40 единиц.
    pub fn scroll(&mut self, amount: f64, discrete: bool) -> bool {
        if discrete {
            return self.key(RingKey::Step(amount.signum() as i32));
        }
        self.scroll += amount;
        let steps = (self.scroll / 40.0).trunc();
        if steps == 0.0 {
            return false;
        }
        self.scroll -= steps * 40.0;
        self.key(RingKey::Step(steps as i32))
    }
    /// Буфер стекла и иконок
    pub fn rect(&self) -> R {
        let s = (disc_radius(self.len()) * 2.0 + SHADOW * 2.0).ceil();
        R::new((self.center.0 - s * 0.5).round(), (self.center.1 - s * 0.5).round(), s, s)
    }
    pub fn slot_center(&self, i: usize) -> (f32, f32) {
        let (ux, uy) = slot(self.len(), i);
        let r = radius(self.len());
        (self.center.0 + ux * r, self.center.1 + uy * r)
    }
    pub fn mark_rect(&self) -> R {
        self.mark_rect_at(angle(self.len(), self.selected))
    }
    /// Подсветка на окружности под углом `a` (она едет по дуге между иконками)
    pub fn mark_rect_at(&self, a: f32) -> R {
        let r = radius(self.len());
        let (x, y) = (self.center.0 + a.cos() * r, self.center.1 + a.sin() * r);
        R::new((x - MARK * 0.5).round(), (y - MARK * 0.5).round(), MARK, MARK)
    }
    pub fn pip_rect_at(&self, a: f32) -> R {
        let r = inner_radius(self.len()) - PIP * 0.5 - 3.0;
        let (x, y) = (self.center.0 + a.cos() * r, self.center.1 + a.sin() * r);
        R::new((x - PIP * 0.5).round(), (y - PIP * 0.5).round(), PIP, PIP)
    }
    /// Буфер иконки `i`, центрированный на её месте
    pub fn icon_rect(&self, i: usize) -> R {
        let (x, y) = self.slot_center(i);
        R::new((x - ICON_BOX * 0.5).round(), (y - ICON_BOX * 0.5).round(), ICON_BOX, ICON_BOX)
    }
    pub fn label_rect(&self) -> R {
        let w = (inner_radius(self.len()) * 2.0 - 12.0).floor();
        R::new((self.center.0 - w * 0.5).round(), (self.center.1 - LABEL_H * 0.5).round(), w, LABEL_H)
    }
}

// ───────────────────────── Отрисовка ─────────────────────────

fn new_buffer(w: i32, h: i32) -> MemoryRenderBuffer {
    MemoryRenderBuffer::new(Fourcc::Abgr8888, (w.max(1), h.max(1)), 1, Transform::Normal, None)
}

/// Буферы кольца. Пока кольцо закрыто (и не гаснет), они 1×1: память не держим.
pub struct RingGfx {
    pub base: MemoryRenderBuffer,
    /// По буферу на иконку: GPU выдвигает их из центра и увеличивает выбранную
    pub icons: Vec<MemoryRenderBuffer>,
    pub mark: MemoryRenderBuffer,
    pub pip: MemoryRenderBuffer,
    pub label: MemoryRenderBuffer,
    allocated: Option<(i32, i32, usize)>,
    label_for: Option<usize>,
}

impl Default for RingGfx {
    fn default() -> Self {
        Self {
            base: new_buffer(1, 1),
            icons: Vec::new(),
            mark: new_buffer(1, 1),
            pip: new_buffer(1, 1),
            label: new_buffer(1, 1),
            allocated: None,
            label_for: None,
        }
    }
}

fn draw(buf: &mut MemoryRenderBuffer, pw: i32, ph: i32, f: impl FnOnce(&mut PixmapMut)) {
    let mut ctx = buf.render();
    let _ = ctx.draw(|px| {
        px.fill(0);
        if let Some(mut pm) = PixmapMut::from_bytes(px, pw as u32, ph as u32) {
            f(&mut pm);
        }
        Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
    });
}

fn white(a: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(1.0, 1.0, 1.0, a.clamp(0.0, 1.0)).unwrap_or(tiny_skia::Color::WHITE)
}
fn black(a: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(0.0, 0.0, 0.0, a.clamp(0.0, 1.0)).unwrap_or(tiny_skia::Color::BLACK)
}

/// Круг (заливка или обводка) линейным градиентом от `from` к `to`.
fn gradient_circle(pm: &mut PixmapMut, c: (f32, f32), r: f32, stroke: Option<f32>, from: (f32, f32), to: (f32, f32), stops: &[(f32, tiny_skia::Color)]) {
    use tiny_skia::{GradientStop, LinearGradient, Paint, PathBuilder, Point, SpreadMode, Stroke, Transform};
    let Some(path) = PathBuilder::from_circle(c.0, c.1, r) else { return };
    let stops = stops.iter().map(|(p, col)| GradientStop::new(*p, *col)).collect();
    let Some(shader) = LinearGradient::new(Point::from_xy(from.0, from.1), Point::from_xy(to.0, to.1), stops, SpreadMode::Pad, Transform::identity()) else { return };
    let paint = Paint { shader, anti_alias: true, ..Default::default() };
    let t = crate::theme::scale_transform();
    match stroke {
        Some(width) => pm.stroke_path(&path, &paint, &Stroke { width, ..Default::default() }, t, None),
        None => pm.fill_path(&path, &paint, tiny_skia::FillRule::Winding, t, None),
    }
}

impl MacShell {
    /// Кольцо на экране: открытое или гаснущее после закрытия.
    pub fn ring_shown(&self) -> Option<&Ring> {
        self.ring.as_ref().or(self.ring_fading.as_ref())
    }

    pub fn ensure_ring_rendered(&mut self, fonts: &mut FontSystem) {
        let Some(ring) = self.ring.as_ref().or(self.ring_fading.as_ref()) else {
            if self.ring_gfx.allocated.is_some() {
                self.ring_gfx = RingGfx::default();
            }
            return;
        };
        let n = ring.len();
        let rect = ring.rect();
        let (pw, ph) = (to_phys(rect.w as i32), to_phys(rect.h as i32));
        let p = palette();
        let dark = crate::theme::appearance() == crate::theme::Appearance::Dark;
        if self.ring_gfx.allocated != Some((pw, ph, n)) {
            let g = &mut self.ring_gfx;
            g.base = new_buffer(pw, ph);
            let ib = to_phys(ICON_BOX as i32);
            g.icons = (0..n).map(|_| new_buffer(ib, ib)).collect();
            let m = to_phys(MARK as i32);
            g.mark = new_buffer(m, m);
            let pp = to_phys(PIP as i32);
            g.pip = new_buffer(pp, pp);
            let lr = ring.label_rect();
            g.label = new_buffer(to_phys(lr.w as i32), to_phys(lr.h as i32));
            g.allocated = Some((pw, ph, n));
            g.label_for = None;
            self.ring_dirty = true;
            crate::theme::set_glass_origin(rect.x, rect.y);
            let c = rect.w * 0.5;
            let (dr, ir) = (disc_radius(n), inner_radius(n));
            draw(&mut g.base, pw, ph, |pm| {
                // Мягкая тень: прозрачные круги, смещённые вниз
                for k in (1..=6).rev() {
                    fill_circle(pm, c, c + 3.0, dr + k as f32 * 1.8, rgba(0, 0, 0, (4.0 * p.shadow_strength.max(0.6)) as u8));
                }
                draw_glass_panel(pm, c - dr, c - dr, dr * 2.0, dr * 2.0, dr, true);
                // Liquid Glass: блик сверху, яркая кромка слева сверху и отсвет преломления
                // справа снизу
                let sheen = if dark { 0.07 } else { 0.22 };
                gradient_circle(pm, (c, c), dr - 1.0, None, (c, c - dr), (c, c + dr * 0.1), &[(0.0, white(sheen)), (1.0, white(0.0))]);
                gradient_circle(pm, (c, c), dr - 1.25, Some(1.5), (c - dr, c - dr), (c + dr, c + dr), &[
                    (0.0, white(if dark { 0.55 } else { 0.95 })),
                    (0.42, white(0.0)),
                    (0.7, white(0.0)),
                    (1.0, white(if dark { 0.22 } else { 0.5 })),
                ]);
                // Центральное углубление: чуть темнее, тень у верхней кромки, свет у нижней
                fill_circle(pm, c, c, ir, p.control_fill);
                gradient_circle(pm, (c, c), ir - 0.75, Some(1.5), (c, c - ir), (c, c + ir), &[
                    (0.0, black(if dark { 0.35 } else { 0.12 })),
                    (0.5, black(0.0)),
                    (1.0, white(if dark { 0.18 } else { 0.7 })),
                ]);
            });
            // Подсветка выбора: акцентное стекло под увеличенной иконкой
            draw(&mut g.mark, m, m, |pm| {
                let (x, w, r) = (2.0, MARK - 4.0, 20.0);
                fill_rounded_rect(pm, x, x, w, w, r, with_alpha(p.accent, if dark { 0.30 } else { 0.18 }));
                stroke_rounded_rect(pm, x, x, w, w, r, 1.75, with_alpha(p.accent, 0.95));
                stroke_rounded_rect(pm, x + 2.0, x + 2.0, w - 4.0, w - 4.0, r - 2.0, 1.0, white(if dark { 0.18 } else { 0.55 }));
            });
            // Огонёк на кромке углубления: показывает направление выбора
            draw(&mut g.pip, pp, pp, |pm| {
                let h = PIP * 0.5;
                fill_circle(pm, h, h, h - 0.5, with_alpha(p.accent, 0.25));
                fill_circle(pm, h, h, h * 0.55, p.accent);
            });
        }
        if std::mem::take(&mut self.ring_dirty) {
            let items = ring.items.clone();
            let icons = &mut self.icons;
            let ib = to_phys(ICON_BOX as i32);
            for (buf, item) in self.ring_gfx.icons.iter_mut().zip(&items) {
                draw(buf, ib, ib, |pm| {
                    let (x, y) = (ICON_PAD, ICON_PAD);
                    draw_any_icon(pm, fonts, icons, &item.icon, &item.label, x, y, ICON_BIG);
                    if item.hidden {
                        fill_circle(pm, x + ICON_BIG * 0.5, y + ICON_BIG + 5.5, 2.3, p.text_secondary);
                    }
                    if item.windows > 1 {
                        let (bx, by) = (x + ICON_BIG - 5.0, y + 5.0);
                        fill_circle(pm, bx, by, 10.5, p.accent);
                        stroke_circle(pm, bx, by, 10.5, 1.25, white(0.85));
                        let text = if item.windows > 9 { "9+".to_string() } else { item.windows.to_string() };
                        fonts.draw_text_centered(pm, &text, bx, by - 8.0, 12.0, p.on_accent, true);
                    }
                });
            }
            self.ring_gfx.label_for = None;
        }
        if self.ring_gfx.label_for != Some(ring.selected) {
            self.ring_gfx.label_for = Some(ring.selected);
            let lr = ring.label_rect();
            let (lw, lh) = (to_phys(lr.w as i32), to_phys(lr.h as i32));
            let item = &ring.items[ring.selected];
            let (name, count) = (item.label.clone(), windows_label(item.windows, item.hidden));
            draw(&mut self.ring_gfx.label, lw, lh, |pm| {
                fonts.draw_text_clipped_centered(pm, &name, lr.w * 0.5, 3.0, lr.w - 4.0, 15.0, p.text, true);
                fonts.draw_text_clipped_centered(pm, &count, lr.w * 0.5, 24.0, lr.w - 4.0, 12.0, p.text_secondary, false);
            });
        }
    }
}

/// Пружинное «выскакивание» с лёгким перелётом (easeOutBack)
fn ease_back(q: f64) -> f64 {
    let (c1, c3) = (1.2, 2.2);
    let t = q - 1.0;
    1.0 + c3 * t * t * t + c1 * t * t
}

/// Как нарисовать иконку `i` при открытости кольца `open` (0…1, пружина) и выделенности `sel`
/// (0…1): иконки по очереди по часовой стрелке выезжают из центра, выбранная крупнее.
pub fn icon_motion(n: usize, i: usize, open: f64, sel: f64) -> (f64, f64, f32) {
    let delay = if n > 1 { STAGGER * i as f64 / (n - 1) as f64 } else { 0.0 };
    let q = ((open - delay) / (1.0 - STAGGER)).clamp(0.0, 1.0);
    let e = ease_back(q);
    let scale = (ICON / ICON_BIG) as f64 + (1.0 - (ICON / ICON_BIG) as f64) * sel;
    // (доля пути от центра, масштаб, прозрачность)
    (e, scale * (0.35 + 0.65 * e), (q * 1.6).min(1.0) as f32)
}

// ───────────────────────── Композитор ─────────────────────────

impl FeatherCompositor {
    /// Программы с открытыми окнами в порядке Дока: закреплённые — в порядке закрепления,
    /// остальные — по первому открытому окну. У программы постоянное место: переход к ней ничего
    /// не переставляет, новая встаёт в конец, закрытая уходит, не сдвигая остальных по кругу.
    /// Второе значение — предыдущая программа (её выбор при открытии), как у Alt+Tab.
    pub fn ring_items(&self) -> (Vec<RingItem>, usize) {
        let mru = self.switcher_items();
        let previous = mru.get(1).or(mru.first()).map(|s| s.key);
        let mut items: Vec<RingItem> = mru
            .into_iter()
            .take(MAX_ITEMS)
            .map(|s| RingItem { key: s.key, window_id: s.window_id, label: s.label, icon: s.icon, windows: 0, hidden: true })
            .collect();
        let mut first_window = vec![u64::MAX; items.len()];
        for w in self.windows.iter().filter(|w| w.mapped && !w.larvis) {
            let key = self.app_key(w);
            if let Some(i) = items.iter().position(|i| i.key == key) {
                items[i].windows += 1;
                items[i].hidden &= w.minimized;
                first_window[i] = first_window[i].min(w.id);
            }
        }
        let pinned = self.pinned_indices();
        let rank = |item: &RingItem, first: u64| match item.key {
            AppKey::App(i) => pinned.iter().position(|p| *p == i).map_or((1, first), |p| (0, p as u64)),
            AppKey::Window(_) => (1, first),
        };
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by_key(|&i| rank(&items[i], first_window[i]));
        let mut slots: Vec<Option<RingItem>> = items.into_iter().map(Some).collect();
        let items: Vec<RingItem> = order.into_iter().filter_map(|i| slots[i].take()).collect();
        let selected = items.iter().position(|i| Some(i.key) == previous).unwrap_or(0);
        (items, selected)
    }

    /// Можно ли сейчас открыть кольцо: не поверх пароля, блокировки, игры с захваченным
    /// указателем и других режимов, которые сами владеют клавиатурой или мышью.
    pub fn ring_allowed(&self) -> bool {
        self.keyboard_config.app_ring
            && !self.larvis_desktop
            && !self.lock.active
            && !self.auth.is_open()
            && !self.capturing_shortcut()
            && !self.sticky_keys
            && self.shell.switcher.is_none()
            && !self.mission_open()
            && self.shell.launchpad.is_none()
            && !self.shell.spotlight_open
            && !(self.shell.control_center_open && self.shell.cc.page == crate::control_panel::Page::Password)
            && !self.screencast.is_open()
            && self.screenshot_select.is_none()
            && self.active_constraint.is_none()
            && self.pressed_buttons.is_empty()
            && self.active_drag.is_none()
            && !self.icon_drag.as_ref().is_some_and(|d| d.active)
    }

    /// Указатель в координатах оболочки (основной монитор).
    fn ring_point(&self) -> (f32, f32) {
        let o = self.primary_monitor().map_or((0, 0), |m| (m.info.rect.x, m.info.rect.y));
        ((self.pointer_location.x - o.0 as f64) as f32, (self.pointer_location.y - o.1 as f64) as f32)
    }

    /// Чистый тап Super открывает кольцо, а второй закрывает его без перехода: к программе
    /// ведёт только щелчок.
    pub fn super_tapped(&mut self) {
        if self.shell.ring.is_some() {
            self.close_ring();
        } else if self.ring_allowed() {
            self.open_ring();
        }
    }

    pub fn open_ring(&mut self) -> bool {
        let (items, selected) = self.ring_items();
        if items.is_empty() {
            return false;
        }
        self.shell.close_menus();
        let (w, h) = (self.shell.screen_w as f32, self.shell.screen_h as f32);
        let mut pointer = self.ring_point();
        if !(0.0..w).contains(&pointer.0) || !(0.0..h).contains(&pointer.1) {
            pointer = (w * 0.5, h * 0.5);
        }
        let mut ring = Ring::new(items, pointer, (w, h));
        ring.selected = selected;
        self.shell.ring = Some(ring);
        self.shell.ring_fading = None;
        self.shell.ring_dirty = true;
        // Клавиатура уходит от программы, пока кольцо открыто
        self.refresh_focus();
        self.refresh_pointer_focus();
        self.needs_redraw = true;
        true
    }

    pub fn close_ring(&mut self) {
        if let Some(ring) = self.shell.ring.take() {
            self.retire_ring(ring);
            self.pointer_focus_dirty = true;
            self.needs_redraw = true;
            self.refresh_focus();
        }
    }

    /// Закрытое кольцо гаснет и втягивается в центр; ввод оно уже не принимает.
    fn retire_ring(&mut self, ring: Ring) {
        if self.animations.enabled() {
            self.shell.ring_fading = Some(ring);
        }
    }

    /// После кадра: погасшее кольцо отпускает буферы.
    pub fn finish_ring(&mut self) {
        if self.shell.ring_fading.is_some() && self.animations.ring_faded() {
            self.shell.ring_fading = None;
            self.needs_redraw = true;
        }
    }

    /// Переходит к программе позиции `index` (или выбранной). Выбор разрешается через живое
    /// окно: закрытая за это время программа не запускается заново.
    pub fn ring_commit(&mut self, index: Option<usize>) {
        let Some(mut ring) = self.shell.ring.take() else { return };
        if let Some(i) = index.filter(|i| *i < ring.len()) {
            ring.selected = i;
        }
        let item = ring.items.get(ring.selected);
        let target = item.and_then(|item| {
            self.windows
                .iter()
                .find(|w| w.mapped && w.id == item.window_id)
                .or_else(|| self.windows.iter().find(|w| w.mapped && !w.larvis && matches!(item.key, AppKey::App(_)) && self.app_key(w) == item.key))
                .map(|w| self.app_key(w))
        });
        self.retire_ring(ring);
        self.pointer_focus_dirty = true;
        self.needs_redraw = true;
        match target {
            Some(key) => {
                self.unfocus_desktop();
                self.activate_app(key);
            }
            None => self.refresh_focus(),
        }
    }

    pub fn ring_key(&mut self, key: RingKey) {
        match key {
            RingKey::Commit => self.ring_commit(None),
            RingKey::Close => self.close_ring(),
            key => {
                if self.shell.ring.as_mut().is_some_and(|r| r.key(key)) {
                    self.needs_redraw = true;
                }
            }
        }
    }

    pub fn ring_motion(&mut self) {
        let (x, y) = self.ring_point();
        if self.shell.ring.as_mut().is_some_and(|r| r.pointer(x, y)) {
            self.needs_redraw = true;
        }
    }

    /// Нажатие кнопки мыши при открытом кольце.
    pub fn ring_button(&mut self, left: bool) {
        let (x, y) = self.ring_point();
        match self.shell.ring.as_ref().map(|r| r.hit(x, y)) {
            Some(Hit::Item(i)) if left => self.ring_commit(Some(i)),
            _ => self.close_ring(),
        }
    }

    pub fn ring_scroll(&mut self, amount: f64, discrete: bool) {
        if self.shell.ring.as_mut().is_some_and(|r| r.scroll(amount, discrete)) {
            self.needs_redraw = true;
        }
    }

    /// Что делает клавиша при открытом кольце (вызывается из фильтра клавиатуры: только решает).
    pub fn ring_key_for(sym: u32, ch: Option<char>, shift: bool) -> Option<RingKey> {
        use smithay::input::keyboard::keysyms as k;
        Some(match sym {
            k::KEY_Left | k::KEY_KP_Left => RingKey::Nav(Dir::Left),
            k::KEY_Right | k::KEY_KP_Right => RingKey::Nav(Dir::Right),
            k::KEY_Up | k::KEY_KP_Up => RingKey::Nav(Dir::Up),
            k::KEY_Down | k::KEY_KP_Down => RingKey::Nav(Dir::Down),
            k::KEY_Tab => RingKey::Step(if shift { -1 } else { 1 }),
            k::KEY_ISO_Left_Tab => RingKey::Step(-1),
            k::KEY_Return | k::KEY_KP_Enter => RingKey::Commit,
            k::KEY_Escape => RingKey::Close,
            _ => return ch.filter(|c| c.is_alphanumeric()).map(RingKey::Letter),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPER: u32 = 125 + 8;
    const SUPER_R: u32 = 126 + 8;
    const TAB: u32 = 15 + 8;

    #[test]
    fn only_a_short_lone_super_tap_triggers() {
        let mut t = SuperTap::default();
        assert!(!t.key(SUPER, true, true, false, 1000));
        assert!(t.key(SUPER, true, false, false, 1120), "short lone tap");
        // Super+Tab: another key while held
        t.key(SUPER, true, true, false, 2000);
        t.key(TAB, false, true, true, 2050);
        t.key(TAB, false, false, true, 2080);
        assert!(!t.key(SUPER, true, false, false, 2100), "chord is not a tap");
        // Thinking with Super held
        t.key(SUPER, true, true, false, 3000);
        assert!(!t.key(SUPER, true, false, false, 3000 + TAP_MAX_MS + 1), "long hold");
        // Autorepeat events of the held key do not restart the clock
        t.key(SUPER, true, true, false, 4000);
        t.key(SUPER, true, true, false, 4250);
        assert!(!t.key(SUPER, true, false, false, 4400), "repeat keeps the first press time");
        // Shift already held: Shift+Super is a chord in the making
        assert!(!t.key(SUPER, true, true, true, 5000));
        assert!(!t.key(SUPER, true, false, false, 5050));
        // A mouse button (Super+drag moves windows) or scrolling
        t.key(SUPER, true, true, false, 6000);
        t.spoil();
        assert!(!t.key(SUPER, true, false, false, 6050));
        // Both Super keys
        t.key(SUPER, true, true, false, 7000);
        t.key(SUPER_R, true, true, true, 7010);
        assert!(!t.key(SUPER, true, false, true, 7020));
        t.key(SUPER_R, true, false, false, 7030);
        // Released modifiers pressed before Super do not count, and a stale spoil is forgotten
        t.spoil();
        t.key(SUPER, true, true, false, 8000);
        assert!(t.key(SUPER, true, false, false, 8090), "spoil without a held Super is ignored");
        // Event clock wrap-around
        t.key(SUPER, true, true, false, u32::MAX - 20);
        assert!(t.key(SUPER, true, false, false, 40));
        // Release without a press (e.g. after focus came back) does nothing
        assert!(!t.key(SUPER, true, false, false, 9000));
        t.key(SUPER, true, true, false, 9100);
        t.reset();
        assert!(!t.key(SUPER, true, false, false, 9150));
    }

    #[test]
    fn slots_start_on_top_and_sectors_follow_the_pointer_direction() {
        let (x, y) = slot(6, 0);
        assert!(x.abs() < 1e-5 && (y + 1.0).abs() < 1e-5, "first on top");
        let (x, _) = slot(6, 1);
        assert!(x > 0.0, "clockwise");
        assert_eq!(slot(2, 0).0, -1.0);
        assert_eq!(slot(2, 1).0, 1.0);
        for n in 1..=MAX_ITEMS {
            for i in 0..n {
                let (ux, uy) = slot(n, i);
                assert_eq!(sector_at(n, ux * 80.0, uy * 80.0), Some(i), "{n}/{i}");
                assert_eq!(hit(n, ux * radius(n), uy * radius(n)), Hit::Item(i));
            }
        }
        assert_eq!(sector_at(6, 3.0, 4.0), None, "dead zone");
        assert_eq!(hit(6, 0.0, 10.0), Hit::Center);
        assert_eq!(hit(6, disc_radius(6) + OUTSIDE + 1.0, 0.0), Hit::Outside);
        // A short flick far beyond the disc still points at a sector
        assert_eq!(sector_at(4, 900.0, 5.0), Some(1));
    }

    #[test]
    fn arrows_move_spatially_and_tab_cycles() {
        // 4: top, right, bottom, left
        assert_eq!(navigate(4, 0, Dir::Right), 1);
        assert_eq!(navigate(4, 0, Dir::Left), 3);
        assert_eq!(navigate(4, 0, Dir::Down), 2);
        assert_eq!(navigate(4, 1, Dir::Down), 2);
        assert_eq!(navigate(4, 2, Dir::Up), 0);
        // 6: from the top-right item Down goes to the bottom-right one, Right has nothing
        assert_eq!(navigate(6, 1, Dir::Down), 2);
        assert_eq!(navigate(6, 0, Dir::Right), 1);
        assert_eq!(navigate(6, 0, Dir::Down), 3);
        // Two apps side by side: any arrow reaches the other one
        for dir in [Dir::Left, Dir::Right, Dir::Up, Dir::Down] {
            assert_eq!(navigate(2, 0, dir), 1, "{dir:?}");
            assert_eq!(navigate(2, 1, dir), 0, "{dir:?}");
        }
        assert_eq!(navigate(1, 0, Dir::Right), 0);
        // From the rightmost of three nothing lies further right
        assert_eq!(navigate(3, 1, Dir::Right), 1);
        // Every item is reachable by arrows alone
        for n in 2..=MAX_ITEMS {
            let mut seen = vec![false; n];
            let mut frontier = vec![0];
            while let Some(i) = frontier.pop() {
                if std::mem::replace(&mut seen[i], true) {
                    continue;
                }
                for dir in [Dir::Left, Dir::Right, Dir::Up, Dir::Down] {
                    frontier.push(navigate(n, i, dir));
                }
            }
            assert!(seen.iter().all(|s| *s), "n={n}");
        }
        assert_eq!(step(5, 4, 1), 0);
        assert_eq!(step(5, 0, -1), 4);
    }

    fn items(names: &[&str]) -> Vec<RingItem> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| RingItem {
                key: AppKey::Window(i as u64),
                window_id: i as u64,
                label: n.to_string(),
                icon: AppIcon { kind: crate::apps::IconKind::GenericApp, name: None },
                windows: 1,
                hidden: false,
            })
            .collect()
    }

    #[test]
    fn ring_opens_on_the_previous_app_fits_the_screen_and_follows_input() {
        let mut r = Ring::new(items(&["Терминал", "Файлы", "Firefox", "Telegram"]), (5.0, 5.0), (1440.0, 900.0));
        assert_eq!(r.selected, 1, "previous app, like Alt+Tab");
        let rect = r.rect();
        assert!(rect.x >= 0.0 && rect.y >= TOP_BAR_HEIGHT as f32, "kept on screen: {rect:?}");
        // The pointer was clamped away from the centre: tiny jitter must not select
        assert!(!r.pointer(7.0, 8.0));
        assert_eq!(r.selected, 1);
        // Moving towards the bottom slot selects it
        let (cx, cy) = r.center;
        assert!(r.pointer(cx + 2.0, cy + 60.0));
        assert_eq!(r.selected, 2);
        assert!(r.key(RingKey::Letter('T')));
        assert_eq!(r.selected, 3, "Telegram after the current selection");
        assert!(r.key(RingKey::Letter('т')));
        assert_eq!(r.selected, 0, "case-insensitive Cyrillic");
        assert!(!r.key(RingKey::Letter('я')));
        assert!(r.scroll(1.0, true));
        assert_eq!(r.selected, 1);
        assert!(!r.scroll(25.0, false));
        assert!(r.scroll(25.0, false), "touchpad accumulates");
        assert_eq!(r.selected, 2);
        assert!(r.key(RingKey::Step(-1)));
        assert_eq!(r.selected, 1);
        let one = Ring::new(items(&["Терминал"]), (700.0, 400.0), (1440.0, 900.0));
        assert_eq!(one.selected, 0);
        assert_eq!(one.center, (700.0, 400.0), "centred on the pointer");
        // A screen too small for the ring centres it
        let tiny = Ring::new(items(&["a", "b", "c"]), (10.0, 10.0), (100.0, 100.0));
        assert_eq!(tiny.center.0, 50.0);
    }

    #[test]
    fn keys_and_labels() {
        use smithay::input::keyboard::keysyms as k;
        let f = FeatherCompositor::ring_key_for;
        assert_eq!(f(k::KEY_Tab, None, false), Some(RingKey::Step(1)));
        assert_eq!(f(k::KEY_Tab, None, true), Some(RingKey::Step(-1)));
        assert_eq!(f(k::KEY_ISO_Left_Tab, None, true), Some(RingKey::Step(-1)));
        assert_eq!(f(k::KEY_Return, None, false), Some(RingKey::Commit));
        assert_eq!(f(k::KEY_KP_Enter, None, false), Some(RingKey::Commit));
        assert_eq!(f(k::KEY_space, Some(' '), false), None, "Space does not switch");
        assert_eq!(f(k::KEY_Escape, None, false), Some(RingKey::Close));
        assert_eq!(f(k::KEY_Up, None, false), Some(RingKey::Nav(Dir::Up)));
        assert_eq!(f(0x6c1, Some('ф'), false), Some(RingKey::Letter('ф')));
        assert_eq!(f(k::KEY_F5, None, false), None);
        assert_eq!(windows_label(1, false), "1 окно");
        assert_eq!(windows_label(3, false), "3 окна");
        assert_eq!(windows_label(5, false), "5 окон");
        assert_eq!(windows_label(11, false), "11 окон");
        assert_eq!(windows_label(22, true), "22 окна · свёрнуто");
    }

    #[test]
    fn icons_fly_out_of_the_centre_in_turn_and_the_selected_one_grows() {
        let (t, s, a) = icon_motion(6, 0, 0.0, 0.0);
        assert!(t.abs() < 1e-9 && a == 0.0 && s < 0.4, "closed: in the centre, invisible");
        let (t, s, a) = icon_motion(6, 5, 1.0, 1.0);
        assert!((t - 1.0).abs() < 1e-9 && (s - 1.0).abs() < 1e-9 && a == 1.0, "open and selected: full size");
        let (_, s, _) = icon_motion(6, 5, 1.0, 0.0);
        assert!((s - f64::from(ICON / ICON_BIG)).abs() < 1e-9, "others at the ring size");
        // Halfway the first icon is further out than the last one
        assert!(icon_motion(6, 0, 0.5, 0.0).0 > icon_motion(6, 5, 0.5, 0.0).0);
        // A small overshoot gives the pop, but never far
        let peak = (0..=100).map(|k| icon_motion(4, 0, f64::from(k) / 100.0, 0.0).0).fold(0.0, f64::max);
        assert!(peak > 1.0 && peak < 1.12, "{peak}");
    }

    #[test]
    fn motion_settles_to_zero_work_and_a_closed_ring_fades_out() {
        use std::time::{Duration, Instant};
        let mut shell = MacShell::new(1440, 900);
        let mut a = crate::animation::Animations::default();
        let t = Instant::now();
        shell.ring = Some(Ring::new(items(&["A", "B", "C", "D"]), (700.0, 450.0), (1440.0, 900.0)));
        assert!(a.advance(&[], &shell, t), "opening animates");
        assert!(a.ring_open() < 0.05);
        assert!(!a.advance(&[], &shell, t + Duration::from_secs(2)), "then nothing runs");
        assert_eq!(a.ring_open(), 1.0);
        // From slot 1 (right) to slot 3 (left) and on to 0 (top): the highlight takes short arcs
        shell.ring.as_mut().unwrap().selected = 0;
        assert!(a.advance(&[], &shell, t + Duration::from_secs(3)));
        assert!(!a.advance(&[], &shell, t + Duration::from_secs(5)));
        let m = a.ring_mark_angle().unwrap();
        assert!((m - angle(4, 0)).abs() < 1e-4, "{m}");
        assert_eq!(a.ring_selected(0, 0), 1.0);
        assert_eq!(a.ring_selected(1, 0), 0.0);
        shell.ring.as_mut().unwrap().selected = 3;
        a.advance(&[], &shell, t + Duration::from_secs(6));
        a.advance(&[], &shell, t + Duration::from_secs(8));
        let m = a.ring_mark_angle().unwrap();
        assert!((m - (angle(4, 3) - TAU)).abs() < 1e-4, "top → left goes counter-clockwise: {m}");
        // Closing: the ring fades, then is gone
        shell.ring_fading = shell.ring.take();
        assert!(!a.ring_faded());
        assert!(a.advance(&[], &shell, t + Duration::from_secs(9)));
        assert!(!a.advance(&[], &shell, t + Duration::from_secs(11)));
        assert!(a.ring_faded());
        shell.ring_fading = None;
        assert!(!a.advance(&[], &shell, t + Duration::from_secs(12)));
    }

    #[test]
    fn first_frame_has_pixels_and_selection_moves_only_the_mark() {
        let mut shell = MacShell::new(1440, 900);
        let mut fonts = FontSystem::new();
        shell.ring = Some(Ring::new(items(&["A", "B", "C"]), (700.0, 450.0), (1440.0, 900.0)));
        shell.ring_dirty = true;
        shell.ensure_ring_rendered(&mut fonts);
        let r = shell.ring.as_ref().unwrap().rect();
        let expected = (to_phys(r.w as i32) * to_phys(r.h as i32) * 4) as usize;
        shell.ring_gfx.base.render().draw(|px| {
            assert_eq!(px.len(), expected);
            assert!(px.iter().any(|v| *v != 0));
            Ok::<_, ()>(Vec::new())
        }).unwrap();
        assert!(!shell.ring_dirty);
        let mark = shell.ring.as_ref().unwrap().mark_rect();
        shell.ring.as_mut().unwrap().key(RingKey::Step(1));
        shell.ensure_ring_rendered(&mut fonts);
        assert!(!shell.ring_dirty, "a new selection does not redraw icons");
        assert_eq!(shell.ring_gfx.label_for, Some(2));
        assert_ne!(shell.ring.as_ref().unwrap().mark_rect(), mark);
        shell.ring = None;
        shell.ensure_ring_rendered(&mut fonts);
        assert!(shell.ring_gfx.allocated.is_none(), "buffers are released when closed");
    }
}
