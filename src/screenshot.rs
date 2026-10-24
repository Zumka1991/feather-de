//! Screenshots: Print — whole screen, Shift+Print — drag a region (a click takes the window
//! under the pointer, Esc cancels), Alt+Print — active window.
//!
//! The scene is rendered once more into an offscreen texture without the cursor and the
//! selection UI. ReadPixels goes into a PBO and is mapped a frame later, so the event loop
//! does not wait for the GPU. PNG encoding and saving run on a short-lived thread; the image
//! becomes the clipboard selection served by the compositor itself (no wl-copy process).
//! Nothing here runs while idle.

use std::{io::Write, path::PathBuf, sync::Arc, time::Duration};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::{GlesRenderer, GlesTexture},
            Bind, ExportMem, Offscreen,
        },
    },
    input::pointer::{CursorIcon, CursorImageStatus},
    reexports::calloop::{
        channel,
        timer::{TimeoutAction, Timer},
    },
    utils::{Rectangle, Transform},
    wayland::selection::data_device::set_data_device_selection,
};

use crate::{render::FeatherElement, FeatherCompositor};

/// What the compositor itself offers on the clipboard: (MIME type, bytes) pairs
pub type ClipboardData = Arc<Vec<(String, Vec<u8>)>>;
pub const PNG_MIME: &str = "image/png";

/// Region selection in progress (Shift+Print)
pub struct Selection {
    /// Logical point where the left button went down
    anchor: Option<(f64, f64)>,
    /// 1×1 px buffers stretched over the dim areas and the outline; one per rectangle, as
    /// every element in a frame needs its own id for damage tracking
    pixels: [MemoryRenderBuffer; 8],
    /// "W × H" in output pixels next to the pointer; repainted only when the size changes
    label: Option<(MemoryRenderBuffer, (i32, i32), String)>,
}

impl Selection {
    fn new() -> Self {
        // Premultiplied RGBA: four edges, then four dim areas
        let pixel = |i: usize| {
            let rgba: [u8; 4] = if i < 4 { [230, 230, 230, 230] } else { [0, 0, 0, 72] };
            MemoryRenderBuffer::from_slice(&rgba, Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None)
        };
        Self { anchor: None, pixels: std::array::from_fn(pixel), label: None }
    }

    pub fn debug_anchor(&mut self, p: (f64, f64)) {
        self.anchor = Some(p);
    }

    fn update_label(&mut self, fonts: &mut crate::text::FontSystem, pointer: (f64, f64), scale: f64) {
        let Some(anchor) = self.anchor else { return };
        let (x0, y0, x1, y1) = phys_rect(anchor, pointer, scale);
        let text = format!("{} × {}", x1 - x0, y1 - y0);
        if self.label.as_ref().is_some_and(|(_, _, old)| *old == text) {
            return;
        }
        let s = crate::theme::ui_scale();
        let (w, h) = (fonts.measure_text(&text, 12.0, false) + 16.0, 22.0);
        let Some(mut pm) = tiny_skia::Pixmap::new((w * s).ceil() as u32, (h * s).ceil() as u32) else { return };
        crate::theme::fill_rounded_rect(&mut pm.as_mut(), 0.0, 0.0, w, h, h * 0.5, crate::theme::rgba(20, 20, 24, 190));
        fonts.draw_text(&mut pm.as_mut(), &text, 8.0, 4.0, 12.0, tiny_skia::Color::WHITE, false);
        let size = (pm.width() as i32, pm.height() as i32);
        // tiny-skia stores premultiplied RGBA bytes, which is Abgr8888 in DRM fourcc terms
        let buffer = MemoryRenderBuffer::from_slice(pm.data(), Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        self.label = Some((buffer, size, text));
    }
}

/// Selection corners in output pixels; the same rounding as the captured region
fn phys_rect(anchor: (f64, f64), pointer: (f64, f64), scale: f64) -> (i32, i32, i32, i32) {
    (
        (anchor.0.min(pointer.0) * scale).round() as i32,
        (anchor.1.min(pointer.1) * scale).round() as i32,
        (anchor.0.max(pointer.0) * scale).round() as i32,
        (anchor.1.max(pointer.1) * scale).round() as i32,
    )
}

pub enum Target {
    Screen,
    /// Logical rectangle; `radius` > 0 makes window corners transparent
    Area { x: f64, y: f64, w: f64, h: f64, radius: f32 },
}

pub struct Finished {
    png: Vec<u8>,
    saved: Option<PathBuf>,
}

impl FeatherCompositor {
    pub fn start_screenshot_selection(&mut self) {
        self.shell.close_menus();
        self.screenshot_select = Some(Selection::new());
        self.compositor_cursor = Some(CursorIcon::Crosshair);
        self.apply_cursor(CursorImageStatus::Named(CursorIcon::Crosshair));
        self.needs_redraw = true;
    }

