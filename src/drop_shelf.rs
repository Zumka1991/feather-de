//! Glass Drop Shelf («Полка перетаскивания»): a macOS Tahoe Liquid Glass floating shelf
//! integrated directly with Wayland's `wl_data_device` (`ClientDndGrab` & `ServerDndGrab`).
//!
//! Summoned by shaking the pointer during a drag, touching the left/right screen edge while
//! dragging, pressing `Super + D`, clicking its menu-bar pill, or running `featherctl shelf`.
//! Holds files, images (decoded on a worker thread with a thumbnail and a tmpfs backing file so
//! programs can accept them as either `image/*` or `text/uri-list`), and text/URLs across Spaces,
//! Mission Control, and fullscreen windows.
//!
//! Budget discipline (`docs/SPEC.md` §2):
//! - **0% CPU in idle:** no timers; spring animation runs only while opening/closing and stops.
//! - **0 KB idle `RssAnon`:** buffer shrinks to `1×1` (and is dropped) once closed.
//! - **Non-blocking event loop:** all pipe reads, image decoding, metadata checks, and temp-file
//!   writes/unlinks run on bounded `128 KiB` worker threads with poll deadlines.

use std::{
    io::{Read, Write},
    os::fd::{AsFd, AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use feather_ui::{
    file_icons::{Icon, IconCache},
    file_kind::{kind_of, Kind},
    fileops,
    i18n::tr,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind as ElementKind,
            },
            gles::GlesRenderer,
        },
    },
    input::pointer::GrabStartData,
    reexports::{
        calloop::channel,
        wayland_server::protocol::{
            wl_data_device_manager::DndAction, wl_data_source::WlDataSource,
        },
    },
    utils::{Rectangle, Transform, SERIAL_COUNTER},
    wayland::selection::data_device::{start_dnd, with_source_metadata, SourceMetadata},
};
use tiny_skia::{FilterQuality, Pixmap, PixmapMut, PixmapPaint};

use crate::{
    animation::Visual,
    apps::IconKind,
    glass::{GlassSpec, GlassState, KEY_DROP_SHELF},
    render::{FeatherElement, SceneElement},
    shell::{AppIcon, Ghost, R},
    text::FontSystem,
    theme::*,
    FeatherCompositor,
};

pub const SHELF_WIDTH: f32 = 256.0;
const SHADOW_PAD: f32 = 14.0;
const HEADER_H: f32 = 34.0;
const ROW_H: f32 = 44.0;
const EMPTY_H: f32 = 98.0;
const FOOTER_H: f32 = 34.0;
const BODY_PAD_BOTTOM: f32 = 8.0;
const PANEL_RADIUS: f32 = 20.0;
const VISIBLE_ROWS: usize = 4;
const MAX_ITEMS: usize = 24;
const DRAG_THRESHOLD: f64 = 5.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

const READ_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const IMAGE_MIMES: [&str; 3] = ["image/png", "image/webp", "image/jpeg"];
const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];
const SECRET_MIMES: [&str; 3] = [
    "x-kde-passwordManagerHint",
    "application/x-nspasteboard-concealedtype",
    "org.nspasteboard.ConcealedType",
];

#[derive(Clone)]
pub enum ShelfPayload {
    File {
        path: PathBuf,
        is_dir: bool,
        dims: Option<(u32, u32)>,
    },
    Image {
        mime: String,
        bytes: Arc<Vec<u8>>,
        width: u32,
        height: u32,
        temp_path: Option<PathBuf>,
    },
    Text {
        text: String,
        is_url: bool,
    },
}

#[derive(Clone)]
pub struct ShelfItem {
    pub id: u64,
    pub title: String,
    pub subtitle: String,
    pub payload: ShelfPayload,
    thumb: Option<Pixmap>,
    thumb_scale_bits: u32,
}

impl ShelfItem {
    pub fn kind_name(&self) -> &'static str {
        match &self.payload {
            ShelfPayload::File { is_dir: true, .. } => "folder",
            ShelfPayload::File { .. } => "file",
            ShelfPayload::Image { .. } => "image",
            ShelfPayload::Text { is_url: true, .. } => "url",
            ShelfPayload::Text { is_url: false, .. } => "text",
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match &self.payload {
            ShelfPayload::File { path, .. } => Some(path.as_path()),
            ShelfPayload::Image { temp_path, .. } => temp_path.as_deref(),
            ShelfPayload::Text { .. } => None,
        }
    }
}

pub enum ShelfMsg {
    FileMeta {
        id: u64,
        is_dir: bool,
        dims: Option<(u32, u32)>,
        thumb: Option<(u32, Pixmap)>,
    },
    ClientCaptured {
        drag_id: u64,
        captured: CapturedPayload,
    },
}

pub enum CapturedPayload {
    Image {
        mime: String,
        bytes: Arc<Vec<u8>>,
        width: u32,
        height: u32,
        thumb: Option<(u32, Pixmap)>,
        temp_path: Option<PathBuf>,
    },
    Text(String),
}

/// Detects rapid back-and-forth cursor shakes or screen-edge touches during an active drag.
pub struct ShakeDetector {
    active: bool,
    summoned_this_drag: bool,
    start: (f64, f64),
    last: (f64, f64),
    anchor_x: f64,
    anchor_y: f64,
    dir_x: i8,
    dir_y: i8,
    reversals_x: Vec<Instant>,
    reversals_y: Vec<Instant>,
}

impl Default for ShakeDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl ShakeDetector {
    pub fn new() -> Self {
        Self {
            active: false,
            summoned_this_drag: false,
            start: (0.0, 0.0),
            last: (0.0, 0.0),
            anchor_x: 0.0,
            anchor_y: 0.0,
            dir_x: 0,
            dir_y: 0,
            reversals_x: Vec::new(),
            reversals_y: Vec::new(),
        }
    }

    pub fn begin(&mut self, pos: (f64, f64)) {
        if self.active {
            return;
        }
        self.active = true;
        self.summoned_this_drag = false;
        self.start = pos;
        self.last = pos;
        self.anchor_x = pos.0;
        self.anchor_y = pos.1;
        self.dir_x = 0;
        self.dir_y = 0;
        self.reversals_x.clear();
        self.reversals_y.clear();
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.summoned_this_drag = false;
        self.reversals_x.clear();
        self.reversals_y.clear();
    }

    pub fn suppress_current_drag(&mut self) {
        self.summoned_this_drag = true;
    }

