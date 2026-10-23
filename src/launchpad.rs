//! Launchpad: full-screen grid of all apps with pages and search.
//!
//! Each visible app cell (icon + label) is built on the raster worker. Ready tiles are
//! GPU-placed without rerasterization; page/search changes queue missing tiles and discard
//! obsolete results. Only the visible page is cached; closing Launchpad releases it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::element::memory::MemoryRenderBuffer,
    },
    utils::{Rectangle, Transform},
};
use tiny_skia::PixmapMut;

use crate::apps::AppRegistry;
use crate::icons::{IconLoader, draw_icon, icon_bucket};
use crate::shell::{AppIcon, R};
use crate::text::FontSystem;
use crate::theme::{draw_search_icon, fill_circle, fill_rounded_rect, rgba, stroke_rounded_rect, to_phys, TOP_BAR_HEIGHT};

const LABEL_SIZE: f32 = 13.0;

#[derive(Debug, Clone, Copy)]
pub struct LaunchpadLayout {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    pub icon: f32,
    pub grid_x: f32,
    pub grid_y: f32,
    pub search: R,
    pub dots_y: f32,
}

impl LaunchpadLayout {
    pub fn per_page(&self) -> usize {
        self.cols * self.rows
    }

    pub fn compute(screen_w: i32, screen_h: i32) -> Self {
        let (sw, sh) = (screen_w as f32, screen_h as f32);
        let search = R::new((sw - 240.0) * 0.5, TOP_BAR_HEIGHT as f32 + 30.0, 240.0, 30.0);
        let cols = if sw >= 1300.0 { 7 } else if sw >= 1000.0 { 6 } else { 5 };
        let rows = if sh >= 900.0 { 5 } else { 4 };
        let margin_x = (sw * 0.12).max(40.0);
        let grid_y = search.y + search.h + 40.0;
        let bottom_reserved = 150.0; // page dots + Dock
        let cell_w = (sw - 2.0 * margin_x) / cols as f32;
        let cell_h = ((sh - grid_y - bottom_reserved) / rows as f32).max(90.0);
        let icon = (cell_w * 0.56).min(cell_h - 36.0).min(96.0).max(40.0);
        Self {
            cols,
            rows,
            cell_w,
            cell_h,
            icon,
            grid_x: margin_x,
            grid_y,
            search,
            dots_y: grid_y + cell_h * rows as f32 + 14.0,
        }
    }

    /// Logical rect of the `slot`-th cell on a page
    pub fn cell(&self, slot: usize) -> R {
        let (c, r) = (slot % self.cols, slot / self.cols);
        R::new(
            self.grid_x + c as f32 * self.cell_w,
            self.grid_y + r as f32 * self.cell_h,
            self.cell_w,
            self.cell_h,
        )
    }

    pub fn tile_size(&self) -> (f32, f32) {
        (self.cell_w.floor(), (self.icon + 32.0).ceil())
    }
}

pub enum LaunchpadHit {
    App(usize),
    Dot(usize),
    Search,
    Background,
}

pub struct Launchpad {
    pub query: String,
    pub page: usize,
    pub apps: Vec<usize>,
    pub layout: LaunchpadLayout,
    tiles: HashMap<usize, MemoryRenderBuffer>,
    pub search_buffer: MemoryRenderBuffer,
    search_dirty: bool,
    pub dots_buffer: MemoryRenderBuffer,
    pub dots_rect: R,
    dots_dirty: bool,
    /// Search field clicked (shows focus ring and caret); typing works regardless
    pub search_focused: bool,
    /// Accumulated touchpad scroll for paging
    pub scroll_acc: f64,
    pub last_scroll: std::time::Instant,
    pub last_flip: std::time::Instant,
    ticket: Arc<()>,
    pending: HashSet<usize>,
}

fn new_buffer(w: i32, h: i32) -> MemoryRenderBuffer {
    MemoryRenderBuffer::new(Fourcc::Abgr8888, (w.max(1), h.max(1)), 1, Transform::Normal, None)
}

