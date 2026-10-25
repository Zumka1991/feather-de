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
pub const NAMES: [&str; 3] = ["Рассвет", "Сияние", "Горное озеро"];
const IDS: [&str; 3] = ["feather-dawn", "feather-aurora", "feather-lake"];
const IMAGES: [&[u8]; 3] = [
    include_bytes!("../assets/wallpapers/feather-dawn.png"),
    include_bytes!("../assets/wallpapers/feather-aurora.png"),
    include_bytes!("../assets/wallpapers/feather-lake.png"),
];
/// Encoded bundled wallpaper (visual checks render the menu bar over each)
#[cfg(test)]
pub fn bundled_bytes(index: usize) -> &'static [u8] {
    IMAGES[index]
}
const THUMBS: [&[u8]; 3] = [
    include_bytes!("../assets/wallpapers/feather-dawn-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-aurora-thumb.png"),
    include_bytes!("../assets/wallpapers/feather-lake-thumb.png"),
];
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
pub fn decode(source: &ImageSource, w: i32, h: i32) -> Result<Vec<u8>, String> {
    let length = w as usize * h as usize * 4;
    // Run this exact executable even after an update replaces/unlinks its pathname.
    // current_exe() then contains " (deleted)" on Linux and cannot be spawned.
    let mut child = Command::new("/proc/self/exe")
        .args([
            "--decode-wallpaper",
            &source.id(),
            &w.to_string(),
            &h.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut pixels = Vec::with_capacity(length);
    let read = child
        .stdout
        .take()
        .unwrap()
        .take(length as u64 + 1)
        .read_to_end(&mut pixels);
    if pixels.len() > length {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if read.is_err() || !status.success() || pixels.len() != length {
        return Err(
            "Не удалось загрузить изображение. Выберите PNG, JPEG или WebP до 32 МБ.".into(),
        );
    }
    Ok(pixels)
}

pub fn helper(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        libc::alarm(10);
        let limit = libc::rlimit {
            rlim_cur: 768 * 1024 * 1024,
            rlim_max: 768 * 1024 * 1024,
        };
        libc::setrlimit(libc::RLIMIT_AS, &limit);
    }
    if args.len() != 5 {
        return Err("decode arguments".into());
    }
    let source = ImageSource::parse(&args[2]).ok_or("invalid image source")?;
    let w = args[3].parse::<u32>()?;
    let h = args[4].parse::<u32>()?;
    if w == 0 || h == 0 || w > 1920 || h > 1080 {
        return Err("invalid image size".into());
    }
    std::io::stdout()
        .lock()
        .write_all(&prepare_pixels(source, w, h)?)?;
    Ok(())
}

fn prepare_pixels(
    source: ImageSource,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(192 * 1024 * 1024);
    let mut picture = match source {
        ImageSource::Bundled(i) => {
            let mut r =
                image::ImageReader::new(std::io::Cursor::new(IMAGES[i])).with_guessed_format()?;
            r.limits(limits);
            r.decode()?
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
            let mut decoder = r.into_decoder()?;
            let orientation = image::ImageDecoder::orientation(&mut decoder)?;
            let mut p = image::DynamicImage::from_decoder(decoder)?;
            p.apply_orientation(orientation);
            p
        }
    };
    // Flatten alpha against a neutral background, preserving RGBA premultiplication.
    picture = picture.resize_to_fill(w, h, image::imageops::FilterType::Triangle);
    let mut pixels = picture.to_rgba8().into_raw();
    for p in pixels.chunks_exact_mut(4) {
        let a = p[3] as u16;
        for c in &mut p[..3] {
            *c = ((*c as u16 * a + 24 * (255 - a) + 127) / 255) as u8;
        }
        p[3] = 255;
    }
    Ok(pixels)
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

thread_local! {
    /// Decoded gallery thumbnails (3 × 240×135, ~0.4 МБ) while the gallery is visible.
    static THUMB_CACHE: std::cell::RefCell<Vec<Option<tiny_skia::Pixmap>>> = const { std::cell::RefCell::new(Vec::new()) };
}
/// Frees the thumbnail cache of this thread once the gallery is no longer painted.
pub fn release_thumbs() {
    THUMB_CACHE.with_borrow_mut(|cache| *cache = Vec::new());
}
/// Thumbnail clipped to a rounded rectangle (pattern fill, no full-size mask).
pub fn paint_thumb_rounded(pm: &mut tiny_skia::PixmapMut, index: usize, rect: crate::shell::R, radius: f32) {
    let Some(pic) = THUMB_CACHE.with_borrow_mut(|cache| {
        if cache.is_empty() {
            *cache = THUMBS.iter().map(|d| tiny_skia::Pixmap::decode_png(d).ok()).collect();
        }
        cache.get(index).cloned().flatten()
    }) else {
        return;
    };
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
        for i in 0..3 {
            assert_eq!(
                prepare_pixels(ImageSource::Bundled(i), 320, 180)
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
            let result = prepare_pixels(ImageSource::File(file), 3, 3).unwrap();
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
            prepare_pixels(ImageSource::File(file), 1, 1).unwrap(),
            [24, 24, 24, 255]
        );
        assert!(prepare_pixels(ImageSource::File(path.join("missing.png")), 3, 3).is_err());
        std::fs::remove_dir_all(path).unwrap();
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
