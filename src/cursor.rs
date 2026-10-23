//! Software/hardware cursor for the DRM backend (nested mode lets the host draw the cursor).
//!
//! Named cursors (cursor-shape protocol, our own resize cursors) come from the xcursor theme;
//! client cursor surfaces are rendered as regular surface trees. Elements are tagged
//! `Kind::Cursor`, so the DRM compositor puts them on the hardware cursor plane: moving the
//! mouse then costs no composition at all.

use std::collections::HashMap;

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Point, Transform},
    wayland::compositor::with_states,
};

/// Nominal cursor size in logical points (macOS uses a slightly larger cursor than 24)
const CURSOR_SIZE_PT: f64 = 24.0;

pub struct CursorManager {
    theme: xcursor::CursorTheme,
    /// (xcursor name, physical size) → buffer and physical hotspot
    cache: HashMap<(&'static str, u32), Option<(MemoryRenderBuffer, Point<i32, smithay::utils::Physical>)>>,
    pub status: CursorImageStatus,
}

impl CursorManager {
    pub fn new() -> Self {
        let theme_name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "breeze_cursors".into());
        Self {
            theme: xcursor::CursorTheme::load(&theme_name),
            cache: HashMap::new(),
            status: CursorImageStatus::default_named(),
        }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Named cursor image for the current scale
    pub fn named(&mut self, icon: CursorIcon, scale: f64) -> Option<&(MemoryRenderBuffer, Point<i32, smithay::utils::Physical>)> {
        let size = (CURSOR_SIZE_PT * scale).round() as u32;
        let key = (icon.name(), size);
        if !self.cache.contains_key(&key) {
            let loaded = std::iter::once(icon.name())
                .chain(icon.alt_names().iter().copied())
                .chain(std::iter::once("default"))
                .find_map(|name| self.load(name, size));
            self.cache.insert(key, loaded);
        }
        self.cache.get(&key).and_then(|c| c.as_ref())
    }

    fn load(&self, name: &str, size: u32) -> Option<(MemoryRenderBuffer, Point<i32, smithay::utils::Physical>)> {
        let path = self.theme.load_icon(name)?;
        let data = std::fs::read(path).ok()?;
        let images = xcursor::parser::parse_xcursor(&data)?;
        // Nearest nominal size; prefer the first frame of animated cursors
        let best = images
            .iter()
            .min_by_key(|img| (img.size as i64 - size as i64).abs())?;
        let buffer = MemoryRenderBuffer::from_slice(
            &best.pixels_rgba,
            Fourcc::Abgr8888,
            (best.width as i32, best.height as i32),
            1,
            Transform::Normal,
            None,
        );
        Some((buffer, Point::from((best.xhot as i32, best.yhot as i32))))
    }
}

/// Hotspot of a client cursor surface (logical)
pub fn surface_hotspot(surface: &WlSurface) -> Point<i32, smithay::utils::Logical> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<CursorImageSurfaceData>()
            .map(|d| d.lock().unwrap().hotspot)
            .unwrap_or_default()
    })
}