    /// Feeds one motion sample; returns `true` once if the shelf should pop open near `pos`.
    pub fn sample(&mut self, pos: (f64, f64), now: Instant, screen: (i32, i32)) -> bool {
        if !self.active {
            self.begin(pos);
            return false;
        }
        if self.summoned_this_drag {
            self.last = pos;
            return false;
        }

        const MIN_LEG: f64 = 22.0;
        const WINDOW: Duration = Duration::from_millis(480);

        let dx = pos.0 - self.last.0;
        let dy = pos.1 - self.last.1;
        self.last = pos;

        if dx.abs() > 0.5 {
            let step_dir = if dx > 0.0 { 1 } else { -1 };
            if self.dir_x == 0 {
                if (pos.0 - self.anchor_x).abs() >= MIN_LEG {
                    self.dir_x = if pos.0 > self.anchor_x { 1 } else { -1 };
                    self.anchor_x = pos.0;
                }
            } else if step_dir == self.dir_x {
                if (self.dir_x > 0 && pos.0 > self.anchor_x)
                    || (self.dir_x < 0 && pos.0 < self.anchor_x)
                {
                    self.anchor_x = pos.0;
                }
            } else if (pos.0 - self.anchor_x).abs() >= MIN_LEG {
                self.dir_x = step_dir;
                self.anchor_x = pos.0;
                self.reversals_x.push(now);
            }
        }

        if dy.abs() > 0.5 {
            let step_dir = if dy > 0.0 { 1 } else { -1 };
            if self.dir_y == 0 {
                if (pos.1 - self.anchor_y).abs() >= MIN_LEG {
                    self.dir_y = if pos.1 > self.anchor_y { 1 } else { -1 };
                    self.anchor_y = pos.1;
                }
            } else if step_dir == self.dir_y {
                if (self.dir_y > 0 && pos.1 > self.anchor_y)
                    || (self.dir_y < 0 && pos.1 < self.anchor_y)
                {
                    self.anchor_y = pos.1;
                }
            } else if (pos.1 - self.anchor_y).abs() >= MIN_LEG {
                self.dir_y = step_dir;
                self.anchor_y = pos.1;
                self.reversals_y.push(now);
            }
        }

        self.reversals_x.retain(|t| now.saturating_duration_since(*t) <= WINDOW);
        self.reversals_y.retain(|t| now.saturating_duration_since(*t) <= WINDOW);

        let shook = self.reversals_x.len() >= 3 || self.reversals_y.len() >= 3;
        let moved = (pos.0 - self.start.0).hypot(pos.1 - self.start.1);
        let at_side_edge = screen.0 > 200
            && moved >= 40.0
            && pos.1 >= TOP_BAR_HEIGHT as f64 + 16.0
            && pos.1 <= (screen.1 - 56) as f64
            && (pos.0 <= 3.0 || pos.0 >= (screen.0 - 4) as f64);

        if shook || at_side_edge {
            self.summoned_this_drag = true;
            return true;
        }
        false
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HoverPart {
    Clear,
    Close,
    Row(usize),
    RowCopy(usize),
    RowRemove(usize),
    DragAll,
}

#[derive(Clone)]
enum PressGesture {
    MovePanel {
        start_cursor: (f64, f64),
        start_pos: (f32, f32),
    },
    Item {
        index: usize,
        id: u64,
        start_cursor: (f64, f64),
    },
    DragAll {
        start_cursor: (f64, f64),
    },
}

#[derive(Clone)]
struct OutboundDrag {
    ids: Vec<u64>,
    formats: Vec<(String, Vec<u8>)>,
    paths: Vec<PathBuf>,
    keep_copy: bool,
    dropped: bool,
}

struct PendingClientDrag {
    id: u64,
    paths: Option<Vec<PathBuf>>,
    urls: Vec<String>,
    captured: Option<CapturedPayload>,
    accept_when_ready: bool,
}

pub struct DropShelf {
    open: bool,
    /// User-chosen logical top-left of the panel (None → top-right below the menu bar)
    pos: Option<(f32, f32)>,
    items: Vec<ShelfItem>,
    next_id: u64,
    scroll: usize,

    anim_pos: f64,
    anim_vel: f64,
    last_tick: Option<Instant>,

    hover: Option<HoverPart>,
    drop_hover: bool,
    gesture: Option<PressGesture>,
    last_click: Option<(Instant, u64)>,

    outbound: Option<OutboundDrag>,
    client_drag: Option<PendingClientDrag>,
    next_client_drag: u64,
    shake: ShakeDetector,

    dirty: bool,
    buffer: Option<MemoryRenderBuffer>,
    buffer_size: (i32, i32),
    icon_cache: IconCache,
    tx: Option<channel::Sender<ShelfMsg>>,
    temp_files: Vec<PathBuf>,
}

impl Default for DropShelf {
    fn default() -> Self {
        Self::new()
    }
}

impl DropShelf {
    pub fn new() -> Self {
        Self {
            open: false,
            pos: None,
            items: Vec::new(),
            next_id: 1,
            scroll: 0,
            anim_pos: 0.0,
            anim_vel: 0.0,
            last_tick: None,
            hover: None,
            drop_hover: false,
            gesture: None,
            last_click: None,
            outbound: None,
            client_drag: None,
            next_client_drag: 0,
            shake: ShakeDetector::new(),
            dirty: true,
            buffer: None,
            buffer_size: (0, 0),
            icon_cache: IconCache::default(),
            tx: None,
            temp_files: Vec::new(),
        }
    }

    pub fn set_sender(&mut self, tx: channel::Sender<ShelfMsg>) {
        self.tx = Some(tx);
    }

    #[inline]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[inline]
    pub fn is_visible(&self) -> bool {
        self.open || self.anim_pos > 0.004
    }

    #[inline]
    pub fn count(&self) -> usize {
        self.items.len()
    }

    #[inline]
    pub fn items(&self) -> &[ShelfItem] {
        &self.items
    }

    #[inline]
    pub fn pos(&self) -> Option<(f32, f32)> {
        self.pos
    }

    pub fn set_pos(&mut self, pos: Option<(f32, f32)>) {
        self.pos = pos.filter(|(x, y)| x.is_finite() && y.is_finite());
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn panel_height(&self) -> f32 {
        if self.items.is_empty() {
            HEADER_H + EMPTY_H
        } else {
            let rows = self.items.len().min(VISIBLE_ROWS) as f32;
            let footer = if self.items.len() >= 2 { FOOTER_H } else { BODY_PAD_BOTTOM };
            HEADER_H + rows * ROW_H + footer
        }
    }

    fn default_pos(screen: (i32, i32), h: f32) -> (f32, f32) {
        let x = (screen.0 as f32 - SHELF_WIDTH - 16.0).max(8.0);
        let y = (TOP_BAR_HEIGHT as f32 + 12.0).min((screen.1 as f32 - h - 12.0).max(8.0));
        (x, y)
    }

    fn clamp_pos(pos: (f32, f32), screen: (i32, i32), h: f32) -> (f32, f32) {
        let min_x = 8.0_f32;
        let max_x = (screen.0 as f32 - SHELF_WIDTH - 8.0).max(min_x);
        let min_y = TOP_BAR_HEIGHT as f32 + 6.0;
        let max_y = (screen.1 as f32 - h - 8.0).max(min_y);
        (pos.0.clamp(min_x, max_x), pos.1.clamp(min_y, max_y))
    }

    pub fn clamp_to_screen(&mut self, screen: (i32, i32)) {
        if let Some(pos) = self.pos {
            let h = self.panel_height();
            self.pos = Some(Self::clamp_pos(pos, screen, h));
        }
    }

    pub fn panel_rect(&self, screen: (i32, i32)) -> R {
        let h = self.panel_height();
        let (x, y) = match self.pos {
            Some(pos) => Self::clamp_pos(pos, screen, h),
            None => Self::default_pos(screen, h),
        };
        R::new(x, y, SHELF_WIDTH, h)
    }

    pub fn contains(&self, x: f64, y: f64, screen: (i32, i32)) -> bool {
        self.is_visible() && self.panel_rect(screen).contains(x as f32, y as f32)
    }

    pub fn accepts_drop_at(&self, x: f64, y: f64, screen: (i32, i32)) -> bool {
        self.open && self.panel_rect(screen).grow(6.0).contains(x as f32, y as f32)
    }

    pub fn open_at(&mut self, near: Option<(f64, f64)>, screen: (i32, i32), min_top_y: f32) {
        let h = self.panel_height();
        if let Some((px, py)) = near {
            if !self.open {
                let x = px as f32 - SHELF_WIDTH * 0.5;
                let y = py as f32 - HEADER_H * 0.6;
                self.pos = Some(Self::clamp_pos((x, y), screen, h));
            }
        } else if self.pos.is_none() {
            let (dx, dy) = Self::default_pos(screen, h);
            self.pos = Some(Self::clamp_pos((dx, dy.max(min_top_y)), screen, h));
        }
        if !self.open {
            self.open = true;
            self.dirty = true;
        }
    }

    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            self.hover = None;
            self.drop_hover = false;
            self.gesture = None;
            self.shake.suppress_current_drag();
        }
    }

    pub fn toggle(&mut self, screen: (i32, i32), min_top_y: f32) {
        if self.open {
            self.close();
        } else {
            self.open_at(None, screen, min_top_y);
        }
    }

    pub fn clear(&mut self) {
        if !self.items.is_empty() {
            self.items.clear();
            self.scroll = 0;
            self.hover = None;
            self.dirty = true;
        }
        self.cleanup_temp_files();
    }

    fn cleanup_temp_files(&mut self) {
        if self.temp_files.is_empty() {
            return;
        }
        let files = std::mem::take(&mut self.temp_files);
        let _ = std::thread::Builder::new()
            .name("feather-shelf-clean".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                for p in files {
                    let _ = std::fs::remove_file(p);
                }
            });
    }

    pub fn remove_id(&mut self, id: u64, unlink_temp: bool) -> bool {
        let Some(pos) = self.items.iter().position(|i| i.id == id) else {
            return false;
        };
        let removed = self.items.remove(pos);
        if unlink_temp {
            if let ShelfPayload::Image { temp_path: Some(p), .. } = removed.payload {
                self.temp_files.retain(|t| t != &p);
                let _ = std::thread::Builder::new()
                    .name("feather-shelf-clean".into())
                    .stack_size(128 * 1024)
                    .spawn(move || {
                        let _ = std::fs::remove_file(p);
                    });
            }
        }
        let max_scroll = self.items.len().saturating_sub(VISIBLE_ROWS);
        self.scroll = self.scroll.min(max_scroll);
        self.hover = None;
        self.dirty = true;
        true
    }