impl Launchpad {
    pub fn new(registry: &AppRegistry, screen_w: i32, screen_h: i32) -> Self {
        let layout = LaunchpadLayout::compute(screen_w, screen_h);
        Self {
            query: String::new(),
            page: 0,
            apps: registry.launchpad_apps(""),
            layout,
            tiles: HashMap::new(),
            search_buffer: new_buffer(1, 1),
            search_dirty: true,
            dots_buffer: new_buffer(1, 1),
            dots_rect: R::new(0.0, 0.0, 0.0, 0.0),
            dots_dirty: true,
            search_focused: false,
            scroll_acc: 0.0,
            last_scroll: std::time::Instant::now(),
            last_flip: std::time::Instant::now() - std::time::Duration::from_secs(1),
            ticket: Arc::new(()),
            pending: HashSet::new(),
        }
    }

    pub fn pages(&self) -> usize {
        self.apps.len().div_ceil(self.layout.per_page()).max(1)
    }

    pub fn set_page(&mut self, page: usize) {
        let page = page.min(self.pages() - 1);
        if page != self.page {
            self.page = page;
            self.cancel_pending();
            self.dots_dirty = true;
        }
    }

    /// Screen size, scale or appearance changed: recompute the grid and drop cached tiles,
    /// keeping the query and (clamped) page
    pub fn invalidate(&mut self, screen_w: i32, screen_h: i32) {
        self.layout = LaunchpadLayout::compute(screen_w, screen_h);
        self.tiles.clear();
        self.page = self.page.min(self.pages() - 1);
        self.search_dirty = true;
        self.dots_dirty = true;
        self.cancel_pending();
    }

    pub fn focus_search(&mut self, focused: bool) {
        if focused != self.search_focused {
            self.search_focused = focused;
            self.search_dirty = true;
        }
    }

    pub fn set_query(&mut self, registry: &AppRegistry, query: String) {
        self.cancel_pending();
        self.query = query;
        self.apps = registry.launchpad_apps(&self.query);
        self.page = 0;
        self.search_dirty = true;
        self.dots_dirty = true;
    }

    /// Apps on the current page with their cell rects
    pub fn page_cells(&self) -> Vec<(usize, R)> {
        let per = self.layout.per_page();
        self.apps
            .iter()
            .skip(self.page * per)
            .take(per)
            .enumerate()
            .map(|(slot, &app)| (app, self.layout.cell(slot)))
            .collect()
    }

    pub fn hit(&self, px: f32, py: f32) -> LaunchpadHit {
        if self.layout.search.contains(px, py) {
            return LaunchpadHit::Search;
        }
        let (tw, th) = self.layout.tile_size();
        for (app, cell) in self.page_cells() {
            let tile = R::new(cell.x + (cell.w - tw) * 0.5, cell.y, tw, th);
            // Only the icon + label area is clickable, not the empty space between cells
            let icon = R::new(cell.x + (cell.w - self.layout.icon) * 0.5 - 6.0, cell.y, self.layout.icon + 12.0, th);
            if icon.contains(px, py) && tile.contains(px, py) {
                return LaunchpadHit::App(app);
            }
        }
        if self.dots_rect.contains(px, py) && self.pages() > 1 {
            let n = self.pages() as f32;
            let rel = (px - self.dots_rect.x) / self.dots_rect.w;
            return LaunchpadHit::Dot(((rel * n).floor() as usize).min(self.pages() - 1));
        }
        LaunchpadHit::Background
    }

    fn cancel_pending(&mut self) { self.ticket = Arc::new(()); self.pending.clear(); }

    pub fn install_tile(&mut self, ticket: &std::sync::Weak<()>, app: usize, buffer: MemoryRenderBuffer) -> bool {
        if !std::sync::Weak::ptr_eq(ticket, &Arc::downgrade(&self.ticket)) { return false; }
        self.pending.remove(&app);
        if !self.page_cells().iter().any(|(id, _)| *id == app) { return false; }
        self.tiles.insert(app, buffer);
        true
    }

    fn request_tiles(&mut self, worker: Option<&crate::raster::RasterSender>, registry: &AppRegistry) {
        let cells = self.page_cells();
        self.tiles.retain(|id, _| cells.iter().any(|(app, _)| id == app));
        let Some(worker) = worker else { return };
        for (app, _) in cells {
            if self.tiles.contains_key(&app) || self.pending.contains(&app) { continue; }
            let entry = &registry.apps[app];
            if worker.submit(crate::raster::Job::Tile(crate::raster::TileJob {
                ticket: Arc::downgrade(&self.ticket), app, icon: AppIcon::of(entry), label: entry.name.clone(), layout: self.layout,
            })) { self.pending.insert(app); }
        }
    }

