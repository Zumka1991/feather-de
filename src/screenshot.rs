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
use std::sync::{
    atomic::{AtomicU8, Ordering},
    RwLock,
};

/// What the compositor itself offers on the clipboard: (MIME type, bytes) pairs
pub type ClipboardData = Arc<Vec<(String, Vec<u8>)>>;
pub const PNG_MIME: &str = "image/png";

/// Image file format used when saving screenshots to disk
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Png,
    Jpeg,
    Webp,
}

impl Format {
    pub const ALL: [Self; 3] = [Self::Png, Self::Jpeg, Self::Webp];

    pub fn id(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Webp => "WebP",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Jpeg,
            2 => Self::Webp,
            _ => Self::Png,
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Png => 0,
            Self::Jpeg => 1,
            Self::Webp => 2,
        }
    }
}

static SCREENSHOT_FORMAT: AtomicU8 = AtomicU8::new(0);
static SCREENSHOT_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

pub fn format() -> Format {
    Format::from_u8(SCREENSHOT_FORMAT.load(Ordering::Relaxed))
}

pub fn set_format(fmt: Format) {
    SCREENSHOT_FORMAT.store(fmt.as_u8(), Ordering::Relaxed);
}

pub fn custom_dir() -> Option<PathBuf> {
    SCREENSHOT_DIR.read().ok().and_then(|g| g.clone())
}

pub fn set_custom_dir(dir: Option<PathBuf>) {
    if let Ok(mut g) = SCREENSHOT_DIR.write() {
        *g = dir.filter(|p| p.is_absolute());
    }
}

pub fn default_dir() -> Option<PathBuf> {
    Some(pictures_dir()?.join("Снимки экрана"))
}

pub fn dir_display_label(custom: Option<&std::path::Path>) -> String {
    let Some(path) = custom else {
        return "Изображения / Снимки экрана".into();
    };
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if path == home {
            return "~".into();
        }
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.to_string_lossy());
        }
    }
    path.to_string_lossy().into_owned()
}