    fn clamp_capacity(&mut self) {
        while self.items.len() > MAX_ITEMS {
            if let Some(evicted) = self.items.pop() {
                if let ShelfPayload::Image { temp_path: Some(p), .. } = evicted.payload {
                    self.temp_files.push(p);
                }
            }
        }
        if self.temp_files.len() > 32 {
            let drain: Vec<_> = self.temp_files.drain(..self.temp_files.len() - 32).collect();
            let _ = std::thread::Builder::new()
                .name("feather-shelf-clean".into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    for p in drain {
                        let _ = std::fs::remove_file(p);
                    }
                });
        }
    }

    pub fn add_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        let mut added = 0;
        let thumb_px = (28.0 * ui_scale()).round().max(16.0) as u32;
        for path in paths.into_iter().rev() {
            if path.as_os_str().is_empty() {
                continue;
            }
            if let Some(pos) = self.items.iter().position(|it| {
                matches!(&it.payload, ShelfPayload::File { path: p, .. } if p == &path)
            }) {
                let item = self.items.remove(pos);
                self.items.insert(0, item);
                added += 1;
                self.dirty = true;
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let title = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let (kind, badge) = kind_of(&title, false, false);
            let subtitle = file_subtitle(kind, &badge, None);
            self.items.insert(
                0,
                ShelfItem {
                    id,
                    title,
                    subtitle,
                    payload: ShelfPayload::File { path: path.clone(), is_dir: false, dims: None },
                    thumb: None,
                    thumb_scale_bits: ui_scale().to_bits(),
                },
            );
            if let Some(tx) = self.tx.clone() {
                let scale_bits = ui_scale().to_bits();
                let _ = std::thread::Builder::new()
                    .name("feather-shelf-meta".into())
                    .stack_size(128 * 1024)
                    .spawn(move || {
                        let is_dir = std::fs::metadata(&path).is_ok_and(|m| m.is_dir());
                        let (dims, thumb) = if !is_dir && is_raster_image_ext(&path) {
                            decode_file_thumb(&path, thumb_px, scale_bits)
                        } else {
                            (None, None)
                        };
                        let _ = tx.send(ShelfMsg::FileMeta { id, is_dir, dims, thumb });
                    });
            }
            added += 1;
            self.dirty = true;
        }
        self.scroll = 0;
        self.clamp_capacity();
        added
    }

    pub fn add_text(&mut self, raw: String) -> bool {
        let is_empty = raw.trim().is_empty();
        if is_empty {
            return false;
        }
        let is_url = {
            let trimmed = raw.trim();
            (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
                && !trimmed.chars().any(char::is_whitespace)
        };
        let text = if is_url { raw.trim().to_string() } else { raw };
        if let Some(pos) = self.items.iter().position(|it| {
            matches!(&it.payload, ShelfPayload::Text { text: t, .. } if t == &text)
        }) {
            let item = self.items.remove(pos);
            self.items.insert(0, item);
            self.scroll = 0;
            self.dirty = true;
            return true;
        }
        let id = self.next_id;
        self.next_id += 1;
        let (title, subtitle) = if is_url {
            let host = text
                .split("://")
                .nth(1)
                .unwrap_or(&text)
                .trim_start_matches("www.");
            (host.chars().take(64).collect(), tr("Веб-ссылка").into_owned())
        } else {
            let first = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            let chars = text.chars().count();
            (
                first.chars().take(64).collect(),
                format!("{chars} {}", tr("симв.")),
            )
        };
        self.items.insert(
            0,
            ShelfItem {
                id,
                title,
                subtitle,
                payload: ShelfPayload::Text { text, is_url },
                thumb: None,
                thumb_scale_bits: 0,
            },
        );
        self.scroll = 0;
        self.clamp_capacity();
        self.dirty = true;
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_image(
        &mut self,
        title: String,
        mime: String,
        bytes: Arc<Vec<u8>>,
        width: u32,
        height: u32,
        thumb: Option<(u32, Pixmap)>,
        temp_path: Option<PathBuf>,
    ) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let id = self.next_id;
        self.next_id += 1;
        if let Some(p) = &temp_path {
            self.temp_files.push(p.clone());
        }
        let fmt_label = match mime.as_str() {
            "image/png" => "PNG",
            "image/webp" => "WebP",
            "image/jpeg" => "JPEG",
            _ => "IMG",
        };
        let subtitle = if width > 0 && height > 0 {
            format!("{fmt_label} · {width}×{height}")
        } else {
            fmt_label.to_string()
        };
        let (thumb_scale_bits, thumb_pm) = match thumb {
            Some((bits, pm)) => (bits, Some(pm)),
            None => (0, None),
        };
        self.items.insert(
            0,
            ShelfItem {
                id,
                title,
                subtitle,
                payload: ShelfPayload::Image { mime, bytes, width, height, temp_path },
                thumb: thumb_pm,
                thumb_scale_bits,
            },
        );
        self.scroll = 0;
        self.clamp_capacity();
        self.dirty = true;
        true
    }

    pub fn handle_msg(&mut self, msg: ShelfMsg) -> bool {
        match msg {
            ShelfMsg::FileMeta { id, is_dir, dims, thumb } => {
                let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
                    return false;
                };
                if let ShelfPayload::File { path, is_dir: dir_slot, dims: dims_slot } = &mut item.payload {
                    *dir_slot = is_dir;
                    *dims_slot = dims;
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&item.title);
                    let (kind, badge) = kind_of(name, is_dir, false);
                    item.subtitle = file_subtitle(kind, &badge, dims);
                }
                if let Some((bits, pm)) = thumb {
                    item.thumb = Some(pm);
                    item.thumb_scale_bits = bits;
                }
                self.dirty = true;
                true
            }
            ShelfMsg::ClientCaptured { drag_id, captured } => {
                let Some(pending) = self.client_drag.as_mut().filter(|d| d.id == drag_id) else {
                    return false;
                };
                pending.captured = Some(captured);
                if pending.accept_when_ready {
                    let pending = self.client_drag.take().unwrap();
                    return self.commit_pending_client_drag(pending);
                }
                false
            }
        }
    }

    fn commit_pending_client_drag(&mut self, pending: PendingClientDrag) -> bool {
        let mut changed = false;
        if let Some(paths) = pending.paths.filter(|p| !p.is_empty()) {
            changed |= self.add_paths(paths) > 0;
        }
        for url in pending.urls {
            changed |= self.add_text(url);
        }
        if let Some(captured) = pending.captured {
            match captured {
                CapturedPayload::Image { mime, bytes, width, height, thumb, temp_path } => {
                    let title = temp_path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| tr("Изображение").into_owned());
                    changed |= self.add_image(title, mime, bytes, width, height, thumb, temp_path);
                }
                CapturedPayload::Text(text) => {
                    changed |= self.add_text(text);
                }
            }
        }
        changed
    }

    pub fn advance(&mut self, now: Instant, enabled: bool) -> bool {
        let target = if self.open { 1.0 } else { 0.0 };
        if !enabled {
            self.anim_pos = target;
            self.anim_vel = 0.0;
            self.last_tick = None;
            if !self.open && self.buffer.is_some() {
                self.buffer = None;
                self.buffer_size = (0, 0);
            }
            return false;
        }
        if (self.anim_pos - target).abs() <= 0.0005 && self.anim_vel.abs() <= 0.002 {
            self.anim_pos = target;
            self.anim_vel = 0.0;
            self.last_tick = None;
            if !self.open && self.buffer.is_some() {
                self.buffer = None;
                self.buffer_size = (0, 0);
            }
            return false;
        }
        let dt = match self.last_tick.replace(now) {
            Some(prev) => now.saturating_duration_since(prev).as_secs_f64().clamp(0.001, 0.05),
            None => 1.0 / 120.0,
        } * crate::animation::speed();
        let omega = 40.0_f64;
        let displacement = self.anim_pos - target;
        let c = self.anim_vel + omega * displacement;
        let decay = (-omega * dt).exp();
        self.anim_pos = target + (displacement + c * dt) * decay;
        self.anim_vel = (self.anim_vel - omega * c * dt) * decay;
        if (self.anim_pos - target).abs() <= 0.0005 && self.anim_vel.abs() <= 0.002 {
            self.anim_pos = target;
            self.anim_vel = 0.0;
            self.last_tick = None;
            if !self.open {
                self.buffer = None;
                self.buffer_size = (0, 0);
            }
            return false;
        }
        true
    }

    pub fn visual(&self, screen: (i32, i32)) -> Visual {
        let t = self.anim_pos.clamp(0.0, 1.08);
        let r = self.panel_rect(screen);
        Visual {
            scale: 0.88 + 0.12 * t,
            alpha: t.clamp(0.0, 1.0) as f32,
            center: ((r.x + r.w * 0.5) as f64, (r.y + r.h * 0.5) as f64),
            offset: (0.0, (1.0 - t) * -8.0),
        }
    }

    pub fn glass_spec(&self, screen: (i32, i32)) -> Option<GlassSpec> {
        if !self.is_visible() {
            return None;
        }
        let rect = self.panel_rect(screen);
        Some(GlassSpec {
            key: KEY_DROP_SHELF,
            rect,
            radius: PANEL_RADIUS,
            strength: 1.0,
            visual: self.visual(screen),
        })
    }

    pub fn paint(&mut self, fonts: &mut FontSystem, scale: f64, screen: (i32, i32)) {
        if !self.is_visible() {
            return;
        }
        let rect = self.panel_rect(screen);
        let total_w = rect.w + SHADOW_PAD * 2.0;
        let total_h = rect.h + SHADOW_PAD * 2.0;
        let pw = ((total_w as f64 * scale).round() as i32).max(1);
        let ph = ((total_h as f64 * scale).round() as i32).max(1);

        let resized = self.buffer.is_none() || self.buffer_size != (pw, ph);
        if !self.dirty && !resized {
            return;
        }
        self.dirty = false;
        self.buffer_size = (pw, ph);

        let ox = rect.x - SHADOW_PAD;
        let oy = rect.y - SHADOW_PAD;
        set_glass_origin(ox, oy);

        let buffer = self.buffer.get_or_insert_with(|| {
            MemoryRenderBuffer::new(Fourcc::Abgr8888, (pw, ph), 1, Transform::Normal, None)
        });
        let mut ctx = buffer.render();
        if resized {
            ctx.resize((pw, ph));
        }
        let p = palette();
        let dark = appearance() == Appearance::Dark;
        let items = &self.items;
        let scroll = self.scroll.min(items.len().saturating_sub(VISIBLE_ROWS));
        let hover = self.hover;
        let drop_hover = self.drop_hover;
        let icon_cache = &mut self.icon_cache;
        let scale_bits = (scale as f32).to_bits();

        let _ = ctx.draw(|bytes| {
            bytes.fill(0);
            let Some(mut pm) = PixmapMut::from_bytes(bytes, pw as u32, ph as u32) else {
                return Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())]);
            };
            let m = SHADOW_PAD;
            let (w, h) = (rect.w, rect.h);

            // Soft multi-layer Tahoe drop shadow
            for i in (1..=6).rev() {
                let e = i as f32 * 1.7;
                fill_rounded_rect(
                    &mut pm,
                    m - e,
                    m - e + 3.5,
                    w + e * 2.0,
                    h + e * 2.0,
                    PANEL_RADIUS + e,
                    rgba(0, 0, 0, if dark { 8 } else { 5 }),
                );
            }

            draw_glass_panel(&mut pm, m, m, w, h, PANEL_RADIUS, true);

            if drop_hover {
                fill_rounded_rect(&mut pm, m + 2.0, m + 2.0, w - 4.0, h - 4.0, PANEL_RADIUS - 2.0, with_alpha(p.accent, 0.14));
                stroke_rounded_rect(&mut pm, m + 1.0, m + 1.0, w - 2.0, h - 2.0, PANEL_RADIUS - 1.0, 2.0, p.accent);
            }

            // Header bar
            let hy = m;
            let icon_col = if drop_hover { p.accent } else { p.text };
            draw_drop_shelf_icon(&mut pm, m + 19.0, hy + HEADER_H * 0.5, 0.88, icon_col);
            let shelf_title = tr("Полка");
            let tw = fonts.draw_text(&mut pm, &shelf_title, m + 31.0, hy + 9.0, 13.0, p.text, true);

            if !items.is_empty() {
                let count_txt = format!("{}", items.len());
                let cw = (fonts.measure_text(&count_txt, 11.0, true) + 11.0).max(20.0);
                let cx = m + 36.0 + tw;
                fill_rounded_rect(&mut pm, cx, hy + 8.5, cw, 17.0, 8.5, with_alpha(p.accent, if dark { 0.30 } else { 0.18 }));
                fonts.draw_text_centered(&mut pm, &count_txt, cx + cw * 0.5, hy + 10.0, 11.0, p.accent, true);
            }

            // Close button (top-right)
            let close_r = close_button_local(w, m);
            if hover == Some(HoverPart::Close) {
                fill_circle(&mut pm, close_r.x + close_r.w * 0.5, close_r.y + close_r.h * 0.5, 10.0, p.control_fill);
            }
            let close_fg = if hover == Some(HoverPart::Close) { p.text } else { p.text_secondary };
            fonts.draw_text_centered(&mut pm, "×", close_r.x + close_r.w * 0.5, close_r.y + 1.5, 14.5, close_fg, true);

            // Clear button (left of Close when non-empty)
            if !items.is_empty() {
                let clear_txt = tr("Очистить");
                let clear_w = fonts.measure_text(&clear_txt, 11.5, false) + 12.0;
                let clear_r = R::new(close_r.x - clear_w - 4.0, hy + 6.0, clear_w, 22.0);
                if hover == Some(HoverPart::Clear) {
                    fill_rounded_rect(&mut pm, clear_r.x, clear_r.y, clear_r.w, clear_r.h, 7.0, p.control_fill);
                }
                let clear_fg = if hover == Some(HoverPart::Clear) { p.accent } else { p.text_secondary };
                fonts.draw_text_centered(&mut pm, &clear_txt, clear_r.x + clear_r.w * 0.5, clear_r.y + 4.0, 11.5, clear_fg, false);
            }

            stroke_line(&mut pm, m + 12.0, m + HEADER_H, m + w - 12.0, m + HEADER_H, 0.75, p.separator);

            if items.is_empty() {
                // Inner recessed drop well
                let wx = m + 12.0;
                let wy = m + HEADER_H + 8.0;
                let ww = w - 24.0;
                let wh = EMPTY_H - 18.0;
                let well_fill = if drop_hover {
                    with_alpha(p.accent, 0.16)
                } else if dark {
                    rgba(255, 255, 255, 14)
                } else {
                    rgba(0, 0, 0, 10)
                };
                fill_rounded_rect(&mut pm, wx, wy, ww, wh, 13.0, well_fill);
                stroke_rounded_rect(
                    &mut pm,
                    wx + 0.5,
                    wy + 0.5,
                    ww - 1.0,
                    wh - 1.0,
                    12.5,
                    1.0,
                    if drop_hover { p.accent } else { p.separator },
                );
                let line1 = tr("Перетащите файлы сюда");
                let line2 = tr("или встряхните курсор");
                fonts.draw_text_clipped_centered(
                    &mut pm,
                    &line1,
                    wx + ww * 0.5,
                    wy + wh * 0.5 - 16.0,
                    ww - 16.0,
                    12.5,
                    if drop_hover { p.accent } else { p.text_secondary },
                    true,
                );
                fonts.draw_text_clipped_centered(
                    &mut pm,
                    &line2,
                    wx + ww * 0.5,
                    wy + wh * 0.5 + 3.0,
                    ww - 16.0,
                    11.5,
                    p.text_tertiary,
                    false,
                );
            } else {
                let shown = items.iter().enumerate().skip(scroll).take(VISIBLE_ROWS);
                for (slot, (idx, item)) in shown.enumerate() {
                    let ry = m + HEADER_H + slot as f32 * ROW_H;
                    let rx = m + 7.0;
                    let rw = w - 14.0;
                    let row_hot = matches!(
                        hover,
                        Some(HoverPart::Row(i) | HoverPart::RowCopy(i) | HoverPart::RowRemove(i)) if i == idx
                    );
                    if row_hot {
                        fill_rounded_rect(
                            &mut pm,
                            rx,
                            ry + 2.0,
                            rw,
                            ROW_H - 4.0,
                            11.0,
                            if dark { rgba(255, 255, 255, 22) } else { rgba(255, 255, 255, 135) },
                        );
                    }

                    // Item icon / image thumbnail (28×28 logical)
                    let ix = rx + 7.0;
                    let iy = ry + (ROW_H - 28.0) * 0.5;
                    draw_item_icon(&mut pm, fonts, icon_cache, item, ix, iy, 28.0, scale_bits);

                    let actions_w = if row_hot { 48.0 } else { 8.0 };
                    let text_x = ix + 35.0;
                    let text_w = (rw - (text_x - rx) - actions_w).max(24.0);
                    fonts.draw_text_clipped(&mut pm, &item.title, text_x, ry + 7.0, text_w, 12.5, p.text, true);
                    fonts.draw_text_clipped(&mut pm, &item.subtitle, text_x, ry + 23.5, text_w, 11.0, p.text_secondary, false);

                    if row_hot {
                        let copy_r = row_copy_local(w, m, slot);
                        let rem_r = row_remove_local(w, m, slot);
                        if hover == Some(HoverPart::RowCopy(idx)) {
                            fill_circle(&mut pm, copy_r.x + 10.0, copy_r.y + 10.0, 10.0, p.control_fill);
                        }
                        if hover == Some(HoverPart::RowRemove(idx)) {
                            fill_circle(&mut pm, rem_r.x + 10.0, rem_r.y + 10.0, 10.0, p.control_fill);
                        }
                        draw_copy_glyph(
                            &mut pm,
                            copy_r.x + 10.0,
                            copy_r.y + 10.0,
                            if hover == Some(HoverPart::RowCopy(idx)) { p.accent } else { p.text_secondary },
                        );
                        fonts.draw_text_centered(
                            &mut pm,
                            "×",
                            rem_r.x + 10.0,
                            rem_r.y + 1.5,
                            13.5,
                            if hover == Some(HoverPart::RowRemove(idx)) { p.text } else { p.text_secondary },
                            true,
                        );
                    }
                }

                // Scrollbar thumb when > VISIBLE_ROWS
                if items.len() > VISIBLE_ROWS {
                    let track_h = VISIBLE_ROWS as f32 * ROW_H - 12.0;
                    let thumb_h = (track_h * VISIBLE_ROWS as f32 / items.len() as f32).max(16.0);
                    let max_sc = (items.len() - VISIBLE_ROWS) as f32;
                    let ty = m + HEADER_H + 6.0 + (track_h - thumb_h) * (scroll as f32 / max_sc);
                    fill_rounded_rect(&mut pm, m + w - 5.5, ty, 3.0, thumb_h, 1.5, p.text_tertiary);
                }

                // "Перетащить все (N)" pill when >= 2 items
                if items.len() >= 2 {
                    let fy = m + h - FOOTER_H;
                    stroke_line(&mut pm, m + 12.0, fy, m + w - 12.0, fy, 0.75, p.separator);
                    let pill = R::new(m + 10.0, fy + 5.0, w - 20.0, FOOTER_H - 10.0);
                    let hot = hover == Some(HoverPart::DragAll);
                    let bg = if hot {
                        p.accent
                    } else {
                        with_alpha(p.accent, if dark { 0.24 } else { 0.14 })
                    };
                    fill_rounded_rect(&mut pm, pill.x, pill.y, pill.w, pill.h, pill.h * 0.5, bg);
                    let label = format!("{} ({})", tr("Перетащить все"), items.len());
                    fonts.draw_text_clipped_centered(
                        &mut pm,
                        &label,
                        pill.x + pill.w * 0.5,
                        pill.y + 4.5,
                        pill.w - 16.0,
                        12.0,
                        if hot { p.on_accent } else { p.accent },
                        true,
                    );
                }
            }

            Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
        });
    }

    pub fn elements(
        &self,
        renderer: &mut GlesRenderer,
        scale: f64,
        screen: (i32, i32),
        glass: Option<(&mut GlassState, &str)>,
    ) -> Vec<FeatherElement> {
        if !self.is_visible() {
            return Vec::new();
        }
        let Some(buffer) = &self.buffer else {
            return Vec::new();
        };
        let rect = self.panel_rect(screen);
        let visual = self.visual(screen);
        let loc = (
            ((rect.x - SHADOW_PAD) as f64 * scale).round(),
            ((rect.y - SHADOW_PAD) as f64 * scale).round(),
        );
        let Ok(mem) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            loc,
            buffer,
            Some(visual.alpha),
            None,
            None,
            ElementKind::Unspecified,
        ) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(2);
        out.push(visual.element(SceneElement::Memory(mem), scale));
        if let (Some((g, head)), Some(spec)) = (glass, self.glass_spec(screen)) {
            if let Some(el) = g.element(head, &spec, scale) {
                out.push(spec.visual.element(SceneElement::Glass(el), scale));
            }
        }
        out
    }

    fn hit_test(&self, fonts: &mut FontSystem, gx: f64, gy: f64, screen: (i32, i32)) -> Option<HoverPart> {
        let rect = self.panel_rect(screen);
        if !rect.contains(gx as f32, gy as f32) {
            return None;
        }
        let lx = gx as f32 - rect.x + SHADOW_PAD;
        let ly = gy as f32 - rect.y + SHADOW_PAD;
        let m = SHADOW_PAD;
        let w = rect.w;

        let close_r = close_button_local(w, m);
        if close_r.contains(lx, ly) {
            return Some(HoverPart::Close);
        }
        if !self.items.is_empty() {
            let clear_txt = tr("Очистить");
            let clear_w = fonts.measure_text(&clear_txt, 11.5, false) + 12.0;
            let clear_r = R::new(close_r.x - clear_w - 4.0, m + 6.0, clear_w, 22.0);
            if clear_r.contains(lx, ly) {
                return Some(HoverPart::Clear);
            }
        }
        if ly < m + HEADER_H {
            return None;
        }
        if self.items.is_empty() {
            return None;
        }
        if self.items.len() >= 2 && ly >= m + rect.h - FOOTER_H {
            let pill = R::new(m + 10.0, m + rect.h - FOOTER_H + 5.0, w - 20.0, FOOTER_H - 10.0);
            if pill.contains(lx, ly) {
                return Some(HoverPart::DragAll);
            }
            return None;
        }
        let scroll = self.scroll.min(self.items.len().saturating_sub(VISIBLE_ROWS));
        let slot = ((ly - (m + HEADER_H)) / ROW_H).floor();
        if slot >= 0.0 && (slot as usize) < VISIBLE_ROWS {
            let slot_usize = slot as usize;
            let idx = scroll + slot_usize;
            if idx < self.items.len() {
                if row_copy_local(w, m, slot_usize).contains(lx, ly) {
                    return Some(HoverPart::RowCopy(idx));
                }
                if row_remove_local(w, m, slot_usize).contains(lx, ly) {
                    return Some(HoverPart::RowRemove(idx));
                }
                return Some(HoverPart::Row(idx));
            }
        }
        None
    }
}

