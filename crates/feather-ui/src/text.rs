use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::sync::Arc;

use tiny_skia::{Color, PixmapMut};
use tiny_skia::{FillRule, Mask, PathBuilder, Transform};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::scale::{text_scale, ui_scale};

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct GlyphKey {
    ch: char,
    size_tenths: u16,
    bold: bool,
}

struct Glyph {
    metrics: Metrics,
    bitmap: Vec<u8>,
    /// Which font produced the glyph (kerning only applies within the same font)
    from_fallback: bool,
}

pub struct FontSystem {
    regular: std::sync::Arc<Font>,
    bold: std::sync::Arc<Font>,
    /// Tried in order for characters the primary font lacks (symbols, then CJK)
    fallback: Vec<std::sync::Arc<Font>>,
    cache: HashMap<GlyphKey, Glyph>,
    cache_bytes: usize,
}

/// Font families in order of preference: Apple's SF Pro if the user installed it, then Inter
/// (the closest freely licensed match), then common Linux sans fonts.
const FAMILIES: &[&str] = &[
    "SF Pro Text",
    "SF Pro Display",
    "SF Pro",
    "Inter",
    "Noto Sans",
    "DejaVu Sans",
];

/// Monospace families for the terminal, best first
const MONO_FAMILIES: &[&str] = &[
    "SF Mono",
    "JetBrains Mono",
    "Fira Code",
    "Cascadia Code",
    "Hack",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
];

/// Fonts shipped with Feather (assets/fonts): family, Regular and the bold weight. They are
/// tried right after Apple's SF fonts, so the interface never depends on the system's fonts
const BUNDLED: &[(&str, &str, &str)] = &[
    ("Inter", "Inter-Regular.otf", "Inter-SemiBold.otf"),
    ("JetBrains Mono", "JetBrainsMono-Regular.ttf", "JetBrainsMono-Bold.ttf"),
];
/// Shipped symbols fallback (⏻ and others Inter lacks)
const BUNDLED_SYMBOLS: &str = "NotoSansSymbols2-Regular.ttf";

