//! Procedural Tahoe-style icons: light-blue folders (with an engraved symbol for user
//! folders), paper documents with a coloured type badge, and line glyphs for the sidebar.
//! Rasterized once per (icon, pixel size) and then only copied.
//! Badge letters use the shared UI font, drawn at the icon's own scale.

use std::collections::HashMap;

use tiny_skia::{Color, FilterQuality, Pixmap, PixmapMut, PixmapPaint, Transform};

use crate::draw::{self as theme, rgba};
use crate::file_kind::Kind;
use crate::scale;
use crate::text::FontSystem;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Glyph {
    Home,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    Trash,
    Drive,
    Computer,
    Folder,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    Folder(Option<Glyph>),
    File(Kind, String),
}

#[derive(Default)]
pub struct IconCache {
    map: HashMap<(Icon, u32), Pixmap>,
}

impl IconCache {
    /// Draws `icon` with its top-left at logical (x, y), `size` logical points square
    pub fn draw(&mut self, pm: &mut PixmapMut, fonts: &mut FontSystem, icon: &Icon, x: f32, y: f32, size: f32) {
        let s = scale::ui_scale();
        let px = (size * s).round().max(1.0) as u32;
        if self.map.len() > 256 {
            self.map.clear();
        }
        let key = (icon.clone(), px);
        if !self.map.contains_key(&key) {
            if let Some(p) = render(fonts, icon, px) {
                self.map.insert(key.clone(), p);
            }
        }
        if let Some(p) = self.map.get(&key) {
            pm.draw_pixmap((x * s).round() as i32, (y * s).round() as i32, p.as_ref(), &PixmapPaint { quality: FilterQuality::Nearest, ..Default::default() }, Transform::identity(), None);
        }
    }
}

/// Icons are drawn in a 100×100 design space scaled to `px`
fn render(fonts: &mut FontSystem, icon: &Icon, px: u32) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(px, px)?;
    scale::with_render_settings(px as f32 / 100.0, scale::appearance(), || {
        let mut pm = pixmap.as_mut();
        match icon {
            Icon::Folder(glyph) => folder(&mut pm, *glyph),
            Icon::File(kind, ext) => document(&mut pm, fonts, *kind, ext),
        }
    });
    Some(pixmap)
}

fn folder(pm: &mut PixmapMut, glyph: Option<Glyph>) {
    // Back plate with the tab
    theme::fill_rrect(pm, 6.0, 16.0, 36.0, 16.0, 5.0, rgba(76, 155, 228, 255));
    theme::fill_rrect(pm, 6.0, 22.0, 88.0, 62.0, 7.0, rgba(76, 155, 228, 255));
    // Front plate, lighter, with a soft top highlight
    theme::fill_rrect_gradient(pm, 6.0, 30.0, 88.0, 56.0, 7.0, rgba(132, 199, 252, 255), rgba(98, 178, 245, 255));
    theme::line(pm, 9.0, 31.0, 91.0, 31.0, 1.2, rgba(255, 255, 255, 120));
    if let Some(glyph) = glyph {
        draw_glyph(pm, glyph, 36.0, 44.0, 28.0, rgba(48, 120, 196, 210));
    }
}

fn badge(kind: Kind) -> Option<Color> {
    Some(match kind {
        Kind::Pdf => rgba(229, 57, 53, 255),
        Kind::Image => rgba(52, 168, 83, 255),
        Kind::Audio => rgba(236, 64, 122, 255),
        Kind::Video => rgba(142, 68, 173, 255),
        Kind::Archive => rgba(120, 120, 128, 255),
        Kind::Code => rgba(10, 132, 255, 255),
        Kind::Executable => rgba(60, 60, 66, 255),
        Kind::Text | Kind::Document => return None,
        Kind::Folder => return None,
    })
}

