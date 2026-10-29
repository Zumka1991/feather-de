//! Freedesktop icon lookup and rasterization for the Dock, Spotlight and the app switcher.
//!
//! Memory/latency conscious: no full theme index is built. Each lookup is a handful of
//! `stat` calls on a worker in the few `apps` directories of the active themes. The event
//! loop only reads a bounded pixel cache and queues misses; a small preview survives eviction.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Bumped when the installed programs change. A new app may bring the first
/// ~/.local/share/icons/hicolor/…/apps, and the theme folders are listed only once: the loader
/// lists them again, and keys of an older number are rasterized anew.
static ICON_DIRS: AtomicU64 = AtomicU64::new(0);

pub fn rescan_icon_dirs() {
    ICON_DIRS.fetch_add(1, Ordering::Relaxed);
}

use tiny_skia::{FilterQuality, Pixmap, PixmapMut, PixmapPaint, Transform};

use crate::theme::{appearance, squircle_path, ui_scale, Appearance};

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct IconKey {
    pub icon: crate::shell::AppIcon,
    pub initial: String,
    pub px: u32,
    pub dark: bool,
    /// `ICON_DIRS` when the key was made
    dirs: u64,
}
impl IconKey {
    pub fn new(icon: &crate::shell::AppIcon, label: &str, px: u32) -> Self {
        Self {
            icon: icon.clone(),
            initial: label
                .chars()
                .next()
                .unwrap_or('A')
                .to_uppercase()
                .to_string(),
            px,
            dark: appearance() == Appearance::Dark,
            dirs: ICON_DIRS.load(Ordering::Relaxed),
        }
    }
}

/// A small preview survives eviction, so cache pressure never turns an app into a
/// blank icon or starts a decode/repaint loop. Both images are prepared on the worker.
pub struct IconPixels {
    pub full: Option<Pixmap>,
    pub preview: Pixmap,
}
impl IconPixels {
    pub fn new(full: Pixmap) -> Self {
        if full.width() <= 64 {
            return Self {
                full: None,
                preview: full,
            };
        }
        let mut preview = Pixmap::new(64, 64).unwrap();
        preview.draw_pixmap(
            0,
            0,
            full.as_ref(),
            &PixmapPaint {
                quality: FilterQuality::Bilinear,
                ..Default::default()
            },
            Transform::from_scale(64.0 / full.width() as f32, 64.0 / full.height() as f32),
            None,
        );
        Self {
            full: Some(full),
            preview,
        }
    }
    fn bytes(&self) -> usize {
        self.preview.data().len() + self.full.as_ref().map_or(0, |p| p.data().len())
    }
    fn image(&self) -> &Pixmap {
        self.full.as_ref().unwrap_or(&self.preview)
    }
}

/// Event-loop cache: misses only enqueue work, never touch the filesystem.
pub struct IconCache {
    raster: HashMap<IconKey, Option<IconPixels>>,
    pending: HashSet<IconKey>,
    recent: VecDeque<IconKey>,
    worker: Option<crate::raster::RasterSender>,
}

pub fn icon_bucket(needed: u32) -> u32 {
    match needed {
        0..=48 => 64,
        49..=192 => 192,
        _ => 320,
    }
}

/// Physical size for an exact-size icon, when `phys` is a whole pixel count the cache
/// supports; otherwise the caller resamples a bucket image.
pub fn exact_icon_px(phys: f32) -> Option<u32> {
    let px = phys.round();
    ((phys - px).abs() < 0.01 && (1.0..=320.0).contains(&px)).then_some(px as u32)
}

/// Unscaled copy of an exact-size icon, snapped to the physical pixel grid.
pub fn blit_icon(target: &mut PixmapMut, icon: &Pixmap, x: f32, y: f32) {
    let _profile = crate::cpu_profile::Span::new(crate::cpu_profile::Part::Icon);
    let s = ui_scale();
    crate::ui_raster::blit_premultiplied(
        target,
        icon.as_ref(),
        (x * s).round() as i32,
        (y * s).round() as i32,
    );
}

impl IconCache {
    pub fn new() -> Self {
        Self {
            raster: HashMap::new(),
            pending: HashSet::new(),
            recent: VecDeque::new(),
            worker: None,
        }
    }
    pub fn attach(&mut self, worker: crate::raster::RasterSender) {
        self.worker = Some(worker);
    }
    pub fn get(&mut self, key: &IconKey) -> Option<&Pixmap> {
        let key = key.clone();
        if !self.raster.contains_key(&key) && !self.pending.contains(&key) {
            if self
                .worker
                .as_ref()
                .is_some_and(|w| w.submit(crate::raster::Job::Icon(key.clone())))
            {
                self.pending.insert(key.clone());
            }
        }
        if self.raster.contains_key(&key) {
            self.recent.retain(|old| old != &key);
            self.recent.push_back(key.clone());
        }
        self.raster
            .get(&key)
            .and_then(Option::as_ref)
            .map(IconPixels::image)
    }

