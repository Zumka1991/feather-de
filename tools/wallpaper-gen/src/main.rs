//! Процедурные обои Пёрышка. Каждый кадр — чистая функция координат пикселя, поэтому
//! результат воспроизводим и не зависит от сторонних изображений.
//!
//! cargo run --release -- <каталог> [имя…] [--size 960x540]
//! Без имён рисует все. Пишет `feather-<имя>.jpg` (2560×1440, качество 92) и
//! `feather-<имя>-thumb.png` (240×135) для галереи настроек.

use std::f32::consts::TAU;
use std::path::PathBuf;

type C = [f32; 3];

const NAMES: [&str; 5] = ["waves", "dunes", "ridges", "emerald", "graphite"];

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().expect("каталог для обоев"));
    let (mut w, mut h) = (2560u32, 1440u32);
    let mut only = Vec::new();
    while let Some(a) = args.next() {
        if a == "--size" {
            let s = args.next().expect("--size WxH");
            let (a, b) = s.split_once('x').expect("WxH");
            w = a.parse().unwrap();
            h = b.parse().unwrap();
        } else {
            only.push(a);
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    for name in NAMES.iter().filter(|n| only.is_empty() || only.iter().any(|o| o == *n)) {
        let started = std::time::Instant::now();
        let img = render(name, w, h);
        let jpg = out.join(format!("feather-{name}.jpg"));
        let file = std::io::BufWriter::new(std::fs::File::create(&jpg).unwrap());
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 92).encode_image(&img).unwrap();
        let thumb = image::imageops::resize(&img, 240, 135, image::imageops::FilterType::Lanczos3);
        thumb.save(out.join(format!("feather-{name}-thumb.png"))).unwrap();
        eprintln!("{name}: {:?}", started.elapsed());
    }
}

fn render(name: &str, w: u32, h: u32) -> image::RgbImage {
    let f: fn(f32, f32, f32, f32) -> C = match name {
        "waves" => waves,
        "dunes" => dunes,
        "ridges" => ridges,
        "emerald" => emerald,
        "graphite" => graphite,
        _ => panic!("нет обоев {name}"),
    };
    let (wf, hf) = (w as f32, h as f32);
    // Пиксели в долях высоты: x ∈ [0, aspect], y ∈ [0, 1]; px — размер пикселя
    let px = 1.0 / hf;
    let mut img = image::RgbImage::new(w, h);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let rows: Vec<Vec<u8>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                s.spawn(move || {
                    let mut out = Vec::new();
                    let mut y = t as u32;
                    while y < h {
                        let mut row = vec![0u8; w as usize * 3];
                        for x in 0..w {
                            // 2×2 сглаживание для тонких линий и краёв
                            let mut acc = [0.0f32; 3];
                            for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                                let c = f((x as f32 + ox) / hf, (y as f32 + oy) / hf, wf / hf, px);
                                for i in 0..3 {
                                    acc[i] += c[i] * 0.25;
                                }
                            }
                            let px8 = encode(acc, x, y);
                            row[x as usize * 3..x as usize * 3 + 3].copy_from_slice(&px8);
                        }
                        out.push((y, row));
                        y += threads as u32;
                    }
                    out
                })
            })
            .collect();
        let mut all: Vec<(u32, Vec<u8>)> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        all.sort_by_key(|r| r.0);
        all.into_iter().map(|r| r.1).collect()
    });
    for (y, row) in rows.into_iter().enumerate() {
        for x in 0..w as usize {
            img.put_pixel(x as u32, y as u32, image::Rgb([row[x * 3], row[x * 3 + 1], row[x * 3 + 2]]));
        }
    }
    img
}

// ───────────── цвет и шум ─────────────