    pub fn cancel_screenshot_selection(&mut self) {
        if self.screenshot_select.take().is_some() {
            self.compositor_cursor = None;
            self.apply_cursor(CursorImageStatus::default_named());
            self.pointer_focus_dirty = true;
            self.needs_redraw = true;
        }
    }

    /// Pointer motion while selecting; true when consumed
    pub fn screenshot_motion(&mut self) -> bool {
        let Some(sel) = self.screenshot_select.as_mut() else { return false };
        if sel.anchor.is_some() {
            let pointer = (self.pointer_location.x, self.pointer_location.y);
            sel.update_label(&mut self.fonts, pointer, self.scale_factor);
            self.needs_redraw = true;
        }
        true
    }

    /// Button while selecting; true when consumed (the client never sees it)
    pub fn screenshot_button(&mut self, left: bool, pressed: bool) -> bool {
        let Some(sel) = self.screenshot_select.as_mut() else { return false };
        let p = (self.pointer_location.x, self.pointer_location.y);
        if !left {
            if pressed { self.cancel_screenshot_selection(); }
            return true;
        }
        if pressed {
            sel.anchor = Some(p);
            sel.update_label(&mut self.fonts, p, self.scale_factor);
            self.needs_redraw = true;
            return true;
        }
        let Some(anchor) = sel.anchor else { return true };
        let (x, y) = (anchor.0.min(p.0), anchor.1.min(p.1));
        let (w, h) = ((anchor.0 - p.0).abs(), (anchor.1 - p.1).abs());
        self.cancel_screenshot_selection();
        if w >= 4.0 && h >= 4.0 {
            self.take_screenshot(Target::Area { x, y, w, h, radius: 0.0 });
        } else if let Some(target) = self.window_target(|win| win.contains_point(p.0, p.1)) {
            self.take_screenshot(target);
        }
        true
    }

    /// Topmost visible window matching `pick`, with rounded corners where we draw them
    pub fn window_target(&self, pick: impl Fn(&crate::window::ManagedWindow) -> bool) -> Option<Target> {
        let win = self.windows.iter().rev().find(|w| w.is_visible() && pick(w))?;
        let r = win.frame_rect();
        let radius = if win.fullscreen || win.maximized { 0.0 } else { crate::theme::WINDOW_CORNER_RADIUS };
        Some(Target::Area { x: r.loc.x as f64, y: r.loc.y as f64, w: r.size.w as f64, h: r.size.h as f64, radius })
    }

