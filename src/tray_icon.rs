//! «Шаблонные» значки строки меню, как template images в macOS: любой значок трея
//! превращается в монохромный рисунок цвета текста панели, единого размера и с общим
//! оптическим центром. Форма берётся из альфы; если значок нарисован на непрозрачной
//! подложке (квадрат или круг), подложка вырезается по контрасту и остаётся рисунок.
//! Маленькие насыщенные метки (красная точка непрочитанного) остаются цветными.
//!
//! Преобразование выполняется один раз при промахе кэша плиток трея в панели
//! (`tray_tile_cache`): при смене значков, масштаба или темы, но не на каждом кадре.
use std::sync::atomic::{AtomicBool, Ordering};
use tiny_skia::{Color, FilterQuality, Pixmap, PixmapPaint, Transform};

static COLORED: AtomicBool = AtomicBool::new(false);

/// Показывать ли оригинальные цветные значки (выключатель в Оформлении)
pub fn colored() -> bool {
    COLORED.load(Ordering::Relaxed)
}

pub fn set_colored(value: bool) {
    COLORED.store(value, Ordering::Relaxed);
}

/// Откуда взята форма значка
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Альфа-силуэт: символьные и уже монохромные значки только перекрашиваются
    Silhouette,
    /// Рисунок на подложке: подложка вырезана по контрасту цвета
    Plate,
}

/// Маска рисунка и цветные метки в координатах исходного значка
pub struct Template {
    pub w: usize,
    pub h: usize,
    /// Покрытие рисунка 0…1
    pub cover: Vec<f32>,
    /// Пиксели цветных меток
    pub badge: Vec<bool>,
    pub shape: Shape,
}

/// Порог «непрозрачного» пикселя
const SOLID: f32 = 0.5;
/// Порог «пустого» пикселя снаружи значка
const CLEAR: f32 = 0.06;
/// Цветовое расстояние (0…1), ниже которого пиксель считается подложкой, и выше
/// которого — целиком рисунком; между ними — сглаженный край.
const PLATE_NEAR: f32 = 0.10;
const PLATE_FAR: f32 = 0.24;

fn unpremultiply(src: &Pixmap) -> Vec<[f32; 4]> {
    src.data()
        .chunks_exact(4)
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            if p[3] == 0 {
                [0.0; 4]
            } else {
                let k = 1.0 / (a * 255.0);
                [
                    (p[0] as f32 * k).min(1.0),
                    (p[1] as f32 * k).min(1.0),
                    (p[2] as f32 * k).min(1.0),
                    a,
                ]
            }
        })
        .collect()
}

/// Оттенок в градусах, насыщенность и яркость (HSV)
fn hsv(p: &[f32; 4]) -> (f32, f32, f32) {
    let max = p[0].max(p[1]).max(p[2]);
    let min = p[0].min(p[1]).min(p[2]);
    let d = max - min;
    let s = if max > 0.0 { d / max } else { 0.0 };
    let h = if d <= 1e-6 {
        0.0
    } else if max == p[0] {
        60.0 * ((p[1] - p[2]) / d).rem_euclid(6.0)
    } else if max == p[1] {
        60.0 * ((p[2] - p[0]) / d + 2.0)
    } else {
        60.0 * ((p[0] - p[1]) / d + 4.0)
    };
    (h, s, max)
}

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// Средний оттенок по кругу
fn mean_hue(hues: impl Iterator<Item = f32>) -> Option<f32> {
    let (mut x, mut y, mut n) = (0.0f32, 0.0f32, 0usize);
    for h in hues {
        x += h.to_radians().cos();
        y += h.to_radians().sin();
        n += 1;
    }
    (n > 0).then(|| y.atan2(x).to_degrees().rem_euclid(360.0))
}

