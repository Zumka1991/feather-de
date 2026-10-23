//! Desktop icons: the XDG desktop folder shown on the wallpaper, Finder-style grid from the
//! top-right corner, click/Ctrl/Shift selection, rubber-band selection, Enter/double-click
//! to open, Delete to trash, a context menu, dragging icons to new places (remembered) and
//! drag-and-drop with programs: icons are offered as `text/uri-list` (server-side DnD),
//! and files dropped from programs onto the desktop or a desktop folder are moved/copied.
//!
//! The folder is read on a short-lived thread and watched with inotify (no polling). Tiles
//! (icon + label, plain and selected) are rasterized on the raster worker, uploaded once to
//! GL textures and their CPU pixels dropped, so icons cost GPU memory, not RssAnon. Selecting
//! or dragging only reuses those textures; nothing is repainted on the CPU.

use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    os::fd::AsFd,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use feather_ui::{
    file_icons::{Icon, IconCache},
    file_kind::{kind_of, Kind},
    fileops,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                texture::{TextureBuffer, TextureRenderElement},
                Kind as ElementKind,
            },
            gles::{GlesRenderer, GlesTexture},
            ImportMem,
        },
    },
    input::pointer::GrabStartData,
    reexports::{
        calloop::{
            channel,
            generic::Generic,
            timer::{TimeoutAction, Timer},
            Interest, Mode, PostAction,
        },
        wayland_server::protocol::{wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_surface::WlSurface},
    },
    utils::{Rectangle, Transform, SERIAL_COUNTER},
    wayland::selection::data_device::{
        current_data_device_selection_userdata, request_data_device_client_selection, start_dnd, with_source_metadata, SourceMetadata,
    },
};

use crate::{
    render::{FeatherElement, SceneElement},
    theme::{rgba, ui_scale, DOCK_RESERVED_HEIGHT, TOP_BAR_HEIGHT},
    FeatherCompositor,
};

pub const CELL_W: f32 = 96.0;
pub const CELL_H: f32 = 108.0;
const ICON: f32 = 64.0;
const MARGIN_RIGHT: f32 = 14.0;
const MARGIN_TOP: f32 = 10.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const DRAG_START: f64 = 4.0;
const URI_LIST: &str = "text/uri-list";
const BTN_LEFT: u32 = 0x110;

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub kind: Kind,
    pub ext: String,
}

impl Item {
    fn icon(&self) -> Icon {
        if self.is_dir { Icon::Folder(None) } else { Icon::File(self.kind, self.ext.clone()) }
    }
}

/// Raster worker job: one item drawn plain and selected at the current scale
pub struct TileJob {
    pub path: PathBuf,
    pub name: String,
    pub icon: Icon,
}

/// Worker result: two RGBA images of the cell size
pub struct TilePixels {
    pub path: PathBuf,
    pub name: String,
    pub size: (i32, i32),
    pub plain: Vec<u8>,
    pub selected: Vec<u8>,
}

struct Tile {
    name: String,
    plain: TextureBuffer<GlesTexture>,
    selected: TextureBuffer<GlesTexture>,
}

/// Grid cell: column counted from the right edge, row from the top
pub type Cell = (usize, usize);

/// Results of short desktop threads
pub enum Msg {
    Listed { generation: u64, items: Vec<Item>, positions: HashMap<String, Cell> },
    /// Paths offered by a program that started a drag (read when the drag begins)
    DragPaths { drag: u64, paths: Vec<PathBuf> },
    /// Files read from the clipboard for "Вставить"
    Paste(Vec<PathBuf>),
}

/// Icons being dragged out of the desktop
#[derive(Clone)]
struct IconDrag {
    start: (f64, f64),
    paths: Vec<PathBuf>,
    active: bool,
}

/// A drag started by a program; the desktop may become its drop target
struct ClientDrag {
    id: u64,
    paths: Option<Vec<PathBuf>>,
    /// Set when dropped over the desktop: the folder to receive the files
    drop_into: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Open,
    ShowInFiles,
    Rename,
    Copy,
    Trash,
    NewFolder,
    Paste,
    OpenDesktop,
    Terminal,
    CleanUp,
    Wallpaper,
}

pub struct Desktop {
    pub dir: PathBuf,
    items: Vec<Item>,
    pub selected: HashSet<PathBuf>,
    tiles: HashMap<PathBuf, Tile>,
    uploads: Vec<TilePixels>,
    requested: HashSet<PathBuf>,
    /// Remembered places by item name; others fill free cells in order
    positions: HashMap<String, Cell>,
    cells: Vec<Cell>,
    /// (rows, columns) the cells were computed for; None after a change
    layout: Option<(usize, usize)>,
    /// Rubber band in logical screen coordinates (anchor, current)
    rubber: Option<((f64, f64), (f64, f64))>,
    rubber_base: HashSet<PathBuf>,
    /// Fill and four edges of the rubber band (1×1 px, stretched)
    band: [MemoryRenderBuffer; 5],
    last_click: Option<(Instant, PathBuf)>,
    anchor: Option<usize>,
    drag: Option<IconDrag>,
    /// A drop of our icons that a program may still accept (it reads the data first)
    dropped: Option<IconDrag>,
    client_drag: Option<ClientDrag>,
    next_drag: u64,
    /// The drag icon surface of a program's drag, drawn at the pointer
    pub dnd_icon: Option<WlSurface>,
    /// Desktop folder under a drag: highlighted as the drop target
    hover_folder: Option<PathBuf>,
    /// Keyboard goes to the desktop (no window focused) after a click on it
    pub focused: bool,
    watcher: Option<fileops::Watcher>,
    reload_armed: bool,
    generation: u64,
}

impl Desktop {
    pub fn new() -> Self {
        let pixel = |i: usize| {
            // Premultiplied: accent fill at 18 %, accent edge at 85 %
            let rgba: [u8; 4] = if i == 0 { [2, 24, 46, 46] } else { [8, 112, 217, 217] };
            MemoryRenderBuffer::from_slice(&rgba, Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None)
        };
        Self {
            dir: fileops::user_dir("DESKTOP", "Desktop"),
            items: Vec::new(),
            selected: HashSet::new(),
            tiles: HashMap::new(),
            uploads: Vec::new(),
            requested: HashSet::new(),
            positions: HashMap::new(),
            cells: Vec::new(),
            layout: None,
            rubber: None,
            rubber_base: HashSet::new(),
            band: std::array::from_fn(pixel),
            last_click: None,
            anchor: None,
            drag: None,
            dropped: None,
            client_drag: None,
            next_drag: 0,
            dnd_icon: None,
            hover_folder: None,
            focused: false,
            watcher: None,
            reload_armed: false,
            generation: 0,
        }
    }

