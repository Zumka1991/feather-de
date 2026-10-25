//! Document loader & renderer for «Просмотр» (`feather-preview`):
//! - Raster images: PNG, JPEG, WebP (via `image` with EXIF orientation and memory bounds)
//! - Vector graphics: SVG (via `resvg`)
//! - PDF documents: multi-page PDF support with both a built-in pure-Rust PDF parser/rasterizer
//!   (handling `/Type /Page`, `/MediaBox`, raw & `/FlateDecode` content streams, PDF text
//!   operators `BT..ET`, `Tj`, `TJ`, `Td`, `TD`, `Tm`, `T*`, `Tf` and vector operators
//!   `m`, `l`, `re`, `S`, `f`, `B`, `rg`, `RG`, `g`, `G`, `w`) and optional `pdftoppm`
//!   acceleration when Poppler is available.

use feather_ui::text::FontSystem;
use image::ImageDecoder;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

const MAX_IMAGE_BYTES: u64 = 48 * 1024 * 1024;
const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PDF_PAGES: usize = 64;
const THUMB_MAX_W: u32 = 112;
const THUMB_MAX_H: u32 = 148;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocFormat {
    Png,
    Jpeg,
    Webp,
    Svg,
    Pdf,
}

impl DocFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Webp => "WebP",
            Self::Svg => "SVG",
            Self::Pdf => "PDF",
        }
    }

    pub fn is_pdf(self) -> bool {
        matches!(self, Self::Pdf)
    }
}

#[derive(Clone)]
pub struct RenderedPage {
    pub pixmap: Pixmap,
    pub thumb: Pixmap,
    pub orig_w: u32,
    pub orig_h: u32,
    pub has_alpha: bool,
}

#[derive(Clone)]
pub struct Document {
    pub path: PathBuf,
    pub title: String,
    pub format: DocFormat,
    pub file_size: u64,
    pub pages: Vec<RenderedPage>,
}

impl Document {
    pub fn rotate_page(&mut self, page_idx: usize, clockwise: bool) {
        let Some(page) = self.pages.get_mut(page_idx) else {
            return;
        };
        if let Some(rotated) = rotate_pixmap_90(&page.pixmap, clockwise) {
            page.thumb = make_thumbnail(&rotated, THUMB_MAX_W, THUMB_MAX_H);
            page.pixmap = rotated;
            std::mem::swap(&mut page.orig_w, &mut page.orig_h);
        }
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["байт", "КБ", "МБ", "ГБ", "ТБ"];
    if bytes < 1000 {
        return format!("{bytes} {}", UNITS[0]);
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit]).replace('.', ",")
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

pub fn rotate_pixmap_90(src: &Pixmap, clockwise: bool) -> Option<Pixmap> {
    let (w, h) = (src.width(), src.height());
    let mut dst = Pixmap::new(h, w)?;
    let src_px = src.data();
    let dst_px = dst.data_mut();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = if clockwise {
                (h - 1 - y, x)
            } else {
                (y, w - 1 - x)
            };
            let si = (y * w + x) as usize * 4;
            let di = (dy * h + dx) as usize * 4;
            dst_px[di..di + 4].copy_from_slice(&src_px[si..si + 4]);
        }
    }
    Some(dst)
}

pub fn make_thumbnail(src: &Pixmap, max_w: u32, max_h: u32) -> Pixmap {
    let (sw, sh) = (src.width().max(1) as f32, src.height().max(1) as f32);
    let scale = (max_w as f32 / sw).min(max_h as f32 / sh).min(1.0);
    let tw = (sw * scale).round().max(1.0) as u32;
    let th = (sh * scale).round().max(1.0) as u32;
    let mut out = Pixmap::new(tw, th).unwrap_or_else(|| Pixmap::new(1, 1).unwrap());
    let paint = PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..Default::default()
    };
    out.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &paint,
        Transform::from_scale(tw as f32 / sw, th as f32 / sh),
        None,
    );
    out
}

pub fn load(path: &Path) -> Result<Document, String> {
    load_inner(path).map_err(|e| format!("Не удалось открыть документ: {e}"))
}

