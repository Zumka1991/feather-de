//! Fast integer document blitter, SSE2 vertical filter, incremental pan cache, and cached
//! window-corner AA mask for `feather-preview`.
//!
//! Avoids full-window path/mask allocations on every frame, clips strictly to the canvas,
//! reuses two flat horizontal scanlines for separable 14-bit bilinear filtering, and shifts
//! the already-rendered viewport during mouse panning so only newly exposed edge strips are
//! sampled (< 1.5 ms/frame even at 2880×1800 HiDPI).

use tiny_skia::{Color, FillRule, Mask, PixmapMut, Transform};

use crate::theme::{rounded_rect_path, ui_scale};

#[derive(Clone, Copy)]
struct BilinearTap {
    off0: u32,
    off1: u32,
    /// `iw + wx == 128` (`0..=128`), keeping `255 * 128 == 32640 <= i16::MAX` for SSE2 `pmaddwd`
    iw: i16,
    wx: i16,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PanMeta {
    cw: i32,
    ch: i32,
    src_ptr: usize,
    src_w: i32,
    src_h: i32,
    img_w: i32,
    img_h: i32,
    zoom_bits: u32,
    scale_bits: u32,
    opaque_white_backdrop: bool,
    has_alpha: bool,
}

struct PanCache {
    meta: PanMeta,
    /// Image top-left relative to `(cx0, cy0)` of the canvas
    rel_x: i32,
    rel_y: i32,
    buf: Vec<u8>,
}

#[derive(Default)]
pub struct RenderScratch {
    /// Byte offsets `sx * 4` into a source scanline
    x_nearest: Vec<u32>,
    x_bilinear: Vec<BilinearTap>,
    /// Precomputed horizontal checkerboard parity (`0` or `1`) per visible pixel
    checker_x: Vec<u8>,
    /// Flat `vw * 4` RGBA scanlines scaled by 128 (`0..=32640`)
    hrow0: Vec<i16>,
    hrow1: Vec<i16>,
    pan_cache: Option<PanCache>,
    corner: Option<CornerCache>,
}

struct CornerCache {
    scale_bits: u32,
    radius_bits: u32,
    edge_rgba: [u8; 4],
    size: usize,
    clip_alpha: Vec<u8>,
    border_px: Vec<[u8; 4]>,
}

#[inline]
fn color_to_premul_u8(c: Color) -> [u8; 4] {
    let p = c.premultiply();
    [
        (p.red() * 255.0 + 0.5) as u8,
        (p.green() * 255.0 + 0.5) as u8,
        (p.blue() * 255.0 + 0.5) as u8,
        (p.alpha() * 255.0 + 0.5) as u8,
    ]
}

#[inline]
pub fn fill_px_rect(
    dst: &mut [u8],
    pw: i32,
    ph: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    rgba: [u8; 4],
) {
    let x0 = x0.clamp(0, pw) as usize;
    let y0 = y0.clamp(0, ph) as usize;
    let x1 = x1.clamp(x0 as i32, pw) as usize;
    let y1 = y1.clamp(y0 as i32, ph) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let stride = pw as usize * 4;
    let px = u32::from_ne_bytes(rgba).to_ne_bytes();
    if x0 == 0 && x1 == pw as usize {
        for p in dst[y0 * stride..y1 * stride].chunks_exact_mut(4) {
            p.copy_from_slice(&px);
        }
    } else {
        for y in y0..y1 {
            let row = &mut dst[y * stride + x0 * 4..y * stride + x1 * 4];
            for p in row.chunks_exact_mut(4) {
                p.copy_from_slice(&px);
            }
        }
    }
}

#[inline]
fn blend_px_rect_over_opaque(
    dst: &mut [u8],
    pw: i32,
    ph: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    src_premul: [u8; 4],
) {
    let a = src_premul[3] as u32;
    if a == 0 {
        return;
    }
    if a == 255 {
        fill_px_rect(dst, pw, ph, x0, y0, x1, y1, src_premul);
        return;
    }
    let x0 = x0.clamp(0, pw) as usize;
    let y0 = y0.clamp(0, ph) as usize;
    let x1 = x1.clamp(x0 as i32, pw) as usize;
    let y1 = y1.clamp(y0 as i32, ph) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let inv_a = 255 - a;
    let (sr, sg, sb) = (
        src_premul[0] as u32,
        src_premul[1] as u32,
        src_premul[2] as u32,
    );
    let stride = pw as usize * 4;
    for y in y0..y1 {
        let row = &mut dst[y * stride + x0 * 4..y * stride + x1 * 4];
        for p in row.chunks_exact_mut(4) {
            p[0] = (sr + ((p[0] as u32 * inv_a + 128) >> 8)) as u8;
            p[1] = (sg + ((p[1] as u32 * inv_a + 128) >> 8)) as u8;
            p[2] = (sb + ((p[2] as u32 * inv_a + 128) >> 8)) as u8;
            p[3] = 255;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_outside_box(
    dst: &mut [u8],
    pw: i32,
    ph: i32,
    cx0: i32,
    cy0: i32,
    cx1: i32,
    cy1: i32,
    ix0: i32,
    iy0: i32,
    ix1: i32,
    iy1: i32,
    rgba: [u8; 4],
) {
    if ix0 >= ix1 || iy0 >= iy1 {
        fill_px_rect(dst, pw, ph, cx0, cy0, cx1, cy1, rgba);
        return;
    }
    fill_px_rect(dst, pw, ph, cx0, cy0, cx1, iy0, rgba);
    fill_px_rect(dst, pw, ph, cx0, iy1, cx1, cy1, rgba);
    fill_px_rect(dst, pw, ph, cx0, iy0, ix0, iy1, rgba);
    fill_px_rect(dst, pw, ph, ix1, iy0, cx1, iy1, rgba);
}

#[allow(clippy::too_many_arguments)]
fn blend_shadow_ring(
    dst: &mut [u8],
    pw: i32,
    ph: i32,
    cx0: i32,
    cy0: i32,
    cx1: i32,
    cy1: i32,
    sx0: i32,
    sy0: i32,
    sx1: i32,
    sy1: i32,
    ix0: i32,
    iy0: i32,
    ix1: i32,
    iy1: i32,
    shadow_premul: [u8; 4],
) {
    let bx0 = sx0.max(cx0);
    let by0 = sy0.max(cy0);
    let bx1 = sx1.min(cx1);
    let by1 = sy1.min(cy1);
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }
    if ix0 >= ix1 || iy0 >= iy1 {
        blend_px_rect_over_opaque(dst, pw, ph, bx0, by0, bx1, by1, shadow_premul);
        return;
    }
    let my0 = iy0.clamp(by0, by1);
    let my1 = iy1.clamp(by0, by1);
    blend_px_rect_over_opaque(dst, pw, ph, bx0, by0, bx1, my0, shadow_premul);
    blend_px_rect_over_opaque(dst, pw, ph, bx0, my1, bx1, by1, shadow_premul);
    blend_px_rect_over_opaque(dst, pw, ph, bx0, my0, ix0.clamp(bx0, bx1), my1, shadow_premul);
    blend_px_rect_over_opaque(dst, pw, ph, ix1.clamp(bx0, bx1), my0, bx1, my1, shadow_premul);
}

/// Horizontal bilinear scanline gather (`out` has length `taps.len() * 4`, scaled by 128).
/// Returns `true` when every sampled pixel on this scanline is 100% opaque (`alpha == 255 * 128`).
#[inline]
fn sample_hrow_flat(src_row: &[u8], taps: &[BilinearTap], out: &mut [i16]) -> bool {
    let len = src_row.len();
    let mut all_opaque = true;
    for (dst, tap) in out.chunks_exact_mut(4).zip(taps.iter()) {
        let i0 = tap.off0 as usize;
        let i1 = tap.off1 as usize;
        if i0 + 4 <= len && i1 + 4 <= len {
            let p0 = &src_row[i0..i0 + 4];
            let p1 = &src_row[i1..i1 + 4];
            let iw = tap.iw;
            let wx = tap.wx;
            let a = p0[3] as i16 * iw + p1[3] as i16 * wx;
            dst[0] = p0[0] as i16 * iw + p1[0] as i16 * wx;
            dst[1] = p0[1] as i16 * iw + p1[1] as i16 * wx;
            dst[2] = p0[2] as i16 * iw + p1[2] as i16 * wx;
            dst[3] = a;
            all_opaque &= a == 255 * 128;
        }
    }
    all_opaque
}

/// Vertical blend of two `i16` scanlines (`0..=32640`) with weights `iw_y + wy == 128`:
/// `((r0 * iw_y + r1 * wy + 8192) >> 14) as u8`.
/// Uses SSE2 `_mm_madd_epi16` on `x86_64` (16 bytes / 4 RGBA pixels per iteration).
#[inline]
fn blend_vrow_opaque(drow: &mut [u8], r0: &[i16], r1: &[i16], iw_y: i32, wy: i32) {
    let n = drow.len().min(r0.len()).min(r1.len());
    let mut i = 0usize;

    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        // SAFETY: SSE2 is baseline on all x86_64 CPUs; slices are bounds-checked via `i + 16 <= n`.
        unsafe {
            let weights = _mm_set1_epi32(((wy & 0xffff) << 16) | (iw_y & 0xffff));
            let bias = _mm_set1_epi32(8192);
            let p_dst = drow.as_mut_ptr();
            let p_r0 = r0.as_ptr();
            let p_r1 = r1.as_ptr();
            while i + 16 <= n {
                let a0 = _mm_loadu_si128(p_r0.add(i) as *const __m128i);
                let b0 = _mm_loadu_si128(p_r1.add(i) as *const __m128i);
                let lo0 = _mm_unpacklo_epi16(a0, b0);
                let hi0 = _mm_unpackhi_epi16(a0, b0);
                let s0 = _mm_srai_epi32(_mm_add_epi32(_mm_madd_epi16(lo0, weights), bias), 14);
                let s1 = _mm_srai_epi32(_mm_add_epi32(_mm_madd_epi16(hi0, weights), bias), 14);
                let p16_0 = _mm_packs_epi32(s0, s1);

                let a1 = _mm_loadu_si128(p_r0.add(i + 8) as *const __m128i);
                let b1 = _mm_loadu_si128(p_r1.add(i + 8) as *const __m128i);
                let lo1 = _mm_unpacklo_epi16(a1, b1);
                let hi1 = _mm_unpackhi_epi16(a1, b1);
                let s2 = _mm_srai_epi32(_mm_add_epi32(_mm_madd_epi16(lo1, weights), bias), 14);
                let s3 = _mm_srai_epi32(_mm_add_epi32(_mm_madd_epi16(hi1, weights), bias), 14);
                let p16_1 = _mm_packs_epi32(s2, s3);

                let out = _mm_packus_epi16(p16_0, p16_1);
                _mm_storeu_si128(p_dst.add(i) as *mut __m128i, out);
                i += 16;
            }
        }
    }

    while i < n {
        drow[i] = ((r0[i] as i32 * iw_y + r1[i] as i32 * wy + 8192) >> 14) as u8;
        i += 1;
    }
}

#[inline]
fn shift_vrow_opaque(drow: &mut [u8], r0: &[i16]) {
    let n = drow.len().min(r0.len());
    let mut i = 0usize;

    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        // SAFETY: SSE2 is baseline on x86_64; bounds-checked via `i + 16 <= n`.
        unsafe {
            let bias = _mm_set1_epi16(64);
            let p_dst = drow.as_mut_ptr();
            let p_r0 = r0.as_ptr();
            while i + 16 <= n {
                let a0 = _mm_srai_epi16(_mm_add_epi16(_mm_loadu_si128(p_r0.add(i) as *const __m128i), bias), 7);
                let a1 = _mm_srai_epi16(_mm_add_epi16(_mm_loadu_si128(p_r0.add(i + 8) as *const __m128i), bias), 7);
                let out = _mm_packus_epi16(a0, a1);
                _mm_storeu_si128(p_dst.add(i) as *mut __m128i, out);
                i += 16;
            }
        }
    }

    while i < n {
        drow[i] = ((r0[i] + 64) >> 7) as u8;
        i += 1;
    }
}

#[inline(always)]
fn checker_bg_from_parity(parity: u8) -> [u8; 3] {
    if parity == 0 {
        [250, 250, 252]
    } else {
        [224, 224, 230]
    }
}

#[inline(always)]
fn composite_over_bg(sr: u8, sg: u8, sb: u8, sa: u8, bg: [u8; 3]) -> [u8; 4] {
    if sa == 255 {
        [sr, sg, sb, 255]
    } else if sa == 0 {
        [bg[0], bg[1], bg[2], 255]
    } else {
        let inv = (255 - sa) as u32;
        [
            (sr as u32 + ((bg[0] as u32 * inv + 128) >> 8)) as u8,
            (sg as u32 + ((bg[1] as u32 * inv + 128) >> 8)) as u8,
            (sb as u32 + ((bg[2] as u32 * inv + 128) >> 8)) as u8,
            255,
        ]
    }
}

impl RenderScratch {
    /// Frees the incremental pan buffer when leaving Manual zoom or closing a document.
    pub fn clear_pan_cache(&mut self) {
        self.pan_cache = None;
    }

