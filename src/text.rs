use std::collections::HashMap;
use std::fs;
use std::process::Command;

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::{Color, PixmapMut};

use crate::theme::ui_scale;

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct GlyphKey {
    ch: char,
    size_tenths: u16,
    bold: bool,
}

struct Glyph {
    metrics: Metrics,
    bitmap: Vec<u8>,
    /// Which font produced the glyph (kerning only applies within the same font)
    from_fallback: bool,
}

pub struct FontSystem {
    regular: Font,
    bold: Font,
    fallback: Option<Font>,
    cache: HashMap<GlyphKey, Glyph>,
}

/// Font families in order of preference: Apple's SF Pro if the user installed it, then Inter
/// (the closest freely licensed match), then common Linux sans fonts.
const FAMILIES: &[&str] = &["SF Pro Text", "SF Pro Display", "SF Pro", "Inter", "Noto Sans", "DejaVu Sans"];

const LEGACY_REGULAR: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
];

/// Asks fontconfig for `family:style`, accepting the result only if it really is that family
fn fc_match(family: &str, style: &str) -> Option<(String, u32)> {
    let out = Command::new("fc-match")
        .arg("-f")
        .arg("%{family[0]}|%{file}|%{index}")
        .arg(format!("{family}:style={style}"))
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut parts = text.split('|');
    let got_family = parts.next()?;
    let file = parts.next()?.to_string();
    let index = parts.next()?.trim().parse::<u32>().unwrap_or(0);
    got_family
        .to_lowercase()
        .starts_with(&family.to_lowercase())
        .then_some((file, index))
}

fn load_font(path: &str, index: u32) -> Option<Font> {
    let bytes = fs::read(path).ok()?;
    Font::from_bytes(
        bytes,
        FontSettings {
            collection_index: index,
            ..FontSettings::default()
        },
    )
    .ok()
}

impl FontSystem {
    pub fn new() -> Self {
        let mut chosen: Option<(Font, Font, &str)> = None;
        for family in FAMILIES {
            let Some((reg_path, reg_idx)) = fc_match(family, "Regular") else {
                continue;
            };
            let Some(regular) = load_font(&reg_path, reg_idx) else {
                continue;
            };
            // macOS uses Semibold for app names, titles and headings
            let bold = ["Semibold", "SemiBold", "Bold"]
                .iter()
                .find_map(|st| fc_match(family, st))
                .and_then(|(p, i)| load_font(&p, i))
                .unwrap_or_else(|| regular.clone());
            chosen = Some((regular, bold, family));
            break;
        }

        let (regular, bold) = match chosen {
            Some((r, b, family)) => {
                tracing::info!("Шрифт интерфейса: {family}");
                (r, b)
            }
            None => {
                let r = LEGACY_REGULAR
                    .iter()
                    .find_map(|p| load_font(p, 0))
                    .expect("Не найден системный шрифт (Inter / Noto Sans / DejaVu Sans)");
                (r.clone(), r)
            }
        };

        // Symbols like ⌘ ⇧ ⌥ ↵ are missing from many UI fonts
        let fallback = ["Noto Sans Symbols 2", "Noto Sans Symbols", "DejaVu Sans"]
            .iter()
            .find_map(|f| fc_match(f, "Regular"))
            .and_then(|(p, i)| load_font(&p, i));

        Self {
            regular,
            bold,
            fallback,
            cache: HashMap::with_capacity(1024),
        }
    }

    fn get_glyph(&mut self, ch: char, phys_size: f32, is_bold: bool) -> &Glyph {
        let key = GlyphKey {
            ch,
            size_tenths: (phys_size * 10.0).round() as u16,
            bold: is_bold,
        };
        if !self.cache.contains_key(&key) {
            let primary = if is_bold { &self.bold } else { &self.regular };
            let (font, from_fallback) = match &self.fallback {
                Some(fb) if primary.lookup_glyph_index(ch) == 0 && fb.lookup_glyph_index(ch) != 0 => (fb, true),
                _ => (primary, false),
            };
            let (metrics, bitmap) = font.rasterize(ch, phys_size);
            self.cache.insert(
                key,
                Glyph {
                    metrics,
                    bitmap,
                    from_fallback,
                },
            );
        }
        self.cache.get(&key).unwrap()
    }

    fn kern(&self, prev: char, ch: char, phys_size: f32, is_bold: bool) -> f32 {
        let font = if is_bold { &self.bold } else { &self.regular };
        font.horizontal_kern(prev, ch, phys_size).unwrap_or(0.0)
    }

    /// Advance of `text` in physical pixels, including kerning
    fn advance_phys(&mut self, text: &str, phys_size: f32, is_bold: bool) -> f32 {
        let mut width = 0.0;
        let mut prev: Option<(char, bool)> = None;
        for ch in text.chars() {
            let glyph = self.get_glyph(ch, phys_size, is_bold);
            let adv = glyph.metrics.advance_width;
            let fb = glyph.from_fallback;
            if let Some((p, pfb)) = prev {
                if !fb && !pfb {
                    width += self.kern(p, ch, phys_size, is_bold);
                }
            }
            width += adv;
            prev = Some((ch, fb));
        }
        width
    }