fn document(pm: &mut PixmapMut, fonts: &mut FontSystem, kind: Kind, ext: &str) {
    let (x, y, w, h, fold) = (18.0, 6.0, 64.0, 88.0, 18.0);
    // Soft shadow, page, folded corner
    theme::fill_rrect(pm, x, y + 1.5, w, h, 4.0, rgba(0, 0, 0, 28));
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x + 4.0, y);
    pb.line_to(x + w - fold, y);
    pb.line_to(x + w, y + fold);
    pb.line_to(x + w, y + h - 4.0);
    pb.quad_to(x + w, y + h, x + w - 4.0, y + h);
    pb.line_to(x + 4.0, y + h);
    pb.quad_to(x, y + h, x, y + h - 4.0);
    pb.line_to(x, y + 4.0);
    pb.quad_to(x, y, x + 4.0, y);
    pb.close();
    if let Some(page) = pb.finish() {
        theme::fill_path(pm, &page, rgba(255, 255, 255, 255));
        let stroke = tiny_skia::Stroke { width: 1.2, ..Default::default() };
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(rgba(0, 0, 0, 45));
        paint.anti_alias = true;
        let s = scale::ui_scale();
        pm.stroke_path(&page, &paint, &stroke, Transform::from_scale(s, s), None);
    }
    let mut corner = tiny_skia::PathBuilder::new();
    corner.move_to(x + w - fold, y);
    corner.line_to(x + w - fold, y + fold - 3.0);
    corner.quad_to(x + w - fold, y + fold, x + w - fold + 3.0, y + fold);
    corner.line_to(x + w, y + fold);
    corner.close();
    if let Some(corner) = corner.finish() {
        theme::fill_path(pm, &corner, rgba(222, 224, 230, 255));
    }
    match kind {
        Kind::Text | Kind::Code | Kind::Document => {
            let ink = if kind == Kind::Code { rgba(10, 132, 255, 150) } else { rgba(0, 0, 0, 40) };
            for i in 0..6 {
                let ly = y + 32.0 + i as f32 * 8.0;
                let lw = if i % 3 == 2 { 26.0 } else { 40.0 };
                theme::line(pm, x + 12.0, ly, x + 12.0 + lw, ly, 2.4, ink);
            }
        }
        Kind::Image => {
            theme::fill_rrect(pm, x + 10.0, y + 26.0, w - 20.0, 34.0, 3.0, rgba(140, 200, 250, 255));
            theme::fill_circle(pm, x + 38.0, y + 35.0, 5.0, rgba(255, 214, 10, 255));
            theme::polyline(pm, &[(x + 12.0, y + 58.0), (x + 26.0, y + 42.0), (x + 36.0, y + 52.0), (x + 44.0, y + 46.0), (x + 52.0, y + 58.0)], 3.0, rgba(52, 168, 83, 255));
        }
        Kind::Audio => draw_glyph(pm, Glyph::Music, x + 18.0, y + 28.0, 28.0, rgba(236, 64, 122, 255)),
        Kind::Video => draw_glyph(pm, Glyph::Videos, x + 18.0, y + 28.0, 28.0, rgba(142, 68, 173, 255)),
        Kind::Archive => {
            for i in 0..5 {
                let zy = y + 10.0 + i as f32 * 9.0;
                theme::fill_rect(pm, x + 28.0, zy, 8.0, 4.0, rgba(120, 120, 128, 255));
                theme::fill_rect(pm, x + 36.0, zy + 4.5, 8.0, 4.0, rgba(120, 120, 128, 255));
            }
        }
        Kind::Executable => {
            theme::stroke_rrect(pm, x + 14.0, y + 28.0, 36.0, 26.0, 3.0, 2.0, rgba(60, 60, 66, 255));
            theme::polyline(pm, &[(x + 20.0, y + 36.0), (x + 26.0, y + 41.0), (x + 20.0, y + 46.0)], 2.4, rgba(60, 60, 66, 255));
        }
        Kind::Pdf | Kind::Folder => {}
    }
    // Extension badge, as Finder labels document types
    if let Some(color) = badge(kind).or_else(|| (!ext.is_empty()).then(|| rgba(142, 142, 147, 255))) {
        let label = ext.to_uppercase();
        let label: String = label.chars().take(4).collect();
        if !label.is_empty() {
            let bw = fonts.measure_text(&label, 11.0, true) + 12.0;
            theme::fill_rrect(pm, x + (w - bw) * 0.5, y + h - 24.0, bw, 16.0, 4.0, color);
            fonts.draw_text_centered(pm, &label, x + w * 0.5, y + h - 22.5, 11.0, rgba(255, 255, 255, 255), true);
        }
    }
}