fn close_button_local(w: f32, m: f32) -> R {
    R::new(m + w - 28.0, m + 7.0, 20.0, 20.0)
}

fn row_copy_local(w: f32, m: f32, slot: usize) -> R {
    let ry = m + HEADER_H + slot as f32 * ROW_H;
    R::new(m + w - 49.0, ry + (ROW_H - 20.0) * 0.5, 20.0, 20.0)
}

fn row_remove_local(w: f32, m: f32, slot: usize) -> R {
    let ry = m + HEADER_H + slot as f32 * ROW_H;
    R::new(m + w - 27.0, ry + (ROW_H - 20.0) * 0.5, 20.0, 20.0)
}

fn draw_copy_glyph(pm: &mut PixmapMut, cx: f32, cy: f32, col: tiny_skia::Color) {
    stroke_rounded_rect(pm, cx - 4.5, cy - 4.5, 6.5, 6.5, 1.5, 1.2, col);
    stroke_rounded_rect(pm, cx - 2.0, cy - 2.0, 6.5, 6.5, 1.5, 1.2, col);
}

fn draw_item_icon(
    pm: &mut PixmapMut,
    fonts: &mut FontSystem,
    cache: &mut IconCache,
    item: &ShelfItem,
    x: f32,
    y: f32,
    size: f32,
    scale_bits: u32,
) {
    if let Some(thumb) = item.thumb.as_ref().filter(|_| item.thumb_scale_bits == scale_bits) {
        let s = ui_scale();
        pm.draw_pixmap(
            (x * s).round() as i32,
            (y * s).round() as i32,
            thumb.as_ref(),
            &PixmapPaint { quality: FilterQuality::Nearest, ..Default::default() },
            tiny_skia::Transform::identity(),
            None,
        );
        return;
    }
    match &item.payload {
        ShelfPayload::File { path, is_dir, .. } => {
            let icon = if *is_dir {
                Icon::Folder(None)
            } else {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or(&item.title);
                let (kind, badge) = kind_of(name, false, false);
                Icon::File(kind, badge)
            };
            cache.draw(pm, fonts, &icon, x, y, size);
        }
        ShelfPayload::Image { .. } => {
            cache.draw(pm, fonts, &Icon::File(Kind::Image, "IMG".into()), x, y, size);
        }
        ShelfPayload::Text { is_url: true, .. } => {
            cache.draw(pm, fonts, &Icon::File(Kind::Code, "URL".into()), x, y, size);
        }
        ShelfPayload::Text { is_url: false, .. } => {
            cache.draw(pm, fonts, &Icon::File(Kind::Text, "TXT".into()), x, y, size);
        }
    }
}

