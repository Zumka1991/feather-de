//! macOS Tahoe–style Screen Capture & Video Recording («Запись экрана», Super+Shift+5):
//!
//! - Floating Liquid Glass HUD bar at the bottom of the screen with 5 modes:
//!   1. Capture Entire Screen (Снимок всего экрана)
//!   2. Capture Selected Window (Снимок окна)
//!   3. Capture Selected Portion (Снимок области)
//!   4. Record Entire Screen (Запись всего экрана)
//!   5. Record Selected Portion (Запись выбранной области)
//! - Interactive region frame with 8 resize handles, interior move drag, exterior dim, and W×H badge.
//! - Quick options right on the bar: countdown timer (Off / 5 s / 10 s), microphone audio,
//!   cursor & click highlight visibility, and frame rate (30 / 60 FPS).
//! - Non-blocking PBO GPU readback during recording + bounded channel (capacity 2) to the
//!   `feather-recorder` background worker thread:
//!   - Primary encoder: `ffmpeg` streaming raw RGBA → H.264 `.mp4` (`yuv420p`, `ultrafast`, `+faststart`,
//!     plus optional PulseAudio/PipeWire microphone audio).
//!   - Pure-Rust fallback encoder: built-in zero-dependency RIFF `AVI` / `MJPG` container writer
//!     using `image::codecs::jpeg::JpegEncoder`, so video recording works out of the box even on
//!     minimal systems without `ffmpeg`.
//! - Nothing runs when idle (0 % CPU, 0 extra memory).

use std::{
    fs::File,
    io::{BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, SyncSender},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::{GlesMapping, GlesRenderer, GlesTexture},
            Bind, ExportMem, Offscreen,
        },
    },
    input::pointer::{CursorIcon, CursorImageStatus},
    reexports::calloop::{
        channel,
        timer::{TimeoutAction, Timer},
    },
    utils::{Physical, Rectangle, Transform},
};
use tiny_skia::{Color, PixmapMut};

use crate::{
    render::FeatherElement,
    shell::R,
    text::FontSystem,
    theme::{
        appearance, draw_glass_panel, fill_circle, fill_rounded_rect, palette, rgba,
        stroke_circle, stroke_line, stroke_rounded_rect, to_phys, ui_scale, Appearance,
        TOP_BAR_HEIGHT,
    },
    FeatherCompositor,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    PhotoScreen,
    PhotoWindow,
    PhotoArea,
    VideoScreen,
    VideoArea,
}

impl CaptureMode {
    pub const ALL: [Self; 5] = [
        Self::PhotoScreen,
        Self::PhotoWindow,
        Self::PhotoArea,
        Self::VideoScreen,
        Self::VideoArea,
    ];

    pub fn is_video(self) -> bool {
        matches!(self, Self::VideoScreen | Self::VideoArea)
    }

    pub fn uses_area(self) -> bool {
        matches!(self, Self::PhotoArea | Self::VideoArea)
    }

