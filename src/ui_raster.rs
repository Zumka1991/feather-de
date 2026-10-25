//! Axis-aligned UI paints. Tiny-skia still rasterizes the AA coverage; spans use
//! integer premultiplied source-over instead of a general RGBA shader pipeline.
use tiny_skia::{Color, FillRule, Mask, Path, PixmapMut, PixmapRef, Transform};

struct Coverage {
    mask: Mask,
    mid: std::ops::Range<usize>,
    x: usize,
    y: usize,
    w: usize,
    total_h: usize,
}
impl Coverage {
    #[inline]
    fn row(&self, row: usize) -> &[u8] {
        let r = if self.mid.contains(&row) {
            self.mid.start
        } else {
            row
        };
        &self.mask.data()[r * self.w..(r + 1) * self.w]
    }
}
fn coverage(dst: &PixmapMut<'_>, path: &Path, scale: f32) -> Option<Coverage> {
    let bounds = path.bounds();
    let x = (bounds.left() * scale).floor().max(0.0) as u32;
    let y = (bounds.top() * scale).floor().max(0.0) as u32;
    let right = (bounds.right() * scale)
        .ceil()
        .clamp(0.0, dst.width() as f32) as u32;
    let bottom = (bounds.bottom() * scale)
        .ceil()
        .clamp(0.0, dst.height() as f32) as u32;
    if right <= x || bottom <= y {
        return None;
    }
    let w = right - x;
    let total_h = bottom - y;
    let r = path
        .points()
        .first()
        .map(|p| (p.x - bounds.left()).max(0.0))
        .unwrap_or(0.0);
    let top_end = ((bounds.top() + r) * scale)
        .ceil()
        .clamp(y as f32, bottom as f32) as u32
        + 2;
    let bot_start = (((bounds.bottom() - r) * scale)
        .floor()
        .clamp(y as f32, bottom as f32) as u32)
        .saturating_sub(2);
    let mid = if top_end > y && bot_start > top_end + 4 && bot_start < bottom {
        (top_end - y) as usize..(bot_start - y) as usize
    } else {
        0..0
    };
    let mut mask = Mask::new(w, total_h)?;
    mask.fill_path(
        path,
        FillRule::Winding,
        true,
        Transform::from_row(scale, 0.0, 0.0, scale, -(x as f32), -(y as f32)),
    );
    Some(Coverage {
        mask,
        mid,
        x: x as usize,
        y: y as usize,
        w: w as usize,
        total_h: total_h as usize,
    })
}