    pub fn take_screenshot(&mut self, target: Target) {
        if self.lock.active {return;}
        let (sw, sh, s) = (self.phys_w, self.phys_h, self.scale_factor);
        let (region, radius) = match target {
            Target::Screen => (Rectangle::<i32, smithay::utils::Physical>::from_size((sw, sh).into()), 0.0),
            Target::Area { x, y, w, h, radius } => {
                let x0 = ((x * s).round() as i32).clamp(0, sw);
                let y0 = ((y * s).round() as i32).clamp(0, sh);
                let x1 = (((x + w) * s).round() as i32).clamp(0, sw);
                let y1 = (((y + h) * s).round() as i32).clamp(0, sh);
                (Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into()), radius * s as f32)
            }
        };
        if region.size.w <= 0 || region.size.h <= 0 {
            return;
        }
        let pointer = (self.pointer_location.x, self.pointer_location.y);
        let profile = self.frame_stats.is_some().then(std::time::Instant::now);
        let renderer = self.backend.renderer();
        let elements = crate::render::build_scene(renderer, &self.shell, &self.windows, s, pointer, self.rounded_program.as_ref(), &self.animations,crate::monitors::Rect{x:0,y:0,w:self.shell.screen_w,h:self.shell.screen_h});
        let mapping = (|| {
            let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (sw, sh).into()).ok()?;
            let mut fb = renderer.bind(&mut texture).ok()?;
            OutputDamageTracker::new((sw, sh), 1.0, Transform::Normal)
                .render_output(renderer, &mut fb, 0, &elements, crate::render::CLEAR_COLOR)
                .ok()?;
            // Smithay renders offscreen textures top-down: row 0 is the top of the screen,
            // so the region needs no flip (unlike reading back the window's back buffer).
            let rows = Rectangle::<i32, smithay::utils::Buffer>::new((region.loc.x, region.loc.y).into(), (region.size.w, region.size.h).into());
            renderer.copy_framebuffer(&fb, rows, Fourcc::Abgr8888).ok()
        })();
        drop(elements);
        let Some(mapping) = mapping else {
            self.shell.notify("Снимок экрана", "Не удалось прочитать изображение");
            return;
        };
        if let Some(t) = profile {
            eprintln!("[feather-profile] снимок: сцена+ReadPixels {:.2} мс (главный поток)", t.elapsed().as_secs_f64() * 1e3);
        }
        let tx = self.screenshot_tx.clone();
        let (w, h) = (region.size.w as u32, region.size.h as u32);
        // Map the PBO a frame later: by then the GPU has finished, nothing stalls the loop
        let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(16)), move |_, _, state| {
            let t = std::time::Instant::now();
            let pixels = state.backend.renderer().map_texture(&mapping).map(<[u8]>::to_vec);
            if state.frame_stats.is_some() {
                eprintln!("[feather-profile] снимок: чтение PBO {:.2} мс (главный поток)", t.elapsed().as_secs_f64() * 1e3);
            }
            match pixels {
                Ok(pixels) => {
                    let tx = tx.clone();
                    let spawned = std::thread::Builder::new().name("feather-screenshot".into()).spawn(move || {
                        let t = std::time::Instant::now();
                        let finished = encode_and_save(pixels, w, h, false, radius);
                        if profile.is_some() {
                            eprintln!("[feather-profile] снимок: PNG+файл {:.1} мс (поток)", t.elapsed().as_secs_f64() * 1e3);
                        }
                        let _ = tx.send(finished);
                    });
                    if spawned.is_err() { state.shell.notify("Снимок экрана", "Не удалось запустить сохранение"); }
                }
                Err(_) => state.shell.notify("Снимок экрана", "Не удалось прочитать изображение"),
            }
            state.needs_redraw = true;
            TimeoutAction::Drop
        });
    }

    pub fn install_screenshot(&mut self, finished: Option<Finished>) {
        if self.lock.active {return;}
        let Some(finished) = finished else {
            self.shell.notify("Снимок экрана", "Не удалось закодировать PNG");
            self.needs_redraw = true;
            return;
        };
        self.set_clipboard(vec![(PNG_MIME.into(), finished.png)]);
        let body = match finished.saved.as_ref().and_then(|p| p.file_name()) {
            Some(name) => format!("В буфере обмена и в файле {}", name.to_string_lossy()),
            None => "Скопирован в буфер обмена".into(),
        };
        self.shell.notify("Снимок экрана", body);
        self.needs_redraw = true;
    }
}

