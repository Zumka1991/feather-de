use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tiny_skia::{
    Color, FillRule, LinearGradient, Paint, PathBuilder, Pixmap, PixmapMut, Point, Rect,
    SpreadMode, Stroke, Transform,
};

// macOS metrics (points)
pub const TOP_BAR_HEIGHT: i32 = 24;
pub const TITLEBAR_HEIGHT: i32 = 28;
/// Room around a window frame for its (large, soft) macOS drop shadow
pub const SHADOW_PAD: i32 = 56;
pub const WINDOW_CORNER_RADIUS: f32 = 12.0;

pub const DOCK_BOTTOM_MARGIN: i32 = 6;
pub const DOCK_PAD_X: f32 = 8.0;
pub const DOCK_PAD_Y: f32 = 7.0;
pub const DOCK_ICON_BASE: f32 = 50.0;
pub const DOCK_ICON_GAP: f32 = 6.0;
pub const DOCK_RADIUS: f32 = 22.0;
/// Vertical space kept free for the Dock when a window is maximized
pub const DOCK_RESERVED_HEIGHT: i32 = 74;

/// Traffic light geometry (relative to the window's top-left corner)
pub const TRAFFIC_RADIUS: f32 = 6.0;
pub const TRAFFIC_CENTERS: [f32; 3] = [16.0, 36.0, 56.0];

static UI_SCALE_BITS: AtomicU32 = AtomicU32::new(0x3f800000); // 1.0f32
static DARK_MODE: AtomicBool = AtomicBool::new(false);

pub fn ui_scale() -> f32 {
    f32::from_bits(UI_SCALE_BITS.load(Ordering::Relaxed))
}

pub fn set_ui_scale(scale: f32) {
    let clamped = scale.clamp(1.0, 3.0);
    UI_SCALE_BITS.store(clamped.to_bits(), Ordering::Relaxed);
}

pub fn to_phys(logical: i32) -> i32 {
    ((logical as f32) * ui_scale()).round().max(1.0) as i32
}

pub fn scale_transform() -> Transform {
    let s = ui_scale();
    Transform::from_scale(s, s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    pub fn name(&self) -> &'static str {
        match self {
            Appearance::Light => "Светлое",
            Appearance::Dark => "Тёмное",
        }
    }
}