    /// Visual tests without input: selects the first two icons and shows a rubber band
    pub fn debug_select(&mut self, screen: (i32, i32)) {
        self.ensure_layout(screen);
        self.selected = self.items.iter().take(2).map(|i| i.path.clone()).collect();
        let (x, y) = self.position(3.min(self.items.len().saturating_sub(1)), screen);
        self.rubber = Some(((x - 40.0, y + 10.0), (x + 60.0, y + 150.0)));
    }

    pub fn debug_index(&self, name: &str) -> Option<usize> {
        self.items.iter().position(|i| i.name == name)
    }

    /// Scale/appearance changed: tiles are rebuilt lazily
    pub fn invalidate(&mut self) {
        self.tiles.clear();
        self.uploads.clear();
        self.requested.clear();
        self.layout = None;
    }

    fn set_items(&mut self, items: Vec<Item>, positions: HashMap<String, Cell>) {
        let keep: HashSet<&PathBuf> = items.iter().map(|i| &i.path).collect();
        self.tiles.retain(|p, _| keep.contains(p));
        self.selected.retain(|p| keep.contains(p));
        self.requested.retain(|p| keep.contains(p));
        // A renamed item keeps its path only if the name is the same; otherwise redraw it
        for item in &items {
            if self.tiles.get(&item.path).is_some_and(|t| t.name != item.name) {
                self.tiles.remove(&item.path);
                self.requested.remove(&item.path);
            }
        }
        self.items = items;
        self.positions = positions;
        self.anchor = None;
        self.layout = None;
    }

    fn grid(screen: (i32, i32)) -> (usize, usize) {
        let top = TOP_BAR_HEIGHT as f32 + MARGIN_TOP;
        let rows = (((screen.1 as f32 - top - DOCK_RESERVED_HEIGHT as f32) / CELL_H).floor() as usize).max(1);
        let cols = (((screen.0 as f32 - MARGIN_RIGHT) / CELL_W).floor() as usize).max(1);
        (rows, cols)
    }

    fn cell_origin(cell: Cell, screen: (i32, i32)) -> (f64, f64) {
        let top = TOP_BAR_HEIGHT as f32 + MARGIN_TOP;
        let x = screen.0 as f32 - MARGIN_RIGHT - CELL_W * (cell.0 as f32 + 1.0);
        (x as f64, (top + cell.1 as f32 * CELL_H) as f64)
    }

    /// Nearest grid cell to a logical point (the centre of a tile)
    fn cell_at(x: f64, y: f64, screen: (i32, i32)) -> Cell {
        let (rows, cols) = Self::grid(screen);
        let top = (TOP_BAR_HEIGHT as f32 + MARGIN_TOP) as f64;
        let col = ((screen.0 as f64 - MARGIN_RIGHT as f64 - x) / CELL_W as f64).floor().clamp(0.0, (cols - 1) as f64);
        let row = ((y - top) / CELL_H as f64).floor().clamp(0.0, (rows - 1) as f64);
        (col as usize, row as usize)
    }

    /// Remembered places first, then the remaining items in free cells, column by column
    pub fn ensure_layout(&mut self, screen: (i32, i32)) {
        let grid = Self::grid(screen);
        if self.layout == Some(grid) && self.cells.len() == self.items.len() {
            return;
        }
        let (rows, cols) = grid;
        let mut taken = HashSet::new();
        let mut cells: Vec<Option<Cell>> = self
            .items
            .iter()
            .map(|item| self.positions.get(&item.name).copied().filter(|c| c.0 < cols && c.1 < rows && taken.insert(*c)))
            .collect();
        let mut next = 0;
        for cell in cells.iter_mut().filter(|c| c.is_none()) {
            while taken.contains(&(next / rows, next % rows)) {
                next += 1;
            }
            let free = (next / rows, next % rows);
            taken.insert(free);
            *cell = Some(free);
        }
        self.cells = cells.into_iter().map(Option::unwrap).collect();
        self.layout = Some(grid);
    }

    /// Top-left of item `i` (logical)
    pub fn position(&self, i: usize, screen: (i32, i32)) -> (f64, f64) {
        let cell = self.cells.get(i).copied().unwrap_or_else(|| {
            let rows = Self::grid(screen).0;
            (i / rows, i % rows)
        });
        Self::cell_origin(cell, screen)
    }

    /// Item whose icon or label is under the point (the cell margins are not hot)
    pub fn item_at(&self, x: f64, y: f64, screen: (i32, i32)) -> Option<usize> {
        (0..self.items.len()).find(|&i| {
            let (ix, iy) = self.position(i, screen);
            let (lx, ly) = (x - ix, y - iy);
            (8.0..CELL_W as f64 - 8.0).contains(&lx) && (0.0..CELL_H as f64 - 4.0).contains(&ly)
        })
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.items.iter().filter(|i| self.selected.contains(&i.path)).map(|i| i.path.clone()).collect()
    }

    /// Moves dragged items by whole cells; occupied targets take the nearest free cell.
    /// All other icons are pinned where they are, so nothing reflows unexpectedly.
    fn move_items(&mut self, paths: &[PathBuf], delta: (isize, isize), screen: (i32, i32)) {
        self.ensure_layout(screen);
        let (rows, cols) = Self::grid(screen);
        let moving: HashSet<&PathBuf> = paths.iter().collect();
        let mut taken: HashSet<Cell> = self.items.iter().zip(&self.cells).filter(|(i, _)| !moving.contains(&i.path)).map(|(_, c)| *c).collect();
        let mut positions: HashMap<String, Cell> = self.items.iter().zip(&self.cells).map(|(i, c)| (i.name.clone(), *c)).collect();
        for (item, cell) in self.items.iter().zip(&self.cells).filter(|(i, _)| moving.contains(&i.path)) {
            let want = (
                (cell.0 as isize + delta.0).clamp(0, cols as isize - 1) as usize,
                (cell.1 as isize + delta.1).clamp(0, rows as isize - 1) as usize,
            );
            let free = (0..(rows.max(cols) as isize))
                .flat_map(|r| {
                    (-r..=r).flat_map(move |dc| (-r..=r).map(move |dr| (dc, dr))).filter(move |(dc, dr)| dc.abs().max(dr.abs()) == r)
                })
                .map(|(dc, dr)| (want.0 as isize + dc, want.1 as isize + dr))
                .filter(|(c, r)| *c >= 0 && *r >= 0 && (*c as usize) < cols && (*r as usize) < rows)
                .map(|(c, r)| (c as usize, r as usize))
                .find(|c| !taken.contains(c));
            if let Some(free) = free {
                taken.insert(free);
                positions.insert(item.name.clone(), free);
            }
        }
        self.positions = positions;
        self.layout = None;
        self.ensure_layout(screen);
        save_positions(&self.positions);
    }