/// Line-art symbol in a `size` box at (x, y): sidebar entries and folder engravings
pub fn draw_glyph(pm: &mut PixmapMut, glyph: Glyph, x: f32, y: f32, size: f32, color: Color) {
    let w = (size * 0.085).max(1.0);
    let p = |fx: f32, fy: f32| (x + fx * size, y + fy * size);
    match glyph {
        Glyph::Home => {
            theme::polyline(pm, &[p(0.08, 0.5), p(0.5, 0.1), p(0.92, 0.5)], w, color);
            theme::polyline(pm, &[p(0.2, 0.42), p(0.2, 0.9), p(0.8, 0.9), p(0.8, 0.42)], w, color);
            theme::polyline(pm, &[p(0.4, 0.9), p(0.4, 0.62), p(0.6, 0.62), p(0.6, 0.9)], w, color);
        }
        Glyph::Desktop | Glyph::Computer => {
            theme::stroke_rrect(pm, x + size * 0.06, y + size * 0.14, size * 0.88, size * 0.56, size * 0.07, w, color);
            theme::line(pm, p(0.5, 0.7).0, p(0.5, 0.7).1, p(0.5, 0.86).0, p(0.5, 0.86).1, w, color);
            theme::line(pm, p(0.3, 0.88).0, p(0.3, 0.88).1, p(0.7, 0.88).0, p(0.7, 0.88).1, w, color);
        }
        Glyph::Documents => {
            theme::polyline(pm, &[p(0.62, 0.06), p(0.2, 0.06), p(0.2, 0.94), p(0.8, 0.94), p(0.8, 0.25), p(0.62, 0.06), p(0.62, 0.25), p(0.8, 0.25)], w, color);
            for fy in [0.45, 0.6, 0.75] {
                theme::line(pm, p(0.33, fy).0, p(0.33, fy).1, p(0.67, fy).0, p(0.67, fy).1, w * 0.8, color);
            }
        }
        Glyph::Downloads => {
            theme::stroke_circle(pm, x + size * 0.5, y + size * 0.5, size * 0.42, w, color);
            theme::line(pm, p(0.5, 0.26).0, p(0.5, 0.26).1, p(0.5, 0.7).0, p(0.5, 0.7).1, w, color);
            theme::polyline(pm, &[p(0.32, 0.53), p(0.5, 0.71), p(0.68, 0.53)], w, color);
        }
        Glyph::Pictures => {
            theme::stroke_rrect(pm, x + size * 0.06, y + size * 0.16, size * 0.88, size * 0.68, size * 0.08, w, color);
            theme::fill_circle(pm, x + size * 0.68, y + size * 0.36, size * 0.07, color);
            theme::polyline(pm, &[p(0.12, 0.78), p(0.38, 0.48), p(0.6, 0.72), p(0.72, 0.6), p(0.9, 0.78)], w, color);
        }
        Glyph::Music => {
            theme::polyline(pm, &[p(0.38, 0.78), p(0.38, 0.14), p(0.86, 0.06), p(0.86, 0.68)], w, color);
            theme::fill_circle(pm, x + size * 0.27, y + size * 0.8, size * 0.13, color);
            theme::fill_circle(pm, x + size * 0.75, y + size * 0.7, size * 0.13, color);
        }
        Glyph::Videos => {
            theme::stroke_rrect(pm, x + size * 0.06, y + size * 0.2, size * 0.88, size * 0.6, size * 0.08, w, color);
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(p(0.4, 0.36).0, p(0.4, 0.36).1);
            pb.line_to(p(0.66, 0.5).0, p(0.66, 0.5).1);
            pb.line_to(p(0.4, 0.64).0, p(0.4, 0.64).1);
            pb.close();
            if let Some(tri) = pb.finish() {
                theme::fill_path(pm, &tri, color);
            }
        }
        Glyph::Trash => {
            theme::line(pm, p(0.12, 0.22).0, p(0.12, 0.22).1, p(0.88, 0.22).0, p(0.88, 0.22).1, w, color);
            theme::polyline(pm, &[p(0.38, 0.22), p(0.38, 0.08), p(0.62, 0.08), p(0.62, 0.22)], w, color);
            theme::polyline(pm, &[p(0.2, 0.22), p(0.26, 0.92), p(0.74, 0.92), p(0.8, 0.22)], w, color);
            for fx in [0.4, 0.6] {
                theme::line(pm, p(fx, 0.38).0, p(fx, 0.38).1, p(fx, 0.78).0, p(fx, 0.78).1, w * 0.8, color);
            }
        }
        Glyph::Drive => {
            theme::stroke_rrect(pm, x + size * 0.06, y + size * 0.3, size * 0.88, size * 0.42, size * 0.1, w, color);
            theme::fill_circle(pm, x + size * 0.76, y + size * 0.51, size * 0.05, color);
        }
        Glyph::Folder => {
            theme::polyline(pm, &[p(0.06, 0.84), p(0.06, 0.16), p(0.38, 0.16), p(0.48, 0.28), p(0.94, 0.28), p(0.94, 0.84), p(0.06, 0.84)], w, color);
            theme::line(pm, p(0.06, 0.4).0, p(0.06, 0.4).1, p(0.94, 0.4).0, p(0.94, 0.4).1, w * 0.8, color);
        }
    }
}