pub fn appearance() -> Appearance {
    if DARK_MODE.load(Ordering::Relaxed) {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

pub fn set_appearance(a: Appearance) {
    DARK_MODE.store(a == Appearance::Dark, Ordering::Relaxed);
}

/// Semantic colours of the system UI (macOS Tahoe look)
pub struct Palette {
    pub menubar_tint: Color,
    pub menubar_text: Color,
    pub menubar_highlight: Color,
    pub glass_fill: Color,
    pub glass_fill_strong: Color,
    pub glass_edge: Color,
    pub glass_outline: Color,
    pub dock_fill: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub text_tertiary: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub separator: Color,
    pub control_fill: Color,
    pub window_bg: Color,
    pub titlebar_active: Color,
    pub titlebar_inactive: Color,
    pub title_active: Color,
    pub title_inactive: Color,
    pub window_outline: Color,
    pub card: Color,
    pub traffic_inactive: Color,
    pub traffic_inactive_edge: Color,
    pub dock_dot: Color,
    pub shadow_strength: f32,
}

pub fn palette() -> Palette {
    match appearance() {
        Appearance::Light => Palette {
            menubar_tint: rgba(255, 255, 255, 70),
            menubar_text: rgba(0, 0, 0, 225),
            menubar_highlight: rgba(0, 0, 0, 28),
            glass_fill: rgba(246, 246, 248, 175),
            glass_fill_strong: rgba(250, 250, 252, 212),
            glass_edge: rgba(255, 255, 255, 210),
            glass_outline: rgba(0, 0, 0, 30),
            dock_fill: rgba(255, 255, 255, 105),
            text: rgba(0, 0, 0, 222),
            text_secondary: rgba(60, 60, 67, 165),
            text_tertiary: rgba(60, 60, 67, 95),
            accent: rgba(0, 122, 255, 255),
            on_accent: rgba(255, 255, 255, 255),
            separator: rgba(0, 0, 0, 26),
            control_fill: rgba(0, 0, 0, 14),
            window_bg: rgba(255, 255, 255, 255),
            titlebar_active: rgba(247, 247, 247, 255),
            titlebar_inactive: rgba(236, 236, 236, 255),
            title_active: rgba(0, 0, 0, 216),
            title_inactive: rgba(0, 0, 0, 70),
            window_outline: rgba(0, 0, 0, 46),
            card: rgba(242, 242, 247, 255),
            traffic_inactive: rgba(221, 221, 221, 255),
            traffic_inactive_edge: rgba(0, 0, 0, 30),
            dock_dot: rgba(0, 0, 0, 175),
            shadow_strength: 1.0,
        },
        Appearance::Dark => Palette {
            menubar_tint: rgba(0, 0, 0, 70),
            menubar_text: rgba(255, 255, 255, 235),
            menubar_highlight: rgba(255, 255, 255, 40),
            glass_fill: rgba(38, 38, 42, 185),
            glass_fill_strong: rgba(44, 44, 48, 215),
            glass_edge: rgba(255, 255, 255, 46),
            glass_outline: rgba(0, 0, 0, 120),
            dock_fill: rgba(60, 60, 66, 120),
            text: rgba(255, 255, 255, 235),
            text_secondary: rgba(235, 235, 245, 155),
            text_tertiary: rgba(235, 235, 245, 85),
            accent: rgba(10, 132, 255, 255),
            on_accent: rgba(255, 255, 255, 255),
            separator: rgba(255, 255, 255, 30),
            control_fill: rgba(255, 255, 255, 22),
            window_bg: rgba(30, 30, 32, 255),
            titlebar_active: rgba(52, 52, 55, 255),
            titlebar_inactive: rgba(40, 40, 42, 255),
            title_active: rgba(255, 255, 255, 220),
            title_inactive: rgba(255, 255, 255, 80),
            window_outline: rgba(255, 255, 255, 34),
            card: rgba(44, 44, 46, 255),
            traffic_inactive: rgba(78, 78, 80, 255),
            traffic_inactive_edge: rgba(0, 0, 0, 60),
            dock_dot: rgba(255, 255, 255, 200),
            shadow_strength: 1.6,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperTheme {
    TahoeDay,
    TahoeNight,
    SequoiaMidnight,
    FeatherCyan,
}

impl WallpaperTheme {
    pub fn name(&self) -> &'static str {
        match self {
            WallpaperTheme::TahoeDay => "Tahoe День",
            WallpaperTheme::TahoeNight => "Tahoe Ночь",
            WallpaperTheme::SequoiaMidnight => "Sequoia Midnight",
            WallpaperTheme::FeatherCyan => "Пёрышко Неон",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            WallpaperTheme::TahoeDay => WallpaperTheme::TahoeNight,
            WallpaperTheme::TahoeNight => WallpaperTheme::SequoiaMidnight,
            WallpaperTheme::SequoiaMidnight => WallpaperTheme::FeatherCyan,
            WallpaperTheme::FeatherCyan => WallpaperTheme::TahoeDay,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            WallpaperTheme::TahoeDay => "tahoe-day",
            WallpaperTheme::TahoeNight => "tahoe-night",
            WallpaperTheme::SequoiaMidnight => "sequoia",
            WallpaperTheme::FeatherCyan => "feather",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [
            WallpaperTheme::TahoeDay,
            WallpaperTheme::TahoeNight,
            WallpaperTheme::SequoiaMidnight,
            WallpaperTheme::FeatherCyan,
        ]
        .into_iter()
        .find(|t| t.id() == id)
    }

    /// Default wallpaper matching an appearance (like macOS dynamic wallpapers)
    pub fn for_appearance(a: Appearance) -> Self {
        match a {
            Appearance::Light => WallpaperTheme::TahoeDay,
            Appearance::Dark => WallpaperTheme::TahoeNight,
        }
    }
}

pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a)
}

pub fn with_alpha(c: Color, a: f32) -> Color {
    let mut c = c;
    c.set_alpha((c.alpha() * a).clamp(0.0, 1.0));
    c
}

pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    if r == 0.0 {
        let rect = Rect::from_xywh(x, y, w, h)?;
        return Some(PathBuilder::from_rect(rect));
    }
    let mut pb = PathBuilder::new();
    let k = 0.552_284_8 * r;

    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Apple's "continuous corner" icon shape, approximated by a superellipse (n = 5)
pub fn squircle_path(x: f32, y: f32, size: f32) -> Option<tiny_skia::Path> {
    let half = size * 0.5;
    let (cx, cy) = (x + half, y + half);
    let mut pb = PathBuilder::new();
    let steps = 96;
    for i in 0..steps {
        let t = i as f32 / steps as f32 * std::f32::consts::TAU;
        let (s, c) = t.sin_cos();
        let px = cx + half * c.signum() * c.abs().powf(2.0 / 5.0);
        let py = cy + half * s.signum() * s.abs().powf(2.0 / 5.0);
        if i == 0 {
            pb.move_to(px, py);
        } else {
            pb.line_to(px, py);
        }
    }
    pb.close();
    pb.finish()
}

fn fill_path(pixmap: &mut PixmapMut, path: &tiny_skia::Path, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pixmap.fill_path(path, &paint, FillRule::Winding, scale_transform(), None);
}

pub fn fill_squircle_gradient(
    pixmap: &mut PixmapMut,
    x: f32,
    y: f32,
    size: f32,
    top: Color,
    bottom: Color,
) {
    if let Some(path) = squircle_path(x, y, size) {
        let mut paint = Paint {
            anti_alias: true,
            ..Default::default()
        };
        if let Some(shader) = LinearGradient::new(
            Point::from_xy(x, y),
            Point::from_xy(x, y + size),
            vec![
                tiny_skia::GradientStop::new(0.0, top),
                tiny_skia::GradientStop::new(1.0, bottom),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            paint.shader = shader;
        } else {
            paint.set_color(top);
        }
        pixmap.fill_path(&path, &paint, FillRule::Winding, scale_transform(), None);
    }
}

pub fn stroke_squircle(pixmap: &mut PixmapMut, x: f32, y: f32, size: f32, width: f32, color: Color) {
    if let Some(path) = squircle_path(x, y, size) {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke {
            width,
            ..Default::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, scale_transform(), None);
    }
}

pub fn fill_rounded_rect(
    pixmap: &mut PixmapMut,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    color: Color,
) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        fill_path(pixmap, &path, color);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn stroke_rounded_rect(
    pixmap: &mut PixmapMut,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    width: f32,
    color: Color,
) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke {
            width,
            ..Default::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, scale_transform(), None);
    }
}

pub fn fill_circle(pixmap: &mut PixmapMut, cx: f32, cy: f32, radius: f32, color: Color) {
    if let Some(path) = PathBuilder::from_circle(cx, cy, radius) {
        fill_path(pixmap, &path, color);
    }
}

pub fn stroke_circle(pixmap: &mut PixmapMut, cx: f32, cy: f32, radius: f32, width: f32, color: Color) {
    if let Some(path) = PathBuilder::from_circle(cx, cy, radius) {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke {
            width,
            ..Default::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, scale_transform(), None);
    }
}

pub fn stroke_line(
    pixmap: &mut PixmapMut,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    width: f32,
    color: Color,
) {
    let mut pb = PathBuilder::new();
    pb.move_to(x1, y1);
    pb.line_to(x2, y2);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke {
            width,
            line_cap: tiny_skia::LineCap::Round,
            ..Default::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, scale_transform(), None);
    }
}

thread_local! {
    /// Blurred wallpaper (low-res) and logical points per pixel, for frosted glass
    static GLASS_BG: std::cell::RefCell<Option<(std::rc::Rc<Pixmap>, f32, f32)>> =
        const { std::cell::RefCell::new(None) };
    /// Logical screen position of the buffer currently being drawn
    static GLASS_ORIGIN: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((0.0, 0.0)) };
}