    /// Queues missing tiles on the raster worker
    pub fn request_tiles(&mut self, worker: Option<&crate::raster::RasterSender>) {
        let Some(worker) = worker else { return };
        for item in &self.items {
            if self.tiles.contains_key(&item.path) || self.requested.contains(&item.path) {
                continue;
            }
            let job = TileJob { path: item.path.clone(), name: item.name.clone(), icon: item.icon() };
            if !worker.submit(crate::raster::Job::DesktopTile(job)) {
                break; // queue full: retried on the next frame
            }
            self.requested.insert(item.path.clone());
        }
    }

    pub fn install(&mut self, pixels: TilePixels) -> bool {
        if !self.items.iter().any(|i| i.path == pixels.path && i.name == pixels.name) {
            self.requested.remove(&pixels.path);
            return false;
        }
        self.uploads.push(pixels);
        true
    }

    /// Moves finished tiles into GL textures and drops their CPU copies. Returns true when
    /// the last pending tile arrived: the worker can then return its scratch memory.
    pub fn upload(&mut self, renderer: &mut GlesRenderer) -> bool {
        if self.uploads.is_empty() {
            return false;
        }
        for pixels in self.uploads.drain(..) {
            let size = pixels.size.into();
            let import = |data: &[u8], renderer: &mut GlesRenderer| {
                renderer
                    .import_memory(data, Fourcc::Abgr8888, size, false)
                    .ok()
                    .map(|t| TextureBuffer::from_texture(renderer, t, 1, Transform::Normal, None))
            };
            if let (Some(plain), Some(selected)) = (import(&pixels.plain, renderer), import(&pixels.selected, renderer)) {
                self.requested.remove(&pixels.path);
                self.tiles.insert(pixels.path, Tile { name: pixels.name, plain, selected });
            }
        }
        self.requested.is_empty()
    }

    pub fn elements(&self, renderer: &mut GlesRenderer, screen: (i32, i32), scale: f64) -> Vec<FeatherElement> {
        let mut out = Vec::new();
        if let Some(((ax, ay), (bx, by))) = self.rubber {
            let r = |v: f64| (v * scale).round() as i32;
            let (x0, y0, x1, y1) = (r(ax.min(bx)), r(ay.min(by)), r(ax.max(bx)), r(ay.max(by)));
            let b = (scale.round() as i32).max(1);
            let src = Rectangle::new((0.0, 0.0).into(), (1.0, 1.0).into());
            let rects = [(x0, y0, x1 - x0, b), (x0, y1 - b, x1 - x0, b), (x0, y0, b, y1 - y0), (x1 - b, y0, b, y1 - y0), (x0, y0, x1 - x0, y1 - y0)];
            for (k, (x, y, w, h)) in rects.into_iter().enumerate() {
                if w <= 0 || h <= 0 {
                    continue;
                }
                let buffer = &self.band[if k == 4 { 0 } else { k + 1 }];
                if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(renderer, (x as f64, y as f64), buffer, None, Some(src), Some((w, h).into()), ElementKind::Unspecified) {
                    out.push(SceneElement::Memory(e).into());
                }
            }
        }
        // A folder under a drag lights up as the drop target
        let target = self.drop_target_folder();
        for (i, item) in self.items.iter().enumerate() {
            let Some(tile) = self.tiles.get(&item.path) else { continue };
            let (x, y) = self.position(i, screen);
            let lit = self.selected.contains(&item.path) || target.as_ref() == Some(&item.path);
            let buffer = if lit { &tile.selected } else { &tile.plain };
            let loc = ((x * scale).round(), (y * scale).round());
            out.push(SceneElement::Texture(TextureRenderElement::from_texture_buffer(loc, buffer, None, None, None, ElementKind::Unspecified)).into());
        }
        out
    }

    /// Dragged icons follow the pointer above everything (windows included)
    pub fn drag_elements(&self, screen: (i32, i32), pointer: (f64, f64), scale: f64) -> Vec<FeatherElement> {
        let Some(drag) = self.drag.as_ref().filter(|d| d.active) else { return Vec::new() };
        let delta = (pointer.0 - drag.start.0, pointer.1 - drag.start.1);
        let mut out = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            if !drag.paths.contains(&item.path) {
                continue;
            }
            let Some(tile) = self.tiles.get(&item.path) else { continue };
            let (x, y) = self.position(i, screen);
            let loc = (((x + delta.0) * scale).round(), ((y + delta.1) * scale).round());
            out.push(SceneElement::Texture(TextureRenderElement::from_texture_buffer(loc, &tile.selected, Some(0.72), None, None, ElementKind::Unspecified)).into());
        }
        out
    }

    /// Folder under the pointer during a drag (not one of the dragged items)
    fn drop_target_folder(&self) -> Option<PathBuf> {
        self.hover_folder.clone()
    }
}

// The drop-target highlight is computed on motion (needs the pointer and screen)
impl Desktop {
    fn update_hover_folder(&mut self, pointer: (f64, f64), screen: (i32, i32), over_desktop: bool) -> bool {
        let dragging = self.drag.as_ref().is_some_and(|d| d.active) || self.client_drag.is_some();
        let folder = if dragging && over_desktop {
            self.item_at(pointer.0, pointer.1, screen)
                .map(|i| &self.items[i])
                .filter(|i| i.is_dir && !self.drag.as_ref().is_some_and(|d| d.paths.contains(&i.path)))
                .map(|i| i.path.clone())
        } else {
            None
        };
        let changed = folder != self.hover_folder;
        self.hover_folder = folder;
        changed
    }
}

