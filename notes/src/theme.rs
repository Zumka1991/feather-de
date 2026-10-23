//! Notes' palette (the Tahoe light palette of Files); scale and drawing primitives are shared with the compositor.

pub use feather_ui::draw::*;
pub use feather_ui::scale::set_ui_scale;

/// macOS Tahoe light palette
pub mod colors {
    use super::rgba;
    use tiny_skia::Color;
    pub fn window() -> Color { rgba(255, 255, 255, 255) }
    pub fn sidebar() -> Color { rgba(236, 236, 240, 255) }
    pub fn sidebar_edge() -> Color { rgba(0, 0, 0, 22) }
    pub fn text() -> Color { rgba(29, 29, 31, 255) }
    pub fn secondary() -> Color { rgba(110, 110, 115, 255) }
    pub fn tertiary() -> Color { rgba(160, 160, 166, 255) }
    pub fn hover() -> Color { rgba(0, 0, 0, 14) }
    pub fn separator() -> Color { rgba(0, 0, 0, 20) }
    pub fn control() -> Color { rgba(0, 0, 0, 13) }
}