fn lin(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn gamma(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
/// sRGB hex → линейный цвет
fn hex(c: u32) -> C {
    [lin(((c >> 16) & 255) as f32 / 255.0), lin(((c >> 8) & 255) as f32 / 255.0), lin((c & 255) as f32 / 255.0)]
}
fn mix(a: C, b: C, t: f32) -> C {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}
fn add(a: C, b: C, k: f32) -> C {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}
fn scale(a: C, k: f32) -> C {
    [a[0] * k, a[1] * k, a[2] * k]
}
/// Плавная многоточечная шкала цветов
fn ramp(stops: &[(f32, C)], t: f32) -> C {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        if t <= w[1].0 {
            let k = (t - w[0].0) / (w[1].0 - w[0].0);
            return mix(w[0].1, w[1].1, k * k * (3.0 - 2.0 * k));
        }
    }
    stops[stops.len() - 1].1
}
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 16_777_216.0
}
fn noise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let (u, v) = (fx * fx * fx * (fx * (fx * 6.0 - 15.0) + 10.0), fy * fy * fy * (fy * (fy * 6.0 - 15.0) + 10.0));
    let a = hash(xi, yi, seed);
    let b = hash(xi + 1, yi, seed);
    let c = hash(xi, yi + 1, seed);
    let d = hash(xi + 1, yi + 1, seed);
    a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v
}
fn fbm(x: f32, y: f32, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut f, mut norm) = (0.0, 0.5, 1.0, 0.0);
    for o in 0..octaves {
        sum += amp * noise(x * f, y * f, seed + o * 17);
        norm += amp;
        amp *= 0.5;
        f *= 2.03;
    }
    sum / norm
}
/// Линейный цвет → sRGB с треугольным дизерингом против полос на градиентах
fn encode(c: C, x: u32, y: u32) -> [u8; 3] {
    let mut out = [0u8; 3];
    for i in 0..3 {
        let n = hash(x as i32, y as i32, 7 + i as u32) + hash(x as i32, y as i32, 91 + i as u32) - 1.0;
        out[i] = (gamma(c[i]) * 255.0 + n * 0.9 + 0.5).clamp(0.0, 255.0) as u8;
    }
    out
}

// ───────────── «Волны»: стеклянные ленты над ночным индиго ─────────────

fn waves(x: f32, y: f32, aspect: f32, px: f32) -> C {
    let u = x / aspect;
    let mut col = ramp(
        &[(0.0, hex(0x070a1c)), (0.45, hex(0x12113a)), (0.8, hex(0x22104a)), (1.0, hex(0x14082a))],
        y,
    );
    let glow = (-((u - 0.72).powi(2) / 0.06 + (y - 0.62).powi(2) / 0.05)).exp();
    col = add(col, hex(0x3a2fa0), glow * 0.35);
    let palette = [hex(0x2fd4ff), hex(0x4f8dff), hex(0x7a5cff), hex(0xb35cff), hex(0xff5fae)];
    for k in 0..9 {
        let kf = k as f32;
        let c = 0.24 + 0.30 * u + kf * 0.04
            + 0.055 * (TAU * (0.62 * u + 0.11 * kf + 0.05)).sin()
            + 0.022 * (TAU * (1.7 * u + 0.29 * kf + 0.4)).sin()
            + 0.008 * (TAU * (3.9 * u - 0.17 * kf)).sin();
        let dy = (y - c) / px;
        if dy < -60.0 {
            continue;
        }
        let tone = ramp(
            &[(0.0, palette[0]), (0.3, palette[1]), (0.55, palette[2]), (0.8, palette[3]), (1.0, palette[4])],
            (u * 0.62 + kf * 0.045).min(1.0),
        );
        let edge = smooth(-1.0, 1.0, dy);
        let fall = (-(dy.max(0.0) * px) / 0.11).exp();
        let body = edge * (0.06 + 0.5 * fall);
        col = mix(col, scale(tone, 0.5 + 0.25 * fall), body);
        // Яркий стеклянный край и мягкое свечение над ним
        let rim = (-(dy / 1.6).powi(2)).exp();
        col = add(col, tone, rim * 0.55);
        let halo = (-(-dy).max(0.0) / 26.0).exp() * (1.0 - edge);
        col = add(col, tone, halo * 0.05);
    }
    // Спокойный верх под строку меню
    col = mix(col, hex(0x070a1c), smooth(0.14, 0.0, y) * 0.4);
    vignette(col, u, y, 0.35)
}

