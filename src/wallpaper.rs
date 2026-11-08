//! Static photo backgrounds. Decode off the compositor thread; retain only GPU pixels.
use smithay::reexports::calloop::{channel, LoopHandle};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSource {
    Bundled(usize),
    File(PathBuf),
}
/// Built-in pictures, in gallery order. The first three are generated paintings, the rest
/// are procedural (tools/wallpaper-gen); all are original artwork of the project.
pub const COUNT: usize = 8;
pub const NAMES: [&str; COUNT] = ["Рассвет", "Сияние", "Горное озеро", "Волны", "Дюны", "Хребты", "Изумруд", "Графит"];
const IDS: [&str; COUNT] = [
    "feather-dawn",
    "feather-aurora",
    "feather-lake",
    "feather-waves",
    "feather-dunes",
    "feather-ridges",
    "feather-emerald",
    "feather-graphite",
];
const IMAGES: [&[u8]; COUNT] = [
    include_bytes!("../assets/wallpapers/feather-dawn.png"),
    include_bytes!("../assets/wallpapers/feather-aurora.png"),
    include_bytes!("../assets/wallpapers/feather-lake.png"),
    include_bytes!("../assets/wallpapers/feather-waves.jpg"),
    include_bytes!("../assets/wallpapers/feather-dunes.jpg"),
    include_bytes!("../assets/wallpapers/feather-ridges.jpg"),
    include_bytes!("../assets/wallpapers/feather-emerald.jpg"),
    include_bytes!("../assets/wallpapers/feather-graphite.jpg"),
];
/// Encoded bundled wallpaper (visual checks render the menu bar over each)
#[cfg(test)]
pub fn bundled_bytes(index: usize) -> &'static [u8] {
    IMAGES[index]
}
const THUMBS: [&[u8]; COUNT] = [
    include_bytes!("../assets/wallpapers/feather-dawn-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-aurora-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-lake-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-waves-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-dunes-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-ridges-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-emerald-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-graphite-thumb.png"),
];

/// How a picture whose proportions differ from the screen's is laid out
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Fit {
    /// Covers the screen, the overflow is cropped around the centre
    #[default]
    Fill,
    /// Whole picture visible, the bars are filled with its own darkened average colour
    Fit,
    /// Squeezed to the screen's proportions
    Stretch,
}
impl Fit {
    pub const ALL: [Fit; 3] = [Fit::Fill, Fit::Fit, Fit::Stretch];
    pub fn id(self) -> &'static str {
        match self {
            Fit::Fill => "fill",
            Fit::Fit => "fit",
            Fit::Stretch => "stretch",
        }
    }
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == id)
    }
    pub fn label(self) -> &'static str {
        match self {
            Fit::Fill => "Заполнить",
            Fit::Fit => "Вписать",
            Fit::Stretch => "Растянуть",
        }
    }
}

/// Automatic change through the built-in collection
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Cycle {
    #[default]
    Off,
    Login,
    HalfHour,
    Hour,
    Day,
}
impl Cycle {
    pub const ALL: [Cycle; 5] = [Cycle::Off, Cycle::Login, Cycle::HalfHour, Cycle::Hour, Cycle::Day];
    pub fn id(self) -> &'static str {
        match self {
            Cycle::Off => "off",
            Cycle::Login => "login",
            Cycle::HalfHour => "30m",
            Cycle::Hour => "1h",
            Cycle::Day => "1d",
        }
    }
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }
    pub fn label(self) -> &'static str {
        match self {
            Cycle::Off => "Нет",
            Cycle::Login => "При входе",
            Cycle::HalfHour => "30 мин",
            Cycle::Hour => "Час",
            Cycle::Day => "День",
        }
    }
    pub fn period(self) -> Option<std::time::Duration> {
        match self {
            Cycle::HalfHour => Some(std::time::Duration::from_secs(30 * 60)),
            Cycle::Hour => Some(std::time::Duration::from_secs(60 * 60)),
            Cycle::Day => Some(std::time::Duration::from_secs(24 * 60 * 60)),
            Cycle::Off | Cycle::Login => None,
        }
    }
}

