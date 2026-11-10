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

/// What changed between two consecutive frames of a window.
///
/// Our programs repaint the whole window, but a compositor uploads to the GPU only what the
/// surface says changed. Damaging the whole buffer makes it copy every pixel of a maximized
/// 4K window (33 MB) on each caret blink; with the real rectangle it copies a few rows.
/// Blocks of `DIFF_BLOCK_W`×`DIFF_BLOCK_H` pixels are hashed, so only ~8 bytes per block are
/// kept instead of a copy of the previous frame.
#[derive(Default)]
pub struct FrameDiff {
    size: (i32, i32),
    hashes: Vec<u64>,
    scratch: Vec<u64>,
}

const DIFF_BLOCK_W: usize = 128;
const DIFF_BLOCK_H: usize = 8;

/// One row segment folded into a block hash. Every step is a bijection of the state, so a
/// change of a single word always changes the hash; four lanes keep the multiplies parallel.
#[inline]
fn diff_fold(seed: u64, bytes: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut lanes = [seed, seed ^ K, seed.rotate_left(17), seed.rotate_left(41) ^ K];
    let mut wide = bytes.chunks_exact(32);
    for chunk in &mut wide {
        for (lane, word) in lanes.iter_mut().zip(chunk.chunks_exact(8)) {
            let word = u64::from_le_bytes([word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7]]);
            *lane = (lane.rotate_left(5) ^ word).wrapping_mul(K);
        }
    }
    let mut h = lanes[0];
    for lane in &lanes[1..] {
        h = (h.rotate_left(5) ^ lane).wrapping_mul(K);
    }
    for px in wide.remainder().chunks_exact(4) {
        let word = u32::from_le_bytes([px[0], px[1], px[2], px[3]]) as u64;
        h = (h.rotate_left(5) ^ word).wrapping_mul(K);
    }
    h
}

impl FrameDiff {
    /// The frame just measured never reached the compositor: the next one is damaged in full
    pub fn reset(&mut self) {
        self.size = (0, 0);
        self.hashes.clear();
    }

    /// Bounding rectangle `(x, y, w, h)` of the pixels that differ from the previous call, for
    /// `wl_surface.damage_buffer`. The first frame and a frame of another size are damaged in
    /// full. It is never empty: an unchanged frame gives one pixel, so the commit still
    /// behaves like every other one (frame callback included) on any compositor.
    pub fn damage(&mut self, pixels: &[u8], w: i32, h: i32) -> (i32, i32, i32, i32) {
        if w <= 0 || h <= 0 || pixels.len() < w as usize * h as usize * 4 {
            self.size = (0, 0);
            self.hashes.clear();
            return (0, 0, w.max(1), h.max(1));
        }
        let (uw, uh) = (w as usize, h as usize);
        let cols = uw.div_ceil(DIFF_BLOCK_W);
        let rows = uh.div_ceil(DIFF_BLOCK_H);
        self.scratch.clear();
        self.scratch.resize(cols * rows, 0);
        let stride = uw * 4;
        for (y, line) in pixels.chunks_exact(stride).take(uh).enumerate() {
            let band = &mut self.scratch[(y / DIFF_BLOCK_H) * cols..][..cols];
            for (hash, segment) in band.iter_mut().zip(line.chunks(DIFF_BLOCK_W * 4)) {
                *hash = diff_fold(*hash, segment);
            }
        }
        let full = self.size != (w, h) || self.hashes.len() != self.scratch.len();
        let mut bounds: Option<(usize, usize, usize, usize)> = None;
        if !full {
            for (i, (new, old)) in self.scratch.iter().zip(&self.hashes).enumerate() {
                if new != old {
                    let (c, r) = (i % cols, i / cols);
                    bounds = Some(match bounds {
                        None => (c, r, c, r),
                        Some((c0, r0, c1, r1)) => (c0.min(c), r0.min(r), c1.max(c), r1.max(r)),
                    });
                }
            }
        }
        std::mem::swap(&mut self.hashes, &mut self.scratch);
        self.size = (w, h);
        if full {
            return (0, 0, w, h);
        }
        match bounds {
            None => (0, 0, 1, 1),
            Some((c0, r0, c1, r1)) => {
                let (x0, y0) = (c0 * DIFF_BLOCK_W, r0 * DIFF_BLOCK_H);
                let x1 = ((c1 + 1) * DIFF_BLOCK_W).min(uw);
                let y1 = ((r1 + 1) * DIFF_BLOCK_H).min(uh);
                (x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32)
            }
        }
    }
}

#[cfg(test)]
mod diff_tests {
    use super::FrameDiff;

    fn frame(w: usize, h: usize) -> Vec<u8> {
        (0..w * h * 4).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn damage_is_full_first_then_covers_exactly_what_changed() {
        let (w, h) = (1001usize, 77usize);
        let mut diff = FrameDiff::default();
        let mut px = frame(w, h);
        assert_eq!(diff.damage(&px, w as i32, h as i32), (0, 0, w as i32, h as i32), "first frame");
        assert_eq!(diff.damage(&px, w as i32, h as i32), (0, 0, 1, 1), "nothing changed");
        // Every single pixel on its own: the rectangle must contain it
        for (x, y) in [(0, 0), (127, 7), (128, 8), (500, 40), (1000, 76), (999, 0), (0, 76)] {
            px[(y * w + x) * 4 + 1] ^= 0x40;
            let (dx, dy, dw, dh) = diff.damage(&px, w as i32, h as i32);
            assert!(dx <= x as i32 && (x as i32) < dx + dw && dy <= y as i32 && (y as i32) < dy + dh, "({x},{y}) outside {:?}", (dx, dy, dw, dh));
            assert!(dx >= 0 && dy >= 0 && dx + dw <= w as i32 && dy + dh <= h as i32, "inside the buffer");
            assert!(dw <= 128 && dh <= 8, "one block for one pixel");
        }
        // Two far corners: the bounding rectangle spans both
        px[0] ^= 1;
        let last = px.len() - 1;
        px[last] ^= 1;
        assert_eq!(diff.damage(&px, w as i32, h as i32), (0, 0, w as i32, h as i32));
        // Swapped pixels (same sum, same xor) are still a change
        px.swap(4 * 300, 4 * 301);
        px.swap(4 * 300 + 1, 4 * 301 + 1);
        assert_ne!(diff.damage(&px, w as i32, h as i32), (0, 0, 1, 1));
    }

    #[test]
    fn a_resized_or_broken_frame_is_damaged_in_full() {
        let mut diff = FrameDiff::default();
        let a = frame(300, 20);
        diff.damage(&a, 300, 20);
        // Same byte length, other shape
        assert_eq!(diff.damage(&a, 200, 30), (0, 0, 200, 30));
        assert_eq!(diff.damage(&a, 200, 30), (0, 0, 1, 1));
        // Too short a slice never panics and asks for everything
        assert_eq!(diff.damage(&a[..16], 200, 30), (0, 0, 200, 30));
        assert_eq!(diff.damage(&a, 200, 30), (0, 0, 200, 30), "state was dropped");
        assert_eq!(diff.damage(&[], 0, 0), (0, 0, 1, 1));
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

