//! Bounded, read-only Quick Look decoding. Called only by the preview worker.
use std::{io::{Read, BufReader}, path::Path};
use image::ImageDecoder;

pub enum Content {
    Image(tiny_skia::Pixmap, u32, u32),
    Text(Vec<String>, bool),
    Info(String),
}

pub fn load(path: &Path) -> Result<Content, String> {
    load_inner(path).map_err(|e| format!("Не удалось открыть предпросмотр: {e}"))
}

fn load_inner(path: &Path) -> Result<Content, Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() { return Ok(Content::Info("Папка".into())); }
    if !meta.is_file() { return Ok(Content::Info("Предпросмотр этого объекта недоступен".into())); }
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY).open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() { return Err("Файл изменился во время открытия".into()); }
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        if meta.len() > 32 * 1024 * 1024 { return Ok(Content::Info("Изображение слишком велико для быстрого просмотра".into())); }
        let mut reader = image::ImageReader::new(BufReader::new(file)).with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(32 * 1024 * 1024);
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        reader.limits(limits);
        let mut decoder = reader.into_decoder()?;
        let (w, h) = decoder.dimensions();
        if w as u64 * h as u64 > 8_000_000 { return Ok(Content::Info("Изображение слишком велико для быстрого просмотра".into())); }
        let orientation = decoder.orientation()?;
        let mut picture = image::DynamicImage::from_decoder(decoder)?;
        picture.apply_orientation(orientation);
        let picture = picture.thumbnail(1200, 800).into_rgba8();
        let (pw, ph) = picture.dimensions();
        let mut pixels = picture.into_raw();
        for p in pixels.chunks_exact_mut(4) {
            let a = p[3] as u16;
            for c in &mut p[..3] { *c = ((*c as u16 * a + 127) / 255) as u8; }
        }
        let pm = tiny_skia::Pixmap::from_vec(pixels, tiny_skia::IntSize::from_wh(pw, ph).ok_or("Пустое изображение")?).ok_or("Пустое изображение")?;
        return Ok(Content::Image(pm, w, h));
    }
    if ext == "svg" && meta.len() <= 16 * 1024 * 1024 {
        let mut svg_bytes = Vec::new();
        file.take(16 * 1024 * 1024).read_to_end(&mut svg_bytes)?;
        let opt = resvg::usvg::Options::default();
        if let Ok(tree) = resvg::usvg::Tree::from_data(&svg_bytes, &opt) {
            let sz = tree.size();
            let (ow, oh) = (sz.width().round().max(1.0) as u32, sz.height().round().max(1.0) as u32);
            let scale = (1000.0 / ow as f32).min(750.0 / oh as f32).clamp(0.1, 4.0);
            let rw = ((ow as f32 * scale).round() as u32).clamp(1, 1200);
            let rh = ((oh as f32 * scale).round() as u32).clamp(1, 900);
            if let Some(mut pm) = tiny_skia::Pixmap::new(rw, rh) {
                let t = tiny_skia::Transform::from_scale(rw as f32 / sz.width().max(1.0), rh as f32 / sz.height().max(1.0));
                resvg::render(&tree, t, &mut pm.as_mut());
                return Ok(Content::Image(pm, ow, oh));
            }
        }
        return Ok(Content::Info("Повреждённый SVG-файл".into()));
    }
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > 65536;
    bytes.truncate(65536);
    // A truncated UTF-8 sequence at the read boundary is allowed, invalid interior bytes aren't.
    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => Some(s),
        Err(e) if truncated && e.error_len().is_none() => std::str::from_utf8(&bytes[..e.valid_up_to()]).ok(),
        _ => None,
    };
    if let Some(text) = text.filter(|t| !t.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))) {
        let lines = text.lines().take(1000).map(|s| s.chars().take(500).collect::<String>().replace('\t', "    ")).collect::<Vec<_>>();
        return Ok(Content::Text(lines, truncated || text.lines().count() > 1000));
    }
    Ok(Content::Info(format!("{} · {}", crate::fsops::kind_of(&path.to_string_lossy(), false, false).0.label(), crate::fsops::human_size(meta.len()))))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_images_text_binary_missing_and_non_regular_files() {
        let root = std::env::temp_dir().join(format!("feather-quick-look-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let image = root.join("прозрачное.png");
        image::RgbaImage::from_pixel(12, 8, image::Rgba([200, 100, 50, 128])).save(&image).unwrap();
        assert!(matches!(load(&image).unwrap(), Content::Image(pm,12,8) if pm.data()[0..4] == [100,50,25,128]));
        let text = root.join("текст.txt");
        std::fs::write(&text, "Привет\nвторая\tстрока").unwrap();
        assert!(matches!(load(&text).unwrap(),Content::Text(lines,false) if lines==["Привет","вторая    строка"]));
        std::fs::write(&text, "я".repeat(40000)).unwrap();
        assert!(matches!(load(&text).unwrap(),Content::Text(_,true)));
        std::fs::write(&text, [0u8, 1, 2]).unwrap();
        assert!(matches!(load(&text).unwrap(),Content::Info(_)));
        assert!(load(&root.join("missing")).is_err());
        assert!(matches!(load(&root).unwrap(),Content::Info(_)));
        let fifo = root.join("pipe");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(matches!(load(&fifo).unwrap(),Content::Info(_)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