// Two 8-bit channels at once, with independent rounded division by 255.
fn mul_channels(pair: u32, a: u32) -> u32 {
    let t = pair * a + 0x0080_0080;
    ((t + ((t >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff
}
fn scale_pixel(pixel: u32, a: u32) -> u32 {
    mul_channels(pixel & 0x00ff_00ff, a) | (mul_channels((pixel >> 8) & 0x00ff_00ff, a) << 8)
}
fn over(dst: &mut [u8], source: u32) {
    let inverse_alpha = 255 - (source >> 24);
    let pixel = if inverse_alpha == 0 {
        source
    } else {
        source + scale_pixel(u32::from_le_bytes(dst.try_into().unwrap()), inverse_alpha)
    };
    dst.copy_from_slice(&pixel.to_le_bytes());
}

pub fn blit_premultiplied(dst: &mut PixmapMut<'_>, src: PixmapRef<'_>, dx: i32, dy: i32) {
    let dw = dst.width() as i32;
    let dh = dst.height() as i32;
    let sw = src.width() as i32;
    let sh = src.height() as i32;
    let x0 = dx.max(0);
    let y0 = dy.max(0);
    let x1 = (dx + sw).min(dw);
    let y1 = (dy + sh).min(dh);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let sx0 = (x0 - dx) as usize;
    let sy0 = (y0 - dy) as usize;
    let w = (x1 - x0) as usize;
    let h = (y1 - y0) as usize;
    let d_stride = dw as usize * 4;
    let s_stride = sw as usize * 4;
    let d_data = dst.data_mut();
    let s_data = src.data();
    for r in 0..h {
        let d_row = &mut d_data[(y0 as usize + r) * d_stride + x0 as usize * 4..][..w * 4];
        let s_row = &s_data[(sy0 + r) * s_stride + sx0 * 4..][..w * 4];
        for (dp, sp) in d_row.chunks_exact_mut(4).zip(s_row.chunks_exact(4)) {
            let a = sp[3];
            if a == 255 {
                dp.copy_from_slice(sp);
            } else if a != 0 {
                over(dp, u32::from_le_bytes(sp.try_into().unwrap()));
            }
        }
    }
}

pub fn solid(dst: &mut PixmapMut<'_>, path: &Path, scale: f32, color: Color) {
    if color.alpha() == 0.0 {
        return;
    }
    let Some(coverage) = coverage(dst, path, scale) else {
        return;
    };
    let c = color.premultiply().to_color_u8();
    let source = u32::from_le_bytes([c.red(), c.green(), c.blue(), c.alpha()]);
    let stride = dst.width() as usize * 4;
    let w = coverage.w;
    let mut cached_span: Option<(*const u8, std::ops::Range<usize>)> = None;
    for row_idx in 0..coverage.total_h {
        let mask = coverage.row(row_idx);
        let span = match &cached_span {
            Some((ptr, s)) if *ptr == mask.as_ptr() => s.clone(),
            _ => {
                let s = full_span(mask);
                cached_span = Some((mask.as_ptr(), s.clone()));
                s
            }
        };
        let start = (coverage.y + row_idx) * stride + coverage.x * 4;
        let row = &mut dst.data_mut()[start..start + w * 4];
        blend_span(&mut row[span.start * 4..span.end * 4], source);
        for range in [0..span.start, span.end..w] {
            for (pixel, &alpha) in row[range.start * 4..range.end * 4]
                .chunks_exact_mut(4)
                .zip(&mask[range])
            {
                if alpha != 0 {
                    over(
                        pixel,
                        if alpha == 255 {
                            source
                        } else {
                            scale_pixel(source, alpha as u32)
                        },
                    );
                }
            }
        }
    }
}

fn sample_axis(position: f32, size: u32) -> (usize, usize, u32) {
    let p = position - 0.5;
    let left = p.floor();
    let a = left.clamp(0.0, (size - 1) as f32) as usize;
    let b = (left + 1.0).clamp(0.0, (size - 1) as f32) as usize;
    (a, b, ((p - left) * 256.0).round() as u32)
}

/// Separable bilinear sampling: reuse the two horizontally interpolated source
/// rows across all destination rows that refer to them. Scratch is panel-sized,
/// not a full-resolution copy of the wallpaper, and is dropped after the paint.
pub fn frosted(
    dst: &mut PixmapMut<'_>,
    path: &Path,
    scale: f32,
    bg: PixmapRef<'_>,
    logical_per_pixel: (f32, f32),
    origin: (f32, f32),
    tint: Option<Color>,
) {
    let Some(coverage) = coverage(dst, path, scale) else {
        return;
    };
    let tint = tint.map_or(0, |c| {
        let c = c.premultiply().to_color_u8();
        u32::from_le_bytes([c.red(), c.green(), c.blue(), c.alpha()])
    });
    let w = coverage.w;
    let xs: Vec<_> = (0..w)
        .map(|x| {
            sample_axis(
                (((coverage.x + x) as f32 + 0.5) / scale + origin.0) / logical_per_pixel.0,
                bg.width(),
            )
        })
        .collect();
    let mut top = vec![[0u16; 4]; w];
    let mut bottom = vec![[0u16; 4]; w];
    let mut rows = (usize::MAX, usize::MAX);
    let interpolate = |out: &mut [[u16; 4]], row: usize| {
        let offset = row * bg.width() as usize * 4;
        for (pixel, &(a, b, fraction)) in out.iter_mut().zip(&xs) {
            let (a, b) = (offset + a * 4, offset + b * 4);
            for c in 0..4 {
                pixel[c] = (bg.data()[a + c] as u32 * (256 - fraction)
                    + bg.data()[b + c] as u32 * fraction) as u16;
            }
        }
    };
    let stride = dst.width() as usize * 4;
    let mut cached_span: Option<(*const u8, std::ops::Range<usize>)> = None;
    for y in 0..coverage.total_h {
        let mask = coverage.row(y);
        let span = match &cached_span {
            Some((ptr, s)) if *ptr == mask.as_ptr() => s.clone(),
            _ => {
                let s = full_span(mask);
                cached_span = Some((mask.as_ptr(), s.clone()));
                s
            }
        };
        let (a, b, fraction) = sample_axis(
            (((coverage.y + y) as f32 + 0.5) / scale + origin.1) / logical_per_pixel.1,
            bg.height(),
        );
        if rows != (a, b) {
            if rows.1 == a {
                std::mem::swap(&mut top, &mut bottom);
            } else {
                interpolate(&mut top, a);
            }
            interpolate(&mut bottom, b);
            rows = (a, b);
        }
        let start = (coverage.y + y) * stride + coverage.x * 4;
        let row = &mut dst.data_mut()[start..start + w * 4];
        sampled_span(
            &mut row[span.start * 4..span.end * 4],
            &top[span.clone()],
            &bottom[span.clone()],
            fraction,
            tint,
        );
        for range in [0..span.start, span.end..w] {
            for (i, (pixel, &alpha)) in row[range.start * 4..range.end * 4]
                .chunks_exact_mut(4)
                .zip(&mask[range.clone()])
                .enumerate()
            {
                if alpha == 0 {
                    continue;
                }
                let i = range.start + i;
                let source = bilinear_pixel(top[i], bottom[i], fraction);
                over(
                    pixel,
                    if alpha == 255 {
                        source
                    } else {
                        scale_pixel(source, alpha as u32)
                    },
                );
                if tint != 0 {
                    over(pixel, scale_pixel(tint, alpha as u32));
                }
            }
        }
    }
}

// The first uninterrupted fully covered run. Other runs/holes still take the
// masked path, so this optimization never assumes that a future shape is convex.
fn full_span(mask: &[u8]) -> std::ops::Range<usize> {
    let start = mask.iter().position(|a| *a == 255).unwrap_or(mask.len());
    let end = mask[start..]
        .iter()
        .position(|a| *a != 255)
        .map_or(mask.len(), |n| start + n);
    start..end
}
fn bilinear_pixel(top: [u16; 4], bottom: [u16; 4], fraction: u32) -> u32 {
    u32::from_le_bytes(std::array::from_fn(|c| {
        ((top[c] as u32 * (256 - fraction) + bottom[c] as u32 * fraction + 32768) >> 16) as u8
    }))
}
fn blend_span(dst: &mut [u8], source: u32) {
    #[cfg(target_arch = "x86_64")]
    {
        simd::blend_span(dst, source);
    }
    #[cfg(not(target_arch = "x86_64"))]
    for pixel in dst.chunks_exact_mut(4) {
        over(pixel, source);
    }
}
fn sampled_span(dst: &mut [u8], top: &[[u16; 4]], bottom: &[[u16; 4]], fraction: u32, tint: u32) {
    #[cfg(target_arch = "x86_64")]
    {
        simd::sampled_span(dst, top, bottom, fraction, tint);
    }
    #[cfg(not(target_arch = "x86_64"))]
    for ((pixel, top), bottom) in dst.chunks_exact_mut(4).zip(top).zip(bottom) {
        over(pixel, bilinear_pixel(*top, *bottom, fraction));
        if tint != 0 {
            over(pixel, tint);
        }
    }
}

// SSE2 is part of the x86_64 ABI. Unaligned loads/stores only access complete
// chunks of the borrowed slices; tails use the same scalar source-over as edges.
#[cfg(target_arch = "x86_64")]
mod simd {
    use super::*;
    use std::arch::x86_64::*;

    pub fn blend_span(dst: &mut [u8], source: u32) {
        let mut chunks = dst.chunks_exact_mut(16);
        // SAFETY: SSE2 is mandatory on this target, and each chunk has 16 bytes.
        unsafe {
            let src = _mm_set1_epi32(source as i32);
            let inv = _mm_set1_epi16((255 - (source >> 24)) as i16);
            let zero = _mm_setzero_si128();
            let bias = _mm_set1_epi16(128);
            for chunk in &mut chunks {
                let result = if source >> 24 == 255 {
                    src
                } else {
                    let d = _mm_loadu_si128(chunk.as_ptr().cast());
                    let reduce = |v| {
                        let v = _mm_add_epi16(_mm_mullo_epi16(v, inv), bias);
                        _mm_srli_epi16::<8>(_mm_add_epi16(v, _mm_srli_epi16::<8>(v)))
                    };
                    _mm_add_epi8(
                        src,
                        _mm_packus_epi16(
                            reduce(_mm_unpacklo_epi8(d, zero)),
                            reduce(_mm_unpackhi_epi8(d, zero)),
                        ),
                    )
                };
                _mm_storeu_si128(chunk.as_mut_ptr().cast(), result);
            }
        }
        for pixel in chunks.into_remainder().chunks_exact_mut(4) {
            over(pixel, source);
        }
    }

    pub fn sampled_span(
        dst: &mut [u8],
        top: &[[u16; 4]],
        bottom: &[[u16; 4]],
        fraction: u32,
        tint: u32,
    ) {
        debug_assert_eq!(dst.len(), top.len() * 4);
        debug_assert_eq!(top.len(), bottom.len());
        let mut x = 0;
        // SAFETY: the loop checks four pixels (32 source bytes, 16 destination
        // bytes) before each load/store; all pointers stay within their slices.
        unsafe {
            let upper_weight = _mm_set1_epi16(((256 - fraction) << 8) as i16);
            let lower_weight = _mm_set1_epi16((fraction << 8) as i16);
            let bias = _mm_set1_epi16(128);
            let alpha = _mm_set1_epi32(0xff00_0000u32 as i32);
            while x + 4 <= top.len() {
                let interpolate = |offset: usize| {
                    let upper = _mm_loadu_si128(top.as_ptr().add(offset).cast());
                    let lower = _mm_loadu_si128(bottom.as_ptr().add(offset).cast());
                    let v = match fraction {
                        0 => upper,
                        256 => lower,
                        _ => _mm_add_epi16(
                            _mm_mulhi_epu16(upper, upper_weight),
                            _mm_mulhi_epu16(lower, lower_weight),
                        ),
                    };
                    _mm_srli_epi16::<8>(_mm_add_epi16(v, bias))
                };
                let source = _mm_packus_epi16(interpolate(x), interpolate(x + 2));
                let opaque =
                    _mm_movemask_epi8(_mm_cmpeq_epi32(_mm_and_si128(source, alpha), alpha))
                        == 0xffff;
                let original = source;
                let source = if tint == 0 || !opaque {
                    source
                } else {
                    let inv = _mm_set1_epi16((255 - (tint >> 24)) as i16);
                    let reduce = |v| {
                        let v = _mm_add_epi16(_mm_mullo_epi16(v, inv), bias);
                        _mm_srli_epi16::<8>(_mm_add_epi16(v, _mm_srli_epi16::<8>(v)))
                    };
                    let zero = _mm_setzero_si128();
                    _mm_add_epi8(
                        _mm_set1_epi32(tint as i32),
                        _mm_packus_epi16(
                            reduce(_mm_unpacklo_epi8(source, zero)),
                            reduce(_mm_unpackhi_epi8(source, zero)),
                        ),
                    )
                };
                if opaque {
                    _mm_storeu_si128(dst.as_mut_ptr().add(x * 4).cast(), source);
                } else {
                    // Wallpaper is opaque. Keep the general premultiplied-alpha
                    // behavior for other callers without another SIMD blend path.
                    let mut pixels = [0u8; 16];
                    _mm_storeu_si128(pixels.as_mut_ptr().cast(), original);
                    for (i, p) in pixels.chunks_exact(4).enumerate() {
                        over(
                            &mut dst[(x + i) * 4..(x + i + 1) * 4],
                            u32::from_le_bytes(p.try_into().unwrap()),
                        );
                        if tint != 0 {
                            over(&mut dst[(x + i) * 4..(x + i + 1) * 4], tint);
                        }
                    }
                }
                x += 4;
            }
        }
        for i in x..top.len() {
            over(
                &mut dst[i * 4..i * 4 + 4],
                bilinear_pixel(top[i], bottom[i], fraction),
            );
            if tint != 0 {
                over(&mut dst[i * 4..i * 4 + 4], tint);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::{Paint, Pattern, Pixmap, SpreadMode};

    fn destination(w: u32, h: u32) -> Pixmap {
        let mut p = Pixmap::new(w, h).unwrap();
        p.fill(Color::from_rgba8(73, 131, 202, 143));
        p
    }
    fn compare(a: &Pixmap, b: &Pixmap, tolerance: u8) {
        let worst = a
            .data()
            .iter()
            .zip(b.data())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(
            worst <= tolerance,
            "maximum channel difference {worst} > {tolerance}"
        );
    }

    #[test]
    fn solid_paints_match_the_previous_rgba_pipeline_at_clipped_and_fractional_edges() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            for (x, y, radius) in [(3.25, 5.5, 12.0), (-8.5, -3.25, 20.0), (40.0, 44.0, 0.0)] {
                let path = crate::theme::rounded_rect_path(x, y, 63.5, 51.25, radius).unwrap();
                for alpha in [0, 20, 140, 245, 255] {
                    let color = Color::from_rgba8(53, 172, 231, alpha);
                    let mut expected = destination(170, 140);
                    let mut actual = expected.clone();
                    let mut paint = Paint::default();
                    paint.set_color(color);
                    paint.anti_alias = true;
                    expected.fill_path(
                        &path,
                        &paint,
                        FillRule::Winding,
                        Transform::from_scale(scale, scale),
                        None,
                    );
                    solid(&mut actual.as_mut(), &path, scale, color);
                    compare(&actual, &expected, 2);
                }
            }
        }
    }

    #[test]
    fn separable_frosted_sampling_preserves_phase_clamping_and_premultiplied_alpha() {
        for opaque in [true, false] {
            let mut source = Pixmap::new(17, 13).unwrap();
            for (i, p) in source.pixels_mut().iter_mut().enumerate() {
                *p = Color::from_rgba8(
                    (i * 31) as u8,
                    (i * 71) as u8,
                    (i * 17) as u8,
                    if opaque { 255 } else { (i * 13) as u8 },
                )
                .premultiply()
                .to_color_u8();
            }
            for scale in [1.0, 1.25, 1.5, 2.0] {
                for origin in [(0.0, 0.0), (-12.25, 6.5), (33.75, 41.0)] {
                    let path =
                        crate::theme::rounded_rect_path(-2.25, 3.5, 63.5, 51.25, 12.0).unwrap();
                    let mut expected = destination(170, 140);
                    let mut actual = expected.clone();
                    let paint = Paint {
                        shader: Pattern::new(
                            source.as_ref(),
                            SpreadMode::Pad,
                            tiny_skia::FilterQuality::Bilinear,
                            1.0,
                            Transform::from_row(7.5, 0.0, 0.0, 8.25, -origin.0, -origin.1),
                        ),
                        anti_alias: true,
                        ..Default::default()
                    };
                    expected.fill_path(
                        &path,
                        &paint,
                        FillRule::Winding,
                        Transform::from_scale(scale, scale),
                        None,
                    );
                    frosted(
                        &mut actual.as_mut(),
                        &path,
                        scale,
                        source.as_ref(),
                        (7.5, 8.25),
                        origin,
                        None,
                    );
                    compare(&actual, &expected, 2);
                }
            }
        }
    }
}

#[cfg(test)]
mod panel_tests {
    use super::*;
    use tiny_skia::{Paint, Pattern, Pixmap, SpreadMode};

    #[test]
    fn fused_panel_keeps_two_layer_coverage_at_antialiased_edges() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let mut bg = Pixmap::new(17, 13).unwrap();
            for (i, p) in bg.pixels_mut().iter_mut().enumerate() {
                *p = Color::from_rgba8(
                    (i * 31) as u8,
                    (i * 71) as u8,
                    (i * 17) as u8,
                    if i % 3 == 0 { 140 } else { 255 },
                )
                .premultiply()
                .to_color_u8();
            }
            for alpha in [20, 140, 245, 255] {
                let tint = Color::from_rgba8(223, 240, 249, alpha);
                let path = crate::theme::rounded_rect_path(-2.25, 3.5, 63.5, 51.25, 12.0).unwrap();
                let mut expected = Pixmap::new(170, 140).unwrap();
                expected.fill(Color::from_rgba8(73, 131, 202, 143));
                let mut actual = expected.clone();
                let transform = Transform::from_scale(scale, scale);
                let paint = Paint {
                    shader: Pattern::new(
                        bg.as_ref(),
                        SpreadMode::Pad,
                        tiny_skia::FilterQuality::Bilinear,
                        1.0,
                        Transform::from_row(7.5, 0.0, 0.0, 8.25, -33.75, -41.0),
                    ),
                    anti_alias: true,
                    ..Default::default()
                };
                expected.fill_path(&path, &paint, FillRule::Winding, transform, None);
                let mut paint = Paint::default();
                paint.set_color(tint);
                paint.anti_alias = true;
                expected.fill_path(&path, &paint, FillRule::Winding, transform, None);
                frosted(
                    &mut actual.as_mut(),
                    &path,
                    scale,
                    bg.as_ref(),
                    (7.5, 8.25),
                    (33.75, 41.0),
                    Some(tint),
                );
                let worst = actual
                    .data()
                    .iter()
                    .zip(expected.data())
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                assert!(
                    worst <= 3,
                    "channel difference {worst}, scale {scale}, alpha {alpha}"
                );
            }
        }
    }
}