pub fn set_glass_background(bg: Option<(std::rc::Rc<Pixmap>, f32, f32)>) {
    GLASS_BG.with(|g| *g.borrow_mut() = bg);
}

/// Where (in logical screen coordinates) the buffer being drawn sits on screen
pub fn set_glass_origin(x: f32, y: f32) {
    GLASS_ORIGIN.with(|o| o.set((x, y)));
}

/// Frosted glass: the blurred wallpaper that is "behind" this rounded rect. Like Hyprland's
/// xray blur it shows the desktop, not windows, and costs nothing per frame.
pub fn fill_frosted(pixmap: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let Some(path) = rounded_rect_path(x, y, w, h, r) else { return };
    let (ox, oy) = GLASS_ORIGIN.with(|o| o.get());
    GLASS_BG.with(|g| {
        if let Some((bg, sx, sy)) = g.borrow().as_ref() {
            let shader = tiny_skia::Pattern::new(
                bg.as_ref().as_ref(),
                SpreadMode::Pad,
                tiny_skia::FilterQuality::Bilinear,
                1.0,
                // pattern px → logical screen → logical buffer coordinates
                Transform::from_row(*sx, 0.0, 0.0, *sy, -ox, -oy),
            );
            let paint = Paint {
                shader,
                anti_alias: true,
                ..Default::default()
            };
            pixmap.fill_path(&path, &paint, FillRule::Winding, scale_transform(), None);
        }
    });
}