    /// Another cached size of the same icon (the smallest that covers `needed`, else the
    /// largest) as a stand-in while `key` is rasterized. No job, eviction order unchanged.
    pub fn other_size(&self, key: &IconKey, needed: u32) -> Option<&Pixmap> {
        self.raster
            .iter()
            .filter(|(k, _)| k.icon == key.icon && k.initial == key.initial && k.dark == key.dark)
            .filter_map(|(_, pixels)| pixels.as_ref().map(IconPixels::image))
            .min_by_key(|pm| if pm.width() >= needed { (0, pm.width()) } else { (1, u32::MAX - pm.width()) })
    }

    #[cfg(test)]
    pub fn pending(&self, key: &IconKey) -> bool {
        self.pending.contains(key)
    }
    pub fn install(&mut self, key: IconKey, pixmap: Option<IconPixels>) {
        self.pending.remove(&key);
        self.recent.retain(|old| old != &key);
        self.recent.push_back(key.clone());
        self.raster.insert(key, pixmap);
        // Cached app icons; Launchpad tile data has a separate, visible-page lifetime.
        while self
            .raster
            .values()
            .flatten()
            .map(IconPixels::bytes)
            .sum::<usize>()
            > 4 * 1024 * 1024
        {
            let Some(old) = self.recent.pop_front() else {
                break;
            };
            if let Some(Some(pixels)) = self.raster.get_mut(&old) {
                pixels.full = None;
            }
        }
        while self.raster.len() > 128 {
            let old = self
                .raster
                .keys()
                .find(|key| !self.recent.contains(key))
                .cloned()
                .or_else(|| self.recent.pop_front());
            let Some(old) = old else { break };
            self.raster.remove(&old);
        }
    }
    pub fn clear_raster(&mut self) {
        self.raster.clear();
        self.pending.clear();
        self.recent.clear();
    }
}

/// Filesystem lookup and decoding owned exclusively by the raster worker.
pub struct IconLoader {
    search_dirs: Option<Vec<PathBuf>>,
    paths: HashMap<String, Option<PathBuf>>,
    /// `ICON_DIRS` the folders and found paths were read under
    dirs: u64,
}

fn user_icon_theme() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let candidates = [
        (format!("{home}/.config/kdeglobals"), "Theme="),
        (
            format!("{home}/.config/gtk-3.0/settings.ini"),
            "gtk-icon-theme-name=",
        ),
        (format!("{home}/.config/lxqt/lxqt.conf"), "icon_theme="),
    ];
    candidates.iter().find_map(|(file, key)| {
        std::fs::read_to_string(file).ok()?.lines().find_map(|l| {
            l.trim()
                .strip_prefix(key)
                .map(|v| v.trim().trim_matches('"').to_string())
                .filter(|v| !v.is_empty())
        })
    })
}

