//! Freedesktop icon lookup and rasterization for the Dock, Spotlight and the app switcher.
//!
//! Memory/latency conscious: no full theme index is built. Each lookup is a handful of
//! `stat` calls in the few `apps` directories of the active themes, and only icons that are
//! actually shown get rasterized, once, at the size they are needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, PixmapMut, Transform};

use crate::theme::{appearance, squircle_path, ui_scale, Appearance};

pub struct IconCache {
    /// `apps` directories of the active icon themes, best first
    search_dirs: Option<Vec<PathBuf>>,
    paths: HashMap<String, Option<PathBuf>>,
    raster: HashMap<(String, u32, bool), Option<Pixmap>>,
}

fn user_icon_theme() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let candidates = [
        (format!("{home}/.config/kdeglobals"), "Theme="),
        (format!("{home}/.config/gtk-3.0/settings.ini"), "gtk-icon-theme-name="),
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
        .filter_map(|part| part.split('x').next()?.split('@').next()?.parse::<u32>().ok())
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

impl IconCache {
    pub fn new() -> Self {
        Self {
            search_dirs: None,
            paths: HashMap::new(),
            raster: HashMap::new(),
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
                for base in [format!("{home}/.local/share/icons"), "/usr/share/icons".to_string()] {
                    dirs.extend(theme_app_dirs(&Path::new(&base).join(t)));
                }
            }
            dirs.push(PathBuf::from("/usr/share/pixmaps"));
            dirs
        })
    }

    fn find(&mut self, name: &str) -> Option<PathBuf> {
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

    /// The icon `name` as a `px`×`px` premultiplied pixmap in the macOS Tahoe style
    pub fn get(&mut self, name: &str, px: u32) -> Option<&Pixmap> {
        let dark = appearance() == Appearance::Dark;
        let key = (name.to_string(), px, dark);
        if !self.raster.contains_key(&key) {
            let pixmap = self.find(name).and_then(|p| rasterize(&p, px)).map(|raw| macify(raw, px, dark));
            self.raster.insert(key.clone(), pixmap);
        }
        self.raster.get(&key).and_then(|p| p.as_ref())
    }

    /// Drop rasterized icons (e.g. after a scale or appearance change)
    pub fn clear_raster(&mut self) {
        self.raster.clear();
    }
}

fn rasterize(path: &Path, px: u32) -> Option<Pixmap> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mut out = Pixmap::new(px, px)?;
    match ext.as_str() {
        "svg" => {
            let data = std::fs::read(path).ok()?;
            let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
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
        // Crop away the icon's own margins so all tiles line up
        let scale = size / bw.max(bh);
        out.draw_pixmap(
            0,
            0,
            raw.as_ref(),
            &paint,
            Transform::from_row(scale, 0.0, 0.0, scale, -(x0 as f32) * scale, -(y0 as f32) * scale),
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
            (tiny_skia::Color::from_rgba8(72, 72, 76, 255), tiny_skia::Color::from_rgba8(52, 52, 56, 255))
        } else {
            (tiny_skia::Color::from_rgba8(255, 255, 255, 255), tiny_skia::Color::from_rgba8(234, 234, 238, 255))
        };
        if let Some(shader) = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(0.0, 0.0),
            tiny_skia::Point::from_xy(0.0, size),
            vec![tiny_skia::GradientStop::new(0.0, top), tiny_skia::GradientStop::new(1.0, bottom)],
            tiny_skia::SpreadMode::Pad,
            Transform::identity(),
        ) {
            tile.shader = shader;
        }
        out.fill_path(&path, &tile, tiny_skia::FillRule::Winding, Transform::identity(), None);
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
