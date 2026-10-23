//! One plain-text file per note in $XDG_DATA_HOME/feather-notes. Reads and writes happen
//! on short worker threads; a write goes to a temporary file first and is renamed over.
use std::{path::PathBuf, time::UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// Creation time in ms; also the file name
    pub id: u64,
    pub text: String,
    /// Unix seconds of the last change
    pub modified: i64,
}

pub fn dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| feather_ui::fileops::home().join(".local/share"));
    data.join("feather-notes")
}

fn path(id: u64) -> PathBuf {
    dir().join(format!("{id}.txt"))
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn new_id() -> u64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// All notes, newest change first. Unreadable or non-note files are skipped.
pub fn load() -> Vec<Note> {
    let mut notes: Vec<Note> = std::fs::read_dir(dir())
        .map(|d| {
            d.flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let id = name.strip_suffix(".txt")?.parse::<u64>().ok()?;
                    let meta = e.metadata().ok().filter(|m| m.is_file() && m.len() <= 4 << 20)?;
                    let text = std::fs::read_to_string(e.path()).ok()?;
                    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
                    Some(Note { id, text, modified })
                })
                .collect()
        })
        .unwrap_or_default();
    notes.sort_by(|a, b| b.modified.cmp(&a.modified).then(b.id.cmp(&a.id)));
    notes
}

pub fn save(id: u64, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let tmp = dir().join(format!(".{id}.txt.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path(id))
}

/// Deleted notes go to the Trash, where they can be put back.
pub fn trash(id: u64) -> bool {
    let p = path(id);
    !p.exists() || feather_ui::fileops::trash(&[p]) == 0
}

/// An empty note never shown again: removed without littering the Trash.
pub fn remove(id: u64) {
    let _ = std::fs::remove_file(path(id));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn save_load_order_and_trash() {
        let root = std::env::temp_dir().join(format!("feather-notes-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("XDG_DATA_HOME", &root);
        save(1, "Покупки\nмолоко").unwrap();
        save(2, "Идеи").unwrap();
        std::fs::write(dir().join("junk.md"), "x").unwrap();
        let notes = load();
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|n| n.id == 1 && n.text == "Покупки\nмолоко"));
        assert!(trash(1));
        assert_eq!(load().len(), 1);
        assert!(feather_ui::fileops::trash_files_dir().join("1.txt").exists());
        remove(2);
        assert!(load().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