pub fn encode_dir_id(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    if let Some(s) = path.to_str() {
        if s.starts_with('/')
            && !s.is_empty()
            && s.trim() == s
            && !s.chars().any(char::is_control)
        {
            return s.to_owned();
        }
    }
    format!(
        "hex:{}",
        path.as_os_str()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

pub fn parse_dir_id(id: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let id = id.trim();
    if id.is_empty() || id == "default" {
        return None;
    }
    if let Some(hex) = id.strip_prefix("hex:") {
        if hex.is_empty() || hex.len() > 32768 || hex.len() % 2 != 0 || !hex.is_ascii() {
            return None;
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        if bytes.contains(&0) {
            return None;
        }
        let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        return path.is_absolute().then_some(path);
    }
    if id.starts_with('/') && id.len() <= 16384 && !id.contains('\0') {
        Some(PathBuf::from(id))
    } else {
        None
    }
}

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
    pub clipboard: Vec<(String, Vec<u8>)>,
    pub saved: Option<PathBuf>,
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
        let elements = crate::render::build_scene(renderer, &self.shell, &self.windows, s, pointer, self.rounded_program.as_ref(), &self.animations,crate::monitors::Rect{x:0,y:0,w:self.shell.screen_w,h:self.shell.screen_h},&mut Vec::new());
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
        let fmt = format();
        let dir = custom_dir();
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
                    let dir = dir.clone();
                    let spawned = std::thread::Builder::new().name("feather-screenshot".into()).spawn(move || {
                        let t = std::time::Instant::now();
                        let finished = encode_and_save_with(pixels, w, h, false, radius, fmt, dir.as_deref());
                        if profile.is_some() {
                            eprintln!("[feather-profile] снимок: {}+файл {:.1} мс (поток)", fmt.label(), t.elapsed().as_secs_f64() * 1e3);
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
        self.play_effect(crate::sounds::Effect::Screenshot,false);
        self.set_clipboard(finished.clipboard);
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

pub fn encode_and_save_with(
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    flipped: bool,
    radius: f32,
    fmt: Format,
    custom_dir: Option<&std::path::Path>,
) -> Option<Finished> {
    let file_bytes = to_image(rgba.clone(), w, h, flipped, radius, fmt)?;
    let saved = save_to(&file_bytes, custom_dir, fmt);
    let clipboard = if fmt == Format::Png {
        vec![(PNG_MIME.into(), file_bytes)]
    } else if let Some(png) = to_png(rgba, w, h, flipped, radius) {
        vec![(fmt.mime().into(), file_bytes), (PNG_MIME.into(), png)]
    } else {
        vec![(fmt.mime().into(), file_bytes)]
    };
    Some(Finished { clipboard, saved })
}

fn prepare_rgba(mut rgba: Vec<u8>, w: u32, h: u32, flipped: bool, radius: f32) -> Option<Vec<u8>> {
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
    }
    Some(rgba)
}

pub fn to_image(rgba: Vec<u8>, w: u32, h: u32, flipped: bool, radius: f32, fmt: Format) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let rgba = prepare_rgba(rgba, w, h, flipped, radius)?;
    match fmt {
        Format::Png => {
            if radius > 0.0 {
                encode(&rgba, w, h, png::ColorType::Rgba)
            } else {
                // The composited frame is opaque: RGB is smaller and faster to encode
                let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
                encode(&rgb, w, h, png::ColorType::Rgb)
            }
        }
        Format::Jpeg => {
            // JPEG has no alpha channel: blend rounded corners smoothly against dark neutral
            let rgb: Vec<u8> = if radius > 0.0 {
                rgba.chunks_exact(4)
                    .flat_map(|p| {
                        let a = p[3] as u16;
                        [
                            ((p[0] as u16 * a) / 255) as u8,
                            ((p[1] as u16 * a) / 255) as u8,
                            ((p[2] as u16 * a) / 255) as u8,
                        ]
                    })
                    .collect()
            } else {
                rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()
            };
            let mut out = Vec::with_capacity(rgb.len() / 4);
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
            encoder
                .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)
                .ok()?;
            Some(out)
        }
        Format::Webp => {
            let mut out = Vec::with_capacity(rgba.len() / 3);
            let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
            if radius > 0.0 {
                encoder
                    .write_image(&rgba, w, h, image::ExtendedColorType::Rgba8)
                    .ok()?;
            } else {
                let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
                encoder
                    .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)
                    .ok()?;
            }
            Some(out)
        }
    }
}

fn to_png(rgba: Vec<u8>, w: u32, h: u32, flipped: bool, radius: f32) -> Option<Vec<u8>> {
    to_image(rgba, w, h, flipped, radius, Format::Png)
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

/// `<dir>/Снимок экрана YYYY-MM-DD в HH.MM.SS.<ext>`
pub fn save_to(bytes: &[u8], custom_dir: Option<&std::path::Path>, fmt: Format) -> Option<PathBuf> {
    let dir = custom_dir.map(PathBuf::from).or_else(default_dir)?;
    std::fs::create_dir_all(&dir).ok()?;
    let ext = fmt.extension();
    let stamp = chrono::Local::now().format(feather_ui::i18n::tr_static("%Y-%m-%d в %H.%M.%S"));
    let name = feather_ui::i18n::tr_static("Снимок экрана");
    let mut path = dir.join(format!("{name} {stamp}.{ext}"));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{name} {stamp} ({n}).{ext}"));
    }
    std::fs::write(&path, bytes).ok()?;
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

    #[test]
    fn formats_encode_save_to_custom_directory_and_roundtrip_paths() {
        let dir = std::env::temp_dir().join(format!("feather-screenshot-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rgba = vec![180u8; 24 * 16 * 4];
        for fmt in Format::ALL {
            assert_eq!(Format::parse(fmt.id()), Some(fmt));
            assert_eq!(Format::from_u8(fmt.as_u8()), fmt);
            let finished = encode_and_save_with(rgba.clone(), 24, 16, false, 6.0, fmt, Some(&dir)).unwrap();
            let path = finished.saved.expect("saved screenshot path");
            assert!(path.starts_with(&dir));
            assert_eq!(path.extension().and_then(|e| e.to_str()), Some(fmt.extension()));
            let decoded = image::load_from_memory(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (24, 16));
            assert!(finished.clipboard.iter().any(|(m, _)| m == PNG_MIME));
            assert!(finished.clipboard.iter().any(|(m, _)| m == fmt.mime()));
        }
        // Second save in the same second gets a collision suffix "(2)"
        let a = save_to(b"png1", Some(&dir), Format::Png).unwrap();
        let b = save_to(b"png2", Some(&dir), Format::Png).unwrap();
        assert_ne!(a, b);
        for p in [
            PathBuf::from("/tmp/Мои снимки экрана"),
            PathBuf::from("/tmp/странный\nпуть=1"),
        ] {
            let encoded = encode_dir_id(&p);
            assert_eq!(parse_dir_id(&encoded), Some(p));
        }
        assert!(parse_dir_id("relative/path").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
