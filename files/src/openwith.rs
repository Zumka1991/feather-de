//! «Открыть с помощью»: the programs that can open a file, from the freedesktop MIME database
//! (globs2, subclasses, aliases) and the .desktop files that declare `MimeType=`. Read only when
//! the user asks for the list, then kept until a folder of programs or mimeapps.list changes.

use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Handler {
    /// Desktop file id ("org.gnome.eog.desktop")
    pub id: String,
    pub name: String,
    exec: String,
    icon: Option<String>,
    file: PathBuf,
    no_display: bool,
    mimes: Vec<String>,
}

#[derive(Default)]
pub struct Db {
    /// (weight, mime, glob, case-sensitive)
    globs: Vec<(u32, String, String, bool)>,
    parents: HashMap<String, Vec<String>>,
    aliases: HashMap<String, String>,
    apps: Vec<Handler>,
    /// mimeapps.list, most important first
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, Vec<String>>,
    watched: Vec<(PathBuf, Option<SystemTime>)>,
}

fn stamp(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs = vec![std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))];
    let system = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    // Snap and Flatpak exports, even when the session's environment lacks them
    dirs.extend([home.join(".local/share/flatpak/exports/share"), PathBuf::from("/var/lib/flatpak/exports/share"), PathBuf::from("/var/lib/snapd/desktop")]);
    dirs.dedup();
    dirs
}