/// Dimmed screen with a clear, outlined selection, drawn above everything (never captured)
pub fn overlay(renderer: &mut GlesRenderer, sel: Option<&Selection>, pointer: (f64, f64), scale: f64, screen: (i32, i32)) -> Vec<FeatherElement> {
    let Some(sel) = sel else { return Vec::new() };
    let (sw, sh) = screen;
    // (buffer, location, stretched size); a 1×1 buffer is stretched with an explicit `src`,
    // otherwise Smithay samples outside the texture
    let mut parts: Vec<(&MemoryRenderBuffer, (i32, i32), Option<(i32, i32)>)> = Vec::with_capacity(9);
    let mut rect = |i: usize, x: i32, y: i32, w: i32, h: i32| {
        if w > 0 && h > 0 {
            parts.push((&sel.pixels[i], (x, y), Some((w, h))));
        }
    };
    match sel.anchor {
        None => rect(4, 0, 0, sw, sh),
        Some(anchor) => {
            let (x0, y0, x1, y1) = phys_rect(anchor, pointer, scale);
            let b = (scale.round() as i32).max(1);
            let tall = y1 - y0 + 2 * b;
            rect(0, x0, y0 - b, x1 - x0, b);
            rect(1, x0, y1, x1 - x0, b);
            rect(2, x0 - b, y0 - b, b, tall);
            rect(3, x1, y0 - b, b, tall);
            rect(4, 0, 0, sw, y0 - b);
            rect(5, 0, y1 + b, sw, sh - y1 - b);
            rect(6, 0, y0 - b, x0 - b, tall);
            rect(7, x1 + b, y0 - b, sw - x1 - b, tall);
            // Size label below-right of the pointer, kept on screen; topmost element
            if let Some((buffer, (lw, lh), _)) = sel.label.as_ref() {
                let gap = (12.0 * scale) as i32;
                let (px, py) = ((pointer.0 * scale) as i32, (pointer.1 * scale) as i32);
                let lx = if px + gap + lw <= sw { px + gap } else { px - gap - lw }.max(0);
                let ly = if py + gap + lh <= sh { py + gap } else { py - gap - lh }.max(0);
                parts.insert(0, (buffer, (lx, ly), None));
            }
        }
    }
    let src = Rectangle::new((0.0, 0.0).into(), (1.0, 1.0).into());
    parts
        .into_iter()
        .filter_map(|(buffer, (x, y), size)| {
            MemoryRenderBufferRenderElement::from_buffer(renderer, (x as f64, y as f64), buffer, None, size.map(|_| src), size.map(Into::into), Kind::Unspecified).ok()
        })
        .map(|e| crate::render::SceneElement::Memory(e).into())
        .collect()
}

pub fn channel_source() -> (channel::Sender<Option<Finished>>, channel::Channel<Option<Finished>>) {
    channel::channel()
}

/// A paste: write the requested format off the event loop (the reader may be slow)
pub fn serve(mime_type: &str, fd: std::os::fd::OwnedFd, data: &ClipboardData) {
    let Some(index) = data.iter().position(|(mime, _)| mime == mime_type) else { return };
    let data = data.clone();
    let _ = std::thread::Builder::new().name("feather-paste".into()).spawn(move || {
        let _ = std::fs::File::from(fd).write_all(&data[index].1);
    });
}

impl FeatherCompositor {
    /// Offers `entries` as the clipboard selection, served by the compositor itself
    pub fn set_clipboard(&mut self, entries: Vec<(String, Vec<u8>)>) {
        let mimes = entries.iter().map(|(m, _)| m.clone()).collect();
        set_data_device_selection(&self.display_handle, &self.seat, mimes, Arc::new(entries));
    }
}

fn encode_and_save(rgba: Vec<u8>, w: u32, h: u32, flipped: bool, radius: f32) -> Option<Finished> {
    let png = to_png(rgba, w, h, flipped, radius)?;
    let saved = save(&png);
    Some(Finished { png, saved })
}

fn to_png(mut rgba: Vec<u8>, w: u32, h: u32, flipped: bool, radius: f32) -> Option<Vec<u8>> {
    let stride = w as usize * 4;
    if w == 0 || h == 0 || rgba.len() < stride * h as usize {
        return None;
    }
    rgba.truncate(stride * h as usize);
    if flipped {
        let (mut top, mut bottom) = (0, h as usize - 1);
        while top < bottom {
            let (a, b) = rgba.split_at_mut(bottom * stride);
            a[top * stride..top * stride + stride].swap_with_slice(&mut b[..stride]);
            top += 1;
            bottom -= 1;
        }
    }
    if radius > 0.0 {
        round_corners(&mut rgba, w, h, radius);
        encode(&rgba, w, h, png::ColorType::Rgba)
    } else {
        // The composited frame is opaque: RGB is smaller and faster to encode
        let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        encode(&rgb, w, h, png::ColorType::Rgb)
    }
}