/// Glass panel used by menus, Spotlight, Control Center and notifications: frosted desktop,
/// translucent fill, bright inner edge (Liquid Glass highlight) and a hairline outline.
pub fn draw_glass_panel(pixmap: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, r: f32, strong: bool) {
    fill_frosted(pixmap, x, y, w, h, r);
    let p = palette();
    fill_rounded_rect(
        pixmap,
        x,
        y,
        w,
        h,
        r,
        if strong { p.glass_fill_strong } else { p.glass_fill },
    );
    stroke_rounded_rect(pixmap, x + 0.75, y + 0.75, w - 1.5, h - 1.5, r - 0.75, 1.0, p.glass_edge);
    stroke_rounded_rect(pixmap, x - 0.25, y - 0.25, w + 0.5, h + 0.5, r + 0.25, 0.5, p.glass_outline);
}

/// Separable box blur (3 passes ≈ gaussian) of a single-channel float image
fn box_blur(data: &mut [f32], w: usize, h: usize, radius: usize) {
    if radius == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; data.len()];
    // Average over the samples actually inside the image, so edges don't darken
    for _ in 0..3 {
        for y in 0..h {
            let row = &data[y * w..(y + 1) * w];
            let mut acc = 0.0;
            for i in 0..=radius.min(w - 1) {
                acc += row[i];
            }
            for x in 0..w {
                let n = ((x + radius).min(w - 1) - x.saturating_sub(radius) + 1) as f32;
                tmp[y * w + x] = acc / n;
                if x + radius + 1 < w {
                    acc += row[x + radius + 1];
                }
                if x >= radius {
                    acc -= row[x - radius];
                }
            }
        }
        for x in 0..w {
            let mut acc = 0.0;
            for i in 0..=radius.min(h - 1) {
                acc += tmp[i * w + x];
            }
            for y in 0..h {
                let n = ((y + radius).min(h - 1) - y.saturating_sub(radius) + 1) as f32;
                data[y * w + x] = acc / n;
                if y + radius + 1 < h {
                    acc += tmp[(y + radius + 1) * w + x];
                }
                if y >= radius {
                    acc -= tmp[(y - radius) * w + x];
                }
            }
        }
    }
}

/// Rasterizes the macOS window shadow for a square "core" window inside an `np`×`np` tile:
/// a wide, soft, downward-offset gaussian plus a tight contact shadow. The window interior is
/// cut out so translucent/rounded client corners never show a dark halo.
pub fn render_shadow_tile(np: u32, focused: bool) -> Option<Pixmap> {
    let s = ui_scale();
    let n = np as usize;
    let pad = SHADOW_PAD as f32;
    let core = np as f32 / s - pad * 2.0;
    let strength = palette().shadow_strength;

    let coverage = |dy: f32| -> Option<Vec<f32>> {
        let mut pm = Pixmap::new(np, np)?;
        {
            let mut m = pm.as_mut();
            fill_rounded_rect(&mut m, pad, pad + dy, core, core, WINDOW_CORNER_RADIUS, Color::BLACK);
        }
        Some(pm.data().chunks_exact(4).map(|px| px[3] as f32 / 255.0).collect())
    };

    let (big_dy, big_sigma, big_alpha) = if focused { (16.0, 22.0, 0.30) } else { (8.0, 12.0, 0.17) };
    let mut wide = coverage(big_dy)?;
    box_blur(&mut wide, n, n, (big_sigma * s * 0.85) as usize);
    let mut contact = coverage(1.0)?;
    box_blur(&mut contact, n, n, (1.5 * s).max(1.0) as usize);
    let inside = coverage(0.0)?;

    let mut pixmap = Pixmap::new(np, np)?;
    for (i, px) in pixmap.data_mut().chunks_exact_mut(4).enumerate() {
        let a = (wide[i] * big_alpha + contact[i] * 0.16) * strength * (1.0 - inside[i]);
        px[0] = 0;
        px[1] = 0;
        px[2] = 0;
        px[3] = (a.clamp(0.0, 1.0) * 255.0) as u8;
    }
    Some(pixmap)
}