    /// Measures text width in logical points (HiDPI scale-aware)
    pub fn measure_text(&mut self, text: &str, logical_size: f32, is_bold: bool) -> f32 {
        let s = ui_scale();
        self.advance_phys(text, logical_size * s, is_bold) / s
    }

    /// Draws UTF-8 text onto a physical HiDPI pixmap at logical `(x, y)` (top of the line box).
    /// Returns the logical width advanced.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        logical_x: f32,
        logical_y: f32,
        logical_size: f32,
        color: Color,
        is_bold: bool,
    ) -> f32 {
        let s = ui_scale();
        let phys_size = logical_size * s;
        let start_x = logical_x * s;
        let mut pen_x = start_x;
        let baseline_y = (logical_y * s) + phys_size * 0.82;
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;

        let ca = (color.alpha() * 255.0) as u32;
        let cr = (color.red() * 255.0) as u32;
        let cg = (color.green() * 255.0) as u32;
        let cb = (color.blue() * 255.0) as u32;

        let mut prev: Option<(char, bool)> = None;
        for ch in text.chars() {
            let from_fallback = self.get_glyph(ch, phys_size, is_bold).from_fallback;
            if let Some((p, pfb)) = prev {
                if !from_fallback && !pfb {
                    pen_x += self.kern(p, ch, phys_size, is_bold);
                }
            }
            prev = Some((ch, from_fallback));

            let glyph = self.get_glyph(ch, phys_size, is_bold);
            let metrics = glyph.metrics;
            let gx0 = (pen_x + metrics.xmin as f32).round() as i32;
            let gy0 = (baseline_y - metrics.height as f32 - metrics.ymin as f32).round() as i32;
            let gw = metrics.width as i32;
            let gh = metrics.height as i32;
            let data = pixmap.data_mut();

            for row in 0..gh {
                let py = gy0 + row;
                if py < 0 || py >= ph {
                    continue;
                }
                for col in 0..gw {
                    let px = gx0 + col;
                    if px < 0 || px >= pw {
                        continue;
                    }
                    let cov = glyph.bitmap[(row * gw + col) as usize] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let alpha = (cov * ca) / 255;
                    if alpha == 0 {
                        continue;
                    }
                    let sr = (cr * alpha) / 255;
                    let sg = (cg * alpha) / 255;
                    let sb = (cb * alpha) / 255;
                    let inv_a = 255 - alpha;

                    let idx = ((py * pw + px) * 4) as usize;
                    data[idx] = (sr + (data[idx] as u32 * inv_a) / 255).min(255) as u8;
                    data[idx + 1] = (sg + (data[idx + 1] as u32 * inv_a) / 255).min(255) as u8;
                    data[idx + 2] = (sb + (data[idx + 2] as u32 * inv_a) / 255).min(255) as u8;
                    data[idx + 3] = (alpha + (data[idx + 3] as u32 * inv_a) / 255).min(255) as u8;
                }
            }

            pen_x += metrics.advance_width;
        }

        (pen_x - start_x) / s
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_centered(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        center_x: f32,
        y: f32,
        size: f32,
        color: Color,
        is_bold: bool,
    ) {
        let w = self.measure_text(text, size, is_bold);
        self.draw_text(pixmap, text, center_x - w * 0.5, y, size, color, is_bold);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_right(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        right_x: f32,
        y: f32,
        size: f32,
        color: Color,
        is_bold: bool,
    ) -> f32 {
        let w = self.measure_text(text, size, is_bold);
        self.draw_text(pixmap, text, right_x - w, y, size, color, is_bold);
        w
    }

    /// Truncates `text` with an ellipsis so it fits `max_width` logical points
    pub fn ellipsize(&mut self, text: &str, max_width: f32, size: f32, is_bold: bool) -> String {
        if self.measure_text(text, size, is_bold) <= max_width {
            return text.to_string();
        }
        let ellipsis_w = self.measure_text("…", size, is_bold);
        let mut out = String::new();
        for ch in text.chars() {
            out.push(ch);
            if self.measure_text(&out, size, is_bold) + ellipsis_w > max_width {
                out.pop();
                break;
            }
        }
        out.push('…');
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_clipped(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        x: f32,
        y: f32,
        max_width: f32,
        size: f32,
        color: Color,
        is_bold: bool,
    ) {
        let fitted = self.ellipsize(text, max_width, size, is_bold);
        self.draw_text(pixmap, &fitted, x, y, size, color, is_bold);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_clipped_centered(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        center_x: f32,
        y: f32,
        max_width: f32,
        size: f32,
        color: Color,
        is_bold: bool,
    ) {
        let fitted = self.ellipsize(text, max_width, size, is_bold);
        self.draw_text_centered(pixmap, &fitted, center_x, y, size, color, is_bold);
    }
}