/// Saved wallpaper options besides the picture itself
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Prefs {
    pub fit: Fit,
    pub cycle: Cycle,
    pub shuffle: bool,
    /// Unix seconds of the last change: the next automatic one counts from here,
    /// also across restarts
    pub changed: u64,
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What the decoder learnt about the source picture
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub size: (u32, u32),
    pub format: &'static str,
    pub bytes: Option<u64>,
}

/// The applied wallpaper as the settings show it: a small copy of exactly what is on screen
/// (crop/bars included), made on the raster worker together with the texture
#[derive(Clone, Debug, Default)]
pub struct Preview {
    pub thumb: Option<std::sync::Arc<tiny_skia::Pixmap>>,
    pub info: Option<Info>,
    /// Average colour, sRGB
    pub average: [u8; 3],
    /// The menu bar draws light text over this wallpaper
    pub bar_dark: bool,
}
/// Width of `Preview::thumb` in pixels (≈330 КБ at 16:9)
pub const PREVIEW_W: u32 = 384;

/// Downscaled copy of the final RGBA wallpaper and its average colour
pub fn preview(pixels: &[u8], size: (i32, i32), info: Option<Info>) -> Preview {
    let (w, h) = (size.0.max(1) as usize, size.1.max(1) as usize);
    if pixels.len() < w * h * 4 {
        return Preview::default();
    }
    let tw = (PREVIEW_W as usize).min(w);
    let th = (tw * h / w).max(1);
    // Box filter straight into the output: no intermediate buffers on the raster worker
    // (the memory budget of a wallpaper change has little headroom)
    let mut out = vec![0u8; tw * th * 4];
    let mut sum = [0u64; 3];
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let mut acc = [0u32; 3];
            for y in y0..y1 {
                let row = &pixels[(y * w + x0) * 4..(y * w + x1) * 4];
                for p in row.chunks_exact(4) {
                    acc[0] += p[0] as u32;
                    acc[1] += p[1] as u32;
                    acc[2] += p[2] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (ty * tw + tx) * 4;
            for c in 0..3 {
                out[o + c] = ((acc[c] + n / 2) / n) as u8;
                sum[c] += out[o + c] as u64;
            }
            out[o + 3] = 255;
        }
    }
    let n = (tw * th) as u64;
    let average = [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8];
    let thumb = tiny_skia::IntSize::from_wh(tw as u32, th as u32)
        .and_then(|size| tiny_skia::Pixmap::from_vec(out, size))
        .map(std::sync::Arc::new);
    Preview { thumb, info, average, bar_dark: false }
}

/// The next picture of the collection after `current` (any other one when shuffling)
pub fn next_bundled(current: Option<&ImageSource>, shuffle: bool, seed: u64) -> usize {
    let at = match current {
        Some(ImageSource::Bundled(i)) if *i < COUNT => Some(*i),
        _ => None,
    };
    if shuffle {
        // Never the same picture twice in a row
        let skip = (seed % (COUNT as u64 - 1)) as usize + 1;
        at.map_or(seed as usize % COUNT, |i| (i + skip) % COUNT)
    } else {
        at.map_or(0, |i| (i + 1) % COUNT)
    }
}