/// Renders the procedural wallpaper into an RGBA8888 buffer
pub fn render_wallpaper(buf: &mut [u8], width: i32, height: i32, theme: WallpaperTheme) {
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;

    let (c_top, c_mid, c_bot, glow1, glow2, glow3) = match theme {
        // Soft pastel sky over a lake, in the spirit of macOS Tahoe's light wallpaper
        WallpaperTheme::TahoeDay => (
            [178.0f32, 208.0, 240.0],
            [214.0, 205.0, 238.0],
            [246.0, 214.0, 200.0],
            (0.22, 0.30, [40.0, 70.0, 15.0], 0.55),
            (0.78, 0.36, [10.0, 40.0, 25.0], 0.55),
            (0.50, 0.86, [5.0, -20.0, -40.0], 0.50),
        ),
        WallpaperTheme::TahoeNight => (
            [14.0f32, 22.0, 52.0],
            [40.0, 34.0, 88.0],
            [26.0, 52.0, 96.0],
            (0.24, 0.32, [150.0, 60.0, 140.0], 0.55),
            (0.76, 0.30, [40.0, 120.0, 230.0], 0.58),
            (0.52, 0.80, [220.0, 120.0, 80.0], 0.42),
        ),
        WallpaperTheme::SequoiaMidnight => (
            [8.0f32, 11.0, 26.0],
            [16.0, 28.0, 58.0],
            [10.0, 18.0, 36.0],
            (0.30, 0.40, [56.0, 110.0, 220.0], 0.50),
            (0.72, 0.55, [110.0, 55.0, 195.0], 0.45),
            (0.45, 0.85, [30.0, 165.0, 180.0], 0.35),
        ),
        WallpaperTheme::FeatherCyan => (
            [9.0f32, 22.0, 38.0],
            [15.0, 52.0, 78.0],
            [12.0, 28.0, 52.0],
            (0.22, 0.32, [0.0, 210.0, 235.0], 0.52),
            (0.78, 0.42, [130.0, 80.0, 250.0], 0.50),
            (0.50, 0.75, [40.0, 225.0, 155.0], 0.42),
        ),
    };

    for y in 0..height {
        let ny = y as f32 / h;
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| {
            [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
        };
        let base = if ny < 0.5 {
            lerp(c_top, c_mid, ny * 2.0)
        } else {
            lerp(c_mid, c_bot, (ny - 0.5) * 2.0)
        };

        let row_offset = (y * width * 4) as usize;
        for x in 0..width {
            let nx = x as f32 / w;
            let wave = ((nx * 4.2 + ny * 2.1).sin() * 0.08) + ((nx * 2.3 - ny * 3.4).cos() * 0.06);

            let d1 = ((nx - glow1.0).powi(2) + (ny + wave - glow1.1).powi(2) * 1.4).sqrt();
            let w1 = (1.0 - (d1 / glow1.3).min(1.0)).powi(2) * 0.58;
            let d2 = ((nx - glow2.0).powi(2) + (ny - wave - glow2.1).powi(2) * 1.3).sqrt();
            let w2 = (1.0 - (d2 / glow2.3).min(1.0)).powi(2) * 0.55;
            let d3 = ((nx - glow3.0).powi(2) + (ny + wave * 0.5 - glow3.1).powi(2) * 1.6).sqrt();
            let w3 = (1.0 - (d3 / glow3.3).min(1.0)).powi(2) * 0.48;

            let ch = |i: usize| {
                (base[i] + glow1.2[i] * w1 + glow2.2[i] * w2 + glow3.2[i] * w3).clamp(0.0, 255.0) as u8
            };
            let idx = row_offset + (x as usize) * 4;
            buf[idx] = ch(0);
            buf[idx + 1] = ch(1);
            buf[idx + 2] = ch(2);
            buf[idx + 3] = 255;
        }
    }
}

/// The Feather ("Пёрышко") mark used in place of the Apple logo
pub fn draw_feather_logo(pixmap: &mut PixmapMut, cx: f32, cy: f32, scale: f32, color: Color) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - 5.5 * scale, cy + 6.0 * scale);
    pb.cubic_to(
        cx - 4.0 * scale,
        cy - 2.0 * scale,
        cx + 1.5 * scale,
        cy - 6.5 * scale,
        cx + 6.5 * scale,
        cy - 6.5 * scale,
    );
    pb.cubic_to(
        cx + 6.0 * scale,
        cy - 0.5 * scale,
        cx + 2.0 * scale,
        cy + 4.0 * scale,
        cx - 3.5 * scale,
        cy + 4.5 * scale,
    );
    pb.close();

    if let Some(path) = pb.finish() {
        fill_path(pixmap, &path, color);
    }
    stroke_line(
        pixmap,
        cx - 6.0 * scale,
        cy + 6.5 * scale,
        cx + 4.0 * scale,
        cy - 3.5 * scale,
        1.6 * scale,
        color,
    );
}