fn config_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs = vec![std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"))];
    let system = std::env::var("XDG_CONFIG_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs
}

impl Db {
    pub fn system() -> Self {
        Self::load(&data_dirs(), &config_dirs())
    }

    /// `data` and `config` are XDG data and config folders, most important first
    pub fn load(data: &[PathBuf], config: &[PathBuf]) -> Self {
        let mut db = Self::default();
        // The MIME database: the first folder that has one wins for each glob/parent
        for dir in data.iter().rev() {
            let mime = dir.join("mime");
            if let Ok(text) = fs::read_to_string(mime.join("globs2")) {
                for line in text.lines().filter(|l| !l.starts_with('#')) {
                    let mut parts = line.splitn(4, ':');
                    let (Some(w), Some(m), Some(g)) = (parts.next(), parts.next(), parts.next()) else { continue };
                    let cs = parts.next().is_some_and(|f| f.split(',').any(|f| f == "cs"));
                    db.globs.push((w.parse().unwrap_or(50), m.to_string(), g.to_string(), cs));
                }
            }
            for (file, map) in [("subclasses", true), ("aliases", false)] {
                let Ok(text) = fs::read_to_string(mime.join(file)) else { continue };
                for (a, b) in text.lines().filter_map(|l| l.split_once(' ')) {
                    if map {
                        db.parents.entry(a.to_string()).or_default().push(b.to_string());
                    } else {
                        db.aliases.insert(a.to_string(), b.to_string());
                    }
                }
            }
            db.watched.push((mime.join("globs2"), stamp(&mime.join("globs2"))));
        }
        // Programs: a desktop id found earlier hides the same id further down
        let lang = feather_ui::scale::lang();
        for dir in data {
            let apps = dir.join("applications");
            db.watched.push((apps.clone(), stamp(&apps)));
            let Ok(entries) = fs::read_dir(&apps) else { continue };
            let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "desktop")).collect();
            found.sort();
            for path in found {
                let id = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if db.apps.iter().any(|a| a.id == id) {
                    continue;
                }
                if let Some(app) = parse_desktop(&path, id, lang.desktop_name_keys()) {
                    db.apps.push(app);
                }
            }
        }
        // mimeapps.list: the user's config first, then the system's and the programs' folders
        let lists = config.iter().map(|d| d.join("mimeapps.list")).chain(data.iter().flat_map(|d| [d.join("applications/mimeapps.list"), d.join("applications/defaults.list")]));
        for list in lists {
            db.watched.push((list.clone(), stamp(&list)));
            let Ok(text) = fs::read_to_string(&list) else { continue };
            let mut section = "";
            for line in text.lines().map(str::trim) {
                if line.starts_with('[') {
                    section = line;
                    continue;
                }
                let Some((mime, ids)) = line.split_once('=') else { continue };
                let map = match section {
                    "[Default Applications]" => &mut db.defaults,
                    "[Added Associations]" => &mut db.added,
                    "[Removed Associations]" => &mut db.removed,
                    _ => continue,
                };
                let to = map.entry(mime.trim().to_string()).or_default();
                for id in ids.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                    if !to.iter().any(|t| t == id) {
                        to.push(id.to_string());
                    }
                }
            }
        }
        db
    }

    /// Nothing that the list is made from has changed since it was read
    pub fn fresh(&self) -> bool {
        self.watched.iter().all(|(p, s)| stamp(p) == *s)
    }

    /// The file's MIME type by name, then a look at its first bytes
    pub fn mime_of(&self, path: &Path) -> String {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let lower = name.to_lowercase();
        let mut best: Option<(u32, usize, &str)> = None;
        for (weight, mime, glob, cs) in &self.globs {
            let hit = if *cs { glob_match(glob, &name) } else { glob_match(&glob.to_lowercase(), &lower) };
            if hit && best.is_none_or(|(w, len, _)| (*weight, glob.len()) > (w, len)) {
                best = Some((*weight, glob.len(), mime));
            }
        }
        if let Some((_, _, mime)) = best {
            return self.unalias(mime).to_string();
        }
        let mut head = [0u8; 512];
        let n = fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
        // A character cut at the end of the sample still counts as text
        let text = n > 0 && !head[..n].contains(&0) && std::str::from_utf8(&head[..n]).map_or_else(|e| e.error_len().is_none(), |_| true);
        if text { "text/plain" } else { "application/octet-stream" }.into()
    }

    fn unalias<'a>(&'a self, mime: &'a str) -> &'a str {
        self.aliases.get(mime).map(String::as_str).unwrap_or(mime)
    }

    /// `mime` and the types it is a kind of (text/x-rust → text/plain), nearest first
    fn lineage(&self, mime: &str) -> Vec<String> {
        let mut out = vec![self.unalias(mime).to_string()];
        let mut i = 0;
        while i < out.len() {
            let parents = self.parents.get(&out[i]).cloned().unwrap_or_default();
            for p in parents {
                let p = self.unalias(&p).to_string();
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        // Every text/* is also text/plain
        if out[0].starts_with("text/") && !out.iter().any(|m| m == "text/plain") {
            out.push("text/plain".into());
        }
        out
    }

    /// Programs for this file, the default one first
    pub fn handlers(&self, path: &Path) -> Vec<Handler> {
        let lineage = self.lineage(&self.mime_of(path));
        let removed = |mime: &str, id: &str| self.removed.get(mime).is_some_and(|r| r.iter().any(|x| x == id));
        let mut out: Vec<Handler> = Vec::new();
        let mut push = |app: &Handler| {
            if !out.iter().any(|a| a.id == app.id) {
                out.push(app.clone());
            }
        };
        for mime in &lineage {
            for map in [&self.defaults, &self.added] {
                for id in map.get(mime).into_iter().flatten() {
                    if let Some(app) = self.apps.iter().find(|a| a.id == *id && !removed(mime, id)) {
                        push(app);
                    }
                }
            }
        }
        // Then those for the exact type before those for its parents: Feather's own programs
        // (what a double-click opens), then by name
        for mime in &lineage {
            let mut fit: Vec<&Handler> = self.apps.iter().filter(|a| !a.no_display && a.mimes.iter().any(|m| self.unalias(m) == mime) && !removed(mime, &a.id)).collect();
            fit.sort_by_cached_key(|a| (!a.exec.starts_with("feather-"), a.name.to_lowercase()));
            fit.into_iter().for_each(&mut push);
        }
        out
    }
}

fn parse_desktop(path: &Path, id: String, name_keys: &[&str]) -> Option<Handler> {
    let text = fs::read_to_string(path).ok()?;
    let (mut name, mut name_loc, mut exec, mut icon, mut mimes) = (None, None, None, None, Vec::new());
    let (mut in_entry, mut no_display, mut application) = (false, false, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = name_keys.iter().find_map(|k| line.strip_prefix(k)) {
            name_loc = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Name=") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Exec=") {
            exec = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Icon=") {
            icon = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("MimeType=") {
            mimes = v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        } else if line == "Type=Application" {
            application = true;
        } else if line == "NoDisplay=true" {
            no_display = true;
        } else if line == "Hidden=true" || line == "Terminal=true" {
            // Removed by the user, or needs a terminal we would have to pick for it
            return None;
        } else if let Some(v) = line.strip_prefix("TryExec=") {
            if !on_path(v.trim()) {
                return None;
            }
        }
    }
    if !application {
        return None;
    }
    let name = name_loc.or(name)?;
    Some(Handler { id, name, exec: exec?, icon, file: path.to_path_buf(), no_display, mimes })
}

fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// `*` and `?` wildcards; `[` classes are taken literally (rare in globs2)
fn glob_match(glob: &str, name: &str) -> bool {
    let (g, n): (Vec<char>, Vec<char>) = (glob.chars().collect(), name.chars().collect());
    let (mut gi, mut ni, mut star, mut mark) = (0, 0, None, 0);
    while ni < n.len() {
        if gi < g.len() && (g[gi] == '?' || g[gi] == n[ni]) {
            gi += 1;
            ni += 1;
        } else if gi < g.len() && g[gi] == '*' {
            star = Some(gi);
            mark = ni;
            gi += 1;
        } else if let Some(s) = star {
            gi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    g[gi..].iter().all(|&c| c == '*')
}

/// Splits Exec= into arguments: double quotes group, a backslash escapes inside them
fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => cur.extend(chars.next()),
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        args.push(cur);
    }
    args
}

impl Handler {
    /// The command line for opening `file`, with the Exec= field codes filled in
    pub fn command(&self, file: &Path) -> Vec<String> {
        let file = file.to_string_lossy();
        let mut used = false;
        let mut out = Vec::new();
        for arg in split_exec(&self.exec) {
            match arg.as_str() {
                "%f" | "%F" | "%u" | "%U" => {
                    used = true;
                    out.push(file.to_string());
                }
                "%i" => {
                    if let Some(icon) = &self.icon {
                        out.extend(["--icon".into(), icon.clone()]);
                    }
                }
                _ => {
                    let mut s = String::new();
                    let mut chars = arg.chars();
                    while let Some(c) = chars.next() {
                        if c != '%' {
                            s.push(c);
                            continue;
                        }
                        match chars.next() {
                            Some('%') => s.push('%'),
                            Some('f' | 'F' | 'u' | 'U') => {
                                used = true;
                                s.push_str(&file);
                            }
                            Some('c') => s.push_str(&self.name),
                            Some('k') => s.push_str(&self.file.to_string_lossy()),
                            _ => {}
                        }
                    }
                    if !s.is_empty() || !arg.contains('%') {
                        out.push(s);
                    }
                }
            }
        }
        if !used {
            out.push(file.into_owned());
        }
        out
    }

    /// Starts the program on `file`; it is never waited for (our SIGCHLD handler reaps it)
    pub fn launch(&self, file: &Path) -> std::io::Result<()> {
        let argv = self.command(file);
        let (program, args) = argv.split_first().ok_or(std::io::ErrorKind::InvalidInput)?;
        let mut cmd = std::process::Command::new(program);
        cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if let Some(dir) = file.parent() {
            cmd.current_dir(dir);
        }
        cmd.spawn().map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("feather-openwith-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("data/mime")).unwrap();
        fs::create_dir_all(dir.join("data/applications")).unwrap();
        fs::create_dir_all(dir.join("config")).unwrap();
        dir
    }

    fn app(dir: &Path, id: &str, name: &str, exec: &str, mimes: &str) {
        fs::write(dir.join("data/applications").join(id), format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nMimeType={mimes}\n")).unwrap();
    }

    #[test]
    fn programs_for_a_file_default_first_then_by_type() {
        let root = fixture("handlers");
        fs::write(root.join("data/mime/globs2"), "50:text/x-rust:*.rs\n50:image/png:*.png\n50:text/x-csrc:*.c\n").unwrap();
        fs::write(root.join("data/mime/subclasses"), "text/x-rust text/plain\n").unwrap();
        fs::write(root.join("data/mime/aliases"), "image/x-png image/png\n").unwrap();
        app(&root, "edit.desktop", "Текстовый редактор", "feather-textedit %F", "text/plain;text/x-rust;");
        app(&root, "gedit.desktop", "Gedit", "gedit %U", "text/plain;");
        app(&root, "writer.desktop", "LibreOffice Writer", "libreoffice --writer %U", "text/plain;");
        app(&root, "eog.desktop", "Просмотр", "eog %U", "image/x-png;");
        app(&root, "gimp.desktop", "GIMP", "gimp-2.10 %U", "image/png;");
        fs::write(root.join("data/applications/term.desktop"), "[Desktop Entry]\nType=Application\nName=Vim\nExec=vim %F\nTerminal=true\nMimeType=text/plain;\n").unwrap();
        fs::write(root.join("config/mimeapps.list"), "[Default Applications]\nimage/png=gimp.desktop\n[Removed Associations]\ntext/plain=gedit.desktop;\n").unwrap();
        let db = Db::load(&[root.join("data")], &[root.join("config")]);
        let names = |file: &str| db.handlers(&root.join(file)).into_iter().map(|h| h.name).collect::<Vec<_>>();
        // A subclass of text/plain finds text editors; a removed association and a terminal app don't show
        // Feather's own editor first, the rest by name
        assert_eq!(names("main.rs"), ["Текстовый редактор", "LibreOffice Writer"]);
        // The default first, then programs that declare an alias of the type
        assert_eq!(names("Снимок.PNG"), ["GIMP", "Просмотр"]);
        // text/* without its own programs falls back to text/plain
        assert_eq!(names("x.c"), ["Текстовый редактор", "LibreOffice Writer"]);
        // Unknown name, readable content
        fs::write(root.join("README"), "hello").unwrap();
        assert_eq!(db.mime_of(&root.join("README")), "text/plain");
        assert!(db.fresh());
    }

    #[test]
    fn exec_field_codes() {
        let h = |exec: &str| Handler { id: "x.desktop".into(), name: "Икс".into(), exec: exec.into(), icon: Some("x".into()), file: "/a/x.desktop".into(), no_display: false, mimes: Vec::new() };
        let f = Path::new("/home/u/Мой файл.txt");
        assert_eq!(h("gedit %U").command(f), ["gedit", "/home/u/Мой файл.txt"]);
        assert_eq!(h("env A=1 \"/opt/my app/run\" --new-window %f").command(f), ["env", "A=1", "/opt/my app/run", "--new-window", "/home/u/Мой файл.txt"]);
        assert_eq!(h("app --file=%f %i %c").command(f), ["app", "--file=/home/u/Мой файл.txt", "--icon", "x", "Икс"]);
        // No field code: the file is appended
        assert_eq!(h("viewer").command(f), ["viewer", "/home/u/Мой файл.txt"]);
        assert!(glob_match("*.tar.gz", "a.tar.gz") && glob_match("makefile", "makefile") && !glob_match("*.rs", "a.rsx"));
    }
}