/// Size encoded in an icon directory ("256x256/apps", "apps/48", "scalable/apps")
fn dir_score(path: &Path) -> u32 {
    let s = path.to_string_lossy();
    if s.contains("scalable") {
        return 10_000;
    }
    s.split('/')
        .filter_map(|part| {
            part.split('x')
                .next()?
                .split('@')
                .next()?
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap_or(1)
}

fn theme_app_dirs(theme_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let Ok(level1) = std::fs::read_dir(theme_root) else {
        return dirs;
    };
    for e in level1.flatten() {
        let p = e.path();
        let name = e.file_name();
        if name == "apps" {
            // breeze layout: apps/<size>
            if let Ok(sizes) = std::fs::read_dir(&p) {
                dirs.extend(sizes.flatten().map(|s| s.path()).filter(|s| s.is_dir()));
            }
        } else {
            let apps = p.join("apps");
            if apps.is_dir() {
                dirs.push(apps);
            }
        }
    }
    // Biggest first; tiny 16/22/24 px variants are useless for the Dock
    dirs.retain(|d| dir_score(d) >= 48);
    dirs.sort_by_key(|d| std::cmp::Reverse(dir_score(d)));
    dirs
}

impl IconLoader {
    pub fn new() -> Self {
        Self {
            search_dirs: None,
            paths: HashMap::new(),
            dirs: ICON_DIRS.load(Ordering::Relaxed),
        }
    }

    fn search_dirs(&mut self) -> &[PathBuf] {
        self.search_dirs.get_or_insert_with(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            let mut themes: Vec<String> = Vec::new();
            if let Some(t) = user_icon_theme() {
                // Papirus-Dark only overrides a few icons; its parent has the full set
                if let Some(base) = t.strip_suffix("-Dark").or_else(|| t.strip_suffix("-Light")) {
                    themes.push(t.clone());
                    themes.push(base.to_string());
                } else {
                    themes.push(t);
                }
            }
            for t in ["Papirus", "breeze", "Adwaita", "hicolor"] {
                if !themes.iter().any(|x| x == t) {
                    themes.push(t.to_string());
                }
            }
            let mut dirs = Vec::new();
            for t in &themes {
                for base in [
                    format!("{home}/.local/share/icons"),
                    "/usr/share/icons".to_string(),
                ] {
                    dirs.extend(theme_app_dirs(&Path::new(&base).join(t)));
                }
            }
            dirs.push(PathBuf::from("/usr/share/pixmaps"));
            dirs
        })
    }

    fn find(&mut self, name: &str) -> Option<PathBuf> {
        let dirs = ICON_DIRS.load(Ordering::Relaxed);
        if dirs != self.dirs {
            self.dirs = dirs;
            self.search_dirs = None;
            self.paths.clear();
        }
        if let Some(cached) = self.paths.get(name) {
            return cached.clone();
        }
        let found = if name.starts_with('/') {
            Path::new(name).is_file().then(|| PathBuf::from(name))
        } else {
            let dirs = self.search_dirs().to_vec();
            dirs.iter().find_map(|d| {
                ["svg", "png"]
                    .iter()
                    .map(|ext| d.join(format!("{name}.{ext}")))
                    .find(|p| p.is_file())
            })
        };
        self.paths.insert(name.to_string(), found.clone());
        found
    }

    pub fn load(&mut self, name: &str, px: u32, dark: bool) -> Option<Pixmap> {
        self.find(name)
            .and_then(|p| rasterize(&p, px))
            .map(|raw| macify(raw, px, dark))
    }
    pub fn load_app(
        &mut self,
        key: &IconKey,
        fonts: &mut crate::text::FontSystem,
    ) -> Option<Pixmap> {
        if let Some(pixmap) = key
            .icon
            .name
            .as_deref()
            .and_then(|name| self.load(name, key.px, key.dark))
        {
            return Some(pixmap);
        }
        let mut pixmap = Pixmap::new(key.px, key.px)?;
        crate::apps::draw_app_icon(
            &mut pixmap.as_mut(),
            fonts,
            key.icon.kind,
            &key.initial,
            0.0,
            0.0,
            key.px as f32 / ui_scale(),
        );
        Some(pixmap)
    }
}

fn rasterize(path: &Path, px: u32) -> Option<Pixmap> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mut out = Pixmap::new(px, px)?;
    match ext.as_str() {
        "svg" => {
            let data = std::fs::read(path).ok()?;
            let tree =
                resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
            let size = tree.size();
            let scale = px as f32 / size.width().max(size.height());
            let dx = (px as f32 - size.width() * scale) * 0.5;
            let dy = (px as f32 - size.height() * scale) * 0.5;
            resvg::render(
                &tree,
                Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
                &mut out.as_mut(),
            );
        }
        "png" => {
            let src = Pixmap::load_png(path).ok()?;
            let scale = px as f32 / src.width().max(src.height()) as f32;
            let dx = (px as f32 - src.width() as f32 * scale) * 0.5;
            let dy = (px as f32 - src.height() as f32 * scale) * 0.5;
            out.draw_pixmap(
                0,
                0,
                src.as_ref(),
                &PixmapPaint {
                    quality: FilterQuality::Bicubic,
                    ..Default::default()
                },
                Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
                None,
            );
        }
        _ => return None,
    }
    Some(out)
}