/// A shipped font file: $FEATHER_FONTS_DIR, then <prefix>/share/feather-de/fonts beside our
/// bin/, then the source tree's assets/fonts (cargo runs and tests)
fn bundled_font(file: &str) -> Option<String> {
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    std::env::var_os("FEATHER_FONTS_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain(exe_dir.map(|d| d.join("../share/feather-de/fonts")))
        .chain([PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts"))])
        .map(|d| d.join(file))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
}

const LEGACY_REGULAR: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    // Debian/Ubuntu layout
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
];

/// Font lookups are cached on disk: every `fc-match` is a process start (~10–30 ms), and
/// a program asks 3–5 of them before its first frame. The cache is keyed by the
/// modification times of the font directories, so installing or removing a font refreshes it.
mod font_cache {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    pub type Answer = Option<(String, u32)>;

    struct Cache {
        stamp: String,
        entries: HashMap<String, Answer>,
        changed: bool,
    }

    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
        Some(base.join("feather-de").join("fonts.cache"))
    }

    /// Modification times of the system and user font directories, and of fontconfig's caches:
    /// a package adding a font to an existing subfolder (truetype/…) leaves /usr/share/fonts
    /// untouched, but its fc-cache trigger rewrites the cache folder
    fn stamp() -> String {
        let home = std::env::var("HOME").unwrap_or_default();
        let dirs = ["/usr/share/fonts".to_string(), "/usr/local/share/fonts".into(), format!("{home}/.local/share/fonts"), format!("{home}/.fonts"), "/etc/fonts/conf.d".into(), "/var/cache/fontconfig".into(), format!("{home}/.cache/fontconfig")];
        dirs.iter()
            .map(|d| std::fs::metadata(d).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()))
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn load() -> Cache {
        let stamp = stamp();
        let mut entries = HashMap::new();
        if let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) {
            let mut lines = text.lines();
            if lines.next() == Some(stamp.as_str()) {
                for line in lines {
                    let mut parts = line.split('\t');
                    let (Some(key), Some(file), Some(index)) = (parts.next(), parts.next(), parts.next()) else { continue };
                    let answer = (file != "-").then(|| (file.to_string(), index.parse().unwrap_or(0)));
                    entries.insert(key.to_string(), answer);
                }
            }
        }
        Cache { stamp, entries, changed: false }
    }

    pub fn lookup(key: &str, resolve: impl FnOnce() -> Answer) -> Answer {
        let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let cache = guard.get_or_insert_with(load);
        if let Some(answer) = cache.entries.get(key) {
            // A cached file that disappeared is looked up again
            if answer.as_ref().is_none_or(|(file, _)| std::path::Path::new(file).exists()) {
                return answer.clone();
            }
        }
        let answer = resolve();
        cache.entries.insert(key.to_string(), answer.clone());
        cache.changed = true;
        answer
    }

    /// Writes new answers (called once fonts are chosen); a temp file and rename keep
    /// concurrent programs from reading half a file
    pub fn save() {
        let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(cache) = guard.as_mut().filter(|c| c.changed) else { return };
        cache.changed = false;
        let Some(path) = path() else { return };
        let mut text = cache.stamp.clone();
        for (key, answer) in &cache.entries {
            let (file, index) = answer.clone().unwrap_or(("-".into(), 0));
            text.push_str(&format!("\n{key}\t{file}\t{index}"));
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

/// Asks fontconfig for `family:style`, accepting the result only if it really is that family
fn fc_match(family: &str, style: &str) -> Option<(String, u32)> {
    font_cache::lookup(&format!("{family}:{style}"), || fc_match_uncached(family, style))
}

fn fc_match_uncached(family: &str, style: &str) -> Option<(String, u32)> {
    let mut child = Command::new("fc-match")
        .arg("-f")
        .arg("%{family[0]}|%{file}|%{index}")
        .arg(format!("{family}:style={style}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    // Not `output()`: programs that set SIGCHLD to SIG_IGN (Terminal, Files) have the child
    // reaped by the kernel, so its wait fails with ECHILD although the answer is complete
    let _ = child.wait();
    let mut parts = text.split('|');
    let got_family = parts.next()?;
    let file = parts.next()?.to_string();
    let index = parts.next()?.trim().parse::<u32>().unwrap_or(0);
    got_family
        .to_lowercase()
        .starts_with(&family.to_lowercase())
        .then_some((file, index))
}

/// Only raw, immutable font tables are shared. Outlines are read for the requested
/// glyph and discarded after rasterization, rather than expanding the entire font.
struct Font {
    data: Arc<memmap2::Mmap>,
    index: u32,
    kern_data: Option<std::ops::Range<usize>>,
    units_per_em: u16,
}

#[derive(Clone, Copy, Default)]
struct Metrics {
    xmin: i32,
    ymin: i32,
    width: usize,
    height: usize,
    advance_width: f32,
    index: u16,
}

struct Outline(PathBuilder);
impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

impl Font {
    // Face parsing borrows the mapped tables and allocates nothing; keeping this
    // borrow local avoids a self-referential font or an unsafe extended lifetime.
    fn face(&self) -> Face<'_> {
        Face::parse(&self.data, self.index).expect("validated font")
    }

    fn lookup_glyph_index(&self, ch: char) -> u16 {
        self.face().glyph_index(ch).unwrap_or(GlyphId(0)).0
    }

    #[cfg(test)]
    fn horizontal_kern(&self, left: char, right: char, px: f32) -> Option<f32> {
        let face = self.face();
        self.kern_indices(face.glyph_index(left)?, face.glyph_index(right)?, px)
    }

    fn kern_indices(&self, left: GlyphId, right: GlyphId, px: f32) -> Option<f32> {
        let data = &self.data[self.kern_data.clone()?];
        ttf_parser::kern::Table::parse(data)?
            .subtables
            .into_iter()
            .filter(|s| s.horizontal && !s.variable && !s.has_cross_stream && !s.has_state_machine)
            .find_map(|s| s.glyphs_kerning(left, right))
            .map(|v| v as f32 * px / self.units_per_em as f32)
    }

    fn rasterize(&self, ch: char, px: f32) -> (Metrics, Vec<u8>) {
        let face = self.face();
        let id = face.glyph_index(ch).unwrap_or(GlyphId(0));
        let scale = px / face.units_per_em() as f32;
        let mut metrics = Metrics {
            advance_width: face.glyph_hor_advance(id).unwrap_or(0) as f32 * scale,
            index: id.0,
            ..Metrics::default()
        };
        let mut outline = Outline(PathBuilder::new());
        if !px.is_finite() || px <= 0.0 || face.outline_glyph(id, &mut outline).is_none() {
            return (metrics, Vec::new());
        }
        let Some(path) = outline.0.finish() else {
            return (metrics, Vec::new());
        };
        // Tight curve bounds, then integer pixel coverage. Font coordinates point
        // upwards; our cached mask is top-to-bottom, like the previous rasterizer.
        let Some(bounds) = path.compute_tight_bounds() else {
            return (metrics, Vec::new());
        };
        metrics.xmin = (bounds.left() * scale).floor() as i32;
        metrics.ymin = (bounds.top() * scale).floor() as i32;
        let top = (bounds.bottom() * scale).ceil();
        metrics.width = ((bounds.right() * scale).ceil() as i32 - metrics.xmin).max(0) as usize;
        metrics.height = (top as i32 - metrics.ymin).max(0) as usize;
        let Some(mut mask) = Mask::new(metrics.width as u32, metrics.height as u32) else {
            return (
                Metrics {
                    width: 0,
                    height: 0,
                    ..metrics
                },
                Vec::new(),
            );
        };
        mask.fill_path(
            &path,
            FillRule::Winding,
            true,
            Transform::from_row(scale, 0.0, 0.0, -scale, -(metrics.xmin as f32), top),
        );
        (metrics, mask.take())
    }
}

#[derive(Default)]
struct FontFiles(HashMap<String, Arc<memmap2::Mmap>>);
impl FontFiles {
    fn load(&mut self, path: &str, index: u32) -> Option<Arc<Font>> {
        let data = if let Some(data) = self.0.get(path) {
            data.clone()
        } else {
            let file = fs::File::open(path).ok()?;
            // System font files are immutable for this mapping's lifetime; package
            // updates replace their inode. No writable view is ever exposed.
            let data = Arc::new(unsafe { memmap2::Mmap::map(&file).ok()? });
            self.0.insert(path.to_owned(), data.clone());
            data
        };
        let face = Face::parse(&data, index).ok()?;
        let units_per_em = face.units_per_em();
        let kern_data = face
            .raw_face()
            .table(ttf_parser::Tag::from_bytes(b"kern"))
            .map(|table| {
                let offset = table.as_ptr() as usize - data.as_ptr() as usize;
                offset..offset + table.len()
            });
        Some(Arc::new(Font {
            data,
            index,
            kern_data,
            units_per_em,
        }))
    }
}

/// Terminal cell geometry in physical pixels
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Baseline offset from the top of the cell
    pub ascent: f32,
}

impl FontSystem {
    pub fn new() -> Self {
        Self::with_families(FAMILIES, ["Semibold", "SemiBold", "Bold"])
    }

    /// The terminal font: a monospace family, with its Bold
    pub fn new_monospace() -> Self {
        Self::with_families(MONO_FAMILIES, ["Bold", "Semibold", "SemiBold"])
    }

    fn with_families(families: &[&str], bold_styles: [&str; 3]) -> Self {
        let mut files = FontFiles::default();
        let mut chosen = None;
        for family in families {
            if let Some(&(_, reg_file, bold_file)) = BUNDLED.iter().find(|b| b.0 == *family) {
                if let Some(regular) = bundled_font(reg_file).and_then(|p| files.load(&p, 0)) {
                    let bold = bundled_font(bold_file).and_then(|p| files.load(&p, 0)).unwrap_or_else(|| regular.clone());
                    chosen = Some((regular, bold, family));
                    break;
                }
            }
            let Some((reg_path, reg_idx)) = fc_match(family, "Regular") else {
                continue;
            };
            let Some(regular) = files.load(&reg_path, reg_idx) else {
                continue;
            };
            // macOS uses Semibold for app names, titles and headings; terminals use Bold
            let bold = bold_styles
                .iter()
                .find_map(|st| fc_match(family, st))
                .and_then(|(p, i)| files.load(&p, i))
                .unwrap_or_else(|| regular.clone());
            chosen = Some((regular, bold, family));
            break;
        }

        let (regular, bold) = match chosen {
            Some((r, b, family)) => {
                tracing::info!("Шрифт интерфейса: {family}");
                (r, b)
            }
            None => {
                let r = LEGACY_REGULAR
                    .iter()
                    .find_map(|p| files.load(p, 0))
                    .expect("Не найден системный шрифт (Inter / Noto Sans / DejaVu Sans)");
                (r.clone(), r)
            }
        };

        // Symbols like ⌘ ⇧ ⌥ ↵ are missing from many UI fonts; Chinese, Japanese and
        // Korean need a CJK font. Files are mapped, so unused fallbacks cost no memory.
        let groups: [&[&str]; 3] = [
            // Debian/Ubuntu spell it "Noto Sans Symbols2"
            &["Noto Sans Symbols 2", "Noto Sans Symbols2", "Noto Sans Symbols"],
            &["Noto Sans CJK SC", "Noto Sans CJK TC", "Noto Sans CJK JP", "Noto Sans CJK KR", "Source Han Sans", "WenQuanYi Zen Hei", "Droid Sans Fallback"],
            &["DejaVu Sans"],
        ];
        let symbols = bundled_font(BUNDLED_SYMBOLS).and_then(|p| files.load(&p, 0));
        let system = &groups[usize::from(symbols.is_some())..];
        let fallback = symbols
            .into_iter()
            .chain(system.iter().filter_map(|group| group.iter().find_map(|f| fc_match(f, "Regular")).and_then(|(p, i)| files.load(&p, i))))
            .collect();

        font_cache::save();
        Self {
            regular,
            bold,
            fallback,
            cache: HashMap::with_capacity(256),
            cache_bytes: 0,
        }
    }

    /// Share immutable font tables; each raster thread owns its own small glyph cache.
    pub fn for_worker(&self) -> Self {
        Self {
            regular: self.regular.clone(),
            bold: self.bold.clone(),
            fallback: self.fallback.clone(),
            cache: HashMap::new(),
            cache_bytes: 0,
        }
    }

    fn get_glyph(&mut self, ch: char, phys_size: f32, is_bold: bool) -> &Glyph {
        let key = GlyphKey {
            ch,
            size_tenths: (phys_size * 10.0).round() as u16,
            bold: is_bold,
        };
        if !self.cache.contains_key(&key) {
            let primary = if is_bold { &self.bold } else { &self.regular };
            let fallback = if primary.lookup_glyph_index(ch) == 0 {
                self.fallback.iter().find(|fb| fb.lookup_glyph_index(ch) != 0)
            } else {
                None
            };
            let (font, from_fallback) = match fallback {
                Some(fb) => (fb, true),
                None => (primary, false),
            };
            let (metrics, bitmap) = font.rasterize(ch, phys_size);
            // Bounded per-thread bitmap cache; a scale change or arbitrary labels
            // cannot retain every glyph size ever seen. Typical visible text fits.
            if self.cache_bytes + bitmap.len() > 1024 * 1024 || self.cache.len() >= 4096 {
                self.cache.clear();
                self.cache_bytes = 0;
            }
            self.cache_bytes += bitmap.len();
            self.cache.insert(
                key,
                Glyph {
                    metrics,
                    bitmap,
                    from_fallback,
                },
            );
        }
        self.cache.get(&key).unwrap()
    }

    fn kern(&self, prev: char, ch: char, phys_size: f32, is_bold: bool) -> f32 {
        let font = if is_bold { &self.bold } else { &self.regular };
        if font.kern_data.is_none() {
            return 0.0;
        }
        let key = |ch| GlyphKey {
            ch,
            size_tenths: (phys_size * 10.0).round() as u16,
            bold: is_bold,
        };
        // Both glyphs were measured just before kerning. Their cached indices
        // avoid reparsing an entire TTC face for each pair in the label.
        let index = |ch| {
            self.cache
                .get(&key(ch))
                .map_or_else(|| font.lookup_glyph_index(ch), |g| g.metrics.index)
        };
        font.kern_indices(GlyphId(index(prev)), GlyphId(index(ch)), phys_size)
            .unwrap_or(0.0)
    }

    /// Advance of `text` in physical pixels, including kerning
    fn advance_phys(&mut self, text: &str, phys_size: f32, is_bold: bool) -> f32 {
        let _profile = crate::cpu_profile::Span::new(crate::cpu_profile::Part::Metrics);
        let mut width = 0.0;
        let mut prev: Option<(char, bool)> = None;
        for ch in text.chars() {
            let glyph = self.get_glyph(ch, phys_size, is_bold);
            let adv = glyph.metrics.advance_width;
            let fb = glyph.from_fallback;
            if let Some((p, pfb)) = prev {
                if !fb && !pfb {
                    width += self.kern(p, ch, phys_size, is_bold);
                }
            }
            width += adv;
            prev = Some((ch, fb));
        }
        width
    }

    /// Measures literal text width without UI translation (used by text input fields).
    pub fn measure_raw(&mut self, text: &str, logical_size: f32, is_bold: bool) -> f32 {
        let s = ui_scale();
        let ts = text_scale();
        self.advance_phys(text, logical_size * ts * s, is_bold) / s
    }

    /// Measures text width in logical points (HiDPI scale-aware, text-size-aware, and localized).
    pub fn measure_text(&mut self, text: &str, logical_size: f32, is_bold: bool) -> f32 {
        let translated = crate::i18n::tr(text);
        self.measure_raw(&translated, logical_size, is_bold)
    }

    /// Cell size for a monospace font at `phys_size` pixels: the advance of "M" and the
    /// font's line height (ascender − descender + gap), rounded to whole pixels
    pub fn cell_metrics(&mut self, phys_size: f32) -> CellMetrics {
        let width = self.get_glyph('M', phys_size, false).metrics.advance_width.round().max(1.0);
        let face = self.regular.face();
        let units = face.units_per_em() as f32;
        let (asc, desc, gap) = (face.ascender() as f32, face.descender() as f32, face.line_gap() as f32);
        let height = ((asc - desc + gap) * phys_size / units).round().max(1.0);
        let ascent = ((asc + gap * 0.5) * phys_size / units).round();
        CellMetrics { width, height, ascent }
    }

    /// One glyph at physical pixel position: `x` is the cell's left edge, `baseline` its
    /// baseline. Colour components are straight (not premultiplied) 0–255.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_glyph(&mut self, pixmap: &mut PixmapMut, ch: char, x: i32, baseline: i32, phys_size: f32, rgb: [u8; 3], bold: bool) {
        let glyph = self.get_glyph(ch, phys_size, bold);
        let m = glyph.metrics;
        let (gw, gh) = (m.width as i32, m.height as i32);
        if gw == 0 || gh == 0 {
            return;
        }
        let gx0 = x + m.xmin;
        let gy0 = baseline - gh - m.ymin;
        let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
        let data = pixmap.data_mut();
        let (cr, cg, cb) = (rgb[0] as u32, rgb[1] as u32, rgb[2] as u32);
        for row in 0.max(-gy0)..gh.min(ph - gy0) {
            let py = gy0 + row;
            let src = &glyph.bitmap[(row * gw) as usize..((row + 1) * gw) as usize];
            for col in 0.max(-gx0)..gw.min(pw - gx0) {
                let a = src[col as usize] as u32;
                if a == 0 {
                    continue;
                }
                let idx = ((py * pw + gx0 + col) * 4) as usize;
                let inv = 255 - a;
                data[idx] = ((cr * a + data[idx] as u32 * inv) / 255) as u8;
                data[idx + 1] = ((cg * a + data[idx + 1] as u32 * inv) / 255) as u8;
                data[idx + 2] = ((cb * a + data[idx + 2] as u32 * inv) / 255) as u8;
                data[idx + 3] = (a + data[idx + 3] as u32 * inv / 255) as u8;
            }
        }
    }

    /// Draws literal UTF-8 text without UI translation (used by text input fields).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_raw(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        logical_x: f32,
        logical_y: f32,
        logical_size: f32,
        color: Color,
        is_bold: bool,
    ) -> f32 {
        let _profile = crate::cpu_profile::Span::new(crate::cpu_profile::Part::Text);
        let s = ui_scale();
        let ts = text_scale();
        let phys_size = logical_size * ts * s;
        let start_x = logical_x * s;
        let mut pen_x = start_x;
        let baseline_y = (logical_y * s) + (logical_size * s) * (0.46 + 0.36 * ts);
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;

        let ca = (color.alpha() * 255.0) as u32;
        let cr = (color.red() * 255.0) as u32;
        let cg = (color.green() * 255.0) as u32;
        let cb = (color.blue() * 255.0) as u32;

        let mut prev: Option<(char, bool)> = None;
        for ch in text.chars() {
            let from_fallback = self.get_glyph(ch, phys_size, is_bold).from_fallback;
            if let Some((p, pfb)) = prev {
                if !from_fallback && !pfb {
                    pen_x += self.kern(p, ch, phys_size, is_bold);
                }
            }
            prev = Some((ch, from_fallback));

            let glyph = self.get_glyph(ch, phys_size, is_bold);
            let metrics = glyph.metrics;
            let gx0 = (pen_x + metrics.xmin as f32).round() as i32;
            let gy0 = (baseline_y - metrics.height as f32 - metrics.ymin as f32).round() as i32;
            let gw = metrics.width as i32;
            let gh = metrics.height as i32;
            let data = pixmap.data_mut();

            for row in 0..gh {
                let py = gy0 + row;
                if py < 0 || py >= ph {
                    continue;
                }
                for col in 0..gw {
                    let px = gx0 + col;
                    if px < 0 || px >= pw {
                        continue;
                    }
                    let cov = glyph.bitmap[(row * gw + col) as usize] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let alpha = (cov * ca) / 255;
                    if alpha == 0 {
                        continue;
                    }
                    let sr = (cr * alpha) / 255;
                    let sg = (cg * alpha) / 255;
                    let sb = (cb * alpha) / 255;
                    let inv_a = 255 - alpha;

                    let idx = ((py * pw + px) * 4) as usize;
                    data[idx] = (sr + (data[idx] as u32 * inv_a) / 255).min(255) as u8;
                    data[idx + 1] = (sg + (data[idx + 1] as u32 * inv_a) / 255).min(255) as u8;
                    data[idx + 2] = (sb + (data[idx + 2] as u32 * inv_a) / 255).min(255) as u8;
                    data[idx + 3] = (alpha + (data[idx + 3] as u32 * inv_a) / 255).min(255) as u8;
                }
            }

            pen_x += metrics.advance_width;
        }

        (pen_x - start_x) / s
    }

    /// Draws UTF-8 text onto a physical HiDPI pixmap at logical `(x, y)` (top of the line box).
    /// Returns the logical width advanced.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        logical_x: f32,
        logical_y: f32,
        logical_size: f32,
        color: Color,
        is_bold: bool,
    ) -> f32 {
        let translated = crate::i18n::tr(text);
        self.draw_raw(
            pixmap,
            &translated,
            logical_x,
            logical_y,
            logical_size,
            color,
            is_bold,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_centered(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        center_x: f32,
        y: f32,
        size: f32,
        color: Color,
        is_bold: bool,
    ) {
        let translated = crate::i18n::tr(text);
        let w = self.measure_raw(&translated, size, is_bold);
        self.draw_raw(pixmap, &translated, center_x - w * 0.5, y, size, color, is_bold);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_right(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        right_x: f32,
        y: f32,
        size: f32,
        color: Color,
        is_bold: bool,
    ) -> f32 {
        let translated = crate::i18n::tr(text);
        let w = self.measure_raw(&translated, size, is_bold);
        self.draw_raw(pixmap, &translated, right_x - w, y, size, color, is_bold);
        w
    }

    fn ellipsize_raw(&mut self, text: &str, max_width: f32, size: f32, is_bold: bool) -> String {
        if self.measure_raw(text, size, is_bold) <= max_width {
            return text.to_string();
        }
        let scale = ui_scale();
        let phys_size = size * text_scale() * scale;
        let limit = max_width * scale - self.advance_phys("…", phys_size, is_bold);
        let mut out = String::new();
        let mut width = 0.0;
        let mut prev: Option<(char, bool)> = None;
        for ch in text.chars() {
            let glyph = self.get_glyph(ch, phys_size, is_bold);
            let (advance, fallback) = (glyph.metrics.advance_width, glyph.from_fallback);
            let kerning = prev
                .filter(|(_, fb)| !*fb && !fallback)
                .map_or(0.0, |(p, _)| self.kern(p, ch, phys_size, is_bold));
            width += kerning + advance;
            if width > limit {
                break;
            }
            out.push(ch);
            prev = Some((ch, fallback));
        }
        out.push('…');
        out
    }

    /// Truncates `text` with an ellipsis so it fits `max_width` logical points
    pub fn ellipsize(&mut self, text: &str, max_width: f32, size: f32, is_bold: bool) -> String {
        let translated = crate::i18n::tr(text);
        self.ellipsize_raw(&translated, max_width, size, is_bold)
    }

    #[allow(clippy::too_many_arguments)]
    /// Finder/desktop labels: up to two lines, preferring a break after a space, dash,
    /// underscore or dot; a word broken mid-way leaves at least three letters for line two,
    /// and the second line ends with an ellipsis when it still does not fit
    pub fn wrap_two_lines(&mut self, text: &str, width: f32, size: f32, is_bold: bool) -> Vec<String> {
        let translated = crate::i18n::tr(text);
        let text = translated.as_ref();
        if self.measure_raw(text, size, is_bold) <= width {
            return vec![text.to_string()];
        }
        let chars: Vec<char> = text.chars().collect();
        let mut split = 0;
        for end in 1..=chars.len() {
            let line: String = chars[..end].iter().collect();
            if self.measure_raw(&line, size, is_bold) > width {
                break;
            }
            split = end;
        }
        match chars[..split].iter().rposition(|c| matches!(c, ' ' | '-' | '_' | '.')) {
            Some(p) if p > split / 2 => split = p + 1,
            _ if chars.len() - split < 3 => split = chars.len().saturating_sub(3).max(1),
            _ => {}
        }
        let split = split.max(1);
        let first: String = chars[..split].iter().collect();
        let rest: String = chars[split..].iter().collect();
        vec![first.trim_end().to_string(), self.ellipsize_raw(rest.trim_start(), width, size, is_bold)]
    }

    pub fn draw_text_clipped(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        x: f32,
        y: f32,
        max_width: f32,
        size: f32,
        color: Color,
        is_bold: bool,
    ) {
        let fitted = self.ellipsize(text, max_width, size, is_bold);
        self.draw_raw(pixmap, &fitted, x, y, size, color, is_bold);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_clipped_centered(
        &mut self,
        pixmap: &mut PixmapMut,
        text: &str,
        center_x: f32,
        y: f32,
        max_width: f32,
        size: f32,
        color: Color,
        is_bold: bool,
    ) {
        let fitted = self.ellipsize(text, max_width, size, is_bold);
        let w = self.measure_raw(&fitted, size, is_bold);
        self.draw_raw(pixmap, &fitted, center_x - w * 0.5, y, size, color, is_bold);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_raster_keeps_advances_kerning_and_glyph_placement() {
        let fonts = FontSystem::new();
        for font in [
            &fonts.regular,
            &fonts.bold,
            &fonts.fallback[0],
        ] {
            let reference = fontdue::Font::from_bytes(
                &font.data[..],
                fontdue::FontSettings {
                    collection_index: font.index,
                    ..Default::default()
                },
            )
            .unwrap();
            for px in [13.0, 26.0, 32.5, 64.0] {
                for ch in [
                    'A', 'V', 'g', 'é', 'Ж', 'Й', 'я', ' ', '\u{301}', '⌘', '⇧', '↵',
                ] {
                    let expected = reference.metrics(ch, px);
                    let (actual, bitmap) = font.rasterize(ch, px);
                    assert!(
                        (actual.advance_width - expected.advance_width).abs() < 0.001,
                        "advance {ch} at {px}"
                    );
                    // Exact curves replace fontdue's pre-flattened outlines. Their
                    // extrema may cover one extra edge pixel; baseline stays put.
                    assert!((actual.xmin - expected.xmin).abs() <= 1, "xmin {ch}");
                    assert!((actual.ymin - expected.ymin).abs() <= 1, "ymin {ch}");
                    assert!(
                        actual.width.abs_diff(expected.width) <= 1,
                        "width {ch}: {} vs {}",
                        actual.width,
                        expected.width
                    );
                    assert!(
                        actual.height.abs_diff(expected.height) <= 1,
                        "height {ch}: {} vs {}",
                        actual.height,
                        expected.height
                    );
                    assert_eq!(bitmap.len(), actual.width * actual.height);
                    if ch != ' ' {
                        assert!(bitmap.iter().any(|v| *v > 0), "empty {ch}");
                    }
                }
                for (a, b) in [('A', 'V'), ('T', 'o'), ('Ж', 'я')] {
                    assert!(
                        (font.horizontal_kern(a, b, px).unwrap_or(0.0)
                            - reference.horizontal_kern(a, b, px).unwrap_or(0.0))
                        .abs()
                            < 0.001
                    );
                }
            }
        }
    }

    #[test]
    fn worker_shares_font_tables_but_not_glyph_bitmaps() {
        let mut fonts = FontSystem::new();
        let mut worker = fonts.for_worker();
        assert!(Arc::ptr_eq(&fonts.regular, &worker.regular));
        fonts.get_glyph('Ж', 26.0, false);
        assert!(worker.cache.is_empty());
        worker.get_glyph('⌘', 30.0, false);
        assert_eq!(fonts.cache.len(), 1);
        assert_eq!(worker.cache.len(), 1);
        // Opening two collection faces must reuse the same mapped file.
        let path = bundled_font("Inter-Regular.otf").unwrap();
        let mut files = FontFiles::default();
        let first = files.load(&path, 0).unwrap();
        let second = files.load(&path, 0).unwrap();
        assert!(Arc::ptr_eq(&first.data, &second.data));
    }

    #[test]
    fn scale_and_label_churn_cannot_grow_the_bitmap_cache_without_bound() {
        let mut fonts = FontSystem::new();
        let mut trimmed = false;
        for px in [18.0, 26.0, 32.5, 48.0, 64.0, 80.0, 96.0] {
            for ch in ('!'..='~').chain('А'..='я') {
                let before = fonts.cache_bytes;
                fonts.get_glyph(ch, px, false);
                trimmed |= fonts.cache_bytes < before;
                assert!(fonts.cache_bytes <= 1024 * 1024);
            }
        }
        assert!(trimmed);
        let bitmap = &fonts.get_glyph('Ж', 26.0, false).bitmap;
        assert!(bitmap.iter().any(|v| *v != 0));
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn linear_ellipsis_matches_prefix_measurement_including_kerning_and_fallback() {
        let mut fonts = FontSystem::new();
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            crate::scale::with_render_settings(scale, crate::scale::Appearance::Light, || {
                for bold in [false, true] {
                    for text in [
                        "AVATAR Toffee",
                        "Не беспокоить",
                        "Тёмный режим",
                        "Wi‑Fi ⌘⇧↵",
                        "Очень длинная подпись приложения",
                    ] {
                        for max in [0.0, 12.0, 40.0, 80.0, 120.0, 220.0] {
                            let expected = if fonts.measure_text(text, 13.0, bold) <= max {
                                text.to_owned()
                            } else {
                                let ellipsis = fonts.measure_text("…", 13.0, bold);
                                let mut out = String::new();
                                for ch in text.chars() {
                                    out.push(ch);
                                    if fonts.measure_text(&out, 13.0, bold) + ellipsis > max {
                                        out.pop();
                                        break;
                                    }
                                }
                                out.push('…');
                                out
                            };
                            assert_eq!(
                                fonts.ellipsize(text, max, 13.0, bold),
                                expected,
                                "{text} {scale} {bold} {max}"
                            );
                        }
                    }
                }
            });
        }
    }
}