    pub fn label(self) -> String {
        feather_ui::i18n::tr(match self {
            Self::PhotoScreen => "Весь экран",
            Self::PhotoWindow => "Окно",
            Self::PhotoArea => "Область",
            Self::VideoScreen => "Запись экрана",
            Self::VideoArea => "Запись области",
        })
        .into_owned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTarget {
    Mode(CaptureMode),
    Timer,
    Mic,
    Cursor,
    Fps,
    Start,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionDrag {
    Move { offset_x: f64, offset_y: f64 },
    Resize { left: bool, right: bool, top: bool, bottom: bool },
    New { anchor_x: f64, anchor_y: f64 },
}

pub struct HudPanel {
    pub mode: CaptureMode,
    /// Logical selection rectangle `(x, y, w, h)` for `PhotoArea` / `VideoArea`
    pub region: (f64, f64, f64, f64),
    pub region_drag: Option<RegionDrag>,
    pub hovered: Option<HitTarget>,
    /// Countdown before capture/recording starts: `(seconds_setting, deadline)`
    pub countdown_end: Option<Instant>,
    pub countdown_left: u32,
    pub buffer: MemoryRenderBuffer,
    pub buffer_size: (i32, i32),
    pub bar_rect: R,
    pub dirty: bool,
    /// 1×1 px GPU buffers for exterior dim (4), outline edges (4), and 8 handles
    pub pixels: [MemoryRenderBuffer; 16],
    /// Cached `"W × H"` label for the selected region
    pub size_label: Option<(MemoryRenderBuffer, (i32, i32), String)>,
}

impl HudPanel {
    fn new(screen_w: i32, screen_h: i32, initial_mode: CaptureMode, region: (f64, f64, f64, f64)) -> Self {
        let bar_rect = Self::compute_bar_rect(screen_w, screen_h);
        let pw = to_phys(bar_rect.w.ceil() as i32 + 24).max(1);
        let ph = to_phys(bar_rect.h.ceil() as i32 + 24).max(1);
        let pixel = |i: usize| {
            let rgba: [u8; 4] = if i < 4 {
                // Crisp selection outline
                [245, 245, 250, 235]
            } else if i < 8 {
                // Exterior dim
                [0, 0, 0, 88]
            } else {
                // Resize handles
                [255, 255, 255, 250]
            };
            MemoryRenderBuffer::from_slice(&rgba, Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None)
        };
        Self {
            mode: initial_mode,
            region,
            region_drag: None,
            hovered: None,
            countdown_end: None,
            countdown_left: 0,
            buffer: MemoryRenderBuffer::new(Fourcc::Abgr8888, (pw, ph), 1, Transform::Normal, None),
            buffer_size: (pw, ph),
            bar_rect,
            dirty: true,
            pixels: std::array::from_fn(pixel),
            size_label: None,
        }
    }

    pub fn compute_bar_rect(screen_w: i32, screen_h: i32) -> R {
        let w = 684.0f32.min((screen_w as f32 - 24.0).max(420.0));
        let h = 56.0;
        let x = ((screen_w as f32 - w) * 0.5).round();
        let dock_margin = (crate::theme::dock_icon() + 44.0).max(88.0);
        let y = (screen_h as f32 - dock_margin - h - 14.0).max(TOP_BAR_HEIGHT as f32 + 24.0).round();
        R::new(x, y, w, h)
    }

    pub fn layout(&self) -> Vec<(R, HitTarget)> {
        let b = self.bar_rect;
        let mut out = Vec::with_capacity(11);
        let mut x = b.x + 12.0;
        let cy = b.y + (b.h - 38.0) * 0.5;
        for (i, mode) in CaptureMode::ALL.into_iter().enumerate() {
            if i == 3 {
                x += 12.0; // Divider between photo and video modes
            }
            out.push((R::new(x, cy, 38.0, 38.0), HitTarget::Mode(mode)));
            x += 42.0;
        }
        x += 12.0; // Divider before options
        for (w, target) in [
            (72.0, HitTarget::Timer),
            (86.0, HitTarget::Mic),
            (76.0, HitTarget::Cursor),
            (58.0, HitTarget::Fps),
        ] {
            out.push((R::new(x, cy + 3.0, w, 32.0), target));
            x += w + 6.0;
        }
        let close_w = 28.0;
        let close_x = b.x + b.w - close_w - 10.0;
        let start_w = (close_x - x - 8.0).clamp(84.0, 128.0);
        let start_x = close_x - start_w - 8.0;
        out.push((R::new(start_x, cy + 2.0, start_w, 34.0), HitTarget::Start));
        out.push((R::new(close_x, b.y + (b.h - close_w) * 0.5, close_w, close_w), HitTarget::Close));
        out
    }

    pub fn hit_at(&self, px: f32, py: f32) -> Option<HitTarget> {
        self.layout()
            .into_iter()
            .find(|(r, _)| r.contains(px, py))
            .map(|(_, t)| t)
    }

    /// Checks if `(px, py)` hits a resize handle/edge or the interior of `self.region`
    pub fn region_hit(&self, px: f64, py: f64) -> RegionDrag {
        let (rx, ry, rw, rh) = self.region;
        let band = 10.0;
        let near_l = (px - rx).abs() <= band && py >= ry - band && py <= ry + rh + band;
        let near_r = (px - (rx + rw)).abs() <= band && py >= ry - band && py <= ry + rh + band;
        let near_t = (py - ry).abs() <= band && px >= rx - band && px <= rx + rw + band;
        let near_b = (py - (ry + rh)).abs() <= band && px >= rx - band && px <= rx + rw + band;
        if near_l || near_r || near_t || near_b {
            RegionDrag::Resize {
                left: near_l,
                right: near_r,
                top: near_t,
                bottom: near_b,
            }
        } else if px >= rx && px <= rx + rw && py >= ry && py <= ry + rh {
            RegionDrag::Move {
                offset_x: px - rx,
                offset_y: py - ry,
            }
        } else {
            RegionDrag::New {
                anchor_x: px,
                anchor_y: py,
            }
        }
    }

    pub fn cursor_for(&self, px: f64, py: f64) -> CursorIcon {
        if self.bar_rect.contains(px as f32, py as f32) {
            return CursorIcon::Default;
        }
        if !self.mode.uses_area() {
            return if self.mode == CaptureMode::PhotoWindow {
                CursorIcon::Pointer
            } else {
                CursorIcon::Crosshair
            };
        }
        match self.region_hit(px, py) {
            RegionDrag::Resize { left, right, top, bottom } => match (left, right, top, bottom) {
                (true, _, true, _) => CursorIcon::NwResize,
                (_, true, true, _) => CursorIcon::NeResize,
                (true, _, _, true) => CursorIcon::SwResize,
                (_, true, _, true) => CursorIcon::SeResize,
                (true, ..) | (_, true, ..) => CursorIcon::EwResize,
                _ => CursorIcon::NsResize,
            },
            RegionDrag::Move { .. } => CursorIcon::Move,
            RegionDrag::New { .. } => CursorIcon::Crosshair,
        }
    }

    fn update_size_label(&mut self, fonts: &mut FontSystem, scale: f64) {
        if !self.mode.uses_area() {
            self.size_label = None;
            return;
        }
        let (x, y, w, h) = self.region;
        let pw = ((w * scale).round() as i32).max(1) & !1;
        let ph = ((h * scale).round() as i32).max(1) & !1;
        let _ = (x, y);
        let text = format!("{pw} × {ph}");
        if self.size_label.as_ref().is_some_and(|(_, _, old)| *old == text) {
            return;
        }
        let s = ui_scale();
        let (lw, lh) = (fonts.measure_text(&text, 12.0, true) + 16.0, 22.0);
        let Some(mut pm) = tiny_skia::Pixmap::new((lw * s).ceil() as u32, (lh * s).ceil() as u32) else {
            return;
        };
        fill_rounded_rect(
            &mut pm.as_mut(),
            0.0,
            0.0,
            lw,
            lh,
            lh * 0.5,
            rgba(22, 22, 26, 205),
        );
        fonts.draw_text(&mut pm.as_mut(), &text, 8.0, 4.0, 12.0, Color::WHITE, true);
        let size = (pm.width() as i32, pm.height() as i32);
        let buffer = MemoryRenderBuffer::from_slice(
            pm.data(),
            Fourcc::Abgr8888,
            size,
            1,
            Transform::Normal,
            None,
        );
        self.size_label = Some((buffer, size, text));
    }
}

pub enum WorkerMsg {
    Frame { rgba: Vec<u8>, pts_ms: u64 },
    Stop,
}

#[derive(Debug, Clone)]
pub struct FinishedRecording {
    pub saved: Option<PathBuf>,
    pub frames: u64,
    pub duration_secs: f32,
    pub used_ffmpeg: bool,
}

pub struct ActiveRecording {
    pub started_at: Instant,
    pub region: Rectangle<i32, Physical>,
    pub fps: u32,
    pub show_cursor: bool,
    pub tx: SyncSender<WorkerMsg>,
    pub offscreen: Option<(GlesTexture, (i32, i32))>,
    pub pending_pbo: Option<(GlesMapping, u64)>,
    pub last_shown_secs: u64,
    pub click_ring_buffer: MemoryRenderBuffer,
    pub click_ring_px: i32,
}

pub struct ScreenRecorderState {
    pub panel: Option<HudPanel>,
    pub recording: Option<ActiveRecording>,
    pub last_mode: CaptureMode,
    pub saved_region: Option<(f64, f64, f64, f64)>,
    pub timer_secs: u32,
    pub record_audio: bool,
    pub show_cursor: bool,
    pub fps: u32,
    pub done_tx: channel::Sender<FinishedRecording>,
}

impl ScreenRecorderState {
    pub fn new(done_tx: channel::Sender<FinishedRecording>) -> Self {
        Self {
            panel: None,
            recording: None,
            last_mode: CaptureMode::VideoScreen,
            saved_region: None,
            timer_secs: 0,
            record_audio: false,
            show_cursor: true,
            fps: 30,
            done_tx,
        }
    }

    pub fn is_open(&self) -> bool {
        self.panel.is_some()
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn elapsed_secs(&self) -> Option<u64> {
        self.recording.as_ref().map(|r| r.started_at.elapsed().as_secs())
    }
}

pub fn channel_source() -> (channel::Sender<FinishedRecording>, channel::Channel<FinishedRecording>) {
    channel::channel()
}

/// Default folder for screen recordings: `~/Видео/Записи экрана` or `$XDG_VIDEOS_DIR/Записи экрана`
pub fn default_videos_dir() -> Option<PathBuf> {
    if let Some(custom) = crate::screenshot::custom_dir() {
        return Some(custom);
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let from_user_dirs = std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.trim().strip_prefix("XDG_VIDEOS_DIR=")?.trim().trim_matches('"');
                Some(match value.strip_prefix("$HOME") {
                    Some(rest) => home.join(rest.trim_start_matches('/')),
                    None => PathBuf::from(value),
                })
            })
        });
    let base = from_user_dirs.unwrap_or_else(|| {
        let ru = home.join("Видео");
        if ru.is_dir() {
            ru
        } else {
            home.join("Videos")
        }
    });
    Some(base.join("Записи экрана"))
}

pub fn next_recording_path(dir: Option<&Path>, ext: &str) -> Option<PathBuf> {
    let dir = dir.map(PathBuf::from).or_else(default_videos_dir)?;
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = chrono::Local::now().format("%Y-%m-%d в %H.%M.%S");
    let mut path = dir.join(format!("Запись экрана {stamp}.{ext}"));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("Запись экрана {stamp} ({n}).{ext}"));
    }
    Some(path)
}

// ───────────────────────── Pure-Rust RIFF AVI / MJPEG Encoder ─────────────────────────

/// Zero-dependency pure-Rust RIFF AVI (`MJPG` fourcc) video writer.
/// Produces standard `.avi` files playable in mpv, VLC, ffplay, and browsers.
pub struct MjpegAviWriter {
    writer: BufWriter<File>,
    path: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    movi_data_pos: u64,
    frame_count: u32,
    index_entries: Vec<(u32, u32)>, // (offset from 'movi', size)
    last_jpeg: Vec<u8>,
}