    fn prepare_checker_x(&mut self, start_x: i32, vw: usize, cell_px: i32) {
        self.checker_x.clear();
        self.checker_x.reserve(vw);
        for i in 0..vw {
            let p = ((start_x + i as i32).div_euclid(cell_px) & 1) as u8;
            self.checker_x.push(p);
        }
    }

    /// Renders a sub-rectangle `[rx0..rx1, ry0..ry1]` inside a destination buffer of width
    /// `dst_w`, where the scaled image sits at `(img_x, img_y)` with size `(img_w, img_h)`.
    /// Precondition: `[rx0..rx1, ry0..ry1]` is contained inside `[img_x..img_x+img_w, img_y..img_y+img_h]`.
    #[allow(clippy::too_many_arguments)]
    fn blit_image_subrect(
        &mut self,
        dst: &mut [u8],
        dst_w: i32,
        rx0: i32,
        ry0: i32,
        rx1: i32,
        ry1: i32,
        img_x: i32,
        img_y: i32,
        img_w: i32,
        img_h: i32,
        src: &[u8],
        src_w: i32,
        src_h: i32,
        opaque_white_backdrop: bool,
        has_alpha: bool,
        zoom: f32,
        cell_px: i32,
    ) {
        if rx0 >= rx1 || ry0 >= ry1 {
            return;
        }
        let vw = (rx1 - rx0) as usize;
        let vh = (ry1 - ry0) as usize;
        let dst_stride = dst_w as usize * 4;
        let src_stride = src_w as usize * 4;

        if has_alpha && !opaque_white_backdrop {
            self.prepare_checker_x(rx0 - img_x, vw, cell_px);
        }

        if img_w == src_w && img_h == src_h {
            let sx0 = (rx0 - img_x) as usize;
            let sy0 = (ry0 - img_y) as usize;
            for j in 0..vh {
                let srow = &src[(sy0 + j) * src_stride + sx0 * 4..][..vw * 4];
                let drow = &mut dst[(ry0 as usize + j) * dst_stride + rx0 as usize * 4..][..vw * 4];
                if !has_alpha || srow.chunks_exact(4).all(|p| p[3] == 255) {
                    drow.copy_from_slice(srow);
                } else if opaque_white_backdrop {
                    for (dp, sp) in drow.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
                        dp.copy_from_slice(&composite_over_bg(
                            sp[0],
                            sp[1],
                            sp[2],
                            sp[3],
                            [255, 255, 255],
                        ));
                    }
                } else {
                    let row_p = ((ry0 + j as i32 - img_y).div_euclid(cell_px) & 1) as u8;
                    for ((dp, sp), &cx_p) in drow
                        .chunks_exact_mut(4)
                        .zip(srow.chunks_exact(4))
                        .zip(self.checker_x.iter())
                    {
                        let bg = checker_bg_from_parity(cx_p ^ row_p);
                        dp.copy_from_slice(&composite_over_bg(sp[0], sp[1], sp[2], sp[3], bg));
                    }
                }
            }
        } else if zoom >= 2.0 {
            self.x_nearest.clear();
            self.x_nearest.reserve(vw);
            let iw = img_w as u64;
            let sw = src_w as u64;
            let max_sx = (src_w - 1) as u64;
            let base_x = (rx0 - img_x) as u64;
            for i in 0..vw {
                let sx = (((base_x + i as u64) * sw) / iw).min(max_sx) as u32;
                self.x_nearest.push(sx * 4);
            }
            let ih = img_h as u64;
            let sh = src_h as u64;
            let max_sy = (src_h - 1) as u64;
            let base_y = (ry0 - img_y) as u64;
            let mut prev_sy = usize::MAX;
            let mut prev_row_p = u8::MAX;
            let mut prev_row_opaque = false;
            for j in 0..vh {
                let sy = (((base_y + j as u64) * sh) / ih).min(max_sy) as usize;
                let row_p = if has_alpha && !opaque_white_backdrop {
                    ((ry0 + j as i32 - img_y).div_euclid(cell_px) & 1) as u8
                } else {
                    0
                };
                let dst_off = (ry0 as usize + j) * dst_stride + rx0 as usize * 4;
                if j > 0 && sy == prev_sy && (prev_row_opaque || row_p == prev_row_p) {
                    let prev_off = dst_off - dst_stride;
                    dst.copy_within(prev_off..prev_off + vw * 4, dst_off);
                    continue;
                }
                prev_sy = sy;
                prev_row_p = row_p;
                let srow = &src[sy * src_stride..sy * src_stride + src_stride];
                let drow = &mut dst[dst_off..dst_off + vw * 4];
                if !has_alpha {
                    prev_row_opaque = true;
                    for (dp, &si) in drow.chunks_exact_mut(4).zip(self.x_nearest.iter()) {
                        let si = si as usize;
                        if si + 4 <= srow.len() {
                            dp.copy_from_slice(&srow[si..si + 4]);
                        }
                    }
                } else if opaque_white_backdrop {
                    prev_row_opaque = true;
                    for (dp, &si) in drow.chunks_exact_mut(4).zip(self.x_nearest.iter()) {
                        let si = si as usize;
                        dp.copy_from_slice(&composite_over_bg(
                            srow[si],
                            srow[si + 1],
                            srow[si + 2],
                            srow[si + 3],
                            [255, 255, 255],
                        ));
                    }
                } else {
                    let mut all_op = true;
                    for ((dp, &si), &cx_p) in drow
                        .chunks_exact_mut(4)
                        .zip(self.x_nearest.iter())
                        .zip(self.checker_x.iter())
                    {
                        let si = si as usize;
                        let sa = srow[si + 3];
                        all_op &= sa == 255;
                        let bg = checker_bg_from_parity(cx_p ^ row_p);
                        dp.copy_from_slice(&composite_over_bg(
                            srow[si],
                            srow[si + 1],
                            srow[si + 2],
                            sa,
                            bg,
                        ));
                    }
                    prev_row_opaque = all_op;
                }
            }
        } else {
            // Separable 14-bit integer bilinear filter (`0..=128` weights) with SSE2 vertical pass
            self.x_bilinear.clear();
            self.x_bilinear.reserve(vw);
            let iw = img_w as i64;
            let sw = src_w as i64;
            let max_sx = (src_w - 1) as i64;
            let base_x = (rx0 - img_x) as i64;
            for i in 0..vw {
                let px = base_x + i as i64;
                let u_fp = (((2 * px + 1) * sw * 64) / iw) - 64;
                if u_fp <= 0 {
                    self.x_bilinear.push(BilinearTap {
                        off0: 0,
                        off1: 0,
                        iw: 128,
                        wx: 0,
                    });
                } else if u_fp >= (max_sx << 7) {
                    let off = (max_sx as u32) * 4;
                    self.x_bilinear.push(BilinearTap {
                        off0: off,
                        off1: off,
                        iw: 128,
                        wx: 0,
                    });
                } else {
                    let sx0 = (u_fp >> 7) as u32;
                    let wx = (u_fp & 127) as i16;
                    self.x_bilinear.push(BilinearTap {
                        off0: sx0 * 4,
                        off1: (sx0 + 1) * 4,
                        iw: 128 - wx,
                        wx,
                    });
                }
            }
            let flat_len = vw * 4;
            self.hrow0.resize(flat_len, 0);
            self.hrow1.resize(flat_len, 0);
            let mut cached_y0 = usize::MAX;
            let mut cached_y1 = usize::MAX;
            let mut op0 = !has_alpha;
            let mut op1 = !has_alpha;

            let ih = img_h as i64;
            let sh = src_h as i64;
            let max_sy = (src_h - 1) as i64;
            let base_y = (ry0 - img_y) as i64;

            for j in 0..vh {
                let py = base_y + j as i64;
                let v_fp = (((2 * py + 1) * sh * 64) / ih) - 64;
                let (sy0, sy1, wy) = if v_fp <= 0 {
                    (0usize, 0usize, 0i32)
                } else if v_fp >= (max_sy << 7) {
                    (max_sy as usize, max_sy as usize, 0i32)
                } else {
                    let y0 = (v_fp >> 7) as usize;
                    (y0, y0 + 1, (v_fp & 127) as i32)
                };

                if sy0 != cached_y0 {
                    if sy0 == cached_y1 {
                        std::mem::swap(&mut self.hrow0, &mut self.hrow1);
                        op0 = op1;
                        cached_y0 = sy0;
                        cached_y1 = usize::MAX;
                    } else {
                        let srow = &src[sy0 * src_stride..sy0 * src_stride + src_stride];
                        op0 = sample_hrow_flat(srow, &self.x_bilinear, &mut self.hrow0);
                        cached_y0 = sy0;
                    }
                }
                if wy > 0 && sy1 != cached_y1 {
                    let srow = &src[sy1 * src_stride..sy1 * src_stride + src_stride];
                    op1 = sample_hrow_flat(srow, &self.x_bilinear, &mut self.hrow1);
                    cached_y1 = sy1;
                }

                let dst_off = (ry0 as usize + j) * dst_stride + rx0 as usize * 4;
                let drow = &mut dst[dst_off..dst_off + flat_len];

                if wy == 0 {
                    if !has_alpha || op0 {
                        shift_vrow_opaque(drow, &self.hrow0);
                    } else if opaque_white_backdrop {
                        for (dp, r0) in drow.chunks_exact_mut(4).zip(self.hrow0.chunks_exact(4)) {
                            dp.copy_from_slice(&composite_over_bg(
                                ((r0[0] + 64) >> 7) as u8,
                                ((r0[1] + 64) >> 7) as u8,
                                ((r0[2] + 64) >> 7) as u8,
                                ((r0[3] + 64) >> 7) as u8,
                                [255, 255, 255],
                            ));
                        }
                    } else {
                        let row_p = ((ry0 + j as i32 - img_y).div_euclid(cell_px) & 1) as u8;
                        for ((dp, r0), &cx_p) in drow
                            .chunks_exact_mut(4)
                            .zip(self.hrow0.chunks_exact(4))
                            .zip(self.checker_x.iter())
                        {
                            let bg = checker_bg_from_parity(cx_p ^ row_p);
                            dp.copy_from_slice(&composite_over_bg(
                                ((r0[0] + 64) >> 7) as u8,
                                ((r0[1] + 64) >> 7) as u8,
                                ((r0[2] + 64) >> 7) as u8,
                                ((r0[3] + 64) >> 7) as u8,
                                bg,
                            ));
                        }
                    }
                } else {
                    let iw_y = 128 - wy;
                    if !has_alpha || (op0 && op1) {
                        blend_vrow_opaque(drow, &self.hrow0, &self.hrow1, iw_y, wy);
                    } else if opaque_white_backdrop {
                        for (dp, (r0, r1)) in drow.chunks_exact_mut(4).zip(
                            self.hrow0
                                .chunks_exact(4)
                                .zip(self.hrow1.chunks_exact(4)),
                        ) {
                            dp.copy_from_slice(&composite_over_bg(
                                ((r0[0] as i32 * iw_y + r1[0] as i32 * wy + 8192) >> 14) as u8,
                                ((r0[1] as i32 * iw_y + r1[1] as i32 * wy + 8192) >> 14) as u8,
                                ((r0[2] as i32 * iw_y + r1[2] as i32 * wy + 8192) >> 14) as u8,
                                ((r0[3] as i32 * iw_y + r1[3] as i32 * wy + 8192) >> 14) as u8,
                                [255, 255, 255],
                            ));
                        }
                    } else {
                        let row_p = ((ry0 + j as i32 - img_y).div_euclid(cell_px) & 1) as u8;
                        for ((dp, (r0, r1)), &cx_p) in drow
                            .chunks_exact_mut(4)
                            .zip(
                                self.hrow0
                                    .chunks_exact(4)
                                    .zip(self.hrow1.chunks_exact(4)),
                            )
                            .zip(self.checker_x.iter())
                        {
                            let bg = checker_bg_from_parity(cx_p ^ row_p);
                            dp.copy_from_slice(&composite_over_bg(
                                ((r0[0] as i32 * iw_y + r1[0] as i32 * wy + 8192) >> 14) as u8,
                                ((r0[1] as i32 * iw_y + r1[1] as i32 * wy + 8192) >> 14) as u8,
                                ((r0[2] as i32 * iw_y + r1[2] as i32 * wy + 8192) >> 14) as u8,
                                ((r0[3] as i32 * iw_y + r1[3] as i32 * wy + 8192) >> 14) as u8,
                                bg,
                            ));
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint_canvas_and_document(
        &mut self,
        pm: &mut PixmapMut,
        canvas_x: f32,
        canvas_y: f32,
        canvas_w: f32,
        canvas_h: f32,
        canvas_bg: Color,
        src_pixmap: &tiny_skia::Pixmap,
        opaque_white_backdrop: bool,
        has_alpha: bool,
        zoom: f32,
        pan_x: f32,
        pan_y: f32,
        use_pan_cache: bool,
        border_col: Color,
    ) {
        let s = ui_scale();
        let pw = pm.width() as i32;
        let ph = pm.height() as i32;
        let cx0 = ((canvas_x * s).round() as i32).clamp(0, pw);
        let cy0 = ((canvas_y * s).round() as i32).clamp(0, ph);
        let cx1 = (((canvas_x + canvas_w) * s).round() as i32).clamp(cx0, pw);
        let cy1 = (((canvas_y + canvas_h) * s).round() as i32).clamp(cy0, ph);
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }

        let src_w = src_pixmap.width() as i32;
        let src_h = src_pixmap.height() as i32;
        let bg_rgba = color_to_premul_u8(canvas_bg);
        if src_w <= 0 || src_h <= 0 {
            fill_px_rect(pm.data_mut(), pw, ph, cx0, cy0, cx1, cy1, bg_rgba);
            return;
        }

        let dw = src_w as f32 * zoom;
        let dh = src_h as f32 * zoom;
        let scale = zoom * s;
        let img_w = (src_w as f32 * scale).round().max(1.0) as i32;
        let img_h = (src_h as f32 * scale).round().max(1.0) as i32;
        let img_x = ((canvas_x + (canvas_w - dw) * 0.5 + pan_x) * s).round() as i32;
        let img_y = ((canvas_y + (canvas_h - dh) * 0.5 + pan_y) * s).round() as i32;

        let cw = cx1 - cx0;
        let ch = cy1 - cy0;
        let bw = (1.0 * s).round().max(1.0) as i32;
        let cell_px = (12.0 * s).round().max(4.0) as i32;
        let src = src_pixmap.data();

        // Fast path for Manual zoom when the zoomed image covers the entire canvas interior
        // (no outer background, shadow, or 1-px border visible inside `[cx0..cx1, cy0..cy1]`).
        let rel_x = img_x - cx0;
        let rel_y = img_y - cy0;
        let covers_interior =
            rel_x <= -bw && rel_y <= -bw && rel_x + img_w >= cw + bw && rel_y + img_h >= ch + bw;
        let buf_bytes = (cw as usize) * (ch as usize) * 4;

        if use_pan_cache && zoom < 2.0 && covers_interior && buf_bytes <= 36 * 1024 * 1024 {
            let meta = PanMeta {
                cw,
                ch,
                src_ptr: src.as_ptr() as usize,
                src_w,
                src_h,
                img_w,
                img_h,
                zoom_bits: zoom.to_bits(),
                scale_bits: s.to_bits(),
                opaque_white_backdrop,
                has_alpha,
            };

            let mut incremental = None;
            if let Some(pc) = &self.pan_cache {
                if pc.meta == meta && pc.buf.len() == buf_bytes {
                    let dx = rel_x - pc.rel_x;
                    let dy = rel_y - pc.rel_y;
                    if dx.abs() < cw && dy.abs() < ch {
                        incremental = Some((dx, dy));
                    }
                }
            }

            if let Some((dx, dy)) = incremental {
                if dx != 0 || dy != 0 {
                    let mut cache = self.pan_cache.take().unwrap();
                    let stride = cw as usize * 4;
                    let x_keep0 = dx.max(0);
                    let x_keep1 = (cw + dx).min(cw);
                    let y_keep0 = dy.max(0);
                    let y_keep1 = (ch + dy).min(ch);
                    let copy_bytes = ((x_keep1 - x_keep0) as usize) * 4;
                    let src_x = (x_keep0 - dx) as usize * 4;
                    let dst_x = x_keep0 as usize * 4;

                    if dy > 0 {
                        for y in (y_keep0..y_keep1).rev() {
                            let sy = (y - dy) as usize;
                            let s_off = sy * stride + src_x;
                            let d_off = y as usize * stride + dst_x;
                            cache.buf.copy_within(s_off..s_off + copy_bytes, d_off);
                        }
                    } else if dy < 0 {
                        for y in y_keep0..y_keep1 {
                            let sy = (y - dy) as usize;
                            let s_off = sy * stride + src_x;
                            let d_off = y as usize * stride + dst_x;
                            cache.buf.copy_within(s_off..s_off + copy_bytes, d_off);
                        }
                    } else if dx != 0 {
                        for y in 0..ch as usize {
                            let row_off = y * stride;
                            cache.buf.copy_within(
                                row_off + src_x..row_off + src_x + copy_bytes,
                                row_off + dst_x,
                            );
                        }
                    }

                    // Render only the newly exposed horizontal & vertical edge strips!
                    if dy > 0 {
                        self.blit_image_subrect(
                            &mut cache.buf,
                            cw,
                            0,
                            0,
                            cw,
                            y_keep0,
                            rel_x,
                            rel_y,
                            img_w,
                            img_h,
                            src,
                            src_w,
                            src_h,
                            opaque_white_backdrop,
                            has_alpha,
                            zoom,
                            cell_px,
                        );
                    } else if dy < 0 {
                        self.blit_image_subrect(
                            &mut cache.buf,
                            cw,
                            0,
                            y_keep1,
                            cw,
                            ch,
                            rel_x,
                            rel_y,
                            img_w,
                            img_h,
                            src,
                            src_w,
                            src_h,
                            opaque_white_backdrop,
                            has_alpha,
                            zoom,
                            cell_px,
                        );
                    }
                    if dx > 0 {
                        self.blit_image_subrect(
                            &mut cache.buf,
                            cw,
                            0,
                            y_keep0,
                            x_keep0,
                            y_keep1,
                            rel_x,
                            rel_y,
                            img_w,
                            img_h,
                            src,
                            src_w,
                            src_h,
                            opaque_white_backdrop,
                            has_alpha,
                            zoom,
                            cell_px,
                        );
                    } else if dx < 0 {
                        self.blit_image_subrect(
                            &mut cache.buf,
                            cw,
                            x_keep1,
                            y_keep0,
                            cw,
                            y_keep1,
                            rel_x,
                            rel_y,
                            img_w,
                            img_h,
                            src,
                            src_w,
                            src_h,
                            opaque_white_backdrop,
                            has_alpha,
                            zoom,
                            cell_px,
                        );
                    }
                    cache.rel_x = rel_x;
                    cache.rel_y = rel_y;
                    self.pan_cache = Some(cache);
                }
            } else {
                let mut buf = self
                    .pan_cache
                    .take()
                    .map(|c| c.buf)
                    .unwrap_or_default();
                buf.resize(buf_bytes, 0);
                self.blit_image_subrect(
                    &mut buf,
                    cw,
                    0,
                    0,
                    cw,
                    ch,
                    rel_x,
                    rel_y,
                    img_w,
                    img_h,
                    src,
                    src_w,
                    src_h,
                    opaque_white_backdrop,
                    has_alpha,
                    zoom,
                    cell_px,
                );
                self.pan_cache = Some(PanCache {
                    meta,
                    rel_x,
                    rel_y,
                    buf,
                });
            }

            // Blit cached canvas straight into the SHM Pixmap
            let cache = self.pan_cache.as_ref().unwrap();
            let dst = pm.data_mut();
            let dst_stride = pw as usize * 4;
            let cache_stride = cw as usize * 4;
            if cx0 == 0 && cw == pw {
                let off = cy0 as usize * dst_stride;
                dst[off..off + buf_bytes].copy_from_slice(&cache.buf);
            } else {
                for y in 0..ch as usize {
                    let d_off = (cy0 as usize + y) * dst_stride + cx0 as usize * 4;
                    let s_off = y * cache_stride;
                    dst[d_off..d_off + cache_stride]
                        .copy_from_slice(&cache.buf[s_off..s_off + cache_stride]);
                }
            }
            return;
        }

        if !use_pan_cache || zoom >= 2.0 {
            self.pan_cache = None;
        }

        let vx0 = img_x.max(cx0);
        let vy0 = img_y.max(cy0);
        let vx1 = (img_x + img_w).min(cx1);
        let vy1 = (img_y + img_h).min(cy1);

        // 1. Fill canvas background ONLY around the visible image rectangle
        fill_outside_box(
            pm.data_mut(),
            pw,
            ph,
            cx0,
            cy0,
            cx1,
            cy1,
            vx0,
            vy0,
            vx1,
            vy1,
            bg_rgba,
        );

        // 2. Soft paper drop-shadow ONLY on visible outer margins
        let pad_outer = (2.0 * s).round().max(1.0) as i32;
        let off_y_outer = (4.0 * s).round().max(1.0) as i32;
        let ext_b_outer = (6.0 * s).round().max(2.0) as i32;
        blend_shadow_ring(
            pm.data_mut(),
            pw,
            ph,
            cx0,
            cy0,
            cx1,
            cy1,
            img_x - pad_outer,
            img_y + off_y_outer,
            img_x + img_w + pad_outer,
            img_y + img_h + ext_b_outer,
            vx0,
            vy0,
            vx1,
            vy1,
            [0, 0, 0, 42],
        );
        let pad_inner = (1.0 * s).round().max(1.0) as i32;
        let off_y_inner = (1.5 * s).round().max(1.0) as i32;
        blend_shadow_ring(
            pm.data_mut(),
            pw,
            ph,
            cx0,
            cy0,
            cx1,
            cy1,
            img_x - pad_inner,
            img_y + off_y_inner,
            img_x + img_w + pad_inner,
            img_y + img_h + off_y_inner + pad_inner,
            vx0,
            vy0,
            vx1,
            vy1,
            [0, 0, 0, 28],
        );

        // 3. Blit visible sub-rectangle of the document
        self.blit_image_subrect(
            pm.data_mut(),
            pw,
            vx0,
            vy0,
            vx1,
            vy1,
            img_x,
            img_y,
            img_w,
            img_h,
            src,
            src_w,
            src_h,
            opaque_white_backdrop,
            has_alpha,
            zoom,
            cell_px,
        );

        // 4. Crisp 1-px frame around visible document edges
        if vx0 < vx1 && vy0 < vy1 {
            let border_px = color_to_premul_u8(border_col);
            let dst = pm.data_mut();
            if img_y >= cy0 && img_y < cy1 {
                blend_px_rect_over_opaque(
                    dst,
                    pw,
                    ph,
                    vx0,
                    img_y,
                    vx1,
                    (img_y + bw).min(cy1),
                    border_px,
                );
            }
            let bot = img_y + img_h - bw;
            if bot < cy1 && bot + bw > cy0 {
                blend_px_rect_over_opaque(
                    dst,
                    pw,
                    ph,
                    vx0,
                    bot.max(cy0),
                    vx1,
                    (img_y + img_h).min(cy1),
                    border_px,
                );
            }
            if img_x >= cx0 && img_x < cx1 {
                blend_px_rect_over_opaque(
                    dst,
                    pw,
                    ph,
                    img_x,
                    vy0,
                    (img_x + bw).min(cx1),
                    vy1,
                    border_px,
                );
            }
            let right = img_x + img_w - bw;
            if right < cx1 && right + bw > cx0 {
                blend_px_rect_over_opaque(
                    dst,
                    pw,
                    ph,
                    right.max(cx0),
                    vy0,
                    (img_x + img_w).min(cx1),
                    vy1,
                    border_px,
                );
            }
        }
    }

    /// Applies the outer 1-px window border and rounded corner AA mask in O(corner_px²)
    /// instead of allocating a full-window `Mask::new(pw, ph)` every frame.
    pub fn paint_window_frame(&mut self, pm: &mut PixmapMut, radius: f32, edge_col: Color) {
        let s = ui_scale();
        let pw = pm.width() as i32;
        let ph = pm.height() as i32;
        if pw <= 0 || ph <= 0 || radius <= 0.0 {
            return;
        }
        let edge_rgba = color_to_premul_u8(edge_col);
        let scale_bits = s.to_bits();
        let radius_bits = radius.to_bits();

        let rebuild = self.corner.as_ref().is_none_or(|c| {
            c.scale_bits != scale_bits || c.radius_bits != radius_bits || c.edge_rgba != edge_rgba
        });
        if rebuild {
            let n = ((radius * s).ceil() as usize + 3).max(4);
            let box_logical = (n as f32 * 2.0 + 8.0) / s;
            let pix_side = (n * 2 + 8) as u32;
            let mut clip_alpha = vec![255u8; n * n];
            if let (Some(path), Some(mut mask)) = (
                rounded_rect_path(0.0, 0.0, box_logical, box_logical, radius),
                Mask::new(pix_side, pix_side),
            ) {
                mask.fill_path(&path, FillRule::Winding, true, Transform::from_scale(s, s));
                let mdata = mask.data();
                for y in 0..n {
                    for x in 0..n {
                        clip_alpha[y * n + x] = mdata[y * pix_side as usize + x];
                    }
                }
            }
            let mut border_px = vec![[0u8; 4]; n * n];
            if let (Some(path), Some(mut bpm)) = (
                rounded_rect_path(0.5, 0.5, box_logical - 1.0, box_logical - 1.0, radius),
                tiny_skia::Pixmap::new(pix_side, pix_side),
            ) {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color(edge_col);
                paint.anti_alias = true;
                let stroke = tiny_skia::Stroke {
                    width: 1.0,
                    ..Default::default()
                };
                bpm.stroke_path(&path, &paint, &stroke, Transform::from_scale(s, s), None);
                let bdata = bpm.data();
                for y in 0..n {
                    for x in 0..n {
                        let si = (y * pix_side as usize + x) * 4;
                        border_px[y * n + x] =
                            [bdata[si], bdata[si + 1], bdata[si + 2], bdata[si + 3]];
                    }
                }
            }
            self.corner = Some(CornerCache {
                scale_bits,
                radius_bits,
                edge_rgba,
                size: n,
                clip_alpha,
                border_px,
            });
        }

        let c = self.corner.as_ref().unwrap();
        let n = c.size.min((pw as usize) / 2).min((ph as usize) / 2);
        let bw = (1.0 * s).round().max(1.0) as i32;
        let dst = pm.data_mut();

        let ni = n as i32;
        blend_px_rect_over_opaque(dst, pw, ph, ni, 0, pw - ni, bw, edge_rgba);
        blend_px_rect_over_opaque(dst, pw, ph, ni, ph - bw, pw - ni, ph, edge_rgba);
        blend_px_rect_over_opaque(dst, pw, ph, 0, ni, bw, ph - ni, edge_rgba);
        blend_px_rect_over_opaque(dst, pw, ph, pw - bw, ni, pw, ph - ni, edge_rgba);

        let uw = pw as usize;
        let uh = ph as usize;
        for cy in 0..n {
            for cx in 0..n {
                let ci = cy * c.size + cx;
                let bp = c.border_px[ci];
                let ca = c.clip_alpha[ci] as u32;
                for (px, py) in [
                    (cx, cy),
                    (uw - 1 - cx, cy),
                    (cx, uh - 1 - cy),
                    (uw - 1 - cx, uh - 1 - cy),
                ] {
                    let idx = (py * uw + px) * 4;
                    if bp[3] > 0 {
                        let ba = bp[3] as u32;
                        let inv = 255 - ba;
                        dst[idx] = (bp[0] as u32 + ((dst[idx] as u32 * inv + 128) >> 8)) as u8;
                        dst[idx + 1] =
                            (bp[1] as u32 + ((dst[idx + 1] as u32 * inv + 128) >> 8)) as u8;
                        dst[idx + 2] =
                            (bp[2] as u32 + ((dst[idx + 2] as u32 * inv + 128) >> 8)) as u8;
                        dst[idx + 3] =
                            (ba + ((dst[idx + 3] as u32 * inv + 128) >> 8)).min(255) as u8;
                    }
                    if ca < 255 {
                        dst[idx] = ((dst[idx] as u32 * ca + 128) >> 8) as u8;
                        dst[idx + 1] = ((dst[idx + 1] as u32 * ca + 128) >> 8) as u8;
                        dst[idx + 2] = ((dst[idx + 2] as u32 * ca + 128) >> 8) as u8;
                        dst[idx + 3] = ((dst[idx + 3] as u32 * ca + 128) >> 8) as u8;
                    }
                }
            }
        }
    }
}