// ───────────── «Дюны»: закат над слоями песка ─────────────

fn dunes(x: f32, y: f32, aspect: f32, px: f32) -> C {
    let u = x / aspect;
    let sky = ramp(
        &[(0.0, hex(0x4b3f8f)), (0.22, hex(0x8a65b0)), (0.40, hex(0xe08fa0)), (0.52, hex(0xffbf94)), (0.6, hex(0xffe0b0))],
        y,
    );
    let (sx, sy) = (0.64, 0.50);
    let d = (((u - sx) * aspect).powi(2) + (y - sy).powi(2)).sqrt();
    let mut col = add(sky, hex(0xffe9c4), (-(d / 0.16).powi(2)).exp() * 0.55 + (-(d / 0.5)).exp() * 0.15);
    let sun = smooth(0.042 + px, 0.042 - px, d);
    col = mix(col, hex(0xfff6e2), sun);
    let layers: [(f32, u32, f32, u32); 6] = [
        (0.53, 0xeeb08f, 0.030, 3),
        (0.58, 0xe09a7d, 0.040, 5),
        (0.645, 0xcc7f68, 0.050, 8),
        (0.72, 0xb06352, 0.060, 11),
        (0.80, 0x8a4840, 0.070, 13),
        (0.89, 0x5e2e2e, 0.080, 17),
    ];
    for (k, &(base, tone, amp, seed)) in layers.iter().enumerate() {
        let kf = k as f32;
        let crest = |u: f32| {
            let t = u * (1.6 + kf * 0.35) + seed as f32 * 0.37;
            // Асимметричный гребень: пологий наветренный склон и крутой подветренный
            let s = (TAU * t).sin() * 0.6 + (TAU * t * 2.1 + 1.3).sin() * 0.25 + (fbm(t * 1.7, kf, 3, seed) - 0.5) * 0.9;
            base - amp * s.abs().powf(0.8) * s.signum() * 0.6 - amp * 0.4
        };
        let c = crest(u);
        let dy = (y - c) / px;
        if dy < -2.0 {
            continue;
        }
        let slope = (crest(u + 0.002) - c) / 0.002;
        let tone = hex(tone);
        // Склон к солнцу (справа) светится, обратный уходит в тень
        let near = (-(dy.max(0.0) * px) / 0.018).exp();
        let lit = smooth(-0.5, 0.6, -slope) * near;
        let mut layer = mix(scale(tone, 0.78), add(tone, hex(0xffc08a), 0.25), lit);
        let depth = (dy * px / 0.08).min(1.0);
        layer = mix(layer, scale(tone, 0.6), depth * 0.5);
        // Дымка: дальние слои растворяются в небе
        layer = mix(layer, hex(0xffd2b0), (0.5 - kf * 0.1).max(0.0));
        let rim = (-(dy / 1.8).powi(2)).exp() * (0.4 + 0.6 * lit);
        layer = add(layer, hex(0xffe4c0), rim * 0.35);
        col = mix(col, layer, smooth(-1.0, 1.0, dy));
    }
    vignette(col, u, y, 0.22)
}

// ───────────── «Хребты»: горы в синий час ─────────────