/// Worker side: icon and a two-line label, plain (white text with a soft shadow) and
/// selected (grey plate behind the icon, accent pill behind the label)
pub fn raster_tile(job: &TileJob, fonts: &mut crate::text::FontSystem, icons: &mut IconCache) -> TilePixels {
    let s = ui_scale();
    let (w, h) = ((CELL_W * s).round() as i32, (CELL_H * s).round() as i32);
    let lines = fonts.wrap_two_lines(&job.name, CELL_W - 10.0, 12.0, false);
    let draw = |selected: bool, fonts: &mut crate::text::FontSystem, icons: &mut IconCache| {
        let mut pm = tiny_skia::Pixmap::new(w as u32, h as u32).expect("tile size");
        let mut m = pm.as_mut();
        let ix = (CELL_W - ICON) * 0.5;
        if selected {
            crate::theme::fill_rounded_rect(&mut m, ix - 6.0, 0.0, ICON + 12.0, ICON + 8.0, 8.0, rgba(0, 0, 0, 64));
        }
        icons.draw(&mut m, fonts, &job.icon, ix, 4.0, ICON);
        for (k, line) in lines.iter().enumerate() {
            let y = ICON + 10.0 + k as f32 * 15.0;
            let tw = fonts.measure_text(line, 12.0, false);
            let x = (CELL_W - tw) * 0.5;
            if selected {
                crate::theme::fill_rounded_rect(&mut m, x - 4.0, y - 1.0, tw + 8.0, 16.0, 4.0, rgba(10, 132, 255, 255));
            } else {
                // Soft dark halo keeps white labels readable on light wallpapers
                for (dx, dy, a) in [(0.0, 1.0, 150), (0.7, 0.4, 70), (-0.7, 0.4, 70), (0.0, -0.5, 45)] {
                    fonts.draw_text(&mut m, line, x + dx, y + dy, 12.0, rgba(0, 0, 0, a), false);
                }
            }
            fonts.draw_text(&mut m, line, x, y, 12.0, rgba(255, 255, 255, 255), false);
        }
        pm.take()
    };
    let plain = draw(false, fonts, icons);
    let selected = draw(true, fonts, icons);
    TilePixels { path: job.path.clone(), name: job.name.clone(), size: (w, h), plain, selected }
}

fn positions_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("feather-de").join("desktop.conf"))
}

/// "column<TAB>row<TAB>name" lines
fn parse_positions(text: &str) -> HashMap<String, Cell> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let col = parts.next()?.parse().ok()?;
            let row = parts.next()?.parse().ok()?;
            Some((parts.next()?.to_string(), (col, row)))
        })
        .collect()
}

fn format_positions(positions: &HashMap<String, Cell>) -> String {
    let mut lines: Vec<_> = positions.iter().map(|(name, (c, r))| format!("{c}\t{r}\t{name}")).collect();
    lines.sort();
    lines.join("\n") + "\n"
}

/// Written on a thread: the event loop never waits for the disk
fn save_positions(positions: &HashMap<String, Cell>) {
    if cfg!(test) {
        return;
    }
    let Some(path) = positions_path() else { return };
    let text = format_positions(positions);
    let _ = std::thread::Builder::new().name("feather-desktop-save".into()).spawn(move || {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("conf.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    });
}

fn list(dir: &Path) -> Vec<Item> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut items: Vec<Item> = read
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            let meta = std::fs::metadata(&path).ok()?;
            use std::os::unix::fs::MetadataExt;
            let (kind, ext) = kind_of(&name, meta.is_dir(), meta.mode() & 0o111 != 0);
            Some(Item { name, path, is_dir: meta.is_dir(), kind, ext })
        })
        .collect();
    items.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| fileops::natural_cmp(&a.name, &b.name)));
    items
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// Clipboard/drag formats for files: KDE/Qt and most apps read uri-list, GTK file
/// managers the GNOME list, editors plain text
pub fn file_formats(paths: &[PathBuf]) -> Vec<(String, Vec<u8>)> {
    let uris = fileops::uri_list(paths);
    let gnome = format!("copy\n{}", uris.trim_end().replace("\r\n", "\n"));
    let text = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n");
    vec![
        (URI_LIST.into(), uris.into_bytes()),
        ("x-special/gnome-copied-files".into(), gnome.into_bytes()),
        ("text/plain;charset=utf-8".into(), text.clone().into_bytes()),
        ("text/plain".into(), text.into_bytes()),
    ]
}

/// Moves (same filesystem) or copies files into `dir` on a thread, as Finder does
fn drop_files(paths: Vec<PathBuf>, dir: PathBuf) {
    let _ = std::thread::Builder::new().name("feather-desktop-drop".into()).spawn(move || {
        let (moving, copying): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.parent().is_some_and(|parent| fileops::same_device(parent, &dir)));
        let failed = fileops::paste(&moving, &dir, true) + fileops::paste(&copying, &dir, false);
        if failed > 0 {
            tracing::warn!("Перенос на рабочий стол: не удалось для {failed}");
        }
    });
}

fn read_pipe(mut file: std::fs::File) -> Vec<PathBuf> {
    let mut text = String::new();
    let _ = Read::by_ref(&mut file).take(1 << 20).read_to_string(&mut text);
    fileops::parse_uri_list(&text)
}

