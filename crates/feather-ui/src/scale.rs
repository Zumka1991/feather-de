//! UI scale, appearance, accent/highlight colors, and text size shared by the compositor and its
//! programs. Global atomics for the event-loop thread, plus per-thread snapshots so a worker's
//! job (or concurrent test) never sees a switch mid-raster.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

static UI_SCALE_BITS: AtomicU32 = AtomicU32::new(0x3f800000); // 1.0f32
static DARK_MODE: AtomicBool = AtomicBool::new(false);
static ACCENT_COLOR: AtomicU8 = AtomicU8::new(0); // AccentColor::Blue
static HIGHLIGHT_COLOR: AtomicU8 = AtomicU8::new(255); // 255 = None ("accent")
static TEXT_SIZE: AtomicU8 = AtomicU8::new(1); // TextSize::Default (100%)

#[derive(Debug, Clone, Copy)]
struct RenderSnapshot {
    scale: f32,
    appearance: Appearance,
    accent: AccentColor,
    highlight: Option<AccentColor>,
    text_size: TextSize,
}

thread_local! {
    static RENDER_SETTINGS: std::cell::Cell<Option<RenderSnapshot>> = const { std::cell::Cell::new(None) };
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

/// macOS Tahoe system accent / highlight colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AccentColor {
    Blue = 0,
    Purple = 1,
    Pink = 2,
    Red = 3,
    Orange = 4,
    Yellow = 5,
    Green = 6,
    Graphite = 7,
}

impl AccentColor {
    pub const ALL: [AccentColor; 8] = [
        AccentColor::Blue,
        AccentColor::Purple,
        AccentColor::Pink,
        AccentColor::Red,
        AccentColor::Orange,
        AccentColor::Yellow,
        AccentColor::Green,
        AccentColor::Graphite,
    ];

    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Blue,
            1 => Self::Purple,
            2 => Self::Pink,
            3 => Self::Red,
            4 => Self::Orange,
            5 => Self::Yellow,
            6 => Self::Green,
            7 => Self::Graphite,
            _ => Self::Blue,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Graphite => "graphite",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Blue => "Синий",
            Self::Purple => "Фиолетовый",
            Self::Pink => "Розовый",
            Self::Red => "Красный",
            Self::Orange => "Оранжевый",
            Self::Yellow => "Жёлтый",
            Self::Green => "Зелёный",
            Self::Graphite => "Графитовый",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "blue" => Some(Self::Blue),
            "purple" => Some(Self::Purple),
            "pink" => Some(Self::Pink),
            "red" => Some(Self::Red),
            "orange" => Some(Self::Orange),
            "yellow" => Some(Self::Yellow),
            "green" => Some(Self::Green),
            "graphite" => Some(Self::Graphite),
            _ => None,
        }
    }

    pub const fn rgb(self, appearance: Appearance) -> (u8, u8, u8) {
        match (self, appearance) {
            (Self::Blue, Appearance::Light) => (0, 122, 255),
            (Self::Blue, Appearance::Dark) => (10, 132, 255),
            (Self::Purple, Appearance::Light) => (175, 82, 222),
            (Self::Purple, Appearance::Dark) => (191, 90, 242),
            (Self::Pink, Appearance::Light) => (255, 45, 85),
            (Self::Pink, Appearance::Dark) => (255, 55, 95),
            (Self::Red, Appearance::Light) => (255, 59, 48),
            (Self::Red, Appearance::Dark) => (255, 69, 58),
            (Self::Orange, Appearance::Light) => (255, 149, 0),
            (Self::Orange, Appearance::Dark) => (255, 159, 10),
            (Self::Yellow, Appearance::Light) => (235, 180, 0),
            (Self::Yellow, Appearance::Dark) => (255, 214, 10),
            (Self::Green, Appearance::Light) => (40, 205, 65),
            (Self::Green, Appearance::Dark) => (48, 209, 88),
            (Self::Graphite, Appearance::Light) => (132, 132, 137),
            (Self::Graphite, Appearance::Dark) => (152, 152, 157),
        }
    }

    pub const fn hex(self, dark: bool) -> &'static str {
        match (self, dark) {
            (Self::Blue, false) => "#007aff",
            (Self::Blue, true) => "#0a84ff",
            (Self::Purple, false) => "#af52de",
            (Self::Purple, true) => "#bf5af2",
            (Self::Pink, false) => "#ff2d55",
            (Self::Pink, true) => "#ff375f",
            (Self::Red, false) => "#ff3b30",
            (Self::Red, true) => "#ff453a",
            (Self::Orange, false) => "#ff9500",
            (Self::Orange, true) => "#ff9f0a",
            (Self::Yellow, false) => "#ebb400",
            (Self::Yellow, true) => "#ffd60a",
            (Self::Green, false) => "#28cd41",
            (Self::Green, true) => "#30d158",
            (Self::Graphite, false) => "#848489",
            (Self::Graphite, true) => "#98989d",
        }
    }
}

/// UI text size preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TextSize {
    Small = 0,
    Default = 1,
    Large = 2,
    ExtraLarge = 3,
}