fn encode(data: &[u8], w: u32, h: u32, color: png::ColorType) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 3);
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.write_header().ok()?.write_image_data(data).ok()?;
    Some(out)
}

/// Transparent, antialiased window corners (the scene itself is opaque)
fn round_corners(rgba: &mut [u8], w: u32, h: u32, r: f32) {
    let r = r.min(w as f32 * 0.5).min(h as f32 * 0.5);
    let n = r.ceil() as u32;
    for y in 0..n.min(h) {
        for x in 0..n.min(w) {
            let d = ((r - x as f32 - 0.5).powi(2) + (r - y as f32 - 0.5).powi(2)).sqrt();
            let coverage = (r - d + 0.5).clamp(0.0, 1.0);
            if coverage >= 1.0 {
                continue;
            }
            for (cx, cy) in [(x, y), (w - 1 - x, y), (x, h - 1 - y), (w - 1 - x, h - 1 - y)] {
                let i = (cy * w + cx) as usize * 4;
                // Straight (unpremultiplied) alpha in PNG: only the alpha channel changes
                rgba[i + 3] = (coverage * 255.0).round() as u8;
            }
        }
    }
}

/// Pictures/Снимки экрана/Снимок экрана YYYY-MM-DD в HH.MM.SS.png
fn save(png: &[u8]) -> Option<PathBuf> {
    let dir = pictures_dir()?.join("Снимки экрана");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = chrono::Local::now().format("%Y-%m-%d в %H.%M.%S");
    let mut path = dir.join(format!("Снимок экрана {stamp}.png"));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("Снимок экрана {stamp} ({n}).png"));
    }
    std::fs::write(&path, png).ok()?;
    Some(path)
}

fn pictures_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let from_user_dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok().and_then(|text| {
        text.lines().find_map(|line| {
            let value = line.trim().strip_prefix("XDG_PICTURES_DIR=")?.trim().trim_matches('"');
            Some(match value.strip_prefix("$HOME") {
                Some(rest) => home.join(rest.trim_start_matches('/')),
                None => PathBuf::from(value),
            })
        })
    });
    Some(from_user_dirs.unwrap_or_else(|| home.join("Pictures")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png: &[u8]) -> (png::OutputInfo, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        (info, buf)
    }

    #[test]
    fn bottom_up_rows_become_an_upright_opaque_png() {
        let (w, h) = (8u32, 3u32);
        let mut rgba = Vec::new();
        // GL order: bottom row blue, then green, top row red
        for color in [[0u8, 0, 255, 255], [0, 255, 0, 255], [255, 0, 0, 255]] {
            for _ in 0..w { rgba.extend_from_slice(&color); }
        }
        let (info, rgb) = decode(&to_png(rgba, w, h, true, 0.0).unwrap());
        assert_eq!((info.width, info.height, info.color_type), (w, h, png::ColorType::Rgb));
        assert_eq!((&rgb[..3], &rgb[24..27], &rgb[48..51]), (&[255, 0, 0][..], &[0, 255, 0][..], &[0, 0, 255][..]));
        assert!(to_png(vec![0; 10], w, h, true, 0.0).is_none());
    }

    #[test]
    fn window_corners_are_transparent_and_antialiased() {
        let png = to_png(vec![200u8; 40 * 40 * 4], 40, 40, false, 12.0).unwrap();
        let (info, rgba) = decode(&png);
        assert_eq!(info.color_type, png::ColorType::Rgba);
        let alpha = |x: usize, y: usize| rgba[(y * 40 + x) * 4 + 3];
        assert_eq!([alpha(0, 0), alpha(39, 0), alpha(0, 39), alpha(39, 39)], [0; 4]);
        assert_eq!([alpha(20, 0), alpha(0, 20), alpha(20, 20)], [200; 3]);
        // Somewhere on the arc coverage is partial
        assert!((0..12).any(|i| (1..255).contains(&alpha(i, 12 - i)) || (1..255).contains(&alpha(i, 11 - i))));
    }
}