fn load_inner(path: &Path) -> Result<Document, Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() {
        return Err("Выбрана папка, а не файл".into());
    }
    if !meta.is_file() {
        return Err("Объект не является обычным файлом".into());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err("Файл изменился во время открытия".into());
    }
    let file_size = meta.len();
    let title = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Документ".into());
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "pdf" => {
            if file_size > MAX_PDF_BYTES {
                return Err("PDF-документ превышает лимит 64 МБ".into());
            }
            let pages = load_pdf(path, file)?;
            Ok(Document {
                path: path.to_path_buf(),
                title,
                format: DocFormat::Pdf,
                file_size,
                pages,
            })
        }
        "svg" => {
            if file_size > MAX_IMAGE_BYTES {
                return Err("SVG-файл слишком велик".into());
            }
            let mut bytes = Vec::new();
            file.take(MAX_IMAGE_BYTES).read_to_end(&mut bytes)?;
            let page = decode_svg(&bytes)?;
            Ok(Document {
                path: path.to_path_buf(),
                title,
                format: DocFormat::Svg,
                file_size,
                pages: vec![page],
            })
        }
        _ => {
            if file_size > MAX_IMAGE_BYTES {
                return Err("Изображение превышает лимит 48 МБ".into());
            }
            let (format, page) = decode_raster_image(file, &ext)?;
            Ok(Document {
                path: path.to_path_buf(),
                title,
                format,
                file_size,
                pages: vec![page],
            })
        }
    }
}

fn decode_raster_image(
    file: std::fs::File,
    ext: &str,
) -> Result<(DocFormat, RenderedPage), Box<dyn std::error::Error>> {
    let reader = image::ImageReader::new(BufReader::new(file)).with_guessed_format()?;
    let guessed = reader.format();
    let format = match guessed {
        Some(image::ImageFormat::Png) => DocFormat::Png,
        Some(image::ImageFormat::Jpeg) => DocFormat::Jpeg,
        Some(image::ImageFormat::WebP) => DocFormat::Webp,
        _ => match ext {
            "png" => DocFormat::Png,
            "jpg" | "jpeg" => DocFormat::Jpeg,
            "webp" => DocFormat::Webp,
            _ => return Err("Неподдерживаемый формат файла".into()),
        },
    };
    let mut reader = reader;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let (orig_w, orig_h) = decoder.dimensions();
    if orig_w == 0 || orig_h == 0 || orig_w as u64 * orig_h as u64 > 16_000_000 {
        return Err("Недопустимый размер изображения".into());
    }
    let orientation = decoder.orientation()?;
    let mut picture = image::DynamicImage::from_decoder(decoder)?;
    picture.apply_orientation(orientation);
    let (orig_w, orig_h) = (picture.width(), picture.height());
    if orig_w > 2560 || orig_h > 2560 {
        picture = picture.thumbnail(2560, 2560);
    }
    let rgba = picture.into_rgba8();
    let (pw, ph) = rgba.dimensions();
    let mut pixels = rgba.into_raw();
    let mut has_alpha = false;
    for p in pixels.chunks_exact_mut(4) {
        let a = p[3] as u16;
        if a < 255 {
            has_alpha = true;
        }
        for c in &mut p[..3] {
            *c = ((*c as u16 * a + 127) / 255) as u8;
        }
    }
    let pixmap = Pixmap::from_vec(
        pixels,
        tiny_skia::IntSize::from_wh(pw, ph).ok_or("Пустое изображение")?,
    )
    .ok_or("Пустое изображение")?;
    let thumb = make_thumbnail(&pixmap, THUMB_MAX_W, THUMB_MAX_H);
    Ok((
        format,
        RenderedPage {
            pixmap,
            thumb,
            orig_w,
            orig_h,
            has_alpha,
        },
    ))
}

fn decode_svg(bytes: &[u8]) -> Result<RenderedPage, Box<dyn std::error::Error>> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &opt)?;
    let size = tree.size();
    let orig_w = size.width().round().max(1.0) as u32;
    let orig_h = size.height().round().max(1.0) as u32;
    let scale = (1600.0 / size.width().max(1.0))
        .min(1600.0 / size.height().max(1.0))
        .clamp(0.25, 4.0);
    let rw = (size.width() * scale).round().max(1.0) as u32;
    let rh = (size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = Pixmap::new(rw, rh).ok_or("Не удалось создать буфер SVG")?;
    resvg::render(
        &tree,
        Transform::from_scale(rw as f32 / size.width(), rh as f32 / size.height()),
        &mut pixmap.as_mut(),
    );
    let thumb = make_thumbnail(&pixmap, THUMB_MAX_W, THUMB_MAX_H);
    Ok(RenderedPage {
        pixmap,
        thumb,
        orig_w,
        orig_h,
        has_alpha: true,
    })
}

fn load_pdf(
    path: &Path,
    file: std::fs::File,
) -> Result<Vec<RenderedPage>, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    file.take(MAX_PDF_BYTES).read_to_end(&mut bytes)?;
    if !bytes.starts_with(b"%PDF-") {
        return Err("Файл не является документом PDF".into());
    }

    // Try external `pdftoppm` first when available (unless FEATHER_PREVIEW_NATIVE_PDF is set),
    // then fall back to our built-in pure-Rust PDF parser & rasterizer.
    if std::env::var_os("FEATHER_PREVIEW_NATIVE_PDF").is_none() {
        if let Some(pages) = try_pdftoppm(path) {
            if !pages.is_empty() {
                return Ok(pages);
            }
        }
    }

    render_pdf_native(&bytes)
}

