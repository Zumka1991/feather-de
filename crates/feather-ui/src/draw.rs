//! Drawing primitives in logical points; the current `ui_scale` maps them to pixels.

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, PixmapMut, Point, Rect,
    SpreadMode, Stroke, Transform,
};

use crate::scale::ui_scale;

pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a)
}

fn transform() -> Transform {
    let s = ui_scale();
    Transform::from_scale(s, s)
}

fn paint(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    p
}

pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    if r == 0.0 {
        return Some(PathBuilder::from_rect(Rect::from_xywh(x, y, w, h)?));
    }
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
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

pub fn fill_path(pm: &mut PixmapMut, path: &tiny_skia::Path, color: Color) {
    pm.fill_path(path, &paint(color), FillRule::Winding, transform(), None);
}

pub fn fill_rect(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, color: Color) {
    if let Some(r) = Rect::from_xywh(x, y, w, h) {
        pm.fill_rect(r, &paint(color), transform(), None);
    }
}

pub fn fill_rrect(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        fill_path(pm, &path, color);
    }
}

pub fn stroke_rrect(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, color: Color) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        let stroke = Stroke { width, ..Default::default() };
        pm.stroke_path(&path, &paint(color), &stroke, transform(), None);
    }
}

pub fn fill_rrect_gradient(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, r: f32, top: Color, bottom: Color) {
    let Some(path) = rounded_rect_path(x, y, w, h, r) else { return };
    let mut p = paint(top);
    if let Some(shader) = LinearGradient::new(
        Point::from_xy(0.0, y),
        Point::from_xy(0.0, y + h),
        vec![GradientStop::new(0.0, top), GradientStop::new(1.0, bottom)],
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        p.shader = shader;
    }
    pm.fill_path(&path, &p, FillRule::Winding, transform(), None);
}

pub fn fill_circle(pm: &mut PixmapMut, cx: f32, cy: f32, r: f32, color: Color) {
    if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
        fill_path(pm, &path, color);
    }
}

pub fn line(pm: &mut PixmapMut, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color) {
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    if let Some(path) = pb.finish() {
        let stroke = Stroke { width, line_cap: tiny_skia::LineCap::Round, line_join: tiny_skia::LineJoin::Round, ..Default::default() };
        pm.stroke_path(&path, &paint(color), &stroke, transform(), None);
    }
}

pub fn polyline(pm: &mut PixmapMut, points: &[(f32, f32)], width: f32, color: Color) {
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in points.iter().enumerate() {
        if i == 0 { pb.move_to(*x, *y) } else { pb.line_to(*x, *y) }
    }
    if let Some(path) = pb.finish() {
        let stroke = Stroke { width, line_cap: tiny_skia::LineCap::Round, line_join: tiny_skia::LineJoin::Round, ..Default::default() };
        pm.stroke_path(&path, &paint(color), &stroke, transform(), None);
    }
}

pub fn stroke_circle(pm: &mut PixmapMut, cx: f32, cy: f32, r: f32, width: f32, color: Color) {
    if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
        let stroke = Stroke { width, ..Default::default() };
        pm.stroke_path(&path, &paint(color), &stroke, transform(), None);
    }
}
/// Copies RGBA pixels into a SHM slot, leaving the allocator's alignment padding alone.
/// SCTK rounds slots up to 64 bytes, including when stride * height is smaller.
pub fn copy_shm_pixels(slot: &mut [u8], pixels: &[u8], rgba: bool) {
    let dst = &mut slot[..pixels.len()];
    dst.copy_from_slice(pixels);
    if !rgba {
        for p in dst.chunks_exact_mut(4) { p.swap(0, 2); }
    }
}

#[cfg(test)]
mod shm_tests {
    use super::*;

    #[test]
    fn resized_frames_fit_aligned_shm_slots_in_both_formats() {
        // Includes the exact dimensions which crashed the terminal at scale 2,
        // fractional-scale odd dimensions, a tiny buffer, and an aligned one.
        for (w,h) in [(1466,438),(733,219),(1,1),(16,16)] {
            let mut frame = tiny_skia::Pixmap::new(w,h).unwrap();
            frame.fill(rgba(10,20,30,255));
            let len = frame.data().len();
            for native_rgba in [true,false] {
                let mut slot = vec![0xa5; (len+63)&!63];
                copy_shm_pixels(&mut slot,frame.data(),native_rgba);
                let expected = if native_rgba {[10,20,30,255]} else {[30,20,10,255]};
                assert!(slot[..len].chunks_exact(4).all(|p|p==expected));
                assert!(slot[len..].iter().all(|&p|p==0xa5),"copied into SHM padding");
            }
        }
    }
}