fn color_distance(a: &[f32; 4], b: &[f32; 3]) -> f32 {
    let (r, g, bl) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    ((r * r + g * g + bl * bl) / 3.0).sqrt()
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn neighbours8(w: usize, h: usize, i: usize) -> impl Iterator<Item = usize> {
    let (x, y) = ((i % w) as i32, (i / w) as i32);
    (-1..=1).flat_map(move |dy| {
        (-1..=1).filter_map(move |dx| {
            let (nx, ny) = (x + dx, y + dy);
            ((dx, dy) != (0, 0) && nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32)
                .then(|| ny as usize * w + nx as usize)
        })
    })
}

/// Маленькие компактные насыщенные пятна, отличные по цвету от остального значка
fn find_badges(px: &[[f32; 4]], w: usize, h: usize) -> Vec<bool> {
    let n = w * h;
    let hsvs: Vec<_> = px.iter().map(hsv).collect();
    let vivid: Vec<bool> = (0..n)
        .map(|i| px[i][3] >= SOLID && hsvs[i].1 >= 0.55 && hsvs[i].2 >= 0.35)
        .collect();
    let mut seen = vec![false; n];
    let mut candidates = Vec::new();
    for start in 0..n {
        if !vivid[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut comp = Vec::new();
        seen[start] = true;
        while let Some(i) = stack.pop() {
            comp.push(i);
            for j in neighbours8(w, h, i) {
                if vivid[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        candidates.push(comp);
    }
    let area = n as f32;
    let solid_total = px.iter().filter(|p| p[3] >= SOLID).count();
    let mut badges: Vec<Vec<usize>> = Vec::new();
    for comp in candidates {
        let size = comp.len();
        if size < 2 || (size as f32) < 0.004 * area || (size as f32) > 0.12 * area {
            continue;
        }
        // Метка — дополнение к значку, а не сам значок
        if solid_total < size * 3 {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for &i in &comp {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
        // Точки и «пилюли»: компактные, без вытянутых штрихов
        if (size as f32) < 0.5 * (bw * bh) as f32 || bw.max(bh) > 3 * bw.min(bh) {
            continue;
        }
        // У края значка, как метки непрочитанного, а не деталь посередине
        let cx = (x0 + x1 + 1) as f32 * 0.5 / w as f32 - 0.5;
        let cy = (y0 + y1 + 1) as f32 * 0.5 / h as f32 - 0.5;
        if cx.abs().max(cy.abs()) < 0.2 {
            continue;
        }
        let hue = mean_hue(comp.iter().map(|&i| hsvs[i].0)).unwrap_or(0.0);
        let mut member = vec![false; n];
        for &i in &comp {
            member[i] = true;
        }
        let rest: Vec<usize> = (0..n).filter(|&i| !member[i] && px[i][3] >= SOLID).collect();
        let rest_vivid: Vec<usize> = rest.iter().copied().filter(|&i| hsvs[i].1 >= 0.35).collect();
        let distinct = if (rest_vivid.len() as f32) < 0.25 * rest.len() as f32 {
            true
        } else {
            let rest_hue = mean_hue(rest_vivid.iter().map(|&i| hsvs[i].0)).unwrap_or(hue);
            hue_distance(hue, rest_hue) > 35.0
        };
        if distinct {
            badges.push(comp);
        }
    }
    let mut mask = vec![false; n];
    // Много разноцветных пятен — это многоцветный логотип, а не метки
    if badges.len() <= 2 {
        for comp in badges {
            for i in comp {
                mask[i] = true;
            }
        }
    }
    mask
}

/// Почти полностью прозрачная область, связанная с краем значка, и она же, расширенная
/// на два пикселя сглаживания. Выбитый полупрозрачный рисунок, даже касаясь края
/// подложки, остаётся внутри.
fn outside_band(px: &[[f32; 4]], badge: &[bool], w: usize, h: usize) -> (Vec<bool>, Vec<bool>) {
    let n = w * h;
    let clear = |i: usize| px[i][3] < CLEAR && !badge[i];
    let mut outside = vec![false; n];
    let mut stack: Vec<usize> = (0..n)
        .filter(|&i| {
            let (x, y) = (i % w, i / w);
            (x == 0 || y == 0 || x == w - 1 || y == h - 1) && clear(i)
        })
        .collect();
    for &i in &stack {
        outside[i] = true;
    }
    while let Some(i) = stack.pop() {
        for j in neighbours8(w, h, i) {
            if !outside[j] && clear(j) {
                outside[j] = true;
                stack.push(j);
            }
        }
    }
    let grow = |m: &[bool]| -> Vec<bool> {
        (0..n).map(|i| m[i] || neighbours8(w, h, i).any(|j| m[j])).collect()
    };
    let band = grow(&grow(&outside));
    (outside, band)
}

fn luma(p: &[f32; 4]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

/// Порог Оцу по яркости: средние двух классов и порог между ними
fn otsu(values: &[f32]) -> Option<(f32, f32, f32)> {
    const BINS: usize = 64;
    if values.len() < 8 {
        return None;
    }
    let mut hist = [0usize; BINS];
    for &v in values {
        hist[((v.clamp(0.0, 1.0) * (BINS - 1) as f32).round()) as usize] += 1;
    }
    let total = values.len() as f32;
    let sum: f32 = hist.iter().enumerate().map(|(i, &c)| i as f32 * c as f32).sum();
    let (mut w0, mut s0, mut best, mut cut) = (0.0f32, 0.0f32, -1.0f32, None);
    for (i, &c) in hist.iter().enumerate().take(BINS - 1) {
        w0 += c as f32;
        s0 += i as f32 * c as f32;
        let w1 = total - w0;
        if w0 == 0.0 || w1 == 0.0 {
            continue;
        }
        let (m0, m1) = (s0 / w0, (sum - s0) / w1);
        let between = w0 * w1 * (m0 - m1) * (m0 - m1);
        if between > best {
            best = between;
            cut = Some((m0, m1, i));
        }
    }
    let (m0, m1, i) = cut?;
    let scale = (BINS - 1) as f32;
    Some((m0 / scale, m1 / scale, (i as f32 + 0.5) / scale))
}

/// Рисунок на подложке: силуэт плотный и крупный, его обод целиком одного тона,
/// а внутри есть пиксели заметно другой яркости (или цвета) либо выбитые прорези.
fn plate_cover(px: &[[f32; 4]], badge: &[bool], w: usize, h: usize) -> Option<Vec<f32>> {
    let n = w * h;
    let solid: Vec<bool> = (0..n).map(|i| px[i][3] >= SOLID && !badge[i]).collect();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    let mut count = 0usize;
    for i in (0..n).filter(|&i| solid[i]) {
        let (x, y) = (i % w, i / w);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
    if (bw as f32) < 0.5 * w as f32 || (bh as f32) < 0.5 * h as f32 {
        return None;
    }
    let (outside, band) = outside_band(px, badge, w, h);
    let inside: Vec<usize> = (0..n).filter(|&i| !outside[i] && !badge[i]).collect();
    let hull = inside.len() as f32;
    let box_area = (bw * bh) as f32;
    if hull < 0.6 * box_area {
        return None;
    }
    // Обод — у прозрачного края силуэта или у края картинки (подложка во весь значок)
    let border = |i: usize| {
        let (x, y) = (i % w, i / w);
        x == 0 || y == 0 || x == w - 1 || y == h - 1
    };
    let rim: Vec<usize> = inside.iter().copied().filter(|&i| solid[i] && (band[i] || border(i))).collect();
    if rim.len() < 8 {
        return None;
    }

    // Яркостный контраст: обод (подложка) весь по одну сторону порога. Разница
    // растягивается от ближайшего к рисунку края подложки (не от её среднего), так что
    // и бледный рисунок на светлом градиенте выделяется целиком, а градиент — нет.
    let lumas: Vec<f32> = inside.iter().filter(|&&i| solid[i]).map(|&i| luma(&px[i])).collect();
    let by_luma = otsu(&lumas).and_then(|(lo, hi, cut)| {
        let dark = rim.iter().filter(|&&i| luma(&px[i]) < cut).count() as f32 / rim.len() as f32;
        let plate_dark = if dark >= 0.85 {
            true
        } else if dark <= 0.15 {
            false
        } else {
            return None;
        };
        let ink = if plate_dark { hi } else { lo };
        let mut plate: Vec<f32> = lumas.iter().copied().filter(|&l| (l < cut) == plate_dark).collect();
        plate.sort_by(f32::total_cmp);
        let edge = if plate_dark {
            plate[(plate.len() - 1) * 95 / 100]
        } else {
            plate[(plate.len() - 1) * 5 / 100]
        };
        ((ink - edge).abs() >= 0.04).then_some((edge, ink))
    });
    // Однородный цвет подложки: рисунок того же тона, но другого цвета
    let mut plate_rgb = [0.0f32; 3];
    for &i in &rim {
        for c in 0..3 {
            plate_rgb[c] += px[i][c] / rim.len() as f32;
        }
    }
    let spread = rim.iter().map(|&i| color_distance(&px[i], &plate_rgb)).sum::<f32>() / rim.len() as f32;
    // Пороги цвета — от самого контрастного рисунка (97-й перцентиль), но не ниже шума обода
    let mut dists: Vec<f32> =
        inside.iter().filter(|&&i| solid[i]).map(|&i| color_distance(&px[i], &plate_rgb)).collect();
    dists.sort_by(f32::total_cmp);
    let d_ref = dists.get((dists.len().max(1) - 1) * 97 / 100).copied().unwrap_or(0.0);
    let by_color = spread <= 0.08 && d_ref >= 0.04_f32.max(3.0 * spread);
    // Без надёжного непрозрачного контраста пороги обычные: так полупрозрачный тёмный
    // рисунок, выбитый в однородной подложке, всё равно отличается от неё
    let (near, far) = if by_color {
        let near = (0.35 * d_ref).min(PLATE_NEAR).max(2.0 * spread + 0.01);
        (near, (0.7 * d_ref).min(PLATE_FAR).max(near + 0.01))
    } else {
        (PLATE_NEAR, PLATE_FAR)
    };
    let uniform = spread <= 0.08;
    let ink = |p: &[f32; 4]| {
        let l = by_luma.map_or(0.0, |(plate, ink)| smoothstep(0.2, 0.7, (luma(p) - plate) / (ink - plate)));
        let c = if uniform { smoothstep(near, far, color_distance(p, &plate_rgb)) } else { 0.0 };
        l.max(c)
    };

    let plate_px = inside.iter().filter(|&&i| solid[i] && ink(&px[i]) < 0.5).count() as f32;
    let drawn_px = inside.iter().filter(|&&i| solid[i] && ink(&px[i]) >= 0.5).count() as f32;
    // Выбитый рисунок (почти прозрачный) — только на простой подложке: круг, квадрат
    let simple = hull >= 0.75 * box_area && bw.max(bh) as f32 <= 1.2 * bw.min(bh) as f32;
    // Простая подложка выпукла: всё внутри её ортогональной оболочки, кроме пикселя
    // сглаживания по краю, — подложка или прорезь, даже если прорезь выходит на край
    // (рычаг логотипа Steam на сером диске).
    let (region, deep) = if simple {
        let mut rows = vec![(usize::MAX, 0usize); h];
        let mut cols = vec![(usize::MAX, 0usize); w];
        for i in (0..n).filter(|&i| solid[i]) {
            let (x, y) = (i % w, i / w);
            rows[y] = (rows[y].0.min(x), rows[y].1.max(x));
            cols[x] = (cols[x].0.min(y), cols[x].1.max(y));
        }
        // Круглая подложка — вписанный эллипс: выбитый рычаг может срезать у диска целые
        // строки, и оболочка по строкам его бы потеряла. Круг узнаётся по пустым углам
        // рамки: у диска непрозрачного нет ближе 0,29 стороны по диагонали угла, у
        // скруглённого квадрата — есть. (Доля заполнения не годится: выбитый рисунок,
        // выходящий за диск, раздувает оболочку.)
        let corner = 0.25 * bw.min(bh) as f32;
        let round = !(0..n).filter(|&i| solid[i]).any(|i| {
            let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
            let (l, r) = (x - x0 as f32, (x1 + 1) as f32 - x);
            let (t, b) = (y - y0 as f32, (y1 + 1) as f32 - y);
            l.min(r) + t.min(b) < corner
        });
        let (ex, ey) = ((x0 + x1 + 1) as f32 * 0.5, (y0 + y1 + 1) as f32 * 0.5);
        let (rx, ry) = (bw as f32 * 0.5, bh as f32 * 0.5);
        let within = |x: usize, y: usize| {
            if round {
                let (dx, dy) = ((x as f32 + 0.5 - ex) / rx, (y as f32 + 0.5 - ey) / ry);
                dx * dx + dy * dy <= 1.0
            } else {
                rows[y].0 <= x && x <= rows[y].1 && cols[x].0 <= y && y <= cols[x].1
            }
        };
        let region: Vec<bool> = (0..n).map(|i| within(i % w, i / w) && !badge[i]).collect();
        let deep: Vec<bool> = (0..n)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                region[i]
                    && x > 0
                    && y > 0
                    && x + 1 < w
                    && y + 1 < h
                    && within(x - 1, y)
                    && within(x + 1, y)
                    && within(x, y - 1)
                    && within(x, y + 1)
            })
            .collect();
        (region, deep)
    } else {
        let region: Vec<bool> = (0..n).map(|i| !outside[i] && !badge[i]).collect();
        let deep: Vec<bool> = (0..n).map(|i| region[i] && !band[i]).collect();
        (region, deep)
    };
    let knocked: f32 = (0..n).filter(|&i| deep[i]).map(|i| 1.0 - px[i][3]).sum();
    if plate_px < 0.3 * hull {
        return None;
    }
    let contrast = (by_luma.is_some() || by_color) && drawn_px >= 0.02 * hull;
    if !contrast && !(simple && knocked >= 0.04 * hull) {
        return None;
    }
    let mut cover = vec![0.0f32; n];
    for i in (0..n).filter(|&i| region[i]) {
        let a = px[i][3];
        // Прорези в подложке — тоже часть рисунка, но не сглаженный внешний край
        // (а у полупрозрачных пикселей края цвет после масштабирования ненадёжен).
        let (hole, edge) = if deep[i] { (1.0, 1.0) } else { (0.0, smoothstep(0.35, 0.75, a)) };
        cover[i] = (a * ink(&px[i]) * edge + (1.0 - a) * hole).clamp(0.0, 1.0);
    }
    let total: f32 = cover.iter().sum();
    (total >= 0.02 * hull && total <= 0.85 * hull).then_some(cover)
}

/// Разобрать значок: маска рисунка, цветные метки и откуда взята форма
pub fn analyze(src: &Pixmap) -> Template {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let px = unpremultiply(src);
    let badge = find_badges(&px, w, h);
    let (mut cover, shape) = match plate_cover(&px, &badge, w, h) {
        Some(cover) => (cover, Shape::Plate),
        None => (
            px.iter().zip(&badge).map(|(p, &b)| if b { 0.0 } else { p[3] }).collect(),
            Shape::Silhouette,
        ),
    };
    // Зазор вокруг метки (около пикселя плитки), чтобы она читалась поверх рисунка
    let mut grown = badge.clone();
    for _ in 0..(w.max(h) / 40).max(1) {
        grown = (0..w * h).map(|i| grown[i] || neighbours8(w, h, i).any(|j| grown[j])).collect();
    }
    for i in (0..w * h).filter(|&i| grown[i] && !badge[i]) {
        cover[i] = 0.0;
    }
    Template { w, h, cover, badge, shape }
}

/// Наибольшая сторона, в которой значок разбирается: крупнее — усредняется до неё
pub const ANALYSIS_MAX: u32 = 96;

/// Уменьшение с усреднением по площади (box-фильтр с дробными весами) для
/// премультиплицированного RGBA: каждый пиксель результата — среднее всего, что под ним,
/// без пропусков, лесенки и муара билинейного уменьшения в разы.
pub fn downscale_area(src: &Pixmap, tw: u32, th: u32) -> Option<Pixmap> {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (tw, th) = (tw.max(1) as usize, th.max(1) as usize);
    if tw > sw || th > sh {
        return None;
    }
    // Веса одной оси: для выходного i — (первый исходный индекс, веса по порядку)
    let weights = |from: usize, to: usize| -> Vec<(usize, Vec<f32>)> {
        let step = from as f32 / to as f32;
        (0..to)
            .map(|i| {
                let (a, b) = (i as f32 * step, (i + 1) as f32 * step);
                let first = a.floor() as usize;
                let last = (b.ceil() as usize).min(from);
                let w = (first..last)
                    .map(|j| ((j + 1) as f32).min(b) - (j as f32).max(a))
                    .map(|w| w.max(0.0) / step)
                    .collect();
                (first, w)
            })
            .collect()
    };
    let (wx, wy) = (weights(sw, tw), weights(sh, th));
    let data = src.data();
    // Сначала по горизонтали, потом по вертикали
    let mut rows = vec![0.0f32; tw * sh * 4];
    for y in 0..sh {
        for (x, (first, w)) in wx.iter().enumerate() {
            let out = &mut rows[(y * tw + x) * 4..(y * tw + x) * 4 + 4];
            for (k, &w) in w.iter().enumerate() {
                let p = &data[(y * sw + first + k) * 4..(y * sw + first + k) * 4 + 4];
                for c in 0..4 {
                    out[c] += p[c] as f32 * w;
                }
            }
        }
    }
    let mut out = Pixmap::new(tw as u32, th as u32)?;
    let dst = out.data_mut();
    for (y, (first, w)) in wy.iter().enumerate() {
        for x in 0..tw {
            let mut acc = [0.0f32; 4];
            for (k, &w) in w.iter().enumerate() {
                let p = &rows[((first + k) * tw + x) * 4..((first + k) * tw + x) * 4 + 4];
                for c in 0..4 {
                    acc[c] += p[c] * w;
                }
            }
            let a = acc[3].round().clamp(0.0, 255.0);
            let o = &mut dst[(y * tw + x) * 4..(y * tw + x) * 4 + 4];
            for c in 0..3 {
                // Премультипликация: цвет не больше альфы
                o[c] = acc[c].round().clamp(0.0, a) as u8;
            }
            o[3] = a as u8;
        }
    }
    Some(out)
}

/// Значок, вписанный усреднением в `max` по наибольшей стороне (меньший — как есть)
pub fn fit_within(src: &Pixmap, max: u32) -> Option<Pixmap> {
    let long = src.width().max(src.height());
    if long <= max {
        return None;
    }
    let k = max as f32 / long as f32;
    downscale_area(
        src,
        ((src.width() as f32 * k).round() as u32).max(1),
        ((src.height() as f32 * k).round() as u32).max(1),
    )
}

/// Плитка `size`×`size` физических пикселей: рисунок цвета `ink`, метки в своём цвете,
/// подогнанные к единому размеру и оптическому центру с привязкой к пиксельной сетке.
/// `None` — в значке нечего рисовать.
pub fn render(src: &Pixmap, size: u32, ink: Color) -> Option<Pixmap> {
    // Разбор в высоком разрешении (но не больше ANALYSIS_MAX), уменьшение — в самом конце
    let fitted = fit_within(src, ANALYSIS_MAX);
    let src = fitted.as_ref().unwrap_or(src);
    let t = analyze(src);
    tracing::debug!("Tray icon template: {:?}", t.shape);
    let (w, h) = (t.w, t.h);
    let orig = src.data();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    let (mut sum, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..w * h {
        let c = t.cover[i];
        let visible = c > 0.04 || (t.badge[i] && orig[i * 4 + 3] > 10);
        if !visible {
            continue;
        }
        let (x, y) = (i % w, i / w);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
        sum += c;
        sx += c * (x as f32 + 0.5);
        sy += c * (y as f32 + 0.5);
    }
    if x0 > x1 || y0 > y1 {
        return None;
    }
    let (bw, bh) = ((x1 - x0 + 1) as f32, (y1 - y0 + 1) as f32);
    // Плотные фигуры (залитый круг) зрительно крупнее контурных — чуть уменьшаем
    let fill = (sum / (bw * bh)).clamp(0.0, 1.0);
    let s = size as f32;
    let target = s * 0.94 * (1.0 - 0.12 * fill);
    let long = bw.max(bh);
    let mut scale = target / long;
    // Длинная сторона рисунка — целое число пикселей
    scale = (long * scale).round().max(1.0) / long;
    let (bcx, bcy) = ((x0 as f32 + x1 as f32 + 1.0) * 0.5, (y0 as f32 + y1 as f32 + 1.0) * 0.5);
    let (cx, cy) = if sum > 0.0 {
        (0.7 * bcx + 0.3 * sx / sum, 0.7 * bcy + 0.3 * sy / sum)
    } else {
        (bcx, bcy)
    };
    // Левый (верхний) край рамки рисунка ложится ровно на пиксель плитки
    let fit = |c: f32, lo: usize, hi: usize| {
        let extent = (hi - lo + 1) as f32 * scale;
        let edge = (s * 0.5 - (c - lo as f32) * scale).round();
        let edge = edge.clamp(0.0, (s - extent).floor().max(0.0));
        edge - lo as f32 * scale
    };
    let (tx, ty) = (fit(cx, x0, x1), fit(cy, y0, y1));

    let ink = ink.to_color_u8();
    let ia = ink.alpha() as f32 / 255.0;
    let mut layer = Pixmap::new(w as u32, h as u32)?;
    for (i, out) in layer.data_mut().chunks_exact_mut(4).enumerate() {
        if t.badge[i] {
            out.copy_from_slice(&orig[i * 4..i * 4 + 4]);
            continue;
        }
        let a = t.cover[i] * ia;
        if a <= 0.0 {
            continue;
        }
        let q = |c: u8| (c as f32 * a).round() as u8;
        out.copy_from_slice(&[q(ink.red()), q(ink.green()), q(ink.blue()), (a * 255.0).round() as u8]);
    }
    // Суперсэмплинг: рисуем в k раз крупнее (уменьшение не сильнее чем вдвое — билинейка
    // ещё честна) и усредняем блоки k×k. Сглаженные края без лесенки, без размытия кляксой.
    let k = ((1.0 / scale).ceil() as u32).clamp(1, 4);
    let kf = k as f32;
    let mut big = Pixmap::new(size * k, size * k)?;
    big.draw_pixmap(
        0,
        0,
        layer.as_ref(),
        &PixmapPaint {
            quality: if k == 1 { FilterQuality::Bicubic } else { FilterQuality::Bilinear },
            ..Default::default()
        },
        Transform::from_row(scale * kf, 0.0, 0.0, scale * kf, tx * kf, ty * kf),
        None,
    );
    if k == 1 {
        return Some(big);
    }
    downscale_area(&big, size, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: usize = 40;

    fn canvas(f: impl Fn(f32, f32) -> Option<[u8; 4]>) -> Pixmap {
        let mut pm = Pixmap::new(S as u32, S as u32).unwrap();
        for (i, px) in pm.data_mut().chunks_exact_mut(4).enumerate() {
            let (x, y) = ((i % S) as f32 + 0.5, (i / S) as f32 + 0.5);
            if let Some([r, g, b, a]) = f(x, y) {
                let k = a as f32 / 255.0;
                px.copy_from_slice(&[
                    (r as f32 * k).round() as u8,
                    (g as f32 * k).round() as u8,
                    (b as f32 * k).round() as u8,
                    a,
                ]);
            }
        }
        pm
    }

    fn in_disc(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> bool {
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    }

    /// Чёрная «звёздочка» (крест из двух полос) на прозрачном фоне
    fn star() -> Pixmap {
        canvas(|x, y| {
            let bar = |a: f32, b: f32| (a - 20.0).abs() < 3.0 && (b - 20.0).abs() < 16.0;
            (bar(x, y) || bar(y, x)).then_some([10, 10, 10, 255])
        })
    }

    fn alpha_at(pm: &Pixmap, x: u32, y: u32) -> u8 {
        pm.pixel(x, y).unwrap().alpha()
    }

    #[test]
    fn alpha_silhouette_is_recoloured_to_the_ink() {
        let t = analyze(&star());
        assert_eq!(t.shape, Shape::Silhouette);
        assert!(t.badge.iter().all(|b| !b));
        let tile = render(&star(), 18, Color::WHITE).unwrap();
        let c = tile.pixel(9, 9).unwrap();
        assert!(c.alpha() > 200, "центр звёздочки закрашен");
        assert_eq!((c.red(), c.green(), c.blue()), (c.alpha(), c.alpha(), c.alpha()), "белый, а не чёрный");
        assert_eq!(alpha_at(&tile, 1, 1), 0, "угол пуст");
        // На светлой панели — тёмные чернила
        let dark = render(&star(), 18, Color::from_rgba8(30, 30, 30, 255)).unwrap();
        assert!(dark.pixel(9, 9).unwrap().red() < 40);
    }

    #[test]
    fn opaque_plate_is_cut_out_and_the_drawing_kept() {
        // Синий скруглённый квадрат на весь значок с белым кругом посередине
        let icon = canvas(|x, y| {
            let inner = |v: f32| (v - 20.0).abs() <= 18.0;
            if !(inner(x) && inner(y)) {
                return None;
            }
            Some(if in_disc(x, y, 20.0, 20.0, 8.0) { [250, 250, 250, 255] } else { [20, 90, 200, 255] })
        });
        let t = analyze(&icon);
        assert_eq!(t.shape, Shape::Plate);
        assert!(t.cover[20 * S + 20] > 0.9, "рисунок остался");
        assert!(t.cover[4 * S + 4] < 0.05, "подложка вырезана");
        let tile = render(&icon, 18, Color::BLACK).unwrap();
        assert!(alpha_at(&tile, 9, 9) > 200);
        assert_eq!(alpha_at(&tile, 0, 0), 0, "от квадрата не осталось угла");
        // Рисунок увеличен до единого размера: круг занимает почти всю плитку
        let row: Vec<u8> = (0..18).map(|x| alpha_at(&tile, x, 9)).collect();
        assert!(row.iter().filter(|&&a| a > 128).count() >= 13, "{row:?}");
    }

    #[test]
    fn gradient_plate_is_cut_out_by_brightness() {
        // Круг с градиентом от тёмно-синего к синему, как у Steam, и светлый рисунок
        let icon = canvas(|x, y| {
            if !in_disc(x, y, 20.0, 20.0, 19.0) {
                return None;
            }
            if in_disc(x, y, 24.0, 15.0, 6.0) || ((x - y).abs() < 3.0 && x < 22.0 && x > 6.0) {
                return Some([235, 238, 240, 255]);
            }
            let t = y / 40.0;
            Some([(10.0 + 20.0 * t) as u8, (30.0 + 80.0 * t) as u8, (70.0 + 150.0 * t) as u8, 255])
        });
        let t = analyze(&icon);
        assert_eq!(t.shape, Shape::Plate);
        assert!(t.cover[15 * S + 24] > 0.9, "рисунок остался");
        assert!(t.cover[34 * S + 30] < 0.05, "светлая часть градиента — всё ещё подложка");
        assert!(t.cover[4 * S + 20] < 0.05);
    }

    #[test]
    fn faint_drawing_on_a_white_square_is_stretched_to_full_ink() {
        // Значок Steam, как его видел пользователь: белый квадрат с лёгким градиентом
        // (255 → 234) и бледно-серый круг с логотипом внутри (222)
        let icon = canvas(|x, y| {
            if in_disc(x, y, 20.0, 20.0, 12.0) && !in_disc(x, y, 24.0, 16.0, 3.5) {
                return Some([222, 222, 222, 255]);
            }
            let v = (255.0 - 21.0 * y / 40.0) as u8;
            Some([v, v, v.saturating_add(4).min(238).max(v), 255])
        });
        let t = analyze(&icon);
        assert_eq!(t.shape, Shape::Plate);
        assert!(t.cover[22 * S + 14] > 0.9, "бледный круг — полноценный рисунок");
        assert!(t.cover[38 * S + 20] < 0.05, "низ градиента подложки не рисуется");
        assert!(t.cover[1 * S + 1] < 0.05);
        assert!(t.cover[16 * S + 24] < 0.3, "логотип в круге остаётся прорезью");
        let tile = render(&icon, 18, Color::WHITE).unwrap();
        assert_eq!(alpha_at(&tile, 0, 0), 0, "от белого квадрата ничего не осталось");
        assert!(alpha_at(&tile, 4, 9) > 200);
    }

    #[test]
    fn steam_like_icon_name_prefers_tray_variants() {
        let c = crate::icons::tray_icon_candidates("steam");
        assert_eq!(c.first().map(String::as_str), Some("steam-symbolic"));
        assert!(c.iter().any(|n| n == "steam_tray_mono"));
        assert_eq!(c.last().map(String::as_str), Some("steam"));
        // Уже панельный вариант и путь из IconThemePath
        assert_eq!(crate::icons::tray_icon_candidates("steam_tray_mono"), ["steam_tray_mono"]);
        let p = crate::icons::tray_icon_candidates("/opt/app/icons/app.png");
        assert_eq!(p.first().map(String::as_str), Some("/opt/app/icons/app-symbolic.svg"));
        assert_eq!(p.last().map(String::as_str), Some("/opt/app/icons/app.png"));
    }

    #[test]
    fn knocked_out_logo_reaching_the_plate_edge_is_kept() {
        // Как steam_tray_mono: серый диск, логотип выбит почти прозрачным (альфа 20),
        // и его рычаг выходит на край диска
        let icon = canvas(|x, y| {
            if !in_disc(x, y, 20.0, 20.0, 18.0) {
                return None;
            }
            let arm = (x - y).abs() < 3.0 && x < 20.0;
            if in_disc(x, y, 24.0, 16.0, 5.0) || arm {
                return Some([0, 0, 0, 20]);
            }
            Some([222, 222, 222, 255])
        });
        let t = analyze(&icon);
        assert_eq!(t.shape, Shape::Plate);
        assert!(t.cover[16 * S + 24] > 0.9);
        assert!(t.cover[12 * S + 12] > 0.9, "середина рычага");
        assert!(t.cover[8 * S + 8] > 0.8, "рычаг у самого края диска");
        assert!(t.cover[30 * S + 20] < 0.05, "подложка вырезана");
    }

    #[test]
    fn transparent_cutout_in_a_plate_counts_as_drawing() {
        // Серый круг с белой полосой и прозрачной прорезью внутри
        let icon = canvas(|x, y| {
            if !in_disc(x, y, 20.0, 20.0, 19.0) {
                return None;
            }
            if (y - 14.0).abs() < 2.0 && (x - 20.0).abs() < 9.0 {
                return Some([255, 255, 255, 255]);
            }
            if (y - 26.0).abs() < 2.0 && (x - 20.0).abs() < 9.0 {
                return None;
            }
            Some([120, 120, 120, 255])
        });
        let t = analyze(&icon);
        assert_eq!(t.shape, Shape::Plate);
        assert!(t.cover[14 * S + 20] > 0.9);
        assert!(t.cover[26 * S + 20] > 0.9, "прорезь — часть рисунка");
        assert!(t.cover[20 * S + 20] < 0.05);
        assert!(t.cover[20 * S + 1] < 0.05, "внешний край подложки не даёт кольца");
    }

    #[test]
    fn saturated_badge_stays_coloured() {
        // Белый самолётик-треугольник и красная точка в углу
        let icon = canvas(|x, y| {
            if in_disc(x, y, 33.0, 33.0, 5.0) {
                return Some([235, 30, 40, 255]);
            }
            (y > 6.0 && y < 26.0 && x > 4.0 && x < 4.0 + (y - 6.0) * 1.3).then_some([255, 255, 255, 255])
        });
        let t = analyze(&icon);
        assert_eq!(t.shape, Shape::Silhouette);
        assert!(t.badge[33 * S + 33]);
        assert!(!t.badge[16 * S + 8]);
        let tile = render(&icon, 18, Color::from_rgba8(30, 30, 30, 255)).unwrap();
        let red = (0..18)
            .flat_map(|y| (0..18).map(move |x| (x, y)))
            .filter_map(|(x, y)| tile.pixel(x, y))
            .any(|p| p.alpha() > 200 && p.red() > 180 && p.green() < 80);
        assert!(red, "красная точка сохранила цвет");
    }

    #[test]
    fn large_coloured_shape_is_not_a_badge() {
        // Целиком цветной щит — это значок, а не метка
        let icon = canvas(|x, y| (in_disc(x, y, 20.0, 20.0, 15.0)).then_some([20, 200, 120, 255]));
        let t = analyze(&icon);
        assert!(t.badge.iter().all(|b| !b));
        let tile = render(&icon, 18, Color::WHITE).unwrap();
        let c = tile.pixel(9, 9).unwrap();
        assert_eq!((c.red(), c.green(), c.blue()), (255, 255, 255));
    }

    #[test]
    fn same_glyph_at_different_offsets_lands_on_the_same_pixels() {
        let at = |dx: f32, dy: f32, r: f32| {
            canvas(move |x, y| in_disc(x, y, 14.0 + dx, 14.0 + dy, r).then_some([0, 0, 0, 255]))
        };
        let a = render(&at(0.0, 0.0, 8.0), 36, Color::WHITE).unwrap();
        let b = render(&at(11.0, 9.0, 8.0), 36, Color::WHITE).unwrap();
        assert_eq!(a.data(), b.data(), "положение в исходнике не влияет на плитку");
        // Маленький и большой круг приводятся к одному размеру
        let c = render(&at(6.0, 6.0, 13.0), 36, Color::WHITE).unwrap();
        let width = |pm: &Pixmap| (0..36).filter(|&x| alpha_at(pm, x, 18) > 128).count() as i32;
        assert!((width(&a) - width(&c)).abs() <= 1, "{} vs {}", width(&a), width(&c));
        // HiDPI: на ×2 вдвое больше пикселей, тот же вид
        let one = render(&at(0.0, 0.0, 8.0), 18, Color::WHITE).unwrap();
        assert!((width(&a) - 2 * (0..18).filter(|&x| alpha_at(&one, x, 9) > 128).count() as i32).abs() <= 2);
    }

    /// Настоящие значки из живого трея (дамп IconPixmap по D-Bus и файл IconName Steam):
    /// как в `tray::pixmap` — крупное усредняется до ANALYSIS_MAX, затем плитка.
    fn real(png: &[u8], size: u32, ink: Color) -> (Template, Pixmap) {
        let src = Pixmap::decode_png(png).unwrap();
        let src = fit_within(&src, ANALYSIS_MAX).unwrap_or(src);
        (analyze(&src), render(&src, size, ink).unwrap())
    }

    /// Доля «мутных» полупрозрачных пикселей среди видимых: у чёткого силуэта это лишь
    /// сглаженный край, у кляксы — почти всё
    /// Допустимая мутность: на 18 px тонкие штрихи — почти сплошь сглаженный край
    fn murk_limit(size: u32) -> f32 {
        if size < 24 { 0.6 } else { 0.45 }
    }

    fn murk(tile: &Pixmap) -> f32 {
        let alphas: Vec<u8> = tile.pixels().iter().map(|p| p.alpha()).filter(|&a| a > 8).collect();
        alphas.iter().filter(|&&a| (40..=215).contains(&a)).count() as f32 / alphas.len().max(1) as f32
    }

    fn ink_share(tile: &Pixmap) -> f32 {
        tile.pixels().iter().map(|p| p.alpha() as f32 / 255.0).sum::<f32>() / (tile.width() * tile.height()) as f32
    }

    fn has_red(tile: &Pixmap) -> bool {
        tile.pixels().iter().any(|p| {
            let c = p.demultiply();
            c.alpha() > 160 && c.red() > 180 && c.green() < 100 && c.blue() < 100
        })
    }

    fn only_ink_or_red(tile: &Pixmap, ink: (u8, u8, u8)) {
        for p in tile.pixels().iter().filter(|p| p.alpha() > 60) {
            let c = p.demultiply();
            let near = |a: u8, b: u8| a.abs_diff(b) <= 12;
            let is_ink = near(c.red(), ink.0) && near(c.green(), ink.1) && near(c.blue(), ink.2);
            assert!(is_ink || c.red() > 150, "посторонний цвет {c:?}");
        }
    }

    #[test]
    fn real_telegram_plane_with_unread_dot() {
        let png = include_bytes!("testdata/tray/telegram_64x64.png");
        for size in [18, 36] {
            let (t, tile) = real(png, size, Color::WHITE);
            assert_eq!(t.shape, Shape::Silhouette);
            assert!(t.badge.iter().any(|&b| b), "красная точка — метка");
            assert!(has_red(&tile), "точка осталась красной на {size}");
            only_ink_or_red(&tile, (255, 255, 255));
            let m = murk(&tile);
            assert!(m < murk_limit(size), "самолётик чёткий, а не клякса: {m:.2} на {size}");
        }
        // Пиксмап 16×16, растянутый до плитки (то, что раньше выбирал tray), заметно мутнее
        let (_, blob) = real(include_bytes!("testdata/tray/telegram_16x16.png"), 36, Color::WHITE);
        let (_, sharp) = real(png, 36, Color::WHITE);
        assert!(murk(&blob) > murk(&sharp) + 0.1, "{:.2} vs {:.2}", murk(&blob), murk(&sharp));
    }

    #[test]
    fn real_mousevpn_shield_loses_its_plate_without_dirt() {
        let png = include_bytes!("testdata/tray/mousevpn_128x128.png");
        for size in [18, 36] {
            let (t, tile) = real(png, size, Color::WHITE);
            assert_eq!(t.shape, Shape::Plate);
            assert!(!has_red(&tile));
            only_ink_or_red(&tile, (255, 255, 255));
            let (m, ink) = (murk(&tile), ink_share(&tile));
            assert!(m < murk_limit(size), "щит без «грязи»: {m:.2} на {size}");
            assert!((0.15..0.6).contains(&ink), "рисунок, а не залитый квадрат: {ink:.2} на {size}");
        }
    }

    #[test]
    fn real_steam_tray_mono_reads_as_the_logo() {
        let png = include_bytes!("testdata/tray/steam_tray_mono.png");
        for size in [18, 36] {
            let (t, tile) = real(png, size, Color::from_rgba8(25, 25, 28, 255));
            assert_eq!(t.shape, Shape::Plate, "серый диск вырезан");
            only_ink_or_red(&tile, (25, 25, 28));
            let (m, ink) = (murk(&tile), ink_share(&tile));
            assert!(m < murk_limit(size), "логотип чёткий: {m:.2} на {size}");
            // Залитый диск дал бы ~0,6; логотип (колесо и рычаг) — заметно меньше
            assert!((0.15..0.5).contains(&ink), "логотип, а не диск: {ink:.2} на {size}");
        }
    }

    #[test]
    fn real_claude_star_is_only_recoloured() {
        let png = include_bytes!("testdata/tray/claude_64x64.png");
        for size in [18, 36] {
            let (t, tile) = real(png, size, Color::WHITE);
            assert_eq!(t.shape, Shape::Silhouette);
            only_ink_or_red(&tile, (255, 255, 255));
            let m = murk(&tile);
            assert!(m < murk_limit(size), "звёздочка чёткая: {m:.2} на {size}");
        }
    }

    #[test]
    fn area_downscale_keeps_mean_and_thin_lines() {
        // Однопиксельная сетка 1:4 при уменьшении вчетверо не пропадает и не мерцает
        let mut src = Pixmap::new(64, 64).unwrap();
        for (i, p) in src.data_mut().chunks_exact_mut(4).enumerate() {
            if (i % 64) % 4 == 0 {
                p.copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let out = downscale_area(&src, 16, 16).unwrap();
        for p in out.pixels() {
            assert!((p.alpha() as i32 - 64).abs() <= 1, "{}", p.alpha());
        }
        assert!(fit_within(&src, 96).is_none());
        let big = Pixmap::new(512, 256).unwrap();
        let fit = fit_within(&big, 96).unwrap();
        assert_eq!((fit.width(), fit.height()), (96, 48));
    }

    #[test]
    fn empty_icon_has_no_template() {
        assert!(render(&Pixmap::new(40, 40).unwrap(), 18, Color::WHITE).is_none());
    }
}