    /// Wheel/touchpad paging. A wheel notch flips one page; a touchpad swipe flips once per
    /// gesture. Returns true if the page changed.
    pub fn scroll(&mut self, delta: f64, discrete: bool) -> bool {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        if now.duration_since(self.last_scroll) > Duration::from_millis(250) {
            self.scroll_acc = 0.0;
        }
        self.last_scroll = now;
        let cooldown = if discrete { 150 } else { 350 };
        if now.duration_since(self.last_flip) < Duration::from_millis(cooldown) {
            return false;
        }
        self.scroll_acc += delta;
        let threshold = if discrete { 1.0 } else { 45.0 };
        if self.scroll_acc.abs() < threshold {
            return false;
        }
        let before = self.page;
        let next = if self.scroll_acc > 0.0 { self.page + 1 } else { self.page.saturating_sub(1) };
        self.scroll_acc = 0.0;
        self.set_page(next);
        if self.page != before {
            self.last_flip = now;
            return true;
        }
        false
    }

    /// Request visible tiles; only the small search/dots buffers are composed here.
    pub fn ensure_rendered(&mut self, fonts: &mut FontSystem, worker: Option<&crate::raster::RasterSender>, registry: &AppRegistry) {
        self.request_tiles(worker, registry);

        if self.search_dirty {
            self.search_dirty = false;
            let r = self.layout.search;
            let (pw, ph) = (to_phys(r.w as i32), to_phys(r.h as i32));
            self.search_buffer = new_buffer(pw, ph);
            let query = self.query.clone();
            let focused = self.search_focused || !query.is_empty();
            let mut ctx = self.search_buffer.render();
            let _ = ctx.draw(|data| {
                if let Some(mut pm) = PixmapMut::from_bytes(data, pw as u32, ph as u32) {
                    fill_rounded_rect(&mut pm, 0.0, 0.0, r.w, r.h, r.h * 0.5, rgba(255, 255, 255, if focused { 70 } else { 46 }));
                    if focused {
                        // macOS focus ring
                        stroke_rounded_rect(&mut pm, 1.25, 1.25, r.w - 2.5, r.h - 2.5, r.h * 0.5 - 1.25, 2.5, rgba(10, 132, 255, 220));
                    } else {
                        stroke_rounded_rect(&mut pm, 0.5, 0.5, r.w - 1.0, r.h - 1.0, r.h * 0.5 - 0.5, 1.0, rgba(255, 255, 255, 70));
                    }
                    let white = rgba(255, 255, 255, 235);
                    if query.is_empty() && focused {
                        draw_search_icon(&mut pm, 16.0, r.h * 0.5, 5.0, white);
                        fonts.draw_text(&mut pm, "Поиск", 30.0, 6.5, 14.0, rgba(255, 255, 255, 130), false);
                        fill_rounded_rect(&mut pm, 30.0, 7.0, 1.5, 16.0, 0.75, white);
                    } else if query.is_empty() {
                        let tw = fonts.measure_text("Поиск", 14.0, false);
                        let x0 = (r.w - tw - 22.0) * 0.5;
                        draw_search_icon(&mut pm, x0 + 7.0, r.h * 0.5, 5.0, rgba(255, 255, 255, 170));
                        fonts.draw_text(&mut pm, "Поиск", x0 + 22.0, 6.5, 14.0, rgba(255, 255, 255, 170), false);
                    } else {
                        draw_search_icon(&mut pm, 16.0, r.h * 0.5, 5.0, white);
                        let fitted = fonts.ellipsize(&query, r.w - 46.0, 14.0, false);
                        let qw = fonts.draw_text(&mut pm, &fitted, 30.0, 6.5, 14.0, white, false);
                        fill_rounded_rect(&mut pm, 31.0 + qw, 7.0, 1.5, 16.0, 0.75, white);
                    }
                }
                Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
            });
        }

        if self.dots_dirty {
            self.dots_dirty = false;
            let n = self.pages();
            let w = (n as f32 * 16.0).max(16.0);
            let h = 16.0;
            let screen_w = self.layout.grid_x * 2.0 + self.layout.cell_w * self.layout.cols as f32;
            self.dots_rect = R::new((screen_w - w) * 0.5, self.layout.dots_y, w, h);
            let (pw, ph) = (to_phys(w.ceil() as i32), to_phys(h as i32));
            self.dots_buffer = new_buffer(pw, ph);
            let page = self.page;
            let mut ctx = self.dots_buffer.render();
            let _ = ctx.draw(|data| {
                data.fill(0);
                if n > 1 {
                    if let Some(mut pm) = PixmapMut::from_bytes(data, pw as u32, ph as u32) {
                        for i in 0..n {
                            let a = if i == page { 240 } else { 90 };
                            fill_circle(&mut pm, 8.0 + i as f32 * 16.0, h * 0.5, 3.5, rgba(255, 255, 255, a));
                        }
                    }
                }
                Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
            });
        }
    }

