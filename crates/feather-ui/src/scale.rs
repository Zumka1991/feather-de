//! UI scale and appearance shared by the compositor and its programs. A global value for the
//! event-loop thread, plus per-thread snapshots so a worker's job never sees a switch mid-raster.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static UI_SCALE_BITS: AtomicU32 = AtomicU32::new(0x3f800000); // 1.0f32
static DARK_MODE: AtomicBool = AtomicBool::new(false);

thread_local! {
    static RENDER_SETTINGS: std::cell::Cell<Option<(f32, Appearance)>> = const { std::cell::Cell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    pub fn name(&self) -> &'static str {
        match self {
            Appearance::Light => "Светлое",
            Appearance::Dark => "Тёмное",
        }
    }
}

/// Runs `draw` with this thread's scale/appearance overridden (unclamped: icons render in a
/// 100-unit design space at any pixel size)
pub fn with_render_settings<T>(scale: f32, appearance: Appearance, draw: impl FnOnce() -> T) -> T {
    struct Restore(Option<(f32, Appearance)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            RENDER_SETTINGS.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(RENDER_SETTINGS.with(|s| s.replace(Some((scale, appearance)))));
    draw()
}

pub fn ui_scale() -> f32 {
    RENDER_SETTINGS.with(|s| s.get().map(|v| v.0)).unwrap_or_else(|| f32::from_bits(UI_SCALE_BITS.load(Ordering::Relaxed)))
}

pub fn set_ui_scale(scale: f32) {
    let clamped = scale.clamp(1.0, 3.0);
    UI_SCALE_BITS.store(clamped.to_bits(), Ordering::Relaxed);
}

pub fn appearance() -> Appearance {
    if let Some(appearance) = RENDER_SETTINGS.with(|s| s.get().map(|v| v.1)) {
        return appearance;
    }
    if DARK_MODE.load(Ordering::Relaxed) { Appearance::Dark } else { Appearance::Light }
}

pub fn set_appearance(a: Appearance) {
    DARK_MODE.store(a == Appearance::Dark, Ordering::Relaxed);
}