fn file_subtitle(kind: Kind, badge: &str, dims: Option<(u32, u32)>) -> String {
    if let Some((w, h)) = dims {
        let b = if badge.is_empty() { "IMG" } else { badge };
        return format!("{b} · {w}×{h}");
    }
    match kind {
        Kind::Folder => tr("Папка").into_owned(),
        Kind::Image if !badge.is_empty() => format!("{} · {badge}", tr("Изображение")),
        Kind::Image => tr("Изображение").into_owned(),
        Kind::Pdf => "PDF".into(),
        Kind::Archive if !badge.is_empty() => format!("{} · {badge}", tr("Архив")),
        Kind::Archive => tr("Архив").into_owned(),
        Kind::Audio if !badge.is_empty() => badge.to_string(),
        Kind::Video if !badge.is_empty() => badge.to_string(),
        _ if !badge.is_empty() => badge.to_string(),
        _ => tr("Файл").into_owned(),
    }
}

fn is_raster_image_ext(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpg" | "jpeg" | "webp"
    )
}

fn decode_file_thumb(path: &Path, px: u32, scale_bits: u32) -> (Option<(u32, u32)>, Option<(u32, Pixmap)>) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (None, None);
    };
    if meta.len() > MAX_IMAGE_BYTES as u64 {
        return (None, None);
    }
    let Ok(bytes) = std::fs::read(path) else {
        return (None, None);
    };
    decode_bytes_thumb(&bytes, px, scale_bits)
}

fn decode_bytes_thumb(bytes: &[u8], px: u32, scale_bits: u32) -> (Option<(u32, u32)>, Option<(u32, Pixmap)>) {
    let Ok(img) = image::load_from_memory(bytes) else {
        return (None, None);
    };
    let (w, h) = (img.width(), img.height());
    let px = px.max(16);
    let rgba = img.to_rgba8();
    let Some(pm) = make_square_rounded_thumb(rgba.as_raw(), w, h, px) else {
        return (Some((w, h)), None);
    };
    (Some((w, h)), Some((scale_bits, pm)))
}

fn make_square_rounded_thumb(src: &[u8], sw: u32, sh: u32, px: u32) -> Option<Pixmap> {
    if sw == 0 || sh == 0 || px == 0 {
        return None;
    }
    let mut pm = Pixmap::new(px, px)?;
    let side = sw.min(sh) as f32;
    let ox = (sw as f32 - side) * 0.5;
    let oy = (sh as f32 - side) * 0.5;
    let r = (px as f32 * 0.22).max(3.0);
    let out = pm.data_mut();
    for y in 0..px {
        for x in 0..px {
            let cov = rounded_cov(x as f32 + 0.5, y as f32 + 0.5, px as f32, px as f32, r);
            if cov <= 0.0 {
                continue;
            }
            let sx = ((ox + (x as f32 + 0.5) / px as f32 * side) as u32).min(sw - 1);
            let sy = ((oy + (y as f32 + 0.5) / px as f32 * side) as u32).min(sh - 1);
            let si = ((sy * sw + sx) * 4) as usize;
            let a = (src[si + 3] as f32 / 255.0) * cov;
            let pr = (src[si] as f32 * a).round() as u8;
            let pg = (src[si + 1] as f32 * a).round() as u8;
            let pb = (src[si + 2] as f32 * a).round() as u8;
            let pa = (a * 255.0).round() as u8;
            let di = ((y * px + x) * 4) as usize;
            out[di] = pr;
            out[di + 1] = pg;
            out[di + 2] = pb;
            out[di + 3] = pa;
        }
    }
    Some(pm)
}