fn ridges(x: f32, y: f32, aspect: f32, px: f32) -> C {
    let u = x / aspect;
    let mut col = ramp(
        &[(0.0, hex(0x0a1430)), (0.25, hex(0x1d2d63)), (0.45, hex(0x5a5c9e)), (0.58, hex(0xc993b8)), (0.66, hex(0xf2b8a8))],
        y,
    );
    // Редкие звёзды в верхней части неба
    let cell = 0.018;
    let (cx, cy) = ((x / cell).floor(), (y / cell).floor());
    let r = hash(cx as i32, cy as i32, 3);
    if r > 0.9 {
        let (ox, oy) = (hash(cx as i32, cy as i32, 5), hash(cx as i32, cy as i32, 9));
        let d = (((x / cell - cx - ox) * cell).powi(2) + ((y / cell - cy - oy) * cell).powi(2)).sqrt() / px;
        let b = (r - 0.9) / 0.1 * smooth(0.5, 0.1, y);
        col = add(col, hex(0xdfe6ff), (-(d / 1.1).powi(2)).exp() * b * 0.9);
    }
    // Тонкий месяц
    let (mx, my, mr) = (0.79 * aspect, 0.2, 0.032);
    let d1 = ((x - mx).powi(2) + (y - my).powi(2)).sqrt();
    let d2 = ((x - mx - 0.012).powi(2) + (y - my + 0.008).powi(2)).sqrt();
    let moon = smooth(mr + px, mr - px, d1) * smooth(mr - px, mr + px, d2);
    col = add(col, hex(0xa9b3ff), (-(d1 / 0.09).powi(2)).exp() * 0.08);
    col = mix(col, hex(0xfbf1df), moon);
    let layers: [(f32, u32, f32, u32); 6] = [
        (0.52, 0xb08fb8, 0.10, 2),
        (0.58, 0x7f6ea6, 0.12, 4),
        (0.65, 0x55548c, 0.13, 6),
        (0.73, 0x343a6e, 0.14, 8),
        (0.82, 0x1d2550, 0.15, 10),
        (0.92, 0x0d1433, 0.16, 12),
    ];
    let fog = hex(0xe8b6c0);
    for (k, &(base, tone, amp, seed)) in layers.iter().enumerate() {
        let kf = k as f32;
        let t = u * (2.2 + kf * 0.6) + seed as f32;
        // «Хребтовый» шум даёт острые вершины
        let n = 1.0 - (2.0 * fbm(t, kf * 3.1, 5, seed) - 1.0).abs();
        let ridge = base - amp * n.powf(1.6) * 0.9 + 0.02 * (TAU * (u * 0.7 + kf * 0.13)).sin();
        let dy = (y - ridge) / px;
        if dy < -2.0 {
            continue;
        }
        let tone = hex(tone);
        let mut layer = add(tone, hex(0xffc2c8), (-(dy / 2.5).powi(2)).exp() * 0.25 * (1.0 - kf / 6.0));
        // Туман стелется в долинах — нижняя часть дальних слоёв светлее
        // По высоте, а не от гребня: иначе под вершинами тянутся вертикальные полосы
        let valley = smooth(base - amp * 0.4, base + 0.12, y) * (0.42 - kf * 0.1).max(0.0);
        layer = mix(layer, fog, valley);
        col = mix(col, layer, smooth(-1.0, 1.0, dy));
    }
    vignette(col, u, y, 0.3)
}

// ───────────── «Изумруд»: жидкий шёлк ─────────────