impl MjpegAviWriter {
    pub fn create(path: PathBuf, width: u32, height: u32, fps: u32) -> std::io::Result<Self> {
        let file = File::create(&path)?;
        let mut writer = BufWriter::with_capacity(256 * 1024, file);
        let fps = fps.max(1);
        let usec_per_frame = 1_000_000 / fps;
        let max_bytes_per_sec = width * height * 3 * fps;

        // RIFF header (patched on finish)
        writer.write_all(b"RIFF")?;
        writer.write_all(&0u32.to_le_bytes())?;
        writer.write_all(b"AVI ")?;

        // LIST hdrl (size = 4 + (8+56) + (8 + 4 + (8+56) + (8+40)) = 192 bytes)
        writer.write_all(b"LIST")?;
        writer.write_all(&192u32.to_le_bytes())?;
        writer.write_all(b"hdrl")?;

        // avih (56 bytes)
        writer.write_all(b"avih")?;
        writer.write_all(&56u32.to_le_bytes())?;
        writer.write_all(&usec_per_frame.to_le_bytes())?; // dwMicroSecPerFrame
        writer.write_all(&max_bytes_per_sec.to_le_bytes())?; // dwMaxBytesPerSec
        writer.write_all(&0u32.to_le_bytes())?; // dwPaddingGranularity
        writer.write_all(&0x110u32.to_le_bytes())?; // dwFlags: AVIF_HASINDEX | AVIF_ISINTERLEAVED
        writer.write_all(&0u32.to_le_bytes())?; // dwTotalFrames (patched at offset 48)
        writer.write_all(&0u32.to_le_bytes())?; // dwInitialFrames
        writer.write_all(&1u32.to_le_bytes())?; // dwStreams
        writer.write_all(&(width * height * 3).to_le_bytes())?; // dwSuggestedBufferSize
        writer.write_all(&width.to_le_bytes())?; // dwWidth
        writer.write_all(&height.to_le_bytes())?; // dwHeight
        writer.write_all(&[0u8; 16])?; // dwReserved[4]

        // LIST strl (116 bytes)
        writer.write_all(b"LIST")?;
        writer.write_all(&116u32.to_le_bytes())?;
        writer.write_all(b"strl")?;

        // strh (56 bytes)
        writer.write_all(b"strh")?;
        writer.write_all(&56u32.to_le_bytes())?;
        writer.write_all(b"vids")?; // fccType
        writer.write_all(b"MJPG")?; // fccHandler
        writer.write_all(&0u32.to_le_bytes())?; // dwFlags
        writer.write_all(&0u16.to_le_bytes())?; // wPriority
        writer.write_all(&0u16.to_le_bytes())?; // wLanguage
        writer.write_all(&0u32.to_le_bytes())?; // dwInitialFrames
        writer.write_all(&1u32.to_le_bytes())?; // dwScale
        writer.write_all(&fps.to_le_bytes())?; // dwRate
        writer.write_all(&0u32.to_le_bytes())?; // dwStart
        writer.write_all(&0u32.to_le_bytes())?; // dwLength (patched at offset 140)
        writer.write_all(&(width * height * 3).to_le_bytes())?; // dwSuggestedBufferSize
        writer.write_all(&8500u32.to_le_bytes())?; // dwQuality
        writer.write_all(&0u32.to_le_bytes())?; // dwSampleSize
        writer.write_all(&0u16.to_le_bytes())?; // rcFrame.left
        writer.write_all(&0u16.to_le_bytes())?; // rcFrame.top
        writer.write_all(&(width.min(65535) as u16).to_le_bytes())?; // rcFrame.right
        writer.write_all(&(height.min(65535) as u16).to_le_bytes())?; // rcFrame.bottom

        // strf (BITMAPINFOHEADER, 40 bytes)
        writer.write_all(b"strf")?;
        writer.write_all(&40u32.to_le_bytes())?;
        writer.write_all(&40u32.to_le_bytes())?; // biSize
        writer.write_all(&(width as i32).to_le_bytes())?; // biWidth
        writer.write_all(&(height as i32).to_le_bytes())?; // biHeight
        writer.write_all(&1u16.to_le_bytes())?; // biPlanes
        writer.write_all(&24u16.to_le_bytes())?; // biBitCount
        writer.write_all(b"MJPG")?; // biCompression
        writer.write_all(&(width * height * 3).to_le_bytes())?; // biSizeImage
        writer.write_all(&0i32.to_le_bytes())?; // biXPelsPerMeter
        writer.write_all(&0i32.to_le_bytes())?; // biYPelsPerMeter
        writer.write_all(&0u32.to_le_bytes())?; // biClrUsed
        writer.write_all(&0u32.to_le_bytes())?; // biClrImportant

        // LIST movi (patched on finish)
        writer.write_all(b"LIST")?;
        writer.write_all(&0u32.to_le_bytes())?; // movi size at offset 216
        let movi_data_pos = writer.stream_position()?;
        writer.write_all(b"movi")?;

        Ok(Self {
            writer,
            path,
            width,
            height,
            fps,
            movi_data_pos,
            frame_count: 0,
            index_entries: Vec::with_capacity(1800),
            last_jpeg: Vec::new(),
        })
    }

    pub fn write_rgba_frame(&mut self, rgba: &[u8]) -> std::io::Result<()> {
        use image::ImageEncoder;
        let expected = (self.width as usize) * (self.height as usize) * 4;
        if rgba.len() < expected {
            return Ok(());
        }
        let rgb: Vec<u8> = rgba[..expected]
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let mut jpeg = Vec::with_capacity(rgb.len() / 6);
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85);
        encoder
            .write_image(&rgb, self.width, self.height, image::ExtendedColorType::Rgb8)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        self.write_encoded_jpeg(&jpeg)?;
        self.last_jpeg = jpeg;
        Ok(())
    }

    pub fn repeat_last_frame(&mut self) -> std::io::Result<()> {
        if !self.last_jpeg.is_empty() {
            let jpeg = self.last_jpeg.clone();
            self.write_encoded_jpeg(&jpeg)?;
        }
        Ok(())
    }

    fn write_encoded_jpeg(&mut self, jpeg: &[u8]) -> std::io::Result<()> {
        let chunk_pos = self.writer.stream_position()?;
        let rel_offset = (chunk_pos - self.movi_data_pos) as u32;
        let size = jpeg.len() as u32;
        self.writer.write_all(b"00dc")?;
        self.writer.write_all(&size.to_le_bytes())?;
        self.writer.write_all(jpeg)?;
        if size & 1 != 0 {
            self.writer.write_all(&[0u8])?;
        }
        self.index_entries.push((rel_offset, size));
        self.frame_count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<(PathBuf, u64)> {
        let movi_end = self.writer.stream_position()?;
        let movi_size = (movi_end - self.movi_data_pos) as u32;

        // Write idx1 chunk
        self.writer.write_all(b"idx1")?;
        let idx_size = (self.index_entries.len() as u32) * 16;
        self.writer.write_all(&idx_size.to_le_bytes())?;
        for (offset, size) in &self.index_entries {
            self.writer.write_all(b"00dc")?;
            self.writer.write_all(&0x10u32.to_le_bytes())?; // AVIIF_KEYFRAME
            self.writer.write_all(&offset.to_le_bytes())?;
            self.writer.write_all(&size.to_le_bytes())?;
        }
        let file_end = self.writer.stream_position()?;
        let riff_size = (file_end - 8) as u32;

        // Patch RIFF size (offset 4), avih dwTotalFrames (offset 48), strh dwLength (offset 140), movi size (offset 216)
        self.writer.seek(SeekFrom::Start(4))?;
        self.writer.write_all(&riff_size.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(48))?;
        self.writer.write_all(&self.frame_count.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(140))?;
        self.writer.write_all(&self.frame_count.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(216))?;
        self.writer.write_all(&movi_size.to_le_bytes())?;
        self.writer.flush()?;
        let _ = self.fps;
        Ok((self.path, self.frame_count as u64))
    }
}

// ───────────────────────── Background Video Encoder Worker ─────────────────────────