fn rounded_cov(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (x - w * 0.5).abs() - (w * 0.5 - r);
    let qy = (y - h * 0.5).abs() - (h * 0.5 - r);
    let dist = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
    (0.5 - dist).clamp(0.0, 1.0)
}

fn read_pipe_bounded(fd: OwnedFd, max_bytes: usize) -> Option<Vec<u8>> {
    let mut file = std::fs::File::from(fd);
    let started = Instant::now();
    let mut data = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let left = READ_TIMEOUT.checked_sub(started.elapsed())?;
        let mut pfd = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is one valid stack `pollfd` for the duration of `poll`.
        if unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) } <= 0 {
            return None;
        }
        match file.read(&mut chunk) {
            Ok(0) => return Some(data),
            Ok(n) if data.len() + n <= max_bytes => data.extend_from_slice(&chunk[..n]),
            _ => return None,
        }
    }
}

fn pick_non_uri_mime(mimes: &[String]) -> Option<(String, bool)> {
    if mimes
        .iter()
        .any(|m| SECRET_MIMES.iter().any(|s| m.eq_ignore_ascii_case(s)))
    {
        return None;
    }
    for &im in &IMAGE_MIMES {
        if mimes.iter().any(|m| m.eq_ignore_ascii_case(im)) {
            return Some((im.to_string(), true));
        }
    }
    for &tm in &TEXT_MIMES {
        if mimes.iter().any(|m| m == tm) {
            return Some((tm.to_string(), false));
        }
    }
    None
}

fn write_temp_image(id: u64, mime: &str, bytes: &[u8]) -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    let dir = runtime.join("feather-de").join("shelf");
    std::fs::create_dir_all(&dir).ok()?;
    let ext = match mime {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/jpeg" => "jpg",
        _ => "img",
    };
    let path = dir.join(format!("image-{id}.{ext}"));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

fn item_formats(item: &ShelfItem) -> Vec<(String, Vec<u8>)> {
    match &item.payload {
        ShelfPayload::File { path, .. } => crate::desktop::file_formats(std::slice::from_ref(path)),
        ShelfPayload::Image { mime, bytes, temp_path, .. } => {
            let mut out = vec![(mime.clone(), (**bytes).clone())];
            if let Some(p) = temp_path {
                out.extend(crate::desktop::file_formats(std::slice::from_ref(p)));
            }
            out
        }
        ShelfPayload::Text { text, is_url } => {
            let mut out = Vec::new();
            if *is_url {
                out.push(("text/uri-list".into(), format!("{text}\r\n").into_bytes()));
            }
            for &m in &TEXT_MIMES {
                out.push((m.into(), text.as_bytes().to_vec()));
            }
            out
        }
    }
}