/// Bounding box of visibly opaque pixels
fn opaque_bbox(p: &Pixmap) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = (p.width(), p.height());
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (i, px) in p.data().chunks_exact(4).enumerate() {
        if px[3] > 24 {
            let (x, y) = (i as u32 % w, i as u32 / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x1 >= x0 && y1 >= y0).then_some((x0, y0, x1 + 1, y1 + 1))
}

/// macOS Tahoe draws every app icon as a squircle tile. Icons that already are a tile
/// (square-ish and almost fully filled) are scaled to the tile; any other shape goes onto a
/// neutral tile at ~72%, exactly like Tahoe treats legacy icons.
fn macify(raw: Pixmap, px: u32, dark: bool) -> Pixmap {
    let Some((x0, y0, x1, y1)) = opaque_bbox(&raw) else {
        return raw;
    };
    let (bw, bh) = ((x1 - x0) as f32, (y1 - y0) as f32);
    let aspect = bw / bh.max(1.0);
    let filled = raw
        .data()
        .chunks_exact(4)
        .enumerate()
        .filter(|(i, px4)| {
            let (x, y) = (*i as u32 % raw.width(), *i as u32 / raw.width());
            px4[3] > 200 && x >= x0 && x < x1 && y >= y0 && y < y1
        })
        .count() as f32
        / (bw * bh).max(1.0);
    let is_tile = (0.9..=1.1).contains(&aspect) && filled > 0.86;

    let Some(mut out) = Pixmap::new(px, px) else {
        return raw;
    };
    let paint = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..Default::default()
    };
    let size = px as f32;
    if is_tile {
        // Crop the icon's margins; centre slightly rectangular tiles in both axes.
        let scale = size / bw.max(bh);
        out.draw_pixmap(
            0,
            0,
            raw.as_ref(),
            &paint,
            Transform::from_row(
                scale,
                0.0,
                0.0,
                scale,
                (size - bw * scale) * 0.5 - x0 as f32 * scale,
                (size - bh * scale) * 0.5 - y0 as f32 * scale,
            ),
            None,
        );
        return out;
    }

    if let Some(path) = squircle_path(0.0, 0.0, size) {
        let mut tile = tiny_skia::Paint {
            anti_alias: true,
            ..Default::default()
        };
        let (top, bottom) = if dark {
            (
                tiny_skia::Color::from_rgba8(72, 72, 76, 255),
                tiny_skia::Color::from_rgba8(52, 52, 56, 255),
            )
        } else {
            (
                tiny_skia::Color::from_rgba8(255, 255, 255, 255),
                tiny_skia::Color::from_rgba8(234, 234, 238, 255),
            )
        };
        if let Some(shader) = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(0.0, 0.0),
            tiny_skia::Point::from_xy(0.0, size),
            vec![
                tiny_skia::GradientStop::new(0.0, top),
                tiny_skia::GradientStop::new(1.0, bottom),
            ],
            tiny_skia::SpreadMode::Pad,
            Transform::identity(),
        ) {
            tile.shader = shader;
        }
        out.fill_path(
            &path,
            &tile,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let inner = size * 0.72;
    let scale = inner / bw.max(bh);
    let ox = (size - bw * scale) * 0.5 - x0 as f32 * scale;
    let oy = (size - bh * scale) * 0.5 - y0 as f32 * scale;
    out.draw_pixmap(
        0,
        0,
        raw.as_ref(),
        &paint,
        Transform::from_row(scale, 0.0, 0.0, scale, ox, oy),
        None,
    );
    out
}

/// Draws a cached icon pixmap at logical `(x, y)` with logical `size` (HiDPI aware)
pub fn draw_icon(target: &mut PixmapMut, icon: &Pixmap, x: f32, y: f32, size: f32) {
    let _profile = crate::cpu_profile::Span::new(crate::cpu_profile::Part::Icon);
    let s = ui_scale();
    let scale = size * s / icon.width() as f32;
    target.draw_pixmap(
        0,
        0,
        icon.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..Default::default()
        },
        Transform::from_row(scale, 0.0, 0.0, scale, x * s, y * s),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_programs_list_icon_folders_again() {
        let icon = crate::shell::AppIcon { kind: crate::apps::IconKind::GenericApp, name: Some("feather-no-such-icon".into()) };
        let mut loader = IconLoader::new();
        assert!(loader.find("feather-no-such-icon").is_none());
        loader.search_dirs = Some(Vec::new());
        // As if `rescan_icon_dirs` ran (the counter itself is shared with parallel tests)
        loader.dirs = loader.dirs.wrapping_sub(1);
        // The missing icon is not remembered and the folders are read again
        assert!(loader.find("feather-no-such-icon").is_none());
        assert!(loader.search_dirs.as_ref().is_some_and(|d| !d.is_empty()));
        // The cached fallback picture does not answer for a key made after the change
        let key = IconKey::new(&icon, "X", 64);
        assert!(IconKey { dirs: key.dirs.wrapping_add(1), ..key.clone() } != key);
    }
}