fn try_pdftoppm(path: &Path) -> Option<Vec<RenderedPage>> {
    let out_dir = std::env::temp_dir().join(format!(
        "feather-pdf-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&out_dir).ok()?;
    let prefix = out_dir.join("page");
    let status = std::process::Command::new("pdftoppm")
        .arg("-png")
        .arg("-r")
        .arg("132")
        .arg("-f")
        .arg("1")
        .arg("-l")
        .arg(MAX_PDF_PAGES.to_string())
        .arg(path)
        .arg(&prefix)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&out_dir);
        return None;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&out_dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    files.sort();
    let mut pages = Vec::new();
    for png_path in files.into_iter().take(MAX_PDF_PAGES) {
        if let Ok(f) = std::fs::File::open(&png_path) {
            if let Ok((_, mut page)) = decode_raster_image(f, "png") {
                page.has_alpha = false;
                pages.push(page);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    Some(pages)
}

// ── Built-in pure-Rust PDF parser & rasterizer ────────────────────────────────

#[derive(Clone, Debug)]
struct PdfPageSpec {
    width_pt: f32,
    height_pt: f32,
    streams: Vec<Vec<u8>>,
}

pub fn render_pdf_native(bytes: &[u8]) -> Result<Vec<RenderedPage>, Box<dyn std::error::Error>> {
    let specs = parse_pdf_pages(bytes);
    let mut fonts = FontSystem::new();
    let mut pages = Vec::with_capacity(specs.len().max(1));
    for (idx, spec) in specs.into_iter().take(MAX_PDF_PAGES).enumerate() {
        let page = rasterize_pdf_page(&spec, idx + 1, &mut fonts)?;
        pages.push(page);
    }
    if pages.is_empty() {
        let fallback = PdfPageSpec {
            width_pt: 595.0,
            height_pt: 842.0,
            streams: Vec::new(),
        };
        pages.push(rasterize_pdf_page(&fallback, 1, &mut fonts)?);
    }
    Ok(pages)
}

fn parse_pdf_pages(bytes: &[u8]) -> Vec<PdfPageSpec> {
    use std::collections::HashMap;
    // Collect indirect objects `N G obj ... endobj`
    let mut objects: HashMap<u32, &[u8]> = HashMap::new();
    let mut i = 0;
    while i + 6 < bytes.len() {
        // Parse `<num> <gen> obj`
        if bytes[i].is_ascii_digit() && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            let start_num = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let Ok(obj_num) = std::str::from_utf8(&bytes[start_num..i])
                .unwrap_or("")
                .parse::<u32>()
            else {
                continue;
            };
            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                i += 1;
            }
            let start_gen = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == start_gen {
                continue;
            }
            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                i += 1;
            }
            if i + 3 <= bytes.len()
                && &bytes[i..i + 3] == b"obj"
                && (i + 3 == bytes.len() || !bytes[i + 3].is_ascii_alphanumeric())
            {
                i += 3;
                let body_start = i;
                if let Some(rel_end) = find_subslice(&bytes[body_start..], b"endobj") {
                    objects.insert(obj_num, &bytes[body_start..body_start + rel_end]);
                    i = body_start + rel_end + 6;
                    continue;
                }
            }
        }
        i += 1;
    }

    let mut default_media_box = (595.0f32, 842.0f32);
    for body in objects.values() {
        if let Some(mb) = parse_media_box(body) {
            default_media_box = mb;
            break;
        }
    }

    let mut page_nums: Vec<u32> = objects
        .iter()
        .filter_map(|(&num, &body)| is_page_dict(body).then_some(num))
        .collect();
    page_nums.sort_unstable();

    let mut pages = Vec::new();
    for num in page_nums {
        let Some(&body) = objects.get(&num) else {
            continue;
        };
        let (width_pt, height_pt) = parse_media_box(body).unwrap_or(default_media_box);
        let mut streams = Vec::new();
        if let Some(s) = extract_object_stream(body) {
            streams.push(s);
        }
        for ref_num in parse_contents_refs(body) {
            if let Some(&ref_body) = objects.get(&ref_num) {
                if let Some(s) = extract_object_stream(ref_body) {
                    streams.push(s);
                }
            }
        }
        pages.push(PdfPageSpec {
            width_pt,
            height_pt,
            streams,
        });
    }

    if pages.is_empty() {
        // Fallback: collect all streams in the file into a single page
        let mut streams = Vec::new();
        let mut nums: Vec<u32> = objects.keys().copied().collect();
        nums.sort_unstable();
        for num in nums {
            if let Some(s) = extract_object_stream(objects[&num]) {
                streams.push(s);
            }
        }
        pages.push(PdfPageSpec {
            width_pt: default_media_box.0,
            height_pt: default_media_box.1,
            streams,
        });
    }

    pages
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn is_page_dict(body: &[u8]) -> bool {
    let header_end = find_subslice(body, b"stream").unwrap_or(body.len());
    let dict = String::from_utf8_lossy(&body[..header_end]);
    let compact: String = dict.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    compact.contains("/Type/Page") && !compact.contains("/Type/Pages")
}

fn parse_media_box(body: &[u8]) -> Option<(f32, f32)> {
    let header_end = find_subslice(body, b"stream").unwrap_or(body.len());
    let text = String::from_utf8_lossy(&body[..header_end]);
    let idx = text.find("/MediaBox")?;
    let rest = &text[idx + 9..];
    let open = rest.find('[')?;
    let close = rest[open + 1..].find(']')?;
    let nums: Vec<f32> = rest[open + 1..open + 1 + close]
        .split_whitespace()
        .filter_map(|t| t.parse::<f32>().ok())
        .collect();
    if nums.len() == 4 {
        let w = (nums[2] - nums[0]).abs().clamp(72.0, 2400.0);
        let h = (nums[3] - nums[1]).abs().clamp(72.0, 2400.0);
        Some((w, h))
    } else {
        None
    }
}

fn parse_contents_refs(body: &[u8]) -> Vec<u32> {
    let header_end = find_subslice(body, b"stream").unwrap_or(body.len());
    let text = String::from_utf8_lossy(&body[..header_end]);
    let Some(idx) = text.find("/Contents") else {
        return Vec::new();
    };
    let rest = text[idx + 9..].trim_start();
    let slice = if let Some(after_open) = rest.strip_prefix('[') {
        let end = after_open.find(']').unwrap_or(after_open.len());
        &after_open[..end]
    } else {
        let end = rest
            .find('/')
            .or_else(|| rest.find(">>"))
            .unwrap_or(rest.len());
        &rest[..end]
    };
    let tokens: Vec<&str> = slice.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < tokens.len() {
        if tokens[i + 2] == "R" {
            if let Ok(obj_id) = tokens[i].parse::<u32>() {
                out.push(obj_id);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

fn extract_object_stream(body: &[u8]) -> Option<Vec<u8>> {
    let stream_pos = find_subslice(body, b"stream")?;
    let header = String::from_utf8_lossy(&body[..stream_pos]);
    let mut data_start = stream_pos + 6;
    if data_start < body.len() && body[data_start] == b'\r' {
        data_start += 1;
    }
    if data_start < body.len() && body[data_start] == b'\n' {
        data_start += 1;
    }
    let end_rel = find_subslice(&body[data_start..], b"endstream")?;
    let mut raw = &body[data_start..data_start + end_rel];
    if raw.ends_with(b"\r\n") {
        raw = &raw[..raw.len() - 2];
    } else if raw.ends_with(b"\n") || raw.ends_with(b"\r") {
        raw = &raw[..raw.len() - 1];
    }
    if header.contains("/FlateDecode") {
        let mut decoder = flate2::read::ZlibDecoder::new(raw);
        let mut out = Vec::new();
        if decoder.read_to_end(&mut out).is_ok() && !out.is_empty() {
            return Some(out);
        }
        let mut raw_deflate = flate2::read::DeflateDecoder::new(raw);
        out.clear();
        if raw_deflate.read_to_end(&mut out).is_ok() && !out.is_empty() {
            return Some(out);
        }
        None
    } else {
        Some(raw.to_vec())
    }
}

fn rasterize_pdf_page(
    spec: &PdfPageSpec,
    page_num: usize,
    fonts: &mut FontSystem,
) -> Result<RenderedPage, Box<dyn std::error::Error>> {
    let scale = 1.5f32;
    let pw = (spec.width_pt * scale).round().clamp(120.0, 2048.0) as u32;
    let ph = (spec.height_pt * scale).round().clamp(120.0, 2048.0) as u32;
    let mut pixmap = Pixmap::new(pw, ph).ok_or("Не удалось создать буфер страницы PDF")?;
    pixmap.fill(Color::from_rgba8(255, 255, 255, 255));

    let prev_scale = feather_ui::scale::ui_scale();
    feather_ui::scale::set_ui_scale(1.0);

    let mut drew_anything = false;
    for stream in &spec.streams {
        if execute_pdf_content_stream(&mut pixmap, fonts, stream, spec.height_pt, scale) {
            drew_anything = true;
        }
    }

    if !drew_anything {
        let mut pm = pixmap.as_mut();
        fonts.draw_raw(
            &mut pm,
            &format!("PDF · Страница {page_num}"),
            36.0 * scale,
            48.0 * scale,
            16.0 * scale,
            Color::from_rgba8(90, 96, 110, 255),
            true,
        );
    }

    feather_ui::scale::set_ui_scale(prev_scale);
    let thumb = make_thumbnail(&pixmap, THUMB_MAX_W, THUMB_MAX_H);
    Ok(RenderedPage {
        pixmap,
        thumb,
        orig_w: spec.width_pt.round() as u32,
        orig_h: spec.height_pt.round() as u32,
        has_alpha: false,
    })
}

#[derive(Clone, Debug)]
enum PdfToken {
    Number(f32),
    String(String),
    Name(String),
    Array(Vec<PdfToken>),
    Op(String),
}

fn tokenize_pdf_stream(input: &[u8]) -> Vec<PdfToken> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'%' {
            while i < input.len() && !matches!(input[i], b'\n' | b'\r') {
                i += 1;
            }
            continue;
        }
        if b == b'(' {
            let (s, next) = parse_pdf_literal_string(input, i + 1);
            out.push(PdfToken::String(s));
            i = next;
            continue;
        }
        if b == b'<' && i + 1 < input.len() && input[i + 1] != b'<' {
            let (s, next) = parse_pdf_hex_string(input, i + 1);
            out.push(PdfToken::String(s));
            i = next;
            continue;
        }
        if b == b'<' && i + 1 < input.len() && input[i + 1] == b'<' {
            i += 2;
            continue;
        }
        if b == b'>' && i + 1 < input.len() && input[i + 1] == b'>' {
            i += 2;
            continue;
        }
        if b == b'[' {
            let mut arr = Vec::new();
            i += 1;
            let start = i;
            let mut depth = 1usize;
            while i < input.len() && depth > 0 {
                if input[i] == b'(' {
                    let (_, next) = parse_pdf_literal_string(input, i + 1);
                    i = next;
                    continue;
                }
                if input[i] == b'[' {
                    depth += 1;
                } else if input[i] == b']' {
                    depth -= 1;
                    if depth == 0 {
                        arr = tokenize_pdf_stream(&input[start..i]);
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            out.push(PdfToken::Array(arr));
            continue;
        }
        if b == b'/' {
            i += 1;
            let start = i;
            while i < input.len()
                && !input[i].is_ascii_whitespace()
                && !matches!(input[i], b'/' | b'(' | b')' | b'<' | b'>' | b'[' | b']')
            {
                i += 1;
            }
            out.push(PdfToken::Name(
                String::from_utf8_lossy(&input[start..i]).into_owned(),
            ));
            continue;
        }
        let start = i;
        while i < input.len()
            && !input[i].is_ascii_whitespace()
            && !matches!(input[i], b'/' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'%')
        {
            i += 1;
        }
        let word = String::from_utf8_lossy(&input[start..i]).into_owned();
        if let Ok(n) = word.parse::<f32>() {
            out.push(PdfToken::Number(n));
        } else if !word.is_empty() {
            out.push(PdfToken::Op(word));
        }
    }
    out
}

fn parse_pdf_literal_string(input: &[u8], mut i: usize) -> (String, usize) {
    let mut bytes = Vec::new();
    let mut depth = 1usize;
    while i < input.len() && depth > 0 {
        match input[i] {
            b'\\' => {
                i += 1;
                if i >= input.len() {
                    break;
                }
                match input[i] {
                    b'n' => bytes.push(b'\n'),
                    b'r' => bytes.push(b'\r'),
                    b't' => bytes.push(b'\t'),
                    b'(' => bytes.push(b'('),
                    b')' => bytes.push(b')'),
                    b'\\' => bytes.push(b'\\'),
                    c if (b'0'..=b'7').contains(&c) => {
                        let mut oct = (c - b'0') as u8;
                        for _ in 0..2 {
                            if i + 1 < input.len() && (b'0'..=b'7').contains(&input[i + 1]) {
                                i += 1;
                                oct = oct.wrapping_mul(8).wrapping_add(input[i] - b'0');
                            } else {
                                break;
                            }
                        }
                        bytes.push(oct);
                    }
                    other => bytes.push(other),
                }
                i += 1;
            }
            b'(' => {
                depth += 1;
                bytes.push(b'(');
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth > 0 {
                    bytes.push(b')');
                }
                i += 1;
            }
            c => {
                bytes.push(c);
                i += 1;
            }
        }
    }
    (decode_pdf_bytes(&bytes), i)
}

fn parse_pdf_hex_string(input: &[u8], mut i: usize) -> (String, usize) {
    let mut hex = Vec::new();
    while i < input.len() && input[i] != b'>' {
        if input[i].is_ascii_hexdigit() {
            hex.push(input[i]);
        }
        i += 1;
    }
    if i < input.len() && input[i] == b'>' {
        i += 1;
    }
    if hex.len() % 2 == 1 {
        hex.push(b'0');
    }
    let bytes: Vec<u8> = hex
        .chunks_exact(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    (decode_pdf_bytes(&bytes), i)
}

fn decode_pdf_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) && bytes.len() >= 4 {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        if let Ok(s) = String::from_utf16(&u16s) {
            return s;
        }
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    bytes.iter().map(|&b| b as char).collect()
}

fn execute_pdf_content_stream(
    pixmap: &mut Pixmap,
    fonts: &mut FontSystem,
    stream: &[u8],
    page_h: f32,
    scale: f32,
) -> bool {
    let tokens = tokenize_pdf_stream(stream);
    let mut stack: Vec<PdfToken> = Vec::new();
    let mut fill_color = Color::from_rgba8(24, 26, 30, 255);
    let mut stroke_color = Color::from_rgba8(24, 26, 30, 255);
    let mut line_width = 1.0f32;
    let mut font_size = 12.0f32;
    let mut font_bold = false;
    let mut text_x = 36.0f32;
    let mut text_y = page_h - 48.0;
    let mut line_x = 36.0f32;
    let mut line_y = page_h - 48.0;
    let mut leading = 14.0f32;
    let mut path_pb = PathBuilder::new();
    let mut has_path = false;
    let mut drew = false;

    let pop_num = |st: &mut Vec<PdfToken>| -> Option<f32> {
        match st.pop()? {
            PdfToken::Number(n) => Some(n),
            _ => None,
        }
    };

    for tok in tokens {
        let PdfToken::Op(op) = tok else {
            stack.push(tok);
            continue;
        };
        match op.as_str() {
            "BT" => {
                text_x = 0.0;
                text_y = 0.0;
                line_x = 0.0;
                line_y = 0.0;
                stack.clear();
            }
            "ET" => stack.clear(),
            "Tf" => {
                if let Some(sz) = pop_num(&mut stack) {
                    font_size = sz.abs().clamp(6.0, 96.0);
                }
                if let Some(PdfToken::Name(name)) = stack.pop() {
                    let lower = name.to_ascii_lowercase();
                    font_bold = lower.contains("bold") || lower.contains("semibold");
                }
                stack.clear();
            }
            "TL" => {
                if let Some(tl) = pop_num(&mut stack) {
                    leading = tl.abs().max(8.0);
                }
                stack.clear();
            }
            "Td" | "TD" => {
                if let (Some(ty), Some(tx)) = (pop_num(&mut stack), pop_num(&mut stack)) {
                    if op == "TD" {
                        leading = (-ty).max(8.0);
                    }
                    line_x += tx;
                    line_y += ty;
                    text_x = line_x;
                    text_y = line_y;
                }
                stack.clear();
            }
            "Tm" => {
                let mut nums = [0.0f32; 6];
                let mut ok = true;
                for k in (0..6).rev() {
                    if let Some(n) = pop_num(&mut stack) {
                        nums[k] = n;
                    } else {
                        ok = false;
                    }
                }
                if ok {
                    let scale_m = nums[0].hypot(nums[1]).max(nums[3].abs());
                    if scale_m > 1.5 && font_size <= 2.0 {
                        font_size = scale_m.clamp(6.0, 96.0);
                    }
                    line_x = nums[4];
                    line_y = nums[5];
                    text_x = line_x;
                    text_y = line_y;
                }
                stack.clear();
            }
            "T*" => {
                line_y -= leading;
                text_x = line_x;
                text_y = line_y;
                stack.clear();
            }
            "Tj" | "'" => {
                if op == "'" {
                    line_y -= leading;
                    text_x = line_x;
                    text_y = line_y;
                }
                if let Some(PdfToken::String(s)) = stack.pop() {
                    if !s.is_empty() {
                        let px = (text_x * scale).max(4.0);
                        let py = ((page_h - text_y - font_size * 0.85) * scale).max(4.0);
                        let sz = (font_size * scale).clamp(8.0, 120.0);
                        fonts.draw_raw(&mut pixmap.as_mut(), &s, px, py, sz, fill_color, font_bold);
                        text_x += fonts.measure_raw(&s, font_size, font_bold);
                        drew = true;
                    }
                }
                stack.clear();
            }
            "TJ" => {
                if let Some(PdfToken::Array(items)) = stack.pop() {
                    let sz = (font_size * scale).clamp(8.0, 120.0);
                    let py = ((page_h - text_y - font_size * 0.85) * scale).max(4.0);
                    for item in items {
                        match item {
                            PdfToken::String(s) if !s.is_empty() => {
                                let px = (text_x * scale).max(4.0);
                                fonts.draw_raw(
                                    &mut pixmap.as_mut(),
                                    &s,
                                    px,
                                    py,
                                    sz,
                                    fill_color,
                                    font_bold,
                                );
                                text_x += fonts.measure_raw(&s, font_size, font_bold);
                                drew = true;
                            }
                            PdfToken::Number(kern) => {
                                text_x -= (kern / 1000.0) * font_size;
                            }
                            _ => {}
                        }
                    }
                }
                stack.clear();
            }
            "rg" => {
                if let (Some(b), Some(g), Some(r)) = (
                    pop_num(&mut stack),
                    pop_num(&mut stack),
                    pop_num(&mut stack),
                ) {
                    fill_color = Color::from_rgba8(
                        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
                        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                        (b.clamp(0.0, 1.0) * 255.0).round() as u8,
                        255,
                    );
                }
                stack.clear();
            }
            "RG" => {
                if let (Some(b), Some(g), Some(r)) = (
                    pop_num(&mut stack),
                    pop_num(&mut stack),
                    pop_num(&mut stack),
                ) {
                    stroke_color = Color::from_rgba8(
                        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
                        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                        (b.clamp(0.0, 1.0) * 255.0).round() as u8,
                        255,
                    );
                }
                stack.clear();
            }
            "g" => {
                if let Some(gray) = pop_num(&mut stack) {
                    let v = (gray.clamp(0.0, 1.0) * 255.0).round() as u8;
                    fill_color = Color::from_rgba8(v, v, v, 255);
                }
                stack.clear();
            }
            "G" => {
                if let Some(gray) = pop_num(&mut stack) {
                    let v = (gray.clamp(0.0, 1.0) * 255.0).round() as u8;
                    stroke_color = Color::from_rgba8(v, v, v, 255);
                }
                stack.clear();
            }
            "w" => {
                if let Some(w) = pop_num(&mut stack) {
                    line_width = (w * scale).clamp(0.75, 24.0);
                }
                stack.clear();
            }
            "m" => {
                if let (Some(y), Some(x)) = (pop_num(&mut stack), pop_num(&mut stack)) {
                    path_pb.move_to(x * scale, (page_h - y) * scale);
                    has_path = true;
                }
                stack.clear();
            }
            "l" => {
                if let (Some(y), Some(x)) = (pop_num(&mut stack), pop_num(&mut stack)) {
                    path_pb.line_to(x * scale, (page_h - y) * scale);
                    has_path = true;
                }
                stack.clear();
            }
            "re" => {
                if let (Some(h), Some(w), Some(y), Some(x)) = (
                    pop_num(&mut stack),
                    pop_num(&mut stack),
                    pop_num(&mut stack),
                    pop_num(&mut stack),
                ) {
                    let rx = x * scale;
                    let ry = (page_h - (y + h)) * scale;
                    let rw = w * scale;
                    let rh = h * scale;
                    if let Some(rect) = tiny_skia::Rect::from_xywh(rx, ry, rw, rh) {
                        path_pb.push_rect(rect);
                        has_path = true;
                    }
                }
                stack.clear();
            }
            "h" => {
                if has_path {
                    path_pb.close();
                }
                stack.clear();
            }
            "f" | "F" | "f*" | "S" | "s" | "B" | "B*" => {
                if has_path {
                    let pb = std::mem::replace(&mut path_pb, PathBuilder::new());
                    has_path = false;
                    if op == "s" {
                        // closed stroke
                    }
                    if let Some(path) = pb.finish() {
                        if matches!(op.as_str(), "f" | "F" | "f*" | "B" | "B*") {
                            let mut paint = Paint::default();
                            paint.set_color(fill_color);
                            paint.anti_alias = true;
                            pixmap.fill_path(
                                &path,
                                &paint,
                                FillRule::Winding,
                                Transform::identity(),
                                None,
                            );
                            drew = true;
                        }
                        if matches!(op.as_str(), "S" | "s" | "B" | "B*") {
                            let mut paint = Paint::default();
                            paint.set_color(stroke_color);
                            paint.anti_alias = true;
                            let stroke = Stroke {
                                width: line_width,
                                ..Default::default()
                            };
                            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                            drew = true;
                        }
                    }
                }
                stack.clear();
            }
            "n" => {
                path_pb = PathBuilder::new();
                has_path = false;
                stack.clear();
            }
            _ => {
                stack.clear();
            }
        }
    }
    drew
}

/// Helper used by tests and E2E checks to build a valid multi-page PDF document with text and shapes.
pub fn sample_pdf_bytes(pages: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::from(&b"%PDF-1.4\n"[..]);
    let mut offsets = Vec::new();
    // 1: Catalog, 2: Pages
    offsets.push(out.len());
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let page_count = pages.len().max(1);
    let mut kids = String::new();
    for idx in 0..page_count {
        let page_obj = 3 + idx * 2;
        if !kids.is_empty() {
            kids.push(' ');
        }
        kids.push_str(&format!("{page_obj} 0 R"));
    }
    offsets.push(out.len());
    out.extend_from_slice(
        format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {page_count} >>\nendobj\n")
            .as_bytes(),
    );

    for (idx, (heading, body)) in pages.iter().enumerate() {
        let page_obj = 3 + idx * 2;
        let stream_obj = page_obj + 1;
        let page_num = idx + 1;
        let content = format!(
            "0.08 0.44 0.94 rg\n48 726 499 58 re f\n\
             1 1 1 rg\nBT /F1 20 Tf 66 756 Td ({heading}) Tj ET\n\
             0.85 0.92 1.0 rg\nBT /F1 11 Tf 66 738 Td (Feather DE Native Document Viewer  |  Page {page_num} of {page_count}) Tj ET\n\
             0.14 0.16 0.20 rg\nBT /F1 14 Tf 48 684 Td ({body}) Tj ET\n\
             0.94 0.95 0.97 rg\n48 520 499 136 re f\n\
             0.82 0.84 0.88 RG 1 w 48 520 499 136 re S\n\
             0.08 0.44 0.94 rg\n68 618 140 16 re f\n\
             0.22 0.25 0.30 rg\nBT /F1 12 Tf 68 590 Td (1. Zero idle CPU wakeups (event-driven Wayland SHM client)) Tj ET\n\
             BT /F1 12 Tf 68 566 Td (2. Sub-millisecond compositor frame budget with GPU transforms) Tj ET\n\
             BT /F1 12 Tf 68 542 Td (3. Supports PNG, JPEG, WebP, SVG and multi-page PDF documents) Tj ET\n\
             0.20 0.72 0.46 rg\n48 360 154 128 re f\n\
             0.98 0.62 0.16 rg\n220 360 154 128 re f\n\
             0.38 0.36 0.90 rg\n393 360 154 128 re f\n\
             1 1 1 rg\nBT /F1 14 Tf 66 426 Td (0% Idle CPU) Tj ET\n\
             BT /F1 14 Tf 238 426 Td (<= 1 ms Frame) Tj ET\n\
             BT /F1 14 Tf 411 426 Td (<= 60 MB RAM) Tj ET\n"
        );
        offsets.push(out.len());
        out.extend_from_slice(
            format!(
                "{page_obj} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> /Contents {stream_obj} 0 R >>\nendobj\n"
            )
            .as_bytes(),
        );
        offsets.push(out.len());
        out.extend_from_slice(
            format!(
                "{stream_obj} 0 obj\n<< /Length {} >>\nstream\n{}endstream\nendobj\n",
                content.len(),
                content
            )
            .as_bytes(),
        );
    }

    let xref_pos = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_png_jpeg_webp_svg_and_multipage_pdf_and_rotates() {
        let dir = std::env::temp_dir().join(format!("feather-preview-doc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let png_path = dir.join("sample.png");
        let img = image::RgbaImage::from_fn(40, 24, |x, y| {
            image::Rgba([(x * 5) as u8, (y * 8) as u8, 180, if x < 10 { 128 } else { 255 }])
        });
        img.save(&png_path).unwrap();
        let mut doc = load(&png_path).unwrap();
        assert_eq!(doc.format, DocFormat::Png);
        assert_eq!(doc.pages.len(), 1);
        assert_eq!((doc.pages[0].orig_w, doc.pages[0].orig_h), (40, 24));
        assert!(doc.pages[0].has_alpha);
        doc.rotate_page(0, true);
        assert_eq!((doc.pages[0].orig_w, doc.pages[0].orig_h), (24, 40));
        assert_eq!((doc.pages[0].pixmap.width(), doc.pages[0].pixmap.height()), (24, 40));

        let svg_path = dir.join("vector.svg");
        std::fs::write(
            &svg_path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80"><rect width="120" height="80" rx="12" fill="#0a84ff"/></svg>"##,
        )
        .unwrap();
        let svg_doc = load(&svg_path).unwrap();
        assert_eq!(svg_doc.format, DocFormat::Svg);
        assert_eq!((svg_doc.pages[0].orig_w, svg_doc.pages[0].orig_h), (120, 80));

        let pdf_path = dir.join("guide.pdf");
        let pdf_bytes = sample_pdf_bytes(&[
            ("Feather DE Preview", "Page 1: Fast native Wayland PDF viewer"),
            ("Architecture & Budgets", "Page 2: Zero idle CPU and <= 60 MB RAM"),
        ]);
        std::fs::write(&pdf_path, &pdf_bytes).unwrap();
        let native_pages = render_pdf_native(&pdf_bytes).unwrap();
        assert_eq!(native_pages.len(), 2);
        assert_eq!((native_pages[0].orig_w, native_pages[0].orig_h), (595, 842));
        // Banner rectangle in the top region is non-white
        assert!(native_pages[0]
            .pixmap
            .data()
            .chunks_exact(4)
            .any(|p| p[0] < 200 && p[2] > 180));

        let pdf_doc = load(&pdf_path).unwrap();
        assert_eq!(pdf_doc.format, DocFormat::Pdf);
        assert_eq!(pdf_doc.pages.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