/// Spotlight magnifying glass
pub fn draw_search_icon(pixmap: &mut PixmapMut, cx: f32, cy: f32, r: f32, color: Color) {
    stroke_circle(pixmap, cx - r * 0.25, cy - r * 0.25, r, (r * 0.28).max(1.3), color);
    stroke_line(
        pixmap,
        cx + r * 0.45,
        cy + r * 0.45,
        cx + r * 1.15,
        cy + r * 1.15,
        (r * 0.3).max(1.4),
        color,
    );
}

/// Control Center "two toggles" glyph
pub fn draw_control_center_icon(pixmap: &mut PixmapMut, cx: f32, cy: f32, color: Color) {
    stroke_rounded_rect(pixmap, cx - 7.0, cy - 5.5, 14.0, 4.8, 2.4, 1.2, color);
    fill_circle(pixmap, cx + 4.6, cy - 3.1, 1.9, color);
    stroke_rounded_rect(pixmap, cx - 7.0, cy + 0.7, 14.0, 4.8, 2.4, 1.2, color);
    fill_circle(pixmap, cx - 4.6, cy + 3.1, 1.9, color);
}

/// Wi-Fi fan glyph (three arcs and a dot)
pub fn draw_wifi_icon(pixmap: &mut PixmapMut, cx: f32, cy: f32, color: Color, enabled: bool) {
    let color = if enabled { color } else { with_alpha(color, 0.35) };
    for (i, r) in [3.0f32, 6.2, 9.4].iter().enumerate() {
        let mut pb = PathBuilder::new();
        let a0 = -std::f32::consts::FRAC_PI_4 * 3.0;
        let a1 = -std::f32::consts::FRAC_PI_4;
        let steps = 12;
        for k in 0..=steps {
            let t = a0 + (a1 - a0) * k as f32 / steps as f32;
            let (px, py) = (cx + r * t.cos(), cy + 4.0 + r * t.sin());
            if k == 0 {
                pb.move_to(px, py);
            } else {
                pb.line_to(px, py);
            }
        }
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(color);
            paint.anti_alias = true;
            let stroke = Stroke {
                width: 1.6,
                line_cap: tiny_skia::LineCap::Round,
                ..Default::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, scale_transform(), None);
        }
        let _ = i;
    }
    fill_circle(pixmap, cx, cy + 3.6, 1.3, color);
}

/// Blurred, darkened wallpaper for full-screen surfaces like Launchpad (macOS hides windows
/// behind a frosted copy of the desktop). Rendered at very low resolution, which is both
/// cheap and part of the blur: the GPU upscales it with linear filtering.
pub fn render_frosted_wallpaper(width: i32, height: i32, theme: WallpaperTheme, darken: f32) -> Vec<u8> {
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    let mut rgba = vec![0u8; w * h * 4];
    render_wallpaper(&mut rgba, w as i32, h as i32, theme);
    let radius = (w / 40).max(2);
    let mut out = rgba.clone();
    for c in 0..3 {
        let mut ch: Vec<f32> = rgba.chunks_exact(4).map(|p| p[c] as f32).collect();
        box_blur(&mut ch, w, h, radius);
        for (i, v) in ch.iter().enumerate() {
            out[i * 4 + c] = (v * darken).clamp(0.0, 255.0) as u8;
        }
    }
    out
}