impl FeatherCompositor {
    /// Reads the desktop folder now and on every inotify change of it
    pub fn start_desktop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = channel::channel::<Msg>();
        self.desktop_tx = Some(tx);
        self.loop_handle.insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(msg) = event {
                state.desktop_message(msg);
            }
        })?;
        if let Some(mut watcher) = fileops::Watcher::new() {
            watcher.watch(&self.shell.desktop.dir);
            let fd = watcher.fd();
            self.shell.desktop.watcher = Some(watcher);
            // SAFETY: the watcher (and its fd) lives in the shell for the whole session
            let source = Generic::new(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }, Interest::READ, Mode::Level);
            self.loop_handle.insert_source(source, |_, _, state| {
                if state.shell.desktop.watcher.as_ref().is_some_and(fileops::Watcher::drain) {
                    state.schedule_desktop_reload();
                }
                Ok(PostAction::Continue)
            })?;
        }
        self.reload_desktop();
        Ok(())
    }

    fn desktop_message(&mut self, msg: Msg) {
        match msg {
            Msg::Listed { generation, items, positions } => {
                let desktop = &mut self.shell.desktop;
                if generation == desktop.generation && (items != desktop.items || positions != desktop.positions) {
                    desktop.set_items(items, positions);
                    self.needs_redraw = true;
                }
            }
            Msg::DragPaths { drag, paths } => {
                let Some(client) = self.shell.desktop.client_drag.as_mut().filter(|d| d.id == drag) else { return };
                client.paths = Some(paths);
                self.finish_client_drop();
            }
            Msg::Paste(paths) => {
                if !paths.is_empty() {
                    let dir = self.shell.desktop.dir.clone();
                    let _ = std::thread::Builder::new().name("feather-desktop-paste".into()).spawn(move || {
                        fileops::paste(&paths, &dir, false);
                    });
                }
            }
        }
    }

    fn reload_desktop(&mut self) {
        let Some(tx) = self.desktop_tx.clone() else { return };
        self.shell.desktop.generation += 1;
        let generation = self.shell.desktop.generation;
        let dir = self.shell.desktop.dir.clone();
        let _ = std::thread::Builder::new().name("feather-desktop".into()).spawn(move || {
            let positions = positions_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse_positions(&t)).unwrap_or_default();
            let _ = tx.send(Msg::Listed { generation, items: list(&dir), positions });
        });
    }

    /// A burst of changes (copying many files) causes one re-read
    fn schedule_desktop_reload(&mut self) {
        if self.shell.desktop.reload_armed {
            return;
        }
        self.shell.desktop.reload_armed = true;
        let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(150)), |_, _, state| {
            state.shell.desktop.reload_armed = false;
            state.reload_desktop();
            TimeoutAction::Drop
        });
    }

    fn screen(&self) -> (i32, i32) {
        (self.shell.screen_w, self.shell.screen_h)
    }

    /// No window, Dock or menu bar under the point
    fn over_bare_desktop(&self, x: f64, y: f64) -> bool {
        y >= TOP_BAR_HEIGHT as f64
            && !self.windows.iter().any(|w| w.is_visible() && w.contains_point(x, y))
            && self.shell.dock_target_at(x, y).is_none()
            && !self.shell.dock_in_drop_zone(x, y)
    }

    /// Left/right press on the bare desktop; returns true when handled
    pub fn desktop_press(&mut self, left: bool, ctrl: bool, shift: bool) -> bool {
        let (x, y) = (self.pointer_location.x, self.pointer_location.y);
        let screen = self.screen();
        self.focus_desktop();
        self.shell.desktop.ensure_layout(screen);
        let hit = self.shell.desktop.item_at(x, y, screen);
        self.needs_redraw = true;
        if !left {
            // Right click: select what is under the pointer, then the menu for it
            if let Some(i) = hit {
                let path = self.shell.desktop.items[i].path.clone();
                if !self.shell.desktop.selected.contains(&path) {
                    self.shell.desktop.selected = HashSet::from([path]);
                }
            } else {
                self.shell.desktop.selected.clear();
            }
            self.open_desktop_menu(hit.is_some(), x, y);
            return true;
        }
        let desktop = &mut self.shell.desktop;
        desktop.dropped = None;
        match hit {
            Some(i) => {
                let path = desktop.items[i].path.clone();
                let now = Instant::now();
                let double = desktop.last_click.as_ref().is_some_and(|(t, p)| *p == path && now.duration_since(*t) < DOUBLE_CLICK);
                desktop.last_click = Some((now, path.clone()));
                if double {
                    desktop.last_click = None;
                    desktop.selected = HashSet::from([path]);
                    self.open_desktop_selection();
                    return true;
                }
                if shift {
                    let a = desktop.anchor.unwrap_or(i);
                    for j in a.min(i)..=a.max(i) {
                        let p = desktop.items[j].path.clone();
                        desktop.selected.insert(p);
                    }
                } else if ctrl {
                    if !desktop.selected.remove(&path) {
                        desktop.selected.insert(path.clone());
                    }
                    desktop.anchor = Some(i);
                } else {
                    if !desktop.selected.contains(&path) {
                        desktop.selected = HashSet::from([path.clone()]);
                    }
                    desktop.anchor = Some(i);
                }
                // Pressing a selected icon may become a drag of the whole selection
                if desktop.selected.contains(&path) {
                    let paths = desktop.selected_paths();
                    desktop.drag = Some(IconDrag { start: (x, y), paths, active: false });
                }
            }
            None => {
                desktop.rubber_base = if ctrl || shift { desktop.selected.clone() } else { HashSet::new() };
                if !(ctrl || shift) {
                    desktop.selected.clear();
                }
                desktop.rubber = Some(((x, y), (x, y)));
                desktop.last_click = None;
            }
        }
        true
    }

    /// Pointer motion: rubber band (consumed) or icon drag (the DnD grab also needs it)
    pub fn desktop_motion(&mut self) -> bool {
        let screen = self.screen();
        let p = (self.pointer_location.x, self.pointer_location.y);
        let over = self.over_bare_desktop(p.0, p.1);
        if self.shell.desktop.update_hover_folder(p, screen, over) {
            self.needs_redraw = true;
        }
        if let Some(drag) = self.shell.desktop.drag.clone() {
            if drag.active {
                self.needs_redraw = true;
            } else if (p.0 - drag.start.0).hypot(p.1 - drag.start.1) > DRAG_START {
                self.start_icon_drag(drag);
            }
            return false;
        }
        let desktop = &mut self.shell.desktop;
        let Some((anchor, _)) = desktop.rubber else { return false };
        desktop.rubber = Some((anchor, p));
        let (x0, x1) = (anchor.0.min(p.0), anchor.0.max(p.0));
        let (y0, y1) = (anchor.1.min(p.1), anchor.1.max(p.1));
        let mut chosen = desktop.rubber_base.clone();
        for i in 0..desktop.items.len() {
            let (ix, iy) = desktop.position(i, screen);
            let (ix0, ix1) = (ix + 8.0, ix + CELL_W as f64 - 8.0);
            let (iy0, iy1) = (iy, iy + CELL_H as f64 - 4.0);
            if ix0 < x1 && ix1 > x0 && iy0 < y1 && iy1 > y0 {
                chosen.insert(desktop.items[i].path.clone());
            }
        }
        desktop.selected = chosen;
        self.needs_redraw = true;
        true
    }

    /// Server-side drag-and-drop: programs see our icons as `text/uri-list`
    fn start_icon_drag(&mut self, mut drag: IconDrag) {
        drag.active = true;
        let start = drag.start;
        self.shell.desktop.drag = Some(drag);
        // The release must reach the grab, although the press was ours
        self.consumed_buttons.retain(|&b| b != BTN_LEFT);
        let metadata = SourceMetadata {
            mime_types: file_formats(&[]).into_iter().map(|(m, _)| m).collect(),
            dnd_action: DndAction::Copy | DndAction::Move,
        };
        let start_data = GrabStartData { focus: None, button: BTN_LEFT, location: start.into() };
        let (dh, seat) = (self.display_handle.clone(), self.seat.clone());
        start_dnd(&dh, &seat, self, SERIAL_COUNTER.next_serial(), Some(start_data), None, metadata);
        self.needs_redraw = true;
    }

    /// Left release: ends a rubber band or a press that never became a drag
    pub fn desktop_release(&mut self) -> bool {
        let desktop = &mut self.shell.desktop;
        if desktop.drag.as_ref().is_some_and(|d| !d.active) {
            desktop.drag = None;
        }
        if desktop.rubber.take().is_some() {
            self.needs_redraw = true;
            return true;
        }
        false
    }

    /// A program asked for the dragged icons in `mime_type`
    pub fn desktop_dnd_send(&mut self, mime_type: &str, fd: std::os::fd::OwnedFd) {
        let desktop = &self.shell.desktop;
        let Some(drag) = desktop.drag.as_ref().or(desktop.dropped.as_ref()) else { return };
        let Some((_, data)) = file_formats(&drag.paths).into_iter().find(|(m, _)| m == mime_type) else { return };
        let _ = std::thread::Builder::new().name("feather-dnd-send".into()).spawn(move || {
            let _ = std::fs::File::from(fd).write_all(&data);
        });
    }

    /// The button went up: a program may now read the data (finished) or refuse (cancelled)
    pub fn desktop_dnd_dropped(&mut self) {
        self.shell.desktop.dropped = self.shell.desktop.drag.take();
        self.shell.desktop.hover_folder = None;
        self.needs_redraw = true;
    }

    /// No program took the icons: a desktop folder takes them, or they move on the grid
    pub fn desktop_dnd_cancelled(&mut self) {
        let Some(drag) = self.shell.desktop.dropped.take() else { return };
        let p = (self.pointer_location.x, self.pointer_location.y);
        if !self.over_bare_desktop(p.0, p.1) {
            return;
        }
        let screen = self.screen();
        let desktop = &mut self.shell.desktop;
        desktop.ensure_layout(screen);
        match desktop.item_at(p.0, p.1, screen).map(|i| desktop.items[i].clone()) {
            Some(target) if target.is_dir && !drag.paths.contains(&target.path) => drop_files(drag.paths, target.path),
            Some(target) if drag.paths.contains(&target.path) && desktop.item_at(drag.start.0, drag.start.1, screen) == desktop.items.iter().position(|i| i.path == target.path) => {}
            _ => {
                // Snap: the pressed icon's tile centre decides the target cell
                let Some(i) = desktop.item_at(drag.start.0, drag.start.1, screen) else { return };
                let (x, y) = desktop.position(i, screen);
                let centre = (x + CELL_W as f64 * 0.5 + p.0 - drag.start.0, y + CELL_H as f64 * 0.5 + p.1 - drag.start.1);
                let from = desktop.cells[i];
                let to = Desktop::cell_at(centre.0, centre.1, screen);
                let delta = (to.0 as isize - from.0 as isize, to.1 as isize - from.1 as isize);
                if delta != (0, 0) {
                    desktop.move_items(&drag.paths, delta, screen);
                }
            }
        }
        self.needs_redraw = true;
    }

    pub fn desktop_dnd_finished(&mut self) {
        self.shell.desktop.dropped = None;
        self.needs_redraw = true;
    }

    /// A program started a drag: read its files right away (the source cannot be read any
    /// more once a drop nobody accepted cancels it)
    pub fn client_drag_started(&mut self, source: Option<WlDataSource>, icon: Option<WlSurface>) {
        self.shell.desktop.dnd_icon = icon;
        let Some(source) = source else { return };
        let offers_files = with_source_metadata(&source, |m| m.mime_types.iter().any(|t| t == URI_LIST)).unwrap_or(false);
        if !offers_files {
            return;
        }
        let Ok((read, write)) = std::io::pipe() else { return };
        self.shell.desktop.next_drag += 1;
        let id = self.shell.desktop.next_drag;
        self.shell.desktop.client_drag = Some(ClientDrag { id, paths: None, drop_into: None });
        source.send(URI_LIST.into(), write.as_fd());
        drop(write);
        let Some(tx) = self.desktop_tx.clone() else { return };
        let _ = std::thread::Builder::new().name("feather-dnd-read".into()).spawn(move || {
            let file = std::fs::File::from(std::os::fd::OwnedFd::from(read));
            let _ = tx.send(Msg::DragPaths { drag: id, paths: read_pipe(file) });
        });
    }

    /// A program's drag ended; with no window under the pointer the desktop takes the files
    pub fn client_drag_dropped(&mut self, target: Option<WlSurface>) {
        self.shell.desktop.dnd_icon = None;
        self.shell.desktop.hover_folder = None;
        self.needs_redraw = true;
        let p = (self.pointer_location.x, self.pointer_location.y);
        if target.is_some() || !self.over_bare_desktop(p.0, p.1) {
            self.shell.desktop.client_drag = None;
            return;
        }
        let screen = self.screen();
        let desktop = &mut self.shell.desktop;
        let folder = desktop.item_at(p.0, p.1, screen).map(|i| &desktop.items[i]).filter(|i| i.is_dir).map(|i| i.path.clone());
        let dir = folder.unwrap_or_else(|| desktop.dir.clone());
        if let Some(client) = desktop.client_drag.as_mut() {
            client.drop_into = Some(dir);
        }
        self.finish_client_drop();
    }

    fn finish_client_drop(&mut self) {
        let desktop = &mut self.shell.desktop;
        let ready = desktop.client_drag.as_ref().is_some_and(|d| d.paths.is_some() && d.drop_into.is_some());
        if !ready {
            return;
        }
        let client = desktop.client_drag.take().unwrap();
        drop_files(client.paths.unwrap(), client.drop_into.unwrap());
    }

    /// The desktop takes the keyboard: no window is active until one is clicked or raised
    pub fn focus_desktop(&mut self) {
        if !self.shell.desktop.focused {
            self.shell.desktop.focused = true;
            self.desktop_focus_top = self.windows.iter().rposition(|w| w.is_visible()).map(|i| self.windows[i].id);
            self.refresh_focus();
        }
    }

    pub fn unfocus_desktop(&mut self) {
        self.shell.desktop.focused = false;
    }

    fn spawn(&self, cmd: &str) {
        crate::apps::spawn_wayland_command(cmd, &self.wayland_socket_name);
    }

    /// Files with a path (and options); Dolphin when our Files is not installed
    fn files(&self, args: &str) {
        let files = crate::apps::files_command();
        self.spawn(&format!("{files} {args}"));
    }

    pub fn open_desktop_selection(&mut self) {
        let ours = crate::apps::files_command() != "dolphin";
        for path in self.shell.desktop.selected_paths() {
            if path.is_dir() && ours {
                self.files(&shell_quote(&path));
            } else {
                self.spawn(&format!("xdg-open {}", shell_quote(&path)));
            }
        }
    }

    fn open_desktop_menu(&mut self, on_item: bool, x: f64, y: f64) {
        use MenuAction::*;
        let several = self.shell.desktop.selected.len() > 1;
        let items: Vec<(&str, MenuAction)> = if on_item {
            let mut items = vec![("Открыть", Open), ("Показать в «Файлах»", ShowInFiles)];
            if !several {
                items.push(("Переименовать…", Rename));
            }
            items.extend([("Скопировать", Copy), ("Переместить в Корзину", Trash)]);
            items
        } else {
            let mut items = vec![("Новая папка", NewFolder)];
            if self.clipboard_has_files() {
                items.push(("Вставить", Paste));
            }
            items.extend([
                ("Открыть «Рабочий стол» в «Файлах»", OpenDesktop),
                ("Открыть в терминале", Terminal),
                ("Упорядочить значки", CleanUp),
                ("Сменить обои", Wallpaper),
            ]);
            items
        };
        let items = items.into_iter().map(|(l, a)| (l.to_string(), crate::shell::DockMenuAction::Desktop(a))).collect();
        self.shell.open_context_menu(items, x as f32, y as f32, &mut self.fonts);
    }

    fn clipboard_has_files(&self) -> bool {
        if let Some(own) = current_data_device_selection_userdata(&self.seat) {
            return own.iter().any(|(m, _)| m == URI_LIST);
        }
        // A program's selection: offered only if it lists files
        self.client_clipboard_mimes.iter().any(|m| m == URI_LIST)
    }

    pub fn desktop_menu_action(&mut self, action: MenuAction) {
        let paths = self.shell.desktop.selected_paths();
        let first = paths.first().cloned();
        let dir = self.shell.desktop.dir.clone();
        match action {
            MenuAction::Open => self.open_desktop_selection(),
            MenuAction::ShowInFiles => {
                if let Some(p) = first {
                    self.files(&shell_quote(&p));
                }
            }
            MenuAction::Rename => {
                if let Some(p) = first {
                    self.files(&format!("--rename {}", shell_quote(&p)));
                }
            }
            MenuAction::Copy => {
                if !paths.is_empty() {
                    self.set_clipboard(file_formats(&paths));
                }
            }
            MenuAction::Trash => {
                let _ = std::thread::Builder::new().name("feather-trash".into()).spawn(move || {
                    fileops::trash(&paths);
                });
            }
            MenuAction::NewFolder => {
                let _ = std::thread::Builder::new().name("feather-mkdir".into()).spawn(move || {
                    let _ = std::fs::create_dir(fileops::unique_name(&dir, "Новая папка"));
                });
            }
            MenuAction::Paste => self.paste_to_desktop(),
            MenuAction::OpenDesktop => self.files(&shell_quote(&dir)),
            MenuAction::Terminal => {
                self.spawn(&format!("cd {} && {}",shell_quote(&dir),self.system_preferences.terminal_command()));
            }
            MenuAction::CleanUp => {
                let desktop = &mut self.shell.desktop;
                desktop.positions.clear();
                desktop.layout = None;
                save_positions(&desktop.positions);
            }
            MenuAction::Wallpaper => self.execute_shell_action(crate::shell::ShellClickAction::CycleTheme),
        }
        self.needs_redraw = true;
    }

    /// Files from the clipboard: our own selection directly, a program's through a pipe
    fn paste_to_desktop(&mut self) {
        let own = current_data_device_selection_userdata(&self.seat)
            .and_then(|own| own.iter().find(|(m, _)| m == URI_LIST).map(|(_, d)| String::from_utf8_lossy(d).into_owned()));
        if let Some(text) = own {
            self.desktop_message(Msg::Paste(fileops::parse_uri_list(&text)));
            return;
        }
        let Ok((read, write)) = std::io::pipe() else { return };
        if request_data_device_client_selection(&self.seat, URI_LIST.into(), std::os::fd::OwnedFd::from(write)).is_err() {
            return;
        }
        let Some(tx) = self.desktop_tx.clone() else { return };
        let _ = std::thread::Builder::new().name("feather-paste-read".into()).spawn(move || {
            let file = std::fs::File::from(std::os::fd::OwnedFd::from(read));
            let _ = tx.send(Msg::Paste(read_pipe(file)));
        });
    }

    /// Keys while the desktop has focus; true when consumed
    pub fn desktop_key(&mut self, sym: u32, ctrl: bool) -> bool {
        use smithay::input::keyboard::keysyms as k;
        if !self.shell.desktop.focused {
            return false;
        }
        let any = !self.shell.desktop.selected.is_empty();
        match sym {
            k::KEY_Return | k::KEY_KP_Enter if any => self.open_desktop_selection(),
            k::KEY_Delete if any => self.desktop_menu_action(MenuAction::Trash),
            k::KEY_a | k::KEY_A if ctrl => {
                let desktop = &mut self.shell.desktop;
                desktop.selected = desktop.items.iter().map(|i| i.path.clone()).collect();
            }
            k::KEY_c | k::KEY_C if ctrl && any => self.desktop_menu_action(MenuAction::Copy),
            k::KEY_v | k::KEY_V if ctrl => self.paste_to_desktop(),
            k::KEY_F2 if self.shell.desktop.selected.len() == 1 => self.desktop_menu_action(MenuAction::Rename),
            k::KEY_Escape if any => self.shell.desktop.selected.clear(),
            _ => return false,
        }
        self.needs_redraw = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item { name: format!("f{i}"), path: PathBuf::from(format!("/d/f{i}")), is_dir: false, kind: Kind::Text, ext: "txt".into() })
            .collect()
    }

    fn desktop(n: usize, positions: HashMap<String, Cell>, screen: (i32, i32)) -> Desktop {
        let mut d = Desktop::new();
        d.set_items(items(n), positions);
        d.ensure_layout(screen);
        d
    }

    #[test]
    fn grid_fills_columns_from_the_right_and_hit_tests_icons() {
        let screen = (1440, 900);
        let d = desktop(12, HashMap::new(), screen);
        let rows = Desktop::grid(screen).0;
        let (x0, y0) = d.position(0, screen);
        assert_eq!(x0, 1440.0 - MARGIN_RIGHT as f64 - CELL_W as f64);
        assert_eq!(d.position(1, screen), (x0, y0 + CELL_H as f64));
        assert_eq!(d.position(rows, screen), (x0 - CELL_W as f64, y0));
        assert_eq!(d.item_at(x0 + 48.0, y0 + 30.0, screen), Some(0));
        assert_eq!(d.item_at(x0 + 2.0, y0 + 30.0, screen), None);
        assert_eq!(d.item_at(100.0, 400.0, screen), None);
        let (cx, cy) = (x0 + 48.0 - CELL_W as f64 * 2.0, y0 + 50.0 + CELL_H as f64);
        assert_eq!(Desktop::cell_at(cx, cy, screen), (2, 1));
    }

    #[test]
    fn remembered_places_win_and_others_fill_free_cells() {
        let screen = (1440, 900);
        let d = desktop(3, HashMap::from([("f2".into(), (0, 0)), ("f0".into(), (4, 3))]), screen);
        assert_eq!(d.cells, [(4, 3), (0, 1), (0, 0)]);
        // Out-of-grid and duplicate places fall back to free cells
        let d = desktop(2, HashMap::from([("f0".into(), (99, 0)), ("f1".into(), (99, 0))]), screen);
        assert_eq!(d.cells, [(0, 0), (0, 1)]);
    }

    #[test]
    fn moving_icons_snaps_avoids_others_and_pins_the_rest() {
        let screen = (1440, 900);
        let mut d = desktop(3, HashMap::new(), screen);
        // f0 onto f1's cell: f1 stays, f0 takes the nearest free cell
        d.move_items(&[PathBuf::from("/d/f0")], (0, 1), screen);
        assert_eq!(d.cells[1], (0, 1));
        assert_ne!(d.cells[0], (0, 1));
        assert_eq!(d.positions.len(), 3);
        d.move_items(&[PathBuf::from("/d/f2")], (3, 0), screen);
        assert_eq!(d.cells[2], (3, 2));
        let text = format_positions(&d.positions);
        assert_eq!(parse_positions(&text), d.positions);
    }

    #[test]
    fn tiles_are_dropped_with_their_items_and_renames_redraw() {
        let mut d = Desktop::new();
        d.set_items(items(3), HashMap::new());
        d.requested.insert(PathBuf::from("/d/f2"));
        d.selected.insert(PathBuf::from("/d/f2"));
        d.set_items(items(2), HashMap::new());
        assert!(d.selected.is_empty() && d.requested.is_empty());
        // A result for an item that is gone is not kept
        let px = TilePixels { path: PathBuf::from("/d/f9"), name: "f9".into(), size: (1, 1), plain: vec![0; 4], selected: vec![0; 4] };
        assert!(!d.install(px));
        let px = TilePixels { path: PathBuf::from("/d/f1"), name: "f1".into(), size: (1, 1), plain: vec![0; 4], selected: vec![0; 4] };
        assert!(d.install(px));
    }

    #[test]
    fn file_formats_for_programs() {
        let formats = file_formats(&[PathBuf::from("/home/u/Мой файл.txt")]);
        let get = |m: &str| String::from_utf8(formats.iter().find(|(k, _)| k == m).unwrap().1.clone()).unwrap();
        assert_eq!(get(URI_LIST), "file:///home/u/%D0%9C%D0%BE%D0%B9%20%D1%84%D0%B0%D0%B9%D0%BB.txt\r\n");
        assert!(get("x-special/gnome-copied-files").starts_with("copy\nfile:///home/u/"));
        assert_eq!(get("text/plain"), "/home/u/Мой файл.txt");
        assert_eq!(fileops::parse_uri_list(&get(URI_LIST)), [PathBuf::from("/home/u/Мой файл.txt")]);
    }

    #[test]
    fn tile_raster_has_both_states() {
        let mut fonts = crate::text::FontSystem::new();
        let mut icons = IconCache::default();
        let job = TileJob { path: "/d/Отчёт за квартал.pdf".into(), name: "Отчёт за квартал.pdf".into(), icon: Icon::File(Kind::Pdf, "pdf".into()) };
        let tile = crate::theme::with_render_settings(2.0, crate::theme::Appearance::Light, || raster_tile(&job, &mut fonts, &mut icons));
        assert_eq!(tile.size, (192, 216));
        assert_eq!(tile.plain.len(), 192 * 216 * 4);
        assert_ne!(tile.plain, tile.selected);
        // The selected label pill is accent blue under the text line
        let px = |data: &[u8], x: usize, y: usize| { let i = (y * 192 + x) * 4; [data[i], data[i + 1], data[i + 2], data[i + 3]] };
        let pill = (0..192).map(|x| px(&tile.selected, x, ((ICON + 10.0 + 14.0) * 2.0) as usize)).any(|p| p[2] > 200 && p[0] < 60);
        assert!(pill);
    }
}
