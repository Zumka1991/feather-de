//! File type by name, as Files and the desktop show it.

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Folder,
    Document,
    Text,
    Code,
    Image,
    Audio,
    Video,
    Archive,
    Pdf,
    Executable,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Folder => "Папка",
            Kind::Document => "Документ",
            Kind::Text => "Текст",
            Kind::Code => "Исходный код",
            Kind::Image => "Изображение",
            Kind::Audio => "Аудио",
            Kind::Video => "Видео",
            Kind::Archive => "Архив",
            Kind::Pdf => "Документ PDF",
            Kind::Executable => "Программа",
        }
    }
}

pub fn kind_of(name: &str, is_dir: bool, executable: bool) -> (Kind, String) {
    if is_dir {
        return (Kind::Folder, String::new());
    }
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let kind = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff" | "heic" | "avif" | "ico" | "xcf" | "kra" => Kind::Image,
        "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" | "aac" | "wma" => Kind::Audio,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "m4v" | "flv" => Kind::Video,
        "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "bz2" | "7z" | "rar" | "deb" | "rpm" | "iso" | "appimage" => Kind::Archive,
        "pdf" => Kind::Pdf,
        "txt" | "md" | "log" | "csv" | "ini" | "conf" | "cfg" | "nfo" => Kind::Text,
        "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "kt" | "sh" | "toml"
        | "json" | "yaml" | "yml" | "html" | "css" | "xml" | "lua" | "zig" | "nix" | "sql" => Kind::Code,
        "" if executable => Kind::Executable,
        _ => Kind::Document,
    };
    (kind, ext)
}