    /// (logical position, buffer) of every element to draw, front-most first
    pub fn elements(&self) -> Vec<((f32, f32), &MemoryRenderBuffer)> {
        let mut out = Vec::new();
        out.push(((self.layout.search.x, self.layout.search.y), &self.search_buffer));
        out.push(((self.dots_rect.x, self.dots_rect.y), &self.dots_buffer));
        let (tw, _) = self.layout.tile_size();
        for (app, cell) in self.page_cells() {
            if let Some(buf) = self.tiles.get(&app) {
                out.push(((cell.x + (cell.w - tw) * 0.5, cell.y), buf));
            }
        }
        out
    }
}

/// CPU-only tile, built on the asset worker. No live Launchpad or GPU state is borrowed.
pub fn raster_tile(job: &crate::raster::TileJob, fonts: &mut FontSystem, icons: &mut IconLoader) -> MemoryRenderBuffer {
    let (tw, th) = job.layout.tile_size();
    let (pw, ph) = (to_phys(tw as i32), to_phys(th as i32));
    let mut buf = new_buffer(pw, ph);
    let icon = job.icon.name.as_deref().and_then(|name| icons.load(name, icon_bucket((job.layout.icon * crate::theme::ui_scale()).ceil() as u32), crate::theme::appearance() == crate::theme::Appearance::Dark));
    let mut ctx = buf.render();
    let _ = ctx.draw(|data| {
        if let Some(mut pm) = PixmapMut::from_bytes(data, pw as u32, ph as u32) {
            let ix = (tw - job.layout.icon) * 0.5;
            if let Some(icon) = &icon { draw_icon(&mut pm, icon, ix, 2.0, job.layout.icon); }
            else { crate::apps::draw_app_icon(&mut pm, fonts, job.icon.kind, &job.label, ix, 2.0, job.layout.icon); }
            let label = fonts.ellipsize(&job.label, tw - 12.0, LABEL_SIZE, false);
            let lw = fonts.measure_text(&label, LABEL_SIZE, false);
            let lx = (tw - lw) * 0.5;
            let ly = job.layout.icon + 9.0;
            fonts.draw_text(&mut pm, &label, lx, ly + 1.0, LABEL_SIZE, rgba(0, 0, 0, 120), false);
            fonts.draw_text(&mut pm, &label, lx, ly, LABEL_SIZE, rgba(255, 255, 255, 250), false);
        }
        Result::<_, ()>::Ok(vec![Rectangle::from_size((pw, ph).into())])
    });
    drop(ctx);
    buf
}

#[cfg(test)]
mod raster_tests {
    use super::*;

    #[test]
    fn old_page_and_closed_launchpad_tickets_do_not_accept_tiles() {
        let registry = AppRegistry::new();
        let mut lp = Launchpad::new(&registry, 1280, 800);
        let old = Arc::downgrade(&lp.ticket);
        let app = lp.page_cells()[0].0;
        lp.invalidate(1600, 1000);
        assert_eq!(old.strong_count(), 0);
        assert!(!lp.install_tile(&old, app, new_buffer(1, 1)));
        let current = Arc::downgrade(&lp.ticket);
        assert!(lp.install_tile(&current, app, new_buffer(1, 1)));
        let closed = Arc::downgrade(&lp.ticket);
        drop(lp);
        assert_eq!(closed.strong_count(), 0);
    }
}