fn spawn_ffmpeg(path: &Path, width: u32, height: u32, fps: u32, with_audio: bool) -> Option<Child> {
    if std::env::var_os("FEATHER_RECORD_PURE_RUST").is_some() {
        return None;
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-s")
        .arg(format!("{width}x{height}"))
        .arg("-r")
        .arg(fps.to_string())
        .arg("-i")
        .arg("pipe:0");
    if with_audio {
        cmd.arg("-f").arg("pulse").arg("-i").arg("default");
    }
    cmd.arg("-vf")
        .arg("crop=trunc(iw/2)*2:trunc(ih/2)*2")
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("ultrafast")
        .arg("-crf")
        .arg("20")
        .arg("-pix_fmt")
        .arg("yuv420p");
    if with_audio {
        cmd.arg("-c:a").arg("aac").arg("-b:a").arg("128k").arg("-shortest");
    }
    cmd.arg("-movflags")
        .arg("+faststart")
        .arg("-y")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().ok()
}

pub fn run_recorder_worker(
    rx: mpsc::Receiver<WorkerMsg>,
    done_tx: channel::Sender<FinishedRecording>,
    width: u32,
    height: u32,
    fps: u32,
    record_audio: bool,
    custom_dir: Option<PathBuf>,
) {
    let started = Instant::now();
    let mp4_path = next_recording_path(custom_dir.as_deref(), "mp4");
    // Try ffmpeg with audio (if requested), then ffmpeg video-only, then pure-Rust AVI/MJPEG
    let mut ffmpeg_child = mp4_path.as_ref().and_then(|p| {
        if record_audio {
            spawn_ffmpeg(p, width, height, fps, true)
                .or_else(|| spawn_ffmpeg(p, width, height, fps, false))
        } else {
            spawn_ffmpeg(p, width, height, fps, false)
        }
    });

    enum Backend {
        Ffmpeg {
            child: Child,
            path: PathBuf,
            frames: u64,
            last_rgba: Vec<u8>,
        },
        PureRust(MjpegAviWriter),
        None,
    }

    let mut backend = if let (Some(child), Some(path)) = (ffmpeg_child.take(), mp4_path) {
        Backend::Ffmpeg {
            child,
            path,
            frames: 0,
            last_rgba: Vec::new(),
        }
    } else if let Some(avi_path) = next_recording_path(custom_dir.as_deref(), "avi") {
        match MjpegAviWriter::create(avi_path, width, height, fps) {
            Ok(w) => Backend::PureRust(w),
            Err(_) => Backend::None,
        }
    } else {
        Backend::None
    };

    let expected_bytes = (width as usize) * (height as usize) * 4;
    while let Ok(msg) = rx.recv() {
        match msg {
            WorkerMsg::Frame { mut rgba, pts_ms } => {
                if rgba.len() < expected_bytes {
                    continue;
                }
                rgba.truncate(expected_bytes);
                let target_frame_idx = ((pts_ms * fps as u64) / 1000).max(1);
                match &mut backend {
                    Backend::Ffmpeg {
                        child,
                        path,
                        frames,
                        last_rgba,
                    } => {
                        let mut ok = true;
                        if let Some(stdin) = child.stdin.as_mut() {
                            while *frames + 1 < target_frame_idx && !last_rgba.is_empty() && (*frames + 8 >= target_frame_idx) {
                                if stdin.write_all(last_rgba).is_err() {
                                    ok = false;
                                    break;
                                }
                                *frames += 1;
                            }
                            if ok && stdin.write_all(&rgba).is_ok() {
                                *frames += 1;
                            } else {
                                ok = false;
                            }
                        } else {
                            ok = false;
                        }
                        if ok {
                            *last_rgba = rgba;
                        } else {
                            // If ffmpeg died (e.g. broken audio source), transparently switch to pure-Rust AVI
                            let _ = child.kill();
                            let _ = child.wait();
                            let _ = std::fs::remove_file(&*path);
                            if let Some(avi_path) = next_recording_path(custom_dir.as_deref(), "avi") {
                                if let Ok(mut w) = MjpegAviWriter::create(avi_path, width, height, fps) {
                                    let _ = w.write_rgba_frame(&rgba);
                                    backend = Backend::PureRust(w);
                                } else {
                                    backend = Backend::None;
                                }
                            } else {
                                backend = Backend::None;
                            }
                        }
                    }
                    Backend::PureRust(writer) => {
                        while (writer.frame_count as u64) + 1 < target_frame_idx
                            && (writer.frame_count as u64) + 8 >= target_frame_idx
                        {
                            let _ = writer.repeat_last_frame();
                        }
                        let _ = writer.write_rgba_frame(&rgba);
                    }
                    Backend::None => {}
                }
            }
            WorkerMsg::Stop => break,
        }
    }

    let duration_secs = started.elapsed().as_secs_f32();
    let finished = match backend {
        Backend::Ffmpeg { mut child, path, frames, .. } => {
            drop(child.stdin.take());
            let status = child.wait().ok();
            let valid = status.is_some_and(|s| s.success())
                && std::fs::metadata(&path).is_ok_and(|m| m.len() > 0);
            FinishedRecording {
                saved: valid.then_some(path),
                frames,
                duration_secs,
                used_ffmpeg: true,
            }
        }
        Backend::PureRust(writer) => match writer.finish() {
            Ok((path, frames)) if frames > 0 => FinishedRecording {
                saved: Some(path),
                frames,
                duration_secs,
                used_ffmpeg: false,
            },
            _ => FinishedRecording {
                saved: None,
                frames: 0,
                duration_secs,
                used_ffmpeg: false,
            },
        },
        Backend::None => FinishedRecording {
            saved: None,
            frames: 0,
            duration_secs,
            used_ffmpeg: false,
        },
    };
    #[cfg(target_os = "linux")]
    unsafe {
        libc::malloc_trim(0);
    }
    let _ = done_tx.send(finished);
}

// ───────────────────────── HUD Rendering & Compositor Integration ─────────────────────────

fn draw_mode_icon(pm: &mut PixmapMut, mode: CaptureMode, cx: f32, cy: f32, col: Color, active: bool) {
    let record_red = if active { Color::WHITE } else { rgba(255, 69, 58, 255) };
    match mode {
        CaptureMode::PhotoScreen => {
            stroke_rounded_rect(pm, cx - 10.0, cy - 7.0, 20.0, 14.0, 2.5, 1.5, col);
            // Corner brackets inside
            stroke_line(pm, cx - 7.0, cy - 4.0, cx - 4.0, cy - 4.0, 1.3, col);
            stroke_line(pm, cx - 7.0, cy - 4.0, cx - 7.0, cy - 1.5, 1.3, col);
            stroke_line(pm, cx + 4.0, cy + 4.0, cx + 7.0, cy + 4.0, 1.3, col);
            stroke_line(pm, cx + 7.0, cy + 1.5, cx + 7.0, cy + 4.0, 1.3, col);
        }
        CaptureMode::PhotoWindow => {
            stroke_rounded_rect(pm, cx - 10.0, cy - 7.5, 20.0, 15.0, 2.5, 1.5, col);
            stroke_line(pm, cx - 9.5, cy - 3.0, cx + 9.5, cy - 3.0, 1.2, col);
            fill_circle(pm, cx - 6.5, cy - 5.2, 1.0, col);
            fill_circle(pm, cx - 3.8, cy - 5.2, 1.0, col);
        }
        CaptureMode::PhotoArea => {
            // Dashed selection frame
            for dx in [-8.0, -2.0, 4.0] {
                stroke_line(pm, cx + dx, cy - 7.0, cx + dx + 3.5, cy - 7.0, 1.5, col);
                stroke_line(pm, cx + dx, cy + 7.0, cx + dx + 3.5, cy + 7.0, 1.5, col);
            }
            for dy in [-5.0, 1.5] {
                stroke_line(pm, cx - 9.5, cy + dy, cx - 9.5, cy + dy + 3.5, 1.5, col);
                stroke_line(pm, cx + 9.5, cy + dy, cx + 9.5, cy + dy + 3.5, 1.5, col);
            }
            stroke_line(pm, cx - 3.0, cy, cx + 3.0, cy, 1.3, col);
            stroke_line(pm, cx, cy - 3.0, cx, cy + 3.0, 1.3, col);
        }
        CaptureMode::VideoScreen => {
            stroke_rounded_rect(pm, cx - 10.0, cy - 7.0, 20.0, 14.0, 2.5, 1.5, col);
            fill_circle(pm, cx, cy, 4.0, record_red);
        }
        CaptureMode::VideoArea => {
            for dx in [-8.0, -2.0, 4.0] {
                stroke_line(pm, cx + dx, cy - 7.0, cx + dx + 3.5, cy - 7.0, 1.5, col);
                stroke_line(pm, cx + dx, cy + 7.0, cx + dx + 3.5, cy + 7.0, 1.5, col);
            }
            for dy in [-5.0, 1.5] {
                stroke_line(pm, cx - 9.5, cy + dy, cx - 9.5, cy + dy + 3.5, 1.5, col);
                stroke_line(pm, cx + 9.5, cy + dy, cx + 9.5, cy + dy + 3.5, 1.5, col);
            }
            fill_circle(pm, cx, cy, 4.0, record_red);
        }
    }
}

impl FeatherCompositor {
    /// Opens the macOS Tahoe–style Screen Capture & Recording HUD, or stops an active recording.
    pub fn toggle_screen_recorder(&mut self) {
        if self.lock.active {
            return;
        }
        if self.screencast.is_recording() {
            self.stop_screen_recording();
            return;
        }
        if self.screencast.panel.is_some() {
            self.close_screen_recorder_hud();
            return;
        }
        self.cancel_screenshot_selection();
        self.shell.close_menus();
        self.shell.launchpad = None;
        let (sw, sh) = (self.shell.screen_w, self.shell.screen_h);
        let default_region = self.screencast.saved_region.unwrap_or_else(|| {
            let rw = (sw as f64 * 0.56).round().max(240.0);
            let rh = (sh as f64 * 0.56).round().max(180.0);
            let rx = ((sw as f64 - rw) * 0.5).round();
            let ry = ((sh as f64 - rh) * 0.45).round().max(TOP_BAR_HEIGHT as f64 + 12.0);
            (rx, ry, rw, rh)
        });
        let mut panel = HudPanel::new(sw, sh, self.screencast.last_mode, default_region);
        panel.update_size_label(&mut self.fonts, self.scale_factor);
        let icon = panel.cursor_for(self.pointer_location.x, self.pointer_location.y);
        self. screencast.panel = Some(panel);
        self.compositor_cursor = Some(icon);
        self.apply_cursor(CursorImageStatus::Named(icon));
        self.needs_redraw = true;
    }

    pub fn close_screen_recorder_hud(&mut self) {
        if let Some(panel) = self.screencast.panel.take() {
            self.screencast.last_mode = panel.mode;
            self.screencast.saved_region = Some(panel.region);
            self.compositor_cursor = None;
            self.apply_cursor(CursorImageStatus::default_named());
            self.pointer_focus_dirty = true;
            self.needs_redraw = true;
        }
    }

    pub fn ensure_screencast_hud_rendered(&mut self) {
        let Some(panel) = self.screencast.panel.as_mut() else {
            return;
        };
        panel.update_size_label(&mut self.fonts, self.scale_factor);
        if !panel.dirty {
            return;
        }
        panel.dirty = false;
        let bar = panel.bar_rect;
        let pad = 12.0f32;
        let pw = to_phys(bar.w.ceil() as i32 + 24).max(1);
        let ph = to_phys(bar.h.ceil() as i32 + 24).max(1);
        if panel.buffer_size != (pw, ph) {
            panel.buffer = MemoryRenderBuffer::new(Fourcc::Abgr8888, (pw, ph), 1, Transform::Normal, None);
            panel.buffer_size = (pw, ph);
        }
        let ox = bar.x - pad;
        let oy = bar.y - pad;
        crate::theme::set_glass_origin(ox, oy);

        let mode = panel.mode;
        let hovered = panel.hovered;
        let countdown_left = panel.countdown_left;
        let counting_down = panel.countdown_end.is_some();
        let layout = panel.layout();
        let timer_secs = self.screencast.timer_secs;
        let record_audio = self.screencast.record_audio;
        let show_cursor = self.screencast.show_cursor;
        let fps = self.screencast.fps;
        let fonts = &mut self.fonts;
        let p = palette();
        let dark = appearance() == Appearance::Dark;

        let mut ctx = panel.buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            if let Some(mut pm) = PixmapMut::from_bytes(buf, pw as u32, ph as u32) {
                draw_glass_panel(&mut pm, pad, pad, bar.w, bar.h, 18.0, false);
                // Dividers
                let div_col = p.separator;
                let div_x1 = pad + 12.0 + 3.0 * 42.0 + 4.0;
                stroke_line(&mut pm, div_x1, pad + 11.0, div_x1, pad + bar.h - 11.0, 1.0, div_col);
                let div_x2 = div_x1 + 12.0 + 2.0 * 42.0 + 4.0;
                stroke_line(&mut pm, div_x2, pad + 11.0, div_x2, pad + bar.h - 11.0, 1.0, div_col);

                for (r, target) in &layout {
                    let rx = r.x - ox;
                    let ry = r.y - oy;
                    let is_hover = hovered == Some(*target);
                    match *target {
                        HitTarget::Mode(m) => {
                            let active = m == mode;
                            if active {
                                fill_rounded_rect(&mut pm, rx, ry, r.w, r.h, 10.0, p.accent);
                            } else if is_hover {
                                fill_rounded_rect(
                                    &mut pm,
                                    rx,
                                    ry,
                                    r.w,
                                    r.h,
                                    10.0,
                                    if dark { rgba(255, 255, 255, 28) } else { rgba(0, 0, 0, 18) },
                                );
                            }
                            let icon_col = if active { p.on_accent } else { p.text };
                            draw_mode_icon(&mut pm, m, rx + r.w * 0.5, ry + r.h * 0.5, icon_col, active);
                        }
                        HitTarget::Timer => {
                            let active = timer_secs > 0;
                            let fill = if active {
                                p.accent
                            } else if is_hover {
                                if dark { rgba(255, 255, 255, 32) } else { rgba(0, 0, 0, 20) }
                            } else {
                                p.control_fill
                            };
                            fill_rounded_rect(&mut pm, rx, ry, r.w, r.h, 9.0, fill);
                            let col = if active { p.on_accent } else { p.text };
                            let text = if timer_secs == 0 {
                                feather_ui::i18n::tr("Таймер").into_owned()
                            } else {
                                format!("{timer_secs} {}", feather_ui::i18n::tr("с"))
                            };
                            fonts.draw_text_centered(&mut pm, &text, rx + r.w * 0.5, ry + 8.0, 12.0, col, active);
                        }
                        HitTarget::Mic => {
                            let active = record_audio;
                            let fill = if active {
                                p.accent
                            } else if is_hover {
                                if dark { rgba(255, 255, 255, 32) } else { rgba(0, 0, 0, 20) }
                            } else {
                                p.control_fill
                            };
                            fill_rounded_rect(&mut pm, rx, ry, r.w, r.h, 9.0, fill);
                            let col = if active { p.on_accent } else { p.text };
                            let label = feather_ui::i18n::tr("Микрофон");
                            fonts.draw_text_centered(&mut pm, &label, rx + r.w * 0.5, ry + 8.0, 12.0, col, active);
                        }
                        HitTarget::Cursor => {
                            let active = show_cursor;
                            let fill = if active {
                                p.accent
                            } else if is_hover {
                                if dark { rgba(255, 255, 255, 32) } else { rgba(0, 0, 0, 20) }
                            } else {
                                p.control_fill
                            };
                            fill_rounded_rect(&mut pm, rx, ry, r.w, r.h, 9.0, fill);
                            let col = if active { p.on_accent } else { p.text };
                            let label = feather_ui::i18n::tr("Курсор");
                            fonts.draw_text_centered(&mut pm, &label, rx + r.w * 0.5, ry + 8.0, 12.0, col, active);
                        }
                        HitTarget::Fps => {
                            let active = fps == 60;
                            let fill = if active {
                                p.accent
                            } else if is_hover {
                                if dark { rgba(255, 255, 255, 32) } else { rgba(0, 0, 0, 20) }
                            } else {
                                p.control_fill
                            };
                            fill_rounded_rect(&mut pm, rx, ry, r.w, r.h, 9.0, fill);
                            let col = if active { p.on_accent } else { p.text };
                            let text = format!("{fps} FPS");
                            fonts.draw_text_centered(&mut pm, &text, rx + r.w * 0.5, ry + 8.0, 12.0, col, true);
                        }
                        HitTarget::Start => {
                            let rec_col = if mode.is_video() {
                                if is_hover { rgba(255, 85, 74, 255) } else { rgba(255, 59, 48, 255) }
                            } else if is_hover {
                                p.accent
                            } else {
                                p.accent
                            };
                            fill_rounded_rect(&mut pm, rx, ry, r.w, r.h, 10.0, rec_col);
                            let label = if counting_down {
                                format!("{} ({countdown_left})", feather_ui::i18n::tr("Отмена"))
                            } else if mode.is_video() {
                                format!("● {}", feather_ui::i18n::tr("Записать"))
                            } else {
                                feather_ui::i18n::tr("Снять").into_owned()
                            };
                            fonts.draw_text_centered(
                                &mut pm,
                                &label,
                                rx + r.w * 0.5,
                                ry + 8.5,
                                12.5,
                                Color::WHITE,
                                true,
                            );
                        }
                        HitTarget::Close => {
                            let fill = if is_hover {
                                if dark { rgba(255, 255, 255, 42) } else { rgba(0, 0, 0, 26) }
                            } else {
                                p.control_fill
                            };
                            fill_circle(&mut pm, rx + r.w * 0.5, ry + r.h * 0.5, r.w * 0.5, fill);
                            fonts.draw_text_centered(&mut pm, "×", rx + r.w * 0.5, ry + 4.0, 15.0, p.text, true);
                        }
                    }
                }
            }
            Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
        });
    }

    pub fn screencast_motion(&mut self) -> bool {
        let Some(panel) = self.screencast.panel.as_mut() else {
            return false;
        };
        let (px, py) = (self.pointer_location.x, self.pointer_location.y);
        let (sw, sh) = (self.shell.screen_w as f64, self.shell.screen_h as f64);
        if let Some(drag) = panel.region_drag {
            let min_size = 64.0;
            match drag {
                RegionDrag::Move { offset_x, offset_y } => {
                    let (_, _, w, h) = panel.region;
                    let nx = (px - offset_x).clamp(0.0, (sw - w).max(0.0)).round();
                    let ny = (py - offset_y).clamp(0.0, (sh - h).max(0.0)).round();
                    panel.region = (nx, ny, w, h);
                }
                RegionDrag::Resize { left, right, top, bottom } => {
                    let (mut x0, mut y0, w, h) = panel.region;
                    let (mut x1, mut y1) = (x0 + w, y0 + h);
                    if left {
                        x0 = px.clamp(0.0, x1 - min_size).round();
                    }
                    if right {
                        x1 = px.clamp(x0 + min_size, sw).round();
                    }
                    if top {
                        y0 = py.clamp(0.0, y1 - min_size).round();
                    }
                    if bottom {
                        y1 = py.clamp(y0 + min_size, sh).round();
                    }
                    panel.region = (x0, y0, x1 - x0, y1 - y0);
                }
                RegionDrag::New { anchor_x, anchor_y } => {
                    let x0 = anchor_x.min(px).clamp(0.0, sw - min_size).round();
                    let y0 = anchor_y.min(py).clamp(0.0, sh - min_size).round();
                    let x1 = anchor_x.max(px).clamp(x0 + min_size, sw).round();
                    let y1 = anchor_y.max(py).clamp(y0 + min_size, sh).round();
                    panel.region = (x0, y0, x1 - x0, y1 - y0);
                }
            }
            self.screencast.saved_region = Some(panel.region);
            self.needs_redraw = true;
            return true;
        }
        let hit = panel.hit_at(px as f32, py as f32);
        if hit != panel.hovered {
            panel.hovered = hit;
            panel.dirty = true;
            self.needs_redraw = true;
        }
        let icon = panel.cursor_for(px, py);
        if self.compositor_cursor != Some(icon) {
            self.compositor_cursor = Some(icon);
            self.apply_cursor(CursorImageStatus::Named(icon));
        }
        true
    }

    pub fn screencast_button(&mut self, left: bool, pressed: bool) -> bool {
        let Some(panel) = self.screencast.panel.as_mut() else {
            return false;
        };
        if !left {
            if pressed {
                self.close_screen_recorder_hud();
            }
            return true;
        }
        if !pressed {
            if panel.region_drag.take().is_some() {
                self.needs_redraw = true;
            }
            return true;
        }
        let (px, py) = (self.pointer_location.x, self.pointer_location.y);
        if let Some(hit) = panel.hit_at(px as f32, py as f32) {
            match hit {
                HitTarget::Mode(mode) => {
                    panel.mode = mode;
                    panel.countdown_end = None;
                    panel.dirty = true;
                    self.screencast.last_mode = mode;
                    self.needs_redraw = true;
                }
                HitTarget::Timer => {
                    self.screencast.timer_secs = match self.screencast.timer_secs {
                        0 => 5,
                        5 => 10,
                        _ => 0,
                    };
                    panel.dirty = true;
                    self.needs_redraw = true;
                }
                HitTarget::Mic => {
                    self.screencast.record_audio = !self.screencast.record_audio;
                    panel.dirty = true;
                    self.needs_redraw = true;
                }
                HitTarget::Cursor => {
                    self.screencast.show_cursor = !self.screencast.show_cursor;
                    panel.dirty = true;
                    self.needs_redraw = true;
                }
                HitTarget::Fps => {
                    self.screencast.fps = if self.screencast.fps == 30 { 60 } else { 30 };
                    panel.dirty = true;
                    self.needs_redraw = true;
                }
                HitTarget::Start => {
                    if panel.countdown_end.is_some() {
                        panel.countdown_end = None;
                        panel.dirty = true;
                        self.needs_redraw = true;
                    } else {
                        self.trigger_screencast_start();
                    }
                }
                HitTarget::Close => {
                    self.close_screen_recorder_hud();
                }
            }
            return true;
        }
        if panel.bar_rect.contains(px as f32, py as f32) {
            return true;
        }
        if panel.mode.uses_area() {
            panel.region_drag = Some(panel.region_hit(px, py));
            self.needs_redraw = true;
        } else if panel.mode == CaptureMode::PhotoWindow {
            let target = self
                .window_target(|win| win.contains_point(px, py))
                .or_else(|| self.window_target(|w| w.activated))
                .unwrap_or(crate::screenshot::Target::Screen);
            self.close_screen_recorder_hud();
            self.take_screenshot(target);
        }
        true
    }

    pub fn trigger_screencast_start(&mut self) {
        let Some(panel) = self.screencast.panel.as_mut() else {
            return;
        };
        if self.screencast.timer_secs > 0 && panel.countdown_end.is_none() {
            let secs = self.screencast.timer_secs;
            panel.countdown_end = Some(Instant::now() + Duration::from_secs(secs as u64));
            panel.countdown_left = secs;
            panel.dirty = true;
            self.needs_redraw = true;
            return;
        }
        let mode = panel.mode;
        let (rx, ry, rw, rh) = panel.region;
        self.close_screen_recorder_hud();
        match mode {
            CaptureMode::PhotoScreen => self.take_screenshot(crate::screenshot::Target::Screen),
            CaptureMode::PhotoWindow => {
                let target = self
                    .window_target(|w| w.activated)
                    .unwrap_or(crate::screenshot::Target::Screen);
                self.take_screenshot(target);
            }
            CaptureMode::PhotoArea => {
                self.take_screenshot(crate::screenshot::Target::Area {
                    x: rx,
                    y: ry,
                    w: rw,
                    h: rh,
                    radius: 0.0,
                });
            }
            CaptureMode::VideoScreen => {
                self.start_screen_recording(None);
            }
            CaptureMode::VideoArea => {
                self.start_screen_recording(Some((rx, ry, rw, rh)));
            }
        }
    }

    pub fn start_screen_recording(&mut self, logical_area: Option<(f64, f64, f64, f64)>) {
        if self.lock.active || self.screencast.is_recording() {
            return;
        }
        let (sw, sh, s) = (self.phys_w, self.phys_h, self.scale_factor);
        let region = match logical_area {
            None => {
                let w = (sw & !1).max(16);
                let h = (sh & !1).max(16);
                Rectangle::<i32, Physical>::from_size((w, h).into())
            }
            Some((x, y, w, h)) => {
                let x0 = ((x * s).round() as i32).clamp(0, sw - 16);
                let y0 = ((y * s).round() as i32).clamp(0, sh - 16);
                let raw_w = (((x + w) * s).round() as i32 - x0).clamp(16, sw - x0);
                let raw_h = (((y + h) * s).round() as i32 - y0).clamp(16, sh - y0);
                let even_w = (raw_w & !1).max(16);
                let even_h = (raw_h & !1).max(16);
                Rectangle::new((x0, y0).into(), (even_w, even_h).into())
            }
        };

        let fps = self.screencast.fps.clamp(15, 60);
        let record_audio = self.screencast.record_audio;
        let show_cursor = self.screencast.show_cursor;
        let custom_dir = crate::screenshot::custom_dir();
        let done_tx = self.screencast.done_tx.clone();
        let (tx, rx) = mpsc::sync_channel::<WorkerMsg>(2);
        let (out_w, out_h) = (region.size.w as u32, region.size.h as u32);

        let spawned = std::thread::Builder::new()
            .name("feather-recorder".into())
            .spawn(move || {
                run_recorder_worker(rx, done_tx, out_w, out_h, fps, record_audio, custom_dir);
            });
        if spawned.is_err() {
            self.shell
                .notify("Запись экрана", "Не удалось запустить поток кодирования видео");
            return;
        }

        // Pre-render a 36×36 click highlight ring buffer
        let ring_px = (36.0 * s).round().max(16.0) as u32;
        let mut ring_pm = tiny_skia::Pixmap::new(ring_px, ring_px).unwrap();
        let r_logical = 18.0;
        fill_circle(&mut ring_pm.as_mut(), r_logical, r_logical, 13.0, rgba(255, 255, 255, 55));
        stroke_circle(&mut ring_pm.as_mut(), r_logical, r_logical, 13.0, 2.0, rgba(255, 69, 58, 220));
        let click_ring_buffer = MemoryRenderBuffer::from_slice(
            ring_pm.data(),
            Fourcc::Abgr8888,
            (ring_px as i32, ring_px as i32),
            1,
            Transform::Normal,
            None,
        );

        self.screencast.recording = Some(ActiveRecording {
            started_at: Instant::now(),
            region,
            fps,
            show_cursor,
            tx,
            offscreen: None,
            pending_pbo: None,
            last_shown_secs: 0,
            click_ring_buffer,
            click_ring_px: ring_px as i32,
        });
        self.shell.recording_secs = Some(0);
        self.shell.topbar_dirty = true;
        self.needs_redraw = true;

        // Capture the first frame immediately and arm the frame-paced timer
        self.capture_recording_frame();
        let interval = Duration::from_millis((1000 / fps).max(16) as u64);
        let _ = self.loop_handle.insert_source(Timer::from_duration(interval), move |_, _, state| {
            if state.lock.active {
                state.stop_screen_recording();
                return TimeoutAction::Drop;
            }
            if !state.screencast.is_recording() {
                return TimeoutAction::Drop;
            }
            state.capture_recording_frame();
            TimeoutAction::ToDuration(interval)
        });
    }

    pub fn capture_recording_frame(&mut self) {
        let Some(rec) = self.screencast.recording.as_mut() else {
            return;
        };
        let elapsed_ms = rec.started_at.elapsed().as_millis() as u64;
        let (sw, sh, s) = (self.phys_w, self.phys_h, self.scale_factor);
        let region = rec.region;
        let show_cursor = rec.show_cursor;
        let pointer = (self.pointer_location.x, self.pointer_location.y);
        let button_down = !self.pressed_buttons.is_empty();

        // 1. Map the previous frame's PBO (finished by GPU during the frame interval) and send to worker
        if let Some((prev_map, pts_ms)) = rec.pending_pbo.take() {
            if let Ok(bytes) = self.backend.renderer().map_texture(&prev_map) {
                let _ = rec.tx.try_send(WorkerMsg::Frame {
                    rgba: bytes.to_vec(),
                    pts_ms,
                });
            }
        }

        // 2. Render current scene into cached offscreen texture and start async PBO readback
        let renderer = self.backend.renderer();
        let mut elements: Vec<FeatherElement> = Vec::with_capacity(68);
        if show_cursor {
            elements.extend(crate::render::cursor_elements(
                renderer,
                &mut self.cursor,
                pointer,
                s,
            ));
            if button_down {
                let rec_ref = self.screencast.recording.as_ref().unwrap();
                let ring_px = rec_ref.click_ring_px;
                let cx = (pointer.0 * s).round() - ring_px as f64 * 0.5;
                let cy = (pointer.1 * s).round() - ring_px as f64 * 0.5;
                if let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    (cx, cy),
                    &rec_ref.click_ring_buffer,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ) {
                    elements.push(crate::render::SceneElement::Memory(elem).into());
                }
            }
        }
        elements.extend(crate::render::build_scene(
            renderer,
            &self.shell,
            &self.windows,
            s,
            pointer,
            self.rounded_program.as_ref(),
            &self.animations,
            crate::monitors::Rect {
                x: 0,
                y: 0,
                w: self.shell.screen_w,
                h: self.shell.screen_h,
            },
        ));

        let rec = self.screencast.recording.as_mut().unwrap();
        if rec.offscreen.as_ref().is_none_or(|(_, sz)| *sz != (sw, sh)) {
            rec.offscreen = renderer
                .create_buffer(Fourcc::Abgr8888, (sw, sh).into())
                .ok()
                .map(|tex| (tex, (sw, sh)));
        }
        let mapping = rec.offscreen.as_mut().and_then(|(texture, _)| {
            let mut fb = renderer.bind(texture).ok()?;
            OutputDamageTracker::new((sw, sh), 1.0, Transform::Normal)
                .render_output(renderer, &mut fb, 0, &elements, crate::render::CLEAR_COLOR)
                .ok()?;
            let rows = Rectangle::<i32, smithay::utils::Buffer>::new(
                (region.loc.x, region.loc.y).into(),
                (region.size.w, region.size.h).into(),
            );
            renderer.copy_framebuffer(&fb, rows, Fourcc::Abgr8888).ok()
        });
        drop(elements);
        if let Some(rec) = self.screencast.recording.as_mut() {
            rec.pending_pbo = mapping.map(|m| (m, elapsed_ms));
        }
    }

    pub fn stop_screen_recording(&mut self) {
        let Some(mut rec) = self.screencast.recording.take() else {
            return;
        };
        let elapsed_ms = rec.started_at.elapsed().as_millis() as u64;
        if let Some((prev_map, pts_ms)) = rec.pending_pbo.take() {
            if let Ok(bytes) = self.backend.renderer().map_texture(&prev_map) {
                let _ = rec.tx.try_send(WorkerMsg::Frame {
                    rgba: bytes.to_vec(),
                    pts_ms: pts_ms.max(elapsed_ms),
                });
            }
        }
        rec.offscreen = None;
        let _ = rec.tx.try_send(WorkerMsg::Stop);
        drop(rec);
        self.shell.recording_secs = None;
        self.shell.topbar_dirty = true;
        self.needs_redraw = true;
    }

    pub fn install_recording(&mut self, finished: FinishedRecording) {
        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }
        if self.lock.active {
            return;
        }
        let Some(path) = finished.saved else {
            self.shell
                .notify("Запись экрана", "Не удалось сохранить видеофайл");
            self.needs_redraw = true;
            return;
        };
        self.play_effect(crate::sounds::Effect::Screenshot, false);
        let uri = format!("file://{}\r\n", path.display());
        self.set_clipboard(vec![
            ("text/uri-list".into(), uri.into_bytes()),
            ("text/plain;charset=utf-8".into(), path.display().to_string().into_bytes()),
        ]);
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let body = format!(
            "Сохранено в файл {} ({:.1} с)",
            file_name,
            finished.duration_secs.max(0.1)
        );
        self.shell.notify("Запись экрана", body);
        self.needs_redraw = true;
    }

    /// Called once per second from the compositor's 1 s housekeeping timer
    pub fn screencast_tick(&mut self) -> bool {
        let mut changed = false;
        if let Some(rec) = self.screencast.recording.as_mut() {
            let secs = rec.started_at.elapsed().as_secs();
            if secs != rec.last_shown_secs {
                rec.last_shown_secs = secs;
                self.shell.recording_secs = Some(secs);
                self.shell.topbar_dirty = true;
                changed = true;
            }
        }
        let start_now = if let Some(panel) = self.screencast.panel.as_mut() {
            if let Some(end) = panel.countdown_end {
                let now = Instant::now();
                if now >= end {
                    panel.countdown_end = None;
                    panel.countdown_left = 0;
                    true
                } else {
                    let left = (end - now).as_secs() as u32 + 1;
                    if left != panel.countdown_left {
                        panel.countdown_left = left;
                        panel.dirty = true;
                        changed = true;
                    }
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        if start_now {
            self.trigger_screencast_start();
            changed = true;
        }
        changed
    }
}

/// GPU scene elements for the Screen Capture HUD bar and interactive region frame (never captured in photos/videos)
pub fn overlay(
    renderer: &mut GlesRenderer,
    state: &ScreenRecorderState,
    scale: f64,
    screen_phys: (i32, i32),
) -> Vec<FeatherElement> {
    let Some(panel) = state.panel.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(20);
    let pad = 12.0;
    let bx = ((panel.bar_rect.x - pad) as f64 * scale).round();
    let by = ((panel.bar_rect.y - pad) as f64 * scale).round();
    if let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        (bx, by),
        &panel.buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    ) {
        out.push(crate::render::SceneElement::Memory(elem).into());
    }

    if panel.mode.uses_area() {
        let (sw, sh) = screen_phys;
        let (rx, ry, rw, rh) = panel.region;
        let x0 = ((rx * scale).round() as i32).clamp(0, sw);
        let y0 = ((ry * scale).round() as i32).clamp(0, sh);
        let x1 = (((rx + rw) * scale).round() as i32).clamp(0, sw);
        let y1 = (((ry + rh) * scale).round() as i32).clamp(0, sh);
        let b = (scale.round() as i32 * 2).max(2);
        let tall = y1 - y0 + 2 * b;
        let src = Rectangle::new((0.0, 0.0).into(), (1.0, 1.0).into());

        // Size badge above-left of the selection rectangle
        if let Some((buf, (lw, lh), _)) = panel.size_label.as_ref() {
            let lx = x0.clamp(4, (sw - lw - 4).max(4));
            let ly = if y0 - lh - 8 >= (TOP_BAR_HEIGHT as f64 * scale) as i32 {
                y0 - lh - 8
            } else {
                (y0 + 8).min(sh - lh - 4)
            };
            if let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (lx as f64, ly as f64),
                buf,
                None,
                None,
                None,
                Kind::Unspecified,
            ) {
                out.push(crate::render::SceneElement::Memory(elem).into());
            }
        }

        // 8 resize handles + 4 edges + 4 exterior dim rectangles
        let h_sz = (8.0 * scale).round() as i32;
        let h_half = h_sz / 2;
        let mx = (x0 + x1) / 2;
        let my = (y0 + y1) / 2;
        let rects: [(usize, i32, i32, i32, i32); 16] = [
            // 8 handles (indices 8..16)
            (8, x0 - h_half, y0 - h_half, h_sz, h_sz),
            (9, x1 - h_half, y0 - h_half, h_sz, h_sz),
            (10, x0 - h_half, y1 - h_half, h_sz, h_sz),
            (11, x1 - h_half, y1 - h_half, h_sz, h_sz),
            (12, mx - h_half, y0 - h_half, h_sz, h_sz),
            (13, mx - h_half, y1 - h_half, h_sz, h_sz),
            (14, x0 - h_half, my - h_half, h_sz, h_sz),
            (15, x1 - h_half, my - h_half, h_sz, h_sz),
            // 4 outline edges (indices 0..4)
            (0, x0, y0 - b, x1 - x0, b),
            (1, x0, y1, x1 - x0, b),
            (2, x0 - b, y0 - b, b, tall),
            (3, x1, y0 - b, b, tall),
            // 4 exterior dim areas (indices 4..8)
            (4, 0, 0, sw, y0 - b),
            (5, 0, y1 + b, sw, sh - y1 - b),
            (6, 0, y0 - b, x0 - b, tall),
            (7, x1 + b, y0 - b, sw - x1 - b, tall),
        ];
        for (idx, x, y, w, h) in rects {
            if w > 0 && h > 0 {
                if let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    (x as f64, y as f64),
                    &panel.pixels[idx],
                    None,
                    Some(src),
                    Some((w, h).into()),
                    Kind::Unspecified,
                ) {
                    out.push(crate::render::SceneElement::Memory(elem).into());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_rust_mjpeg_avi_writer_produces_valid_riff_avi_container() {
        let dir = std::env::temp_dir().join(format!("feather-screencast-avi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.avi");

        let (w, h, fps) = (64, 48, 30);
        let mut writer = MjpegAviWriter::create(path.clone(), w, h, fps).unwrap();
        for i in 0..5u8 {
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            for px in rgba.chunks_exact_mut(4) {
                px.copy_from_slice(&[40 + i * 30, 120, 220 - i * 20, 255]);
            }
            writer.write_rgba_frame(&rgba).unwrap();
        }
        writer.repeat_last_frame().unwrap();
        let (saved_path, frames) = writer.finish().unwrap();
        assert_eq!(saved_path, path);
        assert_eq!(frames, 6);

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"AVI ");
        let riff_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(riff_size as usize + 8, bytes.len());
        let total_frames = u32::from_le_bytes(bytes[48..52].try_into().unwrap());
        assert_eq!(total_frames, 6);
        assert!(bytes.windows(4).any(|w| w == b"idx1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recorder_worker_writes_mp4_via_ffmpeg_and_falls_back_cleanly() {
        let dir = std::env::temp_dir().join(format!("feather-screencast-worker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (done_tx, done_rx) = channel_source();
        let _ = done_rx;
        let (tx, rx) = mpsc::sync_channel(2);
        let dir_clone = dir.clone();
        let handle = std::thread::spawn(move || {
            run_recorder_worker(rx, done_tx, 128, 96, 30, false, Some(dir_clone));
        });
        for frame in 0..6u64 {
            let rgba = vec![180u8; 128 * 96 * 4];
            tx.send(WorkerMsg::Frame {
                rgba,
                pts_ms: frame * 33,
            })
            .unwrap();
        }
        tx.send(WorkerMsg::Stop).unwrap();
        handle.join().unwrap();

        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(files.len(), 1, "expected 1 recorded video file in temp dir");
        let meta = std::fs::metadata(&files[0]).unwrap();
        assert!(meta.len() > 256, "recorded video file must be non-empty");
        let ext = files[0].extension().and_then(|e| e.to_str()).unwrap_or("");
        assert!(ext == "mp4" || ext == "avi", "unexpected extension: {ext}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hud_layout_and_region_handles_cover_all_modes_and_edges() {
        let panel = HudPanel::new(1440, 900, CaptureMode::VideoArea, (200.0, 150.0, 400.0, 300.0));
        let hits = panel.layout();
        assert_eq!(hits.len(), 11);
        for mode in CaptureMode::ALL {
            let (r, _) = hits.iter().find(|(_, t)| *t == HitTarget::Mode(mode)).unwrap();
            assert_eq!(panel.hit_at(r.x + r.w * 0.5, r.y + r.h * 0.5), Some(HitTarget::Mode(mode)));
        }
        // Top-left corner handle resize
        assert_eq!(
            panel.region_hit(200.0, 150.0),
            RegionDrag::Resize {
                left: true,
                right: false,
                top: true,
                bottom: false
            }
        );
        // Interior move
        assert!(matches!(panel.region_hit(350.0, 280.0), RegionDrag::Move { .. }));
        // Exterior new selection
        assert!(matches!(panel.region_hit(50.0, 50.0), RegionDrag::New { .. }));
    }
}