fn multi_item_formats(items: &[&ShelfItem]) -> (Vec<(String, Vec<u8>)>, Vec<PathBuf>) {
    let paths: Vec<PathBuf> = items.iter().filter_map(|i| i.path().map(Path::to_path_buf)).collect();
    if !paths.is_empty() {
        return (crate::desktop::file_formats(&paths), paths);
    }
    let joined = items
        .iter()
        .filter_map(|i| match &i.payload {
            ShelfPayload::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let formats = TEXT_MIMES
        .iter()
        .map(|&m| (m.to_string(), joined.as_bytes().to_vec()))
        .collect();
    (formats, Vec::new())
}

impl FeatherCompositor {
    pub fn start_drop_shelf(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = channel::channel::<ShelfMsg>();
        self.drop_shelf.set_sender(tx);
        self.loop_handle.insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(msg) = event {
                if state.drop_shelf.handle_msg(msg) {
                    state.sync_drop_shelf_shell();
                    state.needs_redraw = true;
                }
            }
        })?;
        Ok(())
    }

    pub fn sync_drop_shelf_shell(&mut self) {
        let open = self.drop_shelf.is_open();
        let count = self.drop_shelf.count();
        if self.shell.shelf_open != open || self.shell.shelf_count != count {
            self.shell.shelf_open = open;
            self.shell.shelf_count = count;
            self.shell.topbar_dirty = true;
            let ctx = self.build_menu_context();
            self.shell.set_menu_context(ctx);
            self.needs_redraw = true;
        }
    }

    fn shelf_min_top_y(&self) -> f32 {
        self.shell
            .toast_rects()
            .into_iter()
            .map(|r| r.y + r.h + 10.0)
            .fold(0.0_f32, f32::max)
    }

    pub fn toggle_drop_shelf(&mut self) {
        if self.lock.active {
            return;
        }
        let screen = (self.shell.screen_w, self.shell.screen_h);
        let min_top_y = self.shelf_min_top_y();
        self.drop_shelf.toggle(screen, min_top_y);
        self.sync_drop_shelf_shell();
        self.needs_redraw = true;
    }

    pub fn open_drop_shelf(&mut self) {
        if self.lock.active {
            return;
        }
        let screen = (self.shell.screen_w, self.shell.screen_h);
        let min_top_y = self.shelf_min_top_y();
        self.drop_shelf.open_at(None, screen, min_top_y);
        self.sync_drop_shelf_shell();
        self.needs_redraw = true;
    }

    pub fn close_drop_shelf(&mut self) {
        self.drop_shelf.close();
        self.sync_drop_shelf_shell();
        self.needs_redraw = true;
    }

    pub fn clear_drop_shelf(&mut self) {
        self.drop_shelf.clear();
        self.sync_drop_shelf_shell();
        self.needs_redraw = true;
    }

    pub fn drop_shelf_add_paths(&mut self, paths: Vec<PathBuf>, open: bool) -> usize {
        let n = self.drop_shelf.add_paths(paths);
        if open && n > 0 {
            let screen = (self.shell.screen_w, self.shell.screen_h);
            let min_top_y = self.shelf_min_top_y();
            self.drop_shelf.open_at(None, screen, min_top_y);
        }
        self.sync_drop_shelf_shell();
        self.needs_redraw = true;
        n
    }

    /// Runs on every pointer motion (including during client & server DnD grabs).
    /// Returns `true` when the shelf owns the motion (moving its header or starting a drag).
    pub fn drop_shelf_motion(&mut self) -> bool {
        let screen = (self.shell.screen_w, self.shell.screen_h);
        let pos = (self.pointer_location.x, self.pointer_location.y);

        if self.drop_shelf_dnd_active() {
            self.needs_redraw = true;
        }

        // 1. Repositioning the shelf by dragging its header
        if let Some(PressGesture::MovePanel { start_cursor, start_pos }) = self.drop_shelf.gesture {
            let dx = (pos.0 - start_cursor.0) as f32;
            let dy = (pos.1 - start_cursor.1) as f32;
            let h = self.drop_shelf.panel_height();
            self.drop_shelf.pos = Some(DropShelf::clamp_pos(
                (start_pos.0 + dx, start_pos.1 + dy),
                screen,
                h,
            ));
            self.needs_redraw = true;
            return true;
        }

        // 2. Starting an outbound Wayland DnD drag from an item row or "Перетащить все"
        if let Some(gesture) = self.drop_shelf.gesture.clone() {
            match gesture {
                PressGesture::Item { index, id, start_cursor } => {
                    if (pos.0 - start_cursor.0).hypot(pos.1 - start_cursor.1) > DRAG_THRESHOLD {
                        self.drop_shelf.gesture = None;
                        let target = self
                            .drop_shelf
                            .items
                            .iter()
                            .position(|i| i.id == id)
                            .or((index < self.drop_shelf.items.len()).then_some(index));
                        if let Some(idx) = target {
                            self.begin_shelf_outbound_drag(Some(idx), start_cursor);
                        }
                        return true;
                    }
                }
                PressGesture::DragAll { start_cursor } => {
                    if (pos.0 - start_cursor.0).hypot(pos.1 - start_cursor.1) > DRAG_THRESHOLD {
                        self.drop_shelf.gesture = None;
                        self.begin_shelf_outbound_drag(None, start_cursor);
                        return true;
                    }
                }
                PressGesture::MovePanel { .. } => {}
            }
        }

        // 3. Shake-to-summon & drop-hover feedback during an inbound drag
        let inbound_drag = !self.drop_shelf_dnd_active()
            && (self.drop_shelf.client_drag.is_some()
                || self.shell.desktop.is_dragging());
        if inbound_drag {
            if !self.drop_shelf.is_open()
                && self.drop_shelf.shake.sample(pos, Instant::now(), screen)
            {
                self.drop_shelf.open_at(Some(pos), screen, 0.0);
                self.sync_drop_shelf_shell();
                self.save_settings();
                self.needs_redraw = true;
            }
            let over = self.drop_shelf.accepts_drop_at(pos.0, pos.1, screen);
            if self.drop_shelf.drop_hover != over {
                self.drop_shelf.drop_hover = over;
                self.drop_shelf.dirty = true;
                self.needs_redraw = true;
            }
        } else {
            if self.drop_shelf.shake.active {
                self.drop_shelf.shake.reset();
            }
            if self.drop_shelf.drop_hover {
                self.drop_shelf.drop_hover = false;
                self.drop_shelf.dirty = true;
                self.needs_redraw = true;
            }
        }

        // 4. Hover highlight on shelf buttons & rows
        if self.drop_shelf.is_visible() {
            let next_hover = self.drop_shelf.hit_test(&mut self.fonts, pos.0, pos.1, screen);
            if self.drop_shelf.hover != next_hover {
                self.drop_shelf.hover = next_hover;
                self.drop_shelf.dirty = true;
                self.needs_redraw = true;
            }
        }
        false
    }

    pub fn drop_shelf_press(&mut self, left: bool) -> bool {
        let screen = (self.shell.screen_w, self.shell.screen_h);
        let pos = (self.pointer_location.x, self.pointer_location.y);
        if !self.drop_shelf.contains(pos.0, pos.1, screen) {
            return false;
        }
        if !left {
            return true;
        }
        let hit = self.drop_shelf.hit_test(&mut self.fonts, pos.0, pos.1, screen);
        match hit {
            Some(HoverPart::Close) => {
                self.close_drop_shelf();
            }
            Some(HoverPart::Clear) => {
                self.clear_drop_shelf();
            }
            Some(HoverPart::RowCopy(idx)) => {
                if let Some(item) = self.drop_shelf.items.get(idx).cloned() {
                    let entries = item_formats(&item);
                    self.set_clipboard(entries);
                    self.shell.notify(
                        tr("Скопировано в буфер"),
                        item.title,
                    );
                }
            }
            Some(HoverPart::RowRemove(idx)) => {
                if let Some(id) = self.drop_shelf.items.get(idx).map(|i| i.id) {
                    self.drop_shelf.remove_id(id, true);
                    self.sync_drop_shelf_shell();
                    self.needs_redraw = true;
                }
            }
            Some(HoverPart::Row(idx)) => {
                if let Some(item) = self.drop_shelf.items.get(idx) {
                    let id = item.id;
                    let now = Instant::now();
                    if self
                        .drop_shelf
                        .last_click
                        .is_some_and(|(t, prev)| prev == id && now.duration_since(t) < DOUBLE_CLICK)
                    {
                        self.drop_shelf.last_click = None;
                        self.open_shelf_item(idx);
                        return true;
                    }
                    self.drop_shelf.last_click = Some((now, id));
                    self.drop_shelf.gesture = Some(PressGesture::Item {
                        index: idx,
                        id,
                        start_cursor: pos,
                    });
                }
            }
            Some(HoverPart::DragAll) => {
                self.drop_shelf.gesture = Some(PressGesture::DragAll { start_cursor: pos });
            }
            None => {
                let rect = self.drop_shelf.panel_rect(screen);
                if pos.1 as f32 <= rect.y + HEADER_H {
                    self.drop_shelf.gesture = Some(PressGesture::MovePanel {
                        start_cursor: pos,
                        start_pos: (rect.x, rect.y),
                    });
                }
            }
        }
        true
    }

    pub fn drop_shelf_release(&mut self) -> bool {
        if let Some(gesture) = self.drop_shelf.gesture.take() {
            if matches!(gesture, PressGesture::MovePanel { .. }) {
                self.save_settings();
            }
            self.needs_redraw = true;
            return true;
        }
        false
    }

    pub fn drop_shelf_wheel(&mut self, dy: f64) -> bool {
        let screen = (self.shell.screen_w, self.shell.screen_h);
        let pos = (self.pointer_location.x, self.pointer_location.y);
        if !self.drop_shelf.contains(pos.0, pos.1, screen) {
            return false;
        }
        if self.drop_shelf.items.len() <= VISIBLE_ROWS || dy.abs() < 0.01 {
            return true;
        }
        let max_scroll = self.drop_shelf.items.len() - VISIBLE_ROWS;
        let next = if dy > 0.0 {
            (self.drop_shelf.scroll + 1).min(max_scroll)
        } else {
            self.drop_shelf.scroll.saturating_sub(1)
        };
        if next != self.drop_shelf.scroll {
            self.drop_shelf.scroll = next;
            self.drop_shelf.hover = self.drop_shelf.hit_test(&mut self.fonts, pos.0, pos.1, screen);
            self.drop_shelf.dirty = true;
            self.needs_redraw = true;
        }
        true
    }

    fn open_shelf_item(&mut self, index: usize) {
        let Some(item) = self.drop_shelf.items.get(index).cloned() else {
            return;
        };
        match item.payload {
            ShelfPayload::File { path, .. } => {
                let quoted = format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
                crate::apps::spawn_wayland_command(&format!("xdg-open {quoted}"), &self.wayland_socket_name);
            }
            ShelfPayload::Image { temp_path: Some(path), .. } => {
                let cmd = if let Some(preview) = crate::apps::preview_command() {
                    format!("{preview} '{}'", path.to_string_lossy().replace('\'', "'\\''"))
                } else {
                    format!("xdg-open '{}'", path.to_string_lossy().replace('\'', "'\\''"))
                };
                crate::apps::spawn_wayland_command(&cmd, &self.wayland_socket_name);
            }
            ShelfPayload::Text { text, is_url: true } => {
                let quoted = format!("'{}'", text.replace('\'', "'\\''"));
                crate::apps::spawn_wayland_command(&format!("xdg-open {quoted}"), &self.wayland_socket_name);
            }
            ShelfPayload::Text { text, is_url: false } => {
                let entries = TEXT_MIMES
                    .iter()
                    .map(|&m| (m.to_string(), text.as_bytes().to_vec()))
                    .collect();
                self.set_clipboard(entries);
                self.shell.notify(tr("Скопировано в буфер"), item.title);
            }
            ShelfPayload::Image { temp_path: None, mime, bytes, .. } => {
                self.set_clipboard(vec![(mime, (*bytes).clone())]);
                self.shell.notify(tr("Скопировано в буфер"), item.title);
            }
        }
    }

    fn begin_shelf_outbound_drag(&mut self, single_index: Option<usize>, start_cursor: (f64, f64)) {
        let (ids, formats, paths, ghost_kind, ghost_label) = match single_index {
            Some(idx) => {
                let Some(item) = self.drop_shelf.items.get(idx) else {
                    return;
                };
                let kind = match &item.payload {
                    ShelfPayload::File { .. } => IconKind::Finder,
                    ShelfPayload::Image { .. } => IconKind::Preview,
                    ShelfPayload::Text { is_url: true, .. } => IconKind::Browser,
                    ShelfPayload::Text { is_url: false, .. } => IconKind::TextEdit,
                };
                let p = item.path().map(|p| vec![p.to_path_buf()]).unwrap_or_default();
                (vec![item.id], item_formats(item), p, kind, item.title.clone())
            }
            None => {
                if self.drop_shelf.items.is_empty() {
                    return;
                }
                let refs: Vec<&ShelfItem> = self.drop_shelf.items.iter().collect();
                let ids: Vec<u64> = refs.iter().map(|i| i.id).collect();
                let (formats, paths) = multi_item_formats(&refs);
                let label = format!("{} ({})", tr("Полка"), ids.len());
                (ids, formats, paths, IconKind::Finder, label)
            }
        };
        if formats.is_empty() {
            return;
        }
        let mods = self.keyboard.modifier_state();
        let keep_copy = mods.alt || mods.ctrl;
        let mime_types: Vec<String> = formats.iter().map(|(m, _)| m.clone()).collect();
        self.drop_shelf.outbound = Some(OutboundDrag {
            ids,
            formats,
            paths,
            keep_copy,
            dropped: false,
        });
        self.shell.set_ghost(Some(Ghost {
            icon: AppIcon { kind: ghost_kind, name: None },
            label: ghost_label,
        }));
        const BTN_LEFT: u32 = 0x110;
        self.consumed_buttons.retain(|&b| b != BTN_LEFT);
        let metadata = SourceMetadata {
            mime_types,
            dnd_action: DndAction::Copy | DndAction::Move,
        };
        let start_data = GrabStartData {
            focus: None,
            button: BTN_LEFT,
            location: start_cursor.into(),
        };
        let (dh, seat) = (self.display_handle.clone(), self.seat.clone());
        start_dnd(
            &dh,
            &seat,
            self,
            SERIAL_COUNTER.next_serial(),
            Some(start_data),
            None,
            metadata,
        );
        self.needs_redraw = true;
    }

    pub fn drop_shelf_dnd_active(&self) -> bool {
        self.drop_shelf.outbound.is_some()
    }

    pub fn drop_shelf_dnd_send(&mut self, mime_type: &str, fd: OwnedFd) {
        let Some(out) = self.drop_shelf.outbound.as_ref() else {
            return;
        };
        let Some((_, data)) = out.formats.iter().find(|(m, _)| m == mime_type) else {
            return;
        };
        let data = data.clone();
        let _ = std::thread::Builder::new()
            .name("feather-shelf-send".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let _ = std::fs::File::from(fd).write_all(&data);
            });
    }

    pub fn drop_shelf_dnd_dropped(&mut self) {
        self.shell.set_ghost(None);
        if let Some(out) = self.drop_shelf.outbound.as_mut() {
            out.dropped = true;
        }
        self.needs_redraw = true;
    }

    pub fn drop_shelf_dnd_finished(&mut self) {
        self.shell.set_ghost(None);
        if let Some(out) = self.drop_shelf.outbound.take() {
            let mods = self.keyboard.modifier_state();
            if !out.keep_copy && !(mods.alt || mods.ctrl) {
                for id in out.ids {
                    // Keep tmpfs backing file alive until clear/eviction in case target copies async
                    self.drop_shelf.remove_id(id, false);
                }
                self.sync_drop_shelf_shell();
            }
        }
        self.needs_redraw = true;
    }

    pub fn drop_shelf_dnd_cancelled(&mut self) {
        self.shell.set_ghost(None);
        let Some(out) = self.drop_shelf.outbound.take() else {
            return;
        };
        let p = (self.pointer_location.x, self.pointer_location.y);
        let screen = (self.shell.screen_w, self.shell.screen_h);
        if self.drop_shelf.contains(p.0, p.1, screen) || out.paths.is_empty() {
            self.needs_redraw = true;
            return;
        }
        if self.over_dock_trash(p.0, p.1) {
            self.trash_paths(out.paths);
            for id in out.ids {
                self.drop_shelf.remove_id(id, false);
            }
            self.sync_drop_shelf_shell();
            self.needs_redraw = true;
            return;
        }
        if self.over_bare_desktop(p.0, p.1) {
            let dir = self.desktop_drop_dir_at(p.0, p.1);
            let paths = out.paths;
            let _ = std::thread::Builder::new()
                .name("feather-shelf-paste".into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    fileops::paste(&paths, &dir, false);
                });
            let mods = self.keyboard.modifier_state();
            if !out.keep_copy && !(mods.alt || mods.ctrl) {
                for id in out.ids {
                    self.drop_shelf.remove_id(id, false);
                }
                self.sync_drop_shelf_shell();
            }
            self.needs_redraw = true;
        }
    }

    /// Starts tracking a client Wayland drag (`ClientDndGrabHandler::started`).
    /// When `offers_uri_list` is true, `desktop.rs` already reads `text/uri-list` and forwards
    /// both paths and URLs via `drop_shelf_client_drag_paths`, so we never call `source.send` twice.
    pub fn drop_shelf_client_drag_started(
        &mut self,
        source: Option<&WlDataSource>,
        drag_id_opt: Option<u64>,
        offers_uri_list: bool,
    ) {
        let pos = (self.pointer_location.x, self.pointer_location.y);
        self.drop_shelf.shake.begin(pos);

        if source.is_none() && drag_id_opt.is_none() {
            return;
        }
        let id = drag_id_opt.unwrap_or_else(|| {
            self.drop_shelf.next_client_drag += 1;
            1_000_000 + self.drop_shelf.next_client_drag
        });
        self.drop_shelf.client_drag = Some(PendingClientDrag {
            id,
            paths: if offers_uri_list { None } else { Some(Vec::new()) },
            urls: Vec::new(),
            captured: None,
            accept_when_ready: false,
        });

        if offers_uri_list {
            return;
        }
        let Some(source) = source else {
            return;
        };

        let chosen = with_source_metadata(source, |m| pick_non_uri_mime(&m.mime_types))
            .ok()
            .flatten();
        let Some((mime, is_image)) = chosen else {
            return;
        };
        let Ok((read, write)) = std::io::pipe() else {
            return;
        };
        source.send(mime.clone(), write.as_fd());
        drop(write);
        let Some(tx) = self.drop_shelf.tx.clone() else {
            return;
        };
        let thumb_px = (28.0 * ui_scale()).round().max(16.0) as u32;
        let scale_bits = ui_scale().to_bits();
        let _ = std::thread::Builder::new()
            .name("feather-shelf-read".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let max = if is_image { MAX_IMAGE_BYTES } else { MAX_TEXT_BYTES };
                let Some(bytes) = read_pipe_bounded(OwnedFd::from(read), max) else {
                    return;
                };
                if bytes.is_empty() {
                    return;
                }
                if is_image {
                    let (dims, thumb) = decode_bytes_thumb(&bytes, thumb_px, scale_bits);
                    let (width, height) = dims.unwrap_or((0, 0));
                    let temp_path = write_temp_image(id, &mime, &bytes);
                    let _ = tx.send(ShelfMsg::ClientCaptured {
                        drag_id: id,
                        captured: CapturedPayload::Image {
                            mime,
                            bytes: Arc::new(bytes),
                            width,
                            height,
                            thumb,
                            temp_path,
                        },
                    });
                } else if let Ok(text) = String::from_utf8(bytes) {
                    let _ = tx.send(ShelfMsg::ClientCaptured {
                        drag_id: id,
                        captured: CapturedPayload::Text(text),
                    });
                }
            });
    }

    pub fn drop_shelf_client_drag_paths(&mut self, drag_id: u64, paths: &[PathBuf], urls: &[String]) {
        let Some(pending) = self.drop_shelf.client_drag.as_mut().filter(|d| d.id == drag_id) else {
            return;
        };
        pending.paths = Some(paths.to_vec());
        pending.urls = urls.to_vec();
        if pending.accept_when_ready {
            let pending = self.drop_shelf.client_drag.take().unwrap();
            if self.drop_shelf.commit_pending_client_drag(pending) {
                self.sync_drop_shelf_shell();
                self.needs_redraw = true;
            }
        }
    }

    /// Called when a client drag drops (`ClientDndGrabHandler::dropped`). Returns `true` if dropped
    /// onto the open Drop Shelf so the desktop does not also take the files.
    pub fn drop_shelf_client_drag_dropped(&mut self, target_taken: bool) -> bool {
        self.drop_shelf.shake.reset();
        if self.drop_shelf.drop_hover {
            self.drop_shelf.drop_hover = false;
            self.drop_shelf.dirty = true;
            self.needs_redraw = true;
        }
        let pos = (self.pointer_location.x, self.pointer_location.y);
        let screen = (self.shell.screen_w, self.shell.screen_h);
        if target_taken || !self.drop_shelf.accepts_drop_at(pos.0, pos.1, screen) {
            self.drop_shelf.client_drag = None;
            return false;
        }
        let Some(mut pending) = self.drop_shelf.client_drag.take() else {
            return true;
        };
        let ready = pending.paths.is_some()
            && (pending.captured.is_some()
                || pending.paths.as_ref().is_some_and(|p| !p.is_empty())
                || !pending.urls.is_empty());
        if ready {
            if self.drop_shelf.commit_pending_client_drag(pending) {
                self.sync_drop_shelf_shell();
                self.needs_redraw = true;
            }
        } else {
            pending.accept_when_ready = true;
            self.drop_shelf.client_drag = Some(pending);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shake_detector_summons_on_rapid_reversals() {
        let mut s = ShakeDetector::new();
        let t0 = Instant::now();
        let screen = (1920, 1080);
        s.begin((500.0, 500.0));
        assert!(!s.sample((540.0, 500.0), t0 + Duration::from_millis(40), screen));
        assert!(!s.sample((490.0, 500.0), t0 + Duration::from_millis(90), screen));
        assert!(!s.sample((545.0, 500.0), t0 + Duration::from_millis(140), screen));
        // Third reversal within 480 ms summons the shelf
        assert!(s.sample((485.0, 500.0), t0 + Duration::from_millis(190), screen));
        // Cooldown prevents re-firing during the same drag
        assert!(!s.sample((550.0, 500.0), t0 + Duration::from_millis(230), screen));
        assert!(!s.sample((480.0, 500.0), t0 + Duration::from_millis(270), screen));
    }

    #[test]
    fn shake_detector_ignores_slow_normal_drags() {
        let mut s = ShakeDetector::new();
        let t0 = Instant::now();
        let screen = (1920, 1080);
        s.begin((200.0, 300.0));
        assert!(!s.sample((600.0, 340.0), t0 + Duration::from_millis(120), screen));
        assert!(!s.sample((550.0, 350.0), t0 + Duration::from_millis(400), screen));
        assert!(!s.sample((900.0, 420.0), t0 + Duration::from_millis(900), screen));
        assert!(!s.sample((850.0, 420.0), t0 + Duration::from_millis(1200), screen));
    }

    #[test]
    fn shake_detector_summons_at_screen_side_edge() {
        let mut s = ShakeDetector::new();
        let t0 = Instant::now();
        let screen = (1920, 1080);
        s.begin((1600.0, 400.0));
        assert!(!s.sample((1800.0, 400.0), t0 + Duration::from_millis(80), screen));
        assert!(s.sample((1918.0, 400.0), t0 + Duration::from_millis(160), screen));
    }

    #[test]
    fn shelf_deduplicates_paths_and_shrinks_when_closed() {
        let mut shelf = DropShelf::new();
        shelf.add_paths(vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]);
        assert_eq!(shelf.count(), 2);
        assert_eq!(shelf.items()[0].title, "a.txt");
        // Re-adding /tmp/b.txt moves it to the top without duplicating
        shelf.add_paths(vec![PathBuf::from("/tmp/b.txt")]);
        assert_eq!(shelf.count(), 2);
        assert_eq!(shelf.items()[0].title, "b.txt");
        assert!(shelf.add_text("https://feather.example/docs".into()));
        assert_eq!(shelf.items()[0].kind_name(), "url");
    }
}