impl ImageSource {
    pub fn name(&self) -> String {
        match self {
            Self::Bundled(i) => NAMES.get(*i).unwrap_or(&"Обои").to_string(),
            Self::File(p) => p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        }
    }
    pub fn id(&self) -> String {
        use std::os::unix::ffi::OsStrExt;
        match self {
            Self::Bundled(i) => format!("builtin:{}", IDS.get(*i).unwrap_or(&IDS[0])),
            Self::File(p) => format!(
                "file:{}",
                p.as_os_str()
                    .as_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
        }
    }
    pub fn parse(id: &str) -> Option<Self> {
        use std::os::unix::ffi::OsStringExt;
        if let Some(id) = id.strip_prefix("builtin:") {
            return IDS.iter().position(|i| *i == id).map(Self::Bundled);
        }
        let text = id.strip_prefix("file:")?;
        if text.is_empty() || text.len() > 32768 || text.len() % 2 != 0 || !text.is_ascii() {
            return None;
        }
        let bytes = (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
            .collect::<Option<Vec<_>>>()?;
        if bytes.contains(&0) {
            return None;
        }
        let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        path.is_absolute().then_some(Self::File(path))
    }
}

pub fn image_size(w: i32, h: i32) -> (i32, i32) {
    let factor = (1920.0 / w.max(1) as f64)
        .min(1080.0 / h.max(1) as f64)
        .min(1.0);
    (
        ((w as f64 * factor).round() as i32).max(1),
        ((h as f64 * factor).round() as i32).max(1),
    )
}

/// A short-lived helper keeps arbitrary JPEG/PNG/WebP decoder allocations out of
/// the compositor and its raster queue. No GUI/Wayland/GL is initialized there.
/// The helper answers with a 12-byte header (source width, height, format) and the pixels.
pub fn decode(source: &ImageSource, w: i32, h: i32, fit: Fit) -> Result<(Vec<u8>, Info), String> {
    let length = w as usize * h as usize * 4;
    // Run this exact executable even after an update replaces/unlinks its pathname.
    // current_exe() then contains " (deleted)" on Linux and cannot be spawned.
    let mut child = Command::new("/proc/self/exe")
        .args([
            "--decode-wallpaper",
            &source.id(),
            &w.to_string(),
            &h.to_string(),
            fit.id(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().unwrap();
    let mut header = [0u8; HEADER];
    let header_read = stdout.read_exact(&mut header);
    let mut pixels = Vec::with_capacity(length);
    let read = stdout.take(length as u64 + 1).read_to_end(&mut pixels);
    if pixels.len() > length {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if header_read.is_err() || read.is_err() || !status.success() || pixels.len() != length {
        return Err(
            "Не удалось загрузить изображение. Выберите PNG, JPEG или WebP до 32 МБ.".into(),
        );
    }
    let word = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
    let info = Info {
        size: (word(0), word(1)),
        format: FORMATS.get(word(2) as usize).copied().unwrap_or("Изображение"),
        bytes: match source {
            ImageSource::Bundled(i) => Some(IMAGES[*i].len() as u64),
            ImageSource::File(p) => std::fs::metadata(p).ok().map(|m| m.len()),
        },
    };
    Ok((pixels, info))
}
const HEADER: usize = 12;
const FORMATS: [&str; 4] = ["PNG", "JPEG", "WebP", "Изображение"];

pub fn helper(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        libc::alarm(10);
        let limit = libc::rlimit {
            rlim_cur: 768 * 1024 * 1024,
            rlim_max: 768 * 1024 * 1024,
        };
        libc::setrlimit(libc::RLIMIT_AS, &limit);
    }
    if args.len() != 6 {
        return Err("decode arguments".into());
    }
    let source = ImageSource::parse(&args[2]).ok_or("invalid image source")?;
    let w = args[3].parse::<u32>()?;
    let h = args[4].parse::<u32>()?;
    let fit = Fit::from_id(&args[5]).ok_or("invalid fit")?;
    if w == 0 || h == 0 || w > 1920 || h > 1080 {
        return Err("invalid image size".into());
    }
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, stdout.lock());
    stream_pixels(source, w, h, fit, &mut out, true)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn prepare_pixels(
    source: ImageSource,
    w: u32,
    h: u32,
    fit: Fit,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    stream_pixels(source, w, h, fit, &mut out, false)?;
    Ok(out)
}

fn stream_pixels(
    source: ImageSource,
    w: u32,
    h: u32,
    fit: Fit,
    out: &mut impl Write,
    header: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use image::GenericImageView;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(192 * 1024 * 1024);
    let (picture, format) = match source {
        ImageSource::Bundled(i) => {
            let mut r =
                image::ImageReader::new(std::io::Cursor::new(IMAGES[i])).with_guessed_format()?;
            r.limits(limits);
            let format = r.format();
            (r.decode()?, format)
        }
        ImageSource::File(path) => {
            let file = std::fs::File::open(path)?;
            let meta = file.metadata()?;
            if !meta.is_file() || meta.len() > 32 * 1024 * 1024 {
                return Err("invalid file".into());
            }
            let mut r =
                image::ImageReader::new(std::io::BufReader::new(file)).with_guessed_format()?;
            r.limits(limits);
            let format = r.format();
            let mut decoder = r.into_decoder()?;
            let orientation = image::ImageDecoder::orientation(&mut decoder)?;
            let mut p = image::DynamicImage::from_decoder(decoder)?;
            p.apply_orientation(orientation);
            (p, format)
        }
    };
    unsafe {
        libc::malloc_trim(0);
    }
    let (pw, ph) = (picture.width().max(1), picture.height().max(1));
    if header {
        let code: u32 = match format {
            Some(image::ImageFormat::Png) => 0,
            Some(image::ImageFormat::Jpeg) => 1,
            Some(image::ImageFormat::WebP) => 2,
            _ => 3,
        };
        for word in [pw, ph, code] {
            out.write_all(&word.to_le_bytes())?;
        }
    }
    // Source window (sx0, sy0, sw, sh) mapped onto the destination window (dx0, dy0, dw, dh);
    // destination pixels outside it are bars
    let (wf, hf, pwf, phf) = (w as f64, h as f64, pw as f64, ph as f64);
    let ((sx0, sy0, sw, sh), (dx0, dy0, dw, dh)) = match fit {
        Fit::Fill => {
            let src = if (pw as u64) * (h as u64) >= (ph as u64) * (w as u64) {
                let sw = phf * wf / hf;
                ((pwf - sw) * 0.5, 0.0, sw, phf)
            } else {
                let sh = pwf * hf / wf;
                (0.0, (phf - sh) * 0.5, pwf, sh)
            };
            (src, (0.0, 0.0, wf, hf))
        }
        Fit::Stretch => ((0.0, 0.0, pwf, phf), (0.0, 0.0, wf, hf)),
        Fit::Fit => {
            let k = (wf / pwf).min(hf / phf);
            let (dw, dh) = ((pwf * k).round().max(1.0), (phf * k).round().max(1.0));
            ((0.0, 0.0, pwf, phf), (((wf - dw) * 0.5).floor(), ((hf - dh) * 0.5).floor(), dw, dh))
        }
    };
    // Bars take the picture's own average colour, darkened, so it reads as a matte
    let bar = {
        let mut sum = [0u64; 3];
        let mut n = 0u64;
        for gy in 0..16 {
            for gx in 0..16 {
                let p = picture.get_pixel(((gx * 2 + 1) * pw / 32).min(pw - 1), ((gy * 2 + 1) * ph / 32).min(ph - 1)).0;
                let a = p[3] as u64;
                for c in 0..3 {
                    sum[c] += (p[c] as u64 * a + 24 * (255 - a)) / 255;
                }
                n += 1;
            }
        }
        [(sum[0] / n * 55 / 100) as u8, (sum[1] / n * 55 / 100) as u8, (sum[2] / n * 55 / 100) as u8, 255]
    };
    let max_x = (pw - 1) as isize;
    let max_y = (ph - 1) as isize;
    let x_samples: Vec<Option<(u32, u32, u32)>> = (0..w)
        .map(|x| {
            let lx = x as f64 - dx0;
            if lx < 0.0 || lx >= dw {
                return None;
            }
            let sx = sx0 + ((lx + 0.5) * sw / dw) - 0.5;
            let xf = sx.floor();
            let tx = ((sx - xf).clamp(0.0, 1.0) * 256.0).round() as u32;
            let x0 = (xf as isize).clamp(0, max_x) as u32;
            let x1 = (xf as isize + 1).clamp(0, max_x) as u32;
            Some((x0, x1, tx))
        })
        .collect();
    let mut row = vec![0u8; w as usize * 4];
    for y in 0..h {
        let ly = y as f64 - dy0;
        if ly < 0.0 || ly >= dh {
            for dst in row.chunks_exact_mut(4) {
                dst.copy_from_slice(&bar);
            }
            out.write_all(&row)?;
            continue;
        }
        let sy = sy0 + ((ly + 0.5) * sh / dh) - 0.5;
        let yf = sy.floor();
        let ty = ((sy - yf).clamp(0.0, 1.0) * 256.0).round() as u32;
        let y0 = (yf as isize).clamp(0, max_y) as u32;
        let y1 = (yf as isize + 1).clamp(0, max_y) as u32;
        let wy0 = 256 - ty;
        let wy1 = ty;
        match &picture {
            image::DynamicImage::ImageRgb8(rgb) => {
                let raw = rgb.as_raw();
                let stride = pw as usize * 3;
                let r0 = &raw[y0 as usize * stride..];
                let r1 = &raw[y1 as usize * stride..];
                for (dst, sample) in row.chunks_exact_mut(4).zip(x_samples.iter()) {
                    let Some((x0, x1, tx)) = *sample else {
                        dst.copy_from_slice(&bar);
                        continue;
                    };
                    let wx0 = 256 - tx;
                    let wx1 = tx;
                    let i0 = x0 as usize * 3;
                    let i1 = x1 as usize * 3;
                    for c in 0..3 {
                        let top = r0[i0 + c] as u32 * wx0 + r0[i1 + c] as u32 * wx1;
                        let bot = r1[i0 + c] as u32 * wx0 + r1[i1 + c] as u32 * wx1;
                        dst[c] = ((top * wy0 + bot * wy1 + 32768) >> 16) as u8;
                    }
                    dst[3] = 255;
                }
            }
            _ => {
                for (dst, sample) in row.chunks_exact_mut(4).zip(x_samples.iter()) {
                    let Some((x0, x1, tx)) = *sample else {
                        dst.copy_from_slice(&bar);
                        continue;
                    };
                    let wx0 = 256 - tx;
                    let wx1 = tx;
                    let p00 = picture.get_pixel(x0, y0).0;
                    let p10 = picture.get_pixel(x1, y0).0;
                    let p01 = picture.get_pixel(x0, y1).0;
                    let p11 = picture.get_pixel(x1, y1).0;
                    let mut rgba = [0u8; 4];
                    for c in 0..4 {
                        let top = p00[c] as u32 * wx0 + p10[c] as u32 * wx1;
                        let bot = p01[c] as u32 * wx0 + p11[c] as u32 * wx1;
                        rgba[c] = ((top * wy0 + bot * wy1 + 32768) >> 16) as u8;
                    }
                    let a = rgba[3] as u16;
                    for c in 0..3 {
                        dst[c] = ((rgba[c] as u16 * a + 24 * (255 - a) + 127) / 255) as u8;
                    }
                    dst[3] = 255;
                }
            }
        }
        out.write_all(&row)?;
    }
    Ok(())
}

pub fn glass(
    pixels: &[u8],
    size: (i32, i32),
    target: (i32, i32),
    darken: f32,
) -> tiny_skia::Pixmap {
    let source = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
        size.0 as u32,
        size.1 as u32,
        pixels,
    )
    .unwrap();
    let small = image::imageops::resize(
        &source,
        target.0 as u32,
        target.1 as u32,
        image::imageops::FilterType::Triangle,
    );
    let data = crate::theme::frost_pixels(small.into_raw(), target.0, target.1, darken);
    tiny_skia::Pixmap::from_vec(
        data,
        tiny_skia::IntSize::from_wh(target.0 as u32, target.1 as u32).unwrap(),
    )
    .unwrap()
}

type Thumb = std::rc::Rc<tiny_skia::Pixmap>;
thread_local! {
    /// Decoded gallery thumbnails (8 × 240×135, ~1 МБ) while the gallery is visible.
    static THUMB_CACHE: std::cell::RefCell<Vec<Option<Thumb>>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Gradient tiles, drawn by the same code as the full-screen gradient
    static GRADIENT_CACHE: std::cell::RefCell<Vec<(crate::theme::WallpaperTheme, Thumb)>> = const { std::cell::RefCell::new(Vec::new()) };
}
/// Frees the thumbnail caches of this thread once the gallery is no longer painted.
pub fn release_thumbs() {
    THUMB_CACHE.with_borrow_mut(|cache| *cache = Vec::new());
    GRADIENT_CACHE.with_borrow_mut(|cache| *cache = Vec::new());
}
/// Gallery thumbnail clipped to a rounded rectangle.
pub fn paint_thumb_rounded(pm: &mut tiny_skia::PixmapMut, index: usize, rect: crate::shell::R, radius: f32) {
    let pic = THUMB_CACHE.with_borrow_mut(|cache| {
        if cache.is_empty() {
            *cache = THUMBS.iter().map(|d| tiny_skia::Pixmap::decode_png(d).ok().map(Thumb::new)).collect();
        }
        cache.get(index).cloned().flatten()
    });
    if let Some(pic) = pic {
        paint_pixmap_rounded(pm, &pic, rect, radius);
    }
}
/// A gradient wallpaper as a gallery tile (rendered once at 240×135).
pub fn paint_gradient_rounded(pm: &mut tiny_skia::PixmapMut, theme: crate::theme::WallpaperTheme, rect: crate::shell::R, radius: f32) {
    let pic = GRADIENT_CACHE.with_borrow_mut(|cache| {
        if let Some((_, pic)) = cache.iter().find(|(t, _)| *t == theme) {
            return Some(pic.clone());
        }
        let (w, h) = (240, 135);
        let mut buf = vec![0; w as usize * h as usize * 4];
        crate::theme::render_wallpaper(&mut buf, w, h, theme);
        let pic = Thumb::new(tiny_skia::Pixmap::from_vec(buf, tiny_skia::IntSize::from_wh(w as u32, h as u32)?)?);
        cache.push((theme, pic.clone()));
        Some(pic)
    });
    if let Some(pic) = pic {
        paint_pixmap_rounded(pm, &pic, rect, radius);
    }
}
/// Any opaque picture scaled into a rounded rectangle (pattern fill, no full-size mask).
pub fn paint_pixmap_rounded(pm: &mut tiny_skia::PixmapMut, pic: &tiny_skia::Pixmap, rect: crate::shell::R, radius: f32) {
    let Some(path) = crate::theme::rounded_rect_path(rect.x, rect.y, rect.w, rect.h, radius) else {
        return;
    };
    let pattern = tiny_skia::Pattern::new(
        pic.as_ref(),
        tiny_skia::SpreadMode::Pad,
        // Near 1:1 at ×2 (240 px source): nearest is visually identical and cheaper.
        if (rect.w * crate::theme::ui_scale() / pic.width() as f32 - 1.0).abs() < 0.1 {
            tiny_skia::FilterQuality::Nearest
        } else {
            tiny_skia::FilterQuality::Bilinear
        },
        1.0,
        tiny_skia::Transform::from_scale(rect.w / pic.width() as f32, rect.h / pic.height() as f32)
            .post_translate(rect.x, rect.y),
    );
    let paint = tiny_skia::Paint {
        shader: pattern,
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(&path, &paint, tiny_skia::FillRule::Winding, crate::theme::scale_transform(), None);
}

impl crate::FeatherCompositor {
    /// At login: «При входе» changes the picture right away, the timed modes resume their
    /// countdown from the saved time of the last change
    pub fn start_wallpaper_cycle(&mut self) {
        if self.shell.wallpaper_prefs.cycle == Cycle::Login {
            self.next_wallpaper();
        } else {
            self.schedule_wallpaper_cycle();
        }
    }
    /// (Re)arms the one-shot timer of the automatic change; nothing runs while it waits
    pub fn schedule_wallpaper_cycle(&mut self) {
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        if let Some(token) = self.wallpaper_cycle_timer.take() {
            self.loop_handle.remove(token);
        }
        let prefs = self.shell.wallpaper_prefs;
        let Some(period) = prefs.cycle.period() else {
            return;
        };
        let elapsed = std::time::Duration::from_secs(unix_now().saturating_sub(prefs.changed));
        let left = period.saturating_sub(elapsed).max(std::time::Duration::from_secs(2));
        self.wallpaper_cycle_timer = self
            .loop_handle
            .insert_source(Timer::from_duration(left), |_, _, state| {
                state.wallpaper_cycle_timer = None;
                state.next_wallpaper();
                TimeoutAction::Drop
            })
            .ok();
    }
    /// The next picture of the built-in collection (in order or shuffled)
    pub fn next_wallpaper(&mut self) {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64 ^ (d.as_secs() << 17));
        let i = next_bundled(self.shell.wallpaper_image.as_ref(), self.shell.wallpaper_prefs.shuffle, seed);
        self.set_wallpaper_image(Some(ImageSource::Bundled(i)));
    }
}

enum PickRequest {
    Wallpaper,
    ScreenshotDir(Option<PathBuf>),
}

enum PickResult {
    Wallpaper(Result<Option<ImageSource>, String>),
    ScreenshotDir(Result<Option<PathBuf>, String>),
}

pub struct Picker(std::sync::mpsc::SyncSender<PickRequest>);
impl Picker {
    pub fn new(
        handle: &LoopHandle<'static, crate::FeatherCompositor>,
        socket: String,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let (done, channel) = channel::sync_channel(1);
        handle.insert_source(channel, |event, _, state| {
            if let channel::Event::Msg(result) = event {
                match result {
                    PickResult::Wallpaper(res) => {
                        state.system_settings.wallpaper_busy = false;
                        match res {
                            Ok(Some(source)) => state.set_wallpaper_image(Some(source)),
                            Ok(None) => {}
                            Err(e) => state.system_settings.error = Some(e),
                        }
                    }
                    PickResult::ScreenshotDir(res) => {
                        state.system_settings.screenshot_dir_busy = false;
                        match res {
                            Ok(Some(dir)) => {
                                crate::screenshot::set_custom_dir(Some(dir));
                                state.save_settings();
                            }
                            Ok(None) => {}
                            Err(e) => state.system_settings.error = Some(e),
                        }
                    }
                }
                state.mark_settings_dirty();
                state.needs_redraw = true;
            }
        })?;
        std::thread::Builder::new()
            .name("feather-picker".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                use feather_ui::picker::{self, Config, Filter, Mode};
                while let Ok(request) = rx.recv() {
                    let test_start = std::env::var_os("FEATHER_PICKER_TEST_FILE")
                        .map(PathBuf::from)
                        .and_then(|p| p.parent().map(PathBuf::from));
                    let msg = match request {
                        PickRequest::Wallpaper => {
                            let start = test_start
                                .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
                            let config = Config {
                                title: "Выберите обои".into(),
                                accept: "Выбрать обои".into(),
                                start: start.as_deref().map(picker::path_bytes),
                                filters: vec![Filter {
                                    name: "Изображения PNG, JPEG, WebP".into(),
                                    rules: ["image/png", "image/jpeg", "image/webp"]
                                        .into_iter()
                                        .map(|m| (1, m.into()))
                                        .collect(),
                                }],
                                ..Default::default()
                            };
                            let result = picker::run(&config, Some(&socket), |_| {}).and_then(|s| {
                                s.map(|s| s.local_paths().map(|p| ImageSource::File(p[0].clone())))
                                    .transpose()
                            });
                            PickResult::Wallpaper(result)
                        }
                        PickRequest::ScreenshotDir(current) => {
                            let start = test_start
                                .or_else(|| current.filter(|p| p.is_dir()))
                                .or_else(|| crate::screenshot::default_dir().filter(|p| p.is_dir()))
                                .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
                            let config = Config {
                                title: "Папка для снимков экрана".into(),
                                accept: "Выбрать папку".into(),
                                mode: Mode::Directory,
                                start: start.as_deref().map(picker::path_bytes),
                                ..Default::default()
                            };
                            let result = picker::run(&config, Some(&socket), |_| {}).and_then(|s| {
                                s.map(|s| s.local_paths().map(|p| p[0].clone())).transpose()
                            });
                            PickResult::ScreenshotDir(result)
                        }
                    };
                    if done.send(msg).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self(tx))
    }
    pub fn open(&self) -> bool {
        self.0.try_send(PickRequest::Wallpaper).is_ok()
    }
    pub fn open_screenshot_dir(&self, current: Option<PathBuf>) -> bool {
        self.0.try_send(PickRequest::ScreenshotDir(current)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_images_and_local_formats_decode_cover_crop_and_flatten_alpha() {
        for i in 0..COUNT {
            assert_eq!(
                prepare_pixels(ImageSource::Bundled(i), 320, 180, Fit::Fill)
                    .unwrap()
                    .len(),
                320 * 180 * 4
            );
        }
        let path =
            std::env::temp_dir().join(format!("feather-wallpaper-codecs-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        let picture = image::RgbaImage::from_fn(9, 3, |x, _| {
            image::Rgba(match x {
                0..=2 => [255, 0, 0, 255],
                3..=5 => [0, 255, 0, 255],
                _ => [0, 0, 255, 255],
            })
        });
        for ext in ["png", "jpg", "webp"] {
            let file = path.join(format!("мои обои.{ext}"));
            let rgb = image::DynamicImage::ImageRgba8(picture.clone()).to_rgb8();
            rgb.save(&file).unwrap();
            let result = prepare_pixels(ImageSource::File(file), 3, 3, Fit::Fill).unwrap();
            assert_eq!(result.len(), 36);
            assert!(
                result.chunks_exact(4).all(|p| p[1] > 200 && p[3] == 255),
                "{ext}: {result:?}"
            );
        }
        let file = path.join("alpha.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 0]))
            .save(&file)
            .unwrap();
        assert_eq!(
            prepare_pixels(ImageSource::File(file), 1, 1, Fit::Fill).unwrap(),
            [24, 24, 24, 255]
        );
        assert!(prepare_pixels(ImageSource::File(path.join("missing.png")), 3, 3, Fit::Fill).is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn fit_modes_crop_letterbox_and_stretch() {
        let path = std::env::temp_dir().join(format!("feather-wallpaper-fit-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        // Wide 8×2 picture: red left half, blue right half, on a 4×4 screen
        let file = path.join("wide.png");
        image::RgbImage::from_fn(8, 2, |x, _| image::Rgb(if x < 4 { [255, 0, 0] } else { [0, 0, 255] }))
            .save(&file)
            .unwrap();
        let px = |p: &[u8], x: usize, y: usize| [p[(y * 4 + x) * 4], p[(y * 4 + x) * 4 + 1], p[(y * 4 + x) * 4 + 2]];
        let fill = prepare_pixels(ImageSource::File(file.clone()), 4, 4, Fit::Fill).unwrap();
        // Centre crop: both halves still meet in the middle, no bars
        assert!(px(&fill, 0, 0)[0] > 200 && px(&fill, 3, 3)[2] > 200);
        let fit = prepare_pixels(ImageSource::File(file.clone()), 4, 4, Fit::Fit).unwrap();
        // Letterbox: the top row is a dark matte of the average colour, the picture is centred
        let bar = px(&fit, 0, 0);
        assert!(bar[0] > 40 && bar[0] < 120 && bar[2] > 40 && bar[2] < 120 && bar[1] < 10, "{bar:?}");
        assert!(px(&fit, 0, 1)[0] > 200 && px(&fit, 3, 1)[2] > 200 && px(&fit, 0, 3) == bar);
        let stretch = prepare_pixels(ImageSource::File(file.clone()), 4, 4, Fit::Stretch).unwrap();
        assert!(px(&stretch, 0, 0)[0] > 200 && px(&stretch, 3, 0)[2] > 200);
        // The helper protocol carries the source size and format before the pixels
        let mut out = Vec::new();
        stream_pixels(ImageSource::File(file), 4, 4, Fit::Fill, &mut out, true).unwrap();
        assert_eq!(&out[..12], &[8, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(out.len(), 12 + 4 * 4 * 4);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn collection_cycles_in_order_and_shuffles_without_repeats() {
        assert_eq!(next_bundled(None, false, 0), 0);
        assert_eq!(next_bundled(Some(&ImageSource::Bundled(COUNT - 1)), false, 0), 0);
        assert_eq!(next_bundled(Some(&ImageSource::Bundled(2)), false, 0), 3);
        for seed in 0..100 {
            let current = ImageSource::Bundled(seed as usize % COUNT);
            assert_ne!(next_bundled(Some(&current), true, seed), seed as usize % COUNT);
            assert!(next_bundled(Some(&current), true, seed) < COUNT);
        }
        for cycle in Cycle::ALL {
            assert_eq!(Cycle::from_id(cycle.id()), Some(cycle));
        }
        for fit in Fit::ALL {
            assert_eq!(Fit::from_id(fit.id()), Some(fit));
        }
    }
    #[test]
    fn preview_is_small_and_averages_colour() {
        let pixels: Vec<u8> = (0..1920 * 1080).flat_map(|_| [200u8, 100, 50, 255]).collect();
        let p = preview(&pixels, (1920, 1080), None);
        let thumb = p.thumb.unwrap();
        assert_eq!((thumb.width(), thumb.height()), (PREVIEW_W, 216));
        assert_eq!(p.average, [200, 100, 50]);
    }
    #[test]
    fn source_roundtrip_handles_spaces_newlines_and_rejects_bad_paths() {
        for s in [
            ImageSource::Bundled(2),
            ImageSource::File(PathBuf::from("/tmp/мои обои\n=.png")),
        ] {
            assert_eq!(ImageSource::parse(&s.id()), Some(s));
        }
        for s in ["file:00", "file:zz", "file:61", "builtin:no"] {
            assert!(ImageSource::parse(s).is_none());
        }
        assert_eq!(image_size(3840, 2160), (1920, 1080));
        assert_eq!(image_size(1080, 1920), (608, 1080));
    }
}