impl TextSize {
    pub const ALL: [TextSize; 4] = [
        TextSize::Small,
        TextSize::Default,
        TextSize::Large,
        TextSize::ExtraLarge,
    ];

    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Small,
            1 => Self::Default,
            2 => Self::Large,
            3 => Self::ExtraLarge,
            _ => Self::Default,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Default => "default",
            Self::Large => "large",
            Self::ExtraLarge => "xlarge",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Small => "Мелкий",
            Self::Default => "Обычный",
            Self::Large => "Крупный",
            Self::ExtraLarge => "Очень крупный",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Small => "Мелкий",
            Self::Default => "Обычный",
            Self::Large => "Крупный",
            Self::ExtraLarge => "Макс.",
        }
    }

    pub const fn percent(self) -> u16 {
        match self {
            Self::Small => 92,
            Self::Default => 100,
            Self::Large => 110,
            Self::ExtraLarge => 120,
        }
    }

    pub const fn factor(self) -> f32 {
        match self {
            Self::Small => 0.92,
            Self::Default => 1.0,
            Self::Large => 1.10,
            Self::ExtraLarge => 1.20,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "small" | "92" | "92%" => Some(Self::Small),
            "default" | "normal" | "100" | "100%" => Some(Self::Default),
            "large" | "110" | "110%" => Some(Self::Large),
            "xlarge" | "extra_large" | "120" | "120%" => Some(Self::ExtraLarge),
            _ => None,
        }
    }
}

/// Runs `draw` with this thread's scale/appearance overridden (unclamped: icons render in a
/// 100-unit design space at any pixel size) and default accent/highlight/text_size.
pub fn with_render_settings<T>(scale: f32, appearance: Appearance, draw: impl FnOnce() -> T) -> T {
    with_full_render_settings(
        scale,
        appearance,
        AccentColor::Blue,
        None,
        TextSize::Default,
        draw,
    )
}

/// Runs `draw` with this thread's full render snapshot overridden.
pub fn with_full_render_settings<T>(
    scale: f32,
    appearance: Appearance,
    accent: AccentColor,
    highlight: Option<AccentColor>,
    text_size: TextSize,
    draw: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<RenderSnapshot>);
    impl Drop for Restore {
        fn drop(&mut self) {
            RENDER_SETTINGS.with(|s| s.set(self.0));
        }
    }
    let snap = RenderSnapshot {
        scale,
        appearance,
        accent,
        highlight,
        text_size,
    };
    let _restore = Restore(RENDER_SETTINGS.with(|s| s.replace(Some(snap))));
    draw()
}

pub fn ui_scale() -> f32 {
    RENDER_SETTINGS
        .with(|s| s.get().map(|v| v.scale))
        .unwrap_or_else(|| f32::from_bits(UI_SCALE_BITS.load(Ordering::Relaxed)))
}

pub fn set_ui_scale(scale: f32) {
    let clamped = scale.clamp(1.0, 3.0);
    UI_SCALE_BITS.store(clamped.to_bits(), Ordering::Relaxed);
}

pub fn appearance() -> Appearance {
    if let Some(appearance) = RENDER_SETTINGS.with(|s| s.get().map(|v| v.appearance)) {
        return appearance;
    }
    if DARK_MODE.load(Ordering::Relaxed) {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

pub fn set_appearance(a: Appearance) {
    DARK_MODE.store(a == Appearance::Dark, Ordering::Relaxed);
}

pub fn accent_color() -> AccentColor {
    if let Some(accent) = RENDER_SETTINGS.with(|s| s.get().map(|v| v.accent)) {
        return accent;
    }
    AccentColor::from_u8(ACCENT_COLOR.load(Ordering::Relaxed))
}

pub fn set_accent_color(c: AccentColor) {
    ACCENT_COLOR.store(c as u8, Ordering::Relaxed);
}

pub fn highlight_choice() -> Option<AccentColor> {
    if let Some(h) = RENDER_SETTINGS.with(|s| s.get().map(|v| v.highlight)) {
        return h;
    }
    let raw = HIGHLIGHT_COLOR.load(Ordering::Relaxed);
    if raw < 8 {
        Some(AccentColor::from_u8(raw))
    } else {
        None
    }
}

pub fn set_highlight_choice(c: Option<AccentColor>) {
    HIGHLIGHT_COLOR.store(c.map_or(255, |v| v as u8), Ordering::Relaxed);
}

pub fn highlight_color() -> AccentColor {
    highlight_choice().unwrap_or_else(accent_color)
}

pub fn text_size() -> TextSize {
    if let Some(ts) = RENDER_SETTINGS.with(|s| s.get().map(|v| v.text_size)) {
        return ts;
    }
    TextSize::from_u8(TEXT_SIZE.load(Ordering::Relaxed))
}

pub fn set_text_size(ts: TextSize) {
    TEXT_SIZE.store(ts as u8, Ordering::Relaxed);
}

pub fn text_scale() -> f32 {
    text_size().factor()
}