fn emerald(x: f32, y: f32, aspect: f32, px: f32) -> C {
    let u = x / aspect;
    // Складки шёлка: синусоидальные полосы в дважды искривлённом пространстве
    let height = |x: f32, y: f32| {
        let (mut qx, mut qy) = (x / aspect * 1.5, y);
        for i in 0..2 {
            let fi = i as f32;
            let nx = qx + 0.22 * (2.4 * qy + 1.3 * fi + 0.4).sin() + 0.12 * (1.3 * qx + 2.0 * qy).sin();
            let ny = qy + 0.18 * (2.0 * qx - 0.9 * fi + 1.9).sin();
            qx = nx;
            qy = ny;
        }
        0.5 + 0.5 * (TAU * (qx * 1.7 + qy * 2.3 + 0.25 * (TAU * qx * 0.6).sin())).sin()
    };
    let h0 = height(x, y);
    let e = px * 3.0;
    // Свет сверху справа: производная вдоль направления света даёт блик и тень складки
    let slope = (height(x + e * 0.7, y - e * 0.7) - h0) / e * 0.06;
    let base = ramp(
        &[(0.0, hex(0x012624)), (0.3, hex(0x035447)), (0.55, hex(0x08866c)), (0.78, hex(0x2dbf91)), (1.0, hex(0x86e8bd))],
        h0 * 0.7 + 0.3 * (1.0 - y),
    );
    let light = slope.clamp(-1.0, 1.0);
    let mut col = scale(base, 0.75 + 0.45 * light.max(-0.6));
    col = add(col, hex(0xd8fff0), smooth(0.35, 1.0, light).powi(2) * 0.45 + light.max(0.0).powi(6) * 0.35);
    col = add(col, hex(0x1c3f8f), (-((u - 0.12).powi(2) / 0.08 + (y - 0.95).powi(2) / 0.1)).exp() * 0.22);
    col = add(col, hex(0xa6dc55), (-((u - 0.9).powi(2) / 0.05 + (y - 0.3).powi(2) / 0.06)).exp() * 0.1);
    col = mix(col, hex(0x022421), smooth(0.16, 0.0, y) * 0.35);
    vignette(col, u, y, 0.3)
}

// ───────────── «Графит»: светящиеся изолинии рельефа ─────────────

fn graphite(x: f32, y: f32, aspect: f32, px: f32) -> C {
    let u = x / aspect;
    let mut col = ramp(&[(0.0, hex(0x0c0d11)), (0.6, hex(0x14161c)), (1.0, hex(0x0e0f13))], y);
    col = add(col, hex(0x1d2333), (-((u - 0.62).powi(2) / 0.12 + (y - 0.55).powi(2) / 0.12)).exp() * 0.6);
    let height = |x: f32, y: f32| {
        let u = x / aspect;
        fbm(x * 1.7, y * 1.7, 4, 41) * 0.9
            + 0.75 * (-((u - 0.66).powi(2) / 0.035 + (y - 0.56).powi(2) / 0.05)).exp()
            + 0.35 * (-((u - 0.3).powi(2) / 0.02 + (y - 0.74).powi(2) / 0.03)).exp()
    };
    let levels = 26.0;
    let h0 = height(x, y);
    let gx = (height(x + px, y) - h0) / px;
    let gy = (height(x, y + px) - h0) / px;
    let grad = (gx * gx + gy * gy).sqrt() * levels * px + 1e-4;
    let v = h0 * levels;
    let dist = ((v + 0.5).fract() - 0.5).abs() / grad;
    let major = ((v + 0.5).floor() as i32).rem_euclid(5) == 0;
    let width = if major { 1.0 } else { 0.55 };
    let line = smooth(width + 0.8, width - 0.4, dist);
    let tone = ramp(&[(0.3, hex(0x2f6bff)), (0.6, hex(0x6a5cff)), (0.85, hex(0xc25cff)), (1.05, hex(0xff6aa8))], h0);
    // Ближе к краям и к строке меню линии гаснут
    let fade = smooth(0.0, 0.35, y) * (1.0 - 0.75 * ((u - 0.6).abs() * 1.6).min(1.0));
    col = add(col, tone, line * fade * if major { 0.85 } else { 0.38 });
    // Мягкое свечение вершин
    col = add(col, tone, smooth(0.7, 1.15, h0) * 0.08 * fade);
    vignette(col, u, y, 0.45)
}

fn vignette(c: C, u: f32, y: f32, k: f32) -> C {
    let d = ((u - 0.5).powi(2) * 1.2 + (y - 0.5).powi(2)).sqrt();
    scale(c, 1.0 - k * smooth(0.35, 0.85, d))
}
