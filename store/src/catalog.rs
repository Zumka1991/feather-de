//! The catalog: AppStream DEP-11 metadata that apt downloads with the package lists
//! (`/var/lib/swcatalog/yaml/*.yml.gz`, icons next to it), read into the store's own model.
//! Only what the store shows is kept; the YAML subset DEP-11 uses is parsed by hand.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where appstream's apt hook keeps the catalog: AppStream 1.0, then the older location
const ROOTS: [&str; 2] = ["/var/lib/swcatalog", "/var/lib/app-info"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppInfo {
    pub id: String,
    pub package: String,
    pub name: String,
    pub summary: String,
    /// Paragraphs of the description; list items start with "• "
    pub description: Vec<String>,
    pub categories: Vec<String>,
    pub icon: Option<PathBuf>,
    pub homepage: String,
    pub developer: String,
    pub license: String,
    /// The .desktop file the package installs, to open the program afterwards
    pub desktop_id: Option<String>,
    /// Lowercased name, untranslated name, package and keywords, for search
    pub search_name: String,
    pub search_keywords: String,
}

pub struct Catalog {
    pub apps: Vec<AppInfo>,
    pub installed: HashSet<String>,
}

/// AppStream locale keys for the interface language, best first; "C" is the untranslated text
fn locale_keys() -> &'static [&'static str] {
    use feather_ui::i18n::Lang;
    match feather_ui::scale::lang() {
        Lang::Ru => &["ru", "ru_RU", "C", "en"],
        Lang::En => &["en", "en_US", "en_GB", "C"],
        Lang::Es => &["es", "es_ES", "C", "en"],
        Lang::De => &["de", "de_DE", "C", "en"],
        Lang::Fr => &["fr", "fr_FR", "C", "en"],
        Lang::Zh => &["zh_CN", "zh", "zh_TW", "C", "en"],
    }
}

/// Everything the catalog files describe, plus what dpkg has installed. Slow (a second of
/// parsing): called on a worker thread.
pub fn load() -> Catalog {
    let root = ROOTS.iter().map(Path::new).find(|r| has_yaml(r)).unwrap_or(Path::new(ROOTS[0]));
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("yaml"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    // Backports are not what apt installs by default; updates and security replace the release
    files.retain(|f| f.file_name().is_some_and(|n| !n.to_string_lossy().contains("backports")));
    files.sort_by_key(|f| {
        let n = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        (n.contains("-updates") || n.contains("-security"), n)
    });
    let keys = locale_keys();
    let mut by_id: HashMap<String, AppInfo> = HashMap::new();
    for file in files {
        let Some(text) = read_maybe_gz(&file) else { continue };
        for app in parse_dep11(&text, &root.join("icons"), keys) {
            by_id.insert(app.id.clone(), app);
        }
    }
    let mut apps: Vec<AppInfo> = by_id.into_values().collect();
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    Catalog { apps, installed: installed_packages() }
}

fn has_yaml(root: &Path) -> bool {
    std::fs::read_dir(root.join("yaml")).is_ok_and(|mut d| d.next().is_some())
}

fn read_maybe_gz(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    if path.extension().is_some_and(|e| e == "gz") {
        flate2::read::GzDecoder::new(file).read_to_string(&mut text).ok()?;
    } else {
        std::io::BufReader::new(file).read_to_string(&mut text).ok()?;
    }
    Some(text)
}

/// Packages dpkg has fully installed
pub fn installed_packages() -> HashSet<String> {
    let Ok(out) = Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Abbrev} ${Package}\n"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return HashSet::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("ii"))
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
        .collect()
}

// ---------- DEP-11 ----------

/// A YAML node of the block-style subset DEP-11 uses
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Scalar(String),
    Map(Vec<(String, Node)>),
    List(Vec<Node>),
}

impl Node {
    fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Node::Scalar(s) => Some(s),
            _ => None,
        }
    }

    fn list(&self) -> &[Node] {
        match self {
            Node::List(items) => items,
            _ => &[],
        }
    }

    /// The best translation of a { C: …, ru: … } map
    fn localized(&self, keys: &[&str]) -> Option<&Node> {
        keys.iter().find_map(|k| self.get(k)).or_else(|| match self {
            Node::Map(entries) => entries.first().map(|(_, v)| v),
            _ => None,
        })
    }
}

#[derive(Clone, Copy)]
struct Line<'a> {
    indent: usize,
    text: &'a str,
}

/// Top-level keys the store reads; the rest (Languages, Provides, Screenshots…) are skipped
/// without building nodes
const WANTED: [&str; 15] = [
    "Origin",
    "Type", "ID", "Package", "Name", "Summary", "Description", "Categories", "Keywords", "Icon", "Url",
    "Developer", "DeveloperName", "ProjectLicense", "Launchable",
];

/// The desktop applications of one DEP-11 file. The first document is the header with the
/// origin, which names the icon folder.
fn parse_dep11(text: &str, icons: &Path, keys: &[&str]) -> Vec<AppInfo> {
    let mut apps = Vec::new();
    let mut origin = String::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut flush = |lines: &mut Vec<Line>, origin: &mut String| {
        if lines.is_empty() {
            return;
        }
        let doc = parse_document(lines);
        lines.clear();
        if let Some(o) = doc.get("Origin").and_then(Node::str) {
            *origin = o.to_string();
            return;
        }
        if let Some(app) = app_from(&doc, icons, origin, keys) {
            apps.push(app);
        }
    };
    for raw in text.lines() {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw == "---" || raw == "..." {
            flush(&mut lines, &mut origin);
            continue;
        }
        let text = raw.trim_start_matches(' ');
        lines.push(Line { indent: raw.len() - text.len(), text });
    }
    flush(&mut lines, &mut origin);
    apps
}

fn parse_document(lines: &mut [Line]) -> Node {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.text.is_empty() || line.indent > 0 || line.text.starts_with('-') {
            i += 1;
            continue;
        }
        let (key, _) = split_key(line.text);
        if WANTED.contains(&key) {
            entries.push(parse_entry(lines, &mut i, 0));
            continue;
        }
        i += 1;
        while i < lines.len() && (lines[i].indent > 0 || lines[i].text.starts_with('-') || lines[i].text.is_empty()) {
            i += 1;
        }
    }
    Node::Map(entries)
}

/// "Key: value" / "Key:" → (key, value)
fn split_key(text: &str) -> (&str, &str) {
    if let Some(i) = text.find(": ") {
        return (&text[..i], text[i + 2..].trim());
    }
    match text.strip_suffix(':') {
        Some(key) => (key, ""),
        None => (text, ""),
    }
}

fn next_content(lines: &[Line], mut i: usize) -> Option<usize> {
    while i < lines.len() && lines[i].text.is_empty() {
        i += 1;
    }
    (i < lines.len()).then_some(i)
}

/// The value after "key:" on its own line: a nested block, or a list at the key's own indent
fn parse_child(lines: &mut [Line], i: &mut usize, indent: usize) -> Node {
    match next_content(lines, *i) {
        Some(n) if lines[n].indent > indent => {
            *i = n;
            let child = lines[n].indent;
            parse_block(lines, i, child)
        }
        Some(n) if lines[n].indent == indent && is_item(lines[n].text) => {
            *i = n;
            parse_list(lines, i, indent)
        }
        _ => Node::Scalar(String::new()),
    }
}

fn is_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn parse_block(lines: &mut [Line], i: &mut usize, indent: usize) -> Node {
    if is_item(lines[*i].text) {
        parse_list(lines, i, indent)
    } else {
        parse_map(lines, i, indent)
    }
}

fn parse_list(lines: &mut [Line], i: &mut usize, indent: usize) -> Node {
    let mut items = Vec::new();
    while let Some(n) = next_content(lines, *i) {
        if lines[n].indent != indent || !is_item(lines[n].text) {
            break;
        }
        *i = n;
        let content = lines[n].text.strip_prefix('-').unwrap_or("").trim_start();
        if content.is_empty() {
            *i += 1;
            items.push(parse_child(lines, i, indent));
        } else if looks_like_key(content) {
            // "- key: value" opens a map whose keys line up with "key"
            let child = indent + (lines[n].text.len() - content.len());
            lines[n] = Line { indent: child, text: content };
            items.push(parse_map(lines, i, child));
        } else {
            *i += 1;
            items.push(Node::Scalar(scalar(content, lines, i, indent)));
        }
    }
    Node::List(items)
}

fn looks_like_key(text: &str) -> bool {
    if text.starts_with(['\'', '"']) {
        return false;
    }
    let (key, _) = split_key(text);
    key.len() < text.len() && !key.is_empty() && !key.contains(' ')
}

fn parse_map(lines: &mut [Line], i: &mut usize, indent: usize) -> Node {
    let mut entries = Vec::new();
    while let Some(n) = next_content(lines, *i) {
        if lines[n].indent != indent || is_item(lines[n].text) {
            break;
        }
        *i = n;
        entries.push(parse_entry(lines, i, indent));
    }
    Node::Map(entries)
}

/// One "key: value" line (at `*i`) with whatever belongs to it
fn parse_entry(lines: &mut [Line], i: &mut usize, indent: usize) -> (String, Node) {
    let (key, value) = split_key(lines[*i].text);
    *i += 1;
    let node = match value {
        "" => parse_child(lines, i, indent),
        ">" | ">-" | ">+" | "|" | "|-" | "|+" => Node::Scalar(block_scalar(lines, i, indent, value.starts_with('>'))),
        _ => Node::Scalar(scalar(value, lines, i, indent)),
    };
    (key.to_string(), node)
}

/// The lines of a block scalar: everything indented deeper than its key
fn block_scalar(lines: &[Line], i: &mut usize, indent: usize, folded: bool) -> String {
    let mut parts: Vec<&str> = Vec::new();
    while *i < lines.len() && (lines[*i].text.is_empty() || lines[*i].indent > indent) {
        parts.push(lines[*i].text.trim_end());
        *i += 1;
    }
    while parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    if !folded {
        return parts.join("\n");
    }
    let mut out = String::new();
    for part in parts {
        if part.is_empty() {
            out.push('\n');
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(part);
        }
    }
    out
}

/// A plain or quoted scalar; long ones continue on deeper-indented lines, joined by a space
fn scalar(first: &str, lines: &[Line], i: &mut usize, indent: usize) -> String {
    let mut text = first.to_string();
    let quote = first.chars().next().filter(|c| *c == '\'' || *c == '"');
    let closed = |t: &str| match quote {
        Some(q) => t.len() > 1 && t.ends_with(q) && !(q == '"' && t.ends_with("\\\"")),
        None => true,
    };
    while *i < lines.len() && lines[*i].indent > indent && !lines[*i].text.is_empty() && (quote.is_none() || !closed(&text)) {
        text.push(' ');
        text.push_str(lines[*i].text.trim_end());
        *i += 1;
    }
    match quote {
        Some('\'') => text.trim_matches('\'').replace("''", "'"),
        Some(_) => unescape_double(text.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(&text)),
        None => text.trim().to_string(),
    }
}

fn unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn app_from(doc: &Node, icons: &Path, origin: &str, keys: &[&str]) -> Option<AppInfo> {
    if doc.get("Type")?.str()? != "desktop-application" {
        return None;
    }
    let id = doc.get("ID")?.str()?.to_string();
    let package = doc.get("Package")?.str()?.to_string();
    let text = |key: &str| doc.get(key).and_then(|n| n.localized(keys)).and_then(Node::str).unwrap_or("").to_string();
    let name = text("Name");
    if name.is_empty() || package.is_empty() {
        return None;
    }
    let untranslated = doc.get("Name").and_then(|n| n.get("C")).and_then(Node::str).unwrap_or("");
    let keywords: Vec<&str> = doc
        .get("Keywords")
        .map(|k| keys.iter().chain(["C"].iter()).filter_map(|l| k.get(l)).flat_map(|n| n.list().iter().filter_map(Node::str)).collect())
        .unwrap_or_default();
    let developer = doc
        .get("Developer")
        .and_then(|d| d.get("name"))
        .or_else(|| doc.get("DeveloperName"))
        .and_then(|n| n.localized(keys))
        .and_then(Node::str)
        .unwrap_or("")
        .to_string();
    let summary = text("Summary");
    Some(AppInfo {
        search_name: format!("{} {} {}", name.to_lowercase(), untranslated.to_lowercase(), package.to_lowercase()),
        search_keywords: format!("{} {}", keywords.join(" ").to_lowercase(), summary.to_lowercase()),
        description: paragraphs(&text("Description")),
        categories: doc.get("Categories").map(|c| c.list().iter().filter_map(Node::str).map(str::to_string).collect()).unwrap_or_default(),
        icon: doc.get("Icon").and_then(|icon| cached_icon(icon, icons, origin)),
        homepage: doc.get("Url").and_then(|u| u.get("homepage")).and_then(Node::str).unwrap_or("").to_string(),
        license: doc.get("ProjectLicense").and_then(Node::str).unwrap_or("").to_string(),
        desktop_id: doc.get("Launchable").and_then(|l| l.get("desktop-id")).and_then(|d| d.list().first()).and_then(Node::str).map(str::to_string),
        id,
        package,
        name,
        summary,
        developer,
    })
}

/// The 64×64 icon apt unpacked for this origin (else the nearest size), if it is there
fn cached_icon(icon: &Node, icons: &Path, origin: &str) -> Option<PathBuf> {
    let mut found: Vec<(u32, PathBuf)> = Vec::new();
    for item in icon.get("cached")?.list() {
        let Some(name) = item.get("name").and_then(Node::str) else { continue };
        let width: u32 = item.get("width").and_then(Node::str).and_then(|w| w.parse().ok()).unwrap_or(0);
        let scale = item.get("scale").and_then(Node::str).unwrap_or("1");
        let dir = if scale == "1" { format!("{width}x{width}") } else { format!("{width}x{width}@{scale}") };
        let size = width * scale.parse::<u32>().unwrap_or(1);
        found.push((size, icons.join(origin).join(dir).join(name)));
    }
    // 64 first, then larger ones (scaled down well), then smaller
    found.sort_by_key(|(size, _)| if *size >= 64 { *size - 64 } else { 1000 + 64 - *size });
    found.into_iter().map(|(_, p)| p).find(|p| p.is_file())
}

/// DEP-11 descriptions are AppStream markup: <p>, <ul>/<ol> with <li>, inline <em>/<code>
pub fn paragraphs(markup: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let push = |current: &mut String, out: &mut Vec<String>| {
        let text = current.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() && text != "•" {
            out.push(decode_entities(&text));
        }
        current.clear();
    };
    let mut rest = markup;
    while let Some(open) = rest.find('<') {
        current.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            rest = &rest[open..];
            break;
        };
        let tag = rest[open + 1..open + close].trim().to_ascii_lowercase();
        rest = &rest[open + close + 1..];
        let name = tag.trim_start_matches('/').split_whitespace().next().unwrap_or("");
        match name {
            "p" | "ul" | "ol" => push(&mut current, &mut out),
            "li" if !tag.starts_with('/') => {
                push(&mut current, &mut out);
                current.push_str("• ");
            }
            "li" => push(&mut current, &mut out),
            _ => {}
        }
    }
    current.push_str(rest);
    push(&mut current, &mut out);
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&amp;", "&")
}

/// The program's launcher from its .desktop file: name and command
pub fn launcher(app: &AppInfo) -> Option<String> {
    let mut dirs: Vec<PathBuf> = vec![PathBuf::from("/usr/share/applications"), PathBuf::from("/usr/local/share/applications")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.insert(0, PathBuf::from(home).join(".local/share/applications"));
    }
    let id = app.desktop_id.clone().unwrap_or_else(|| format!("{}.desktop", app.id.trim_end_matches(".desktop")));
    dirs.iter().find_map(|d| std::fs::read_to_string(d.join(&id)).ok()).and_then(|text| desktop_exec(&text))
}

/// The command of a .desktop entry, without field codes
pub fn desktop_exec(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if let Some(v) = line.strip_prefix("Exec=").filter(|_| in_entry) {
            let command: Vec<&str> = v.split_whitespace().filter(|p| !p.starts_with('%')).collect();
            return (!command.is_empty()).then(|| command.join(" "));
        }
    }
    None
}

/// Starts a program detached from the store, so closing the store leaves it running
pub fn launch(command: &str) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let _ = feather_ui::process::new_session(&mut cmd).spawn();
}

/// Opens a web page in the default browser
pub fn open_url(url: &str) {
    let mut cmd = Command::new("xdg-open");
    cmd.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let _ = feather_ui::process::new_session(&mut cmd).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---
File: DEP-11
Version: '1.0'
Origin: ubuntu-resolute-universe
---
Type: desktop-application
ID: org.gimp.GIMP
Package: gimp
Name:
  C: GNU Image Manipulation Program
  ru: Редактор изображений GIMP
Summary:
  C: Create images and edit photographs
  ru: 'Создание изображений и обработка фотографий'
Description:
  C: >-
    <p>GIMP is an advanced picture editor &amp; more.</p>

    <ul>
      <li>Layers</li>
      <li>Filters</li>
    </ul>
Categories:
- Graphics
- 2DGraphics
Keywords:
  C:
  - photo
  ru:
  - фото
Icon:
  cached:
  - name: gimp_gimp.png
    width: 48
    height: 48
  - name: gimp_gimp.png
    width: 64
    height: 64
  stock: gimp
Url:
  homepage: https://www.gimp.org/
Developer:
  id: org.gimp
  name:
    C: The GIMP team
Launchable:
  desktop-id:
  - gimp.desktop
Languages:
- locale: ru
  percentage: 100
---
Type: font
ID: some.font
Package: fonts-some
Name:
  C: Some font
---
Type: desktop-application
ID: long.desktop
Package: long
Name:
  C: \"Quoted \\u00e9 name\"
Summary:
  C: A summary that the generator wrapped
    onto a second line
";

    #[test]
    fn dep11_desktop_applications() {
        let apps = parse_dep11(SAMPLE, Path::new("/nonexistent"), &["ru", "C"]);
        assert_eq!(apps.len(), 2, "{apps:#?}");
        let gimp = &apps[0];
        assert_eq!(gimp.id, "org.gimp.GIMP");
        assert_eq!(gimp.package, "gimp");
        assert_eq!(gimp.name, "Редактор изображений GIMP");
        assert_eq!(gimp.summary, "Создание изображений и обработка фотографий");
        assert_eq!(gimp.description, ["GIMP is an advanced picture editor & more.", "• Layers", "• Filters"]);
        assert_eq!(gimp.categories, ["Graphics", "2DGraphics"]);
        assert_eq!(gimp.homepage, "https://www.gimp.org/");
        assert_eq!(gimp.developer, "The GIMP team");
        assert_eq!(gimp.desktop_id.as_deref(), Some("gimp.desktop"));
        assert!(gimp.search_name.contains("gnu image") && gimp.search_keywords.contains("фото"));
        assert_eq!(gimp.icon, None, "missing icon files are not offered");
        assert_eq!(apps[1].name, "Quoted é name");
        assert_eq!(apps[1].summary, "A summary that the generator wrapped onto a second line");
    }

    #[test]
    fn description_markup() {
        assert_eq!(paragraphs("<p>One\n two</p><ol><li>a <em>b</em></li></ol><p>x &lt; y</p>"), ["One two", "• a b", "x < y"]);
        assert_eq!(desktop_exec("[Desktop Entry]\nName=X\nExec=gimp-3.0 %U\n[Desktop Action new]\nExec=other\n"), Some("gimp-3.0".into()));
    }

    /// The real catalog of this machine: cargo test -p feather-store -- --ignored --nocapture
    #[test]
    #[ignore]
    fn loads_the_system_catalog() {
        let t = std::time::Instant::now();
        let catalog = load();
        eprintln!("{} apps, {} installed packages, {:.0} ms", catalog.apps.len(), catalog.installed.len(), t.elapsed().as_secs_f64() * 1e3);
        let with_icons = catalog.apps.iter().filter(|a| a.icon.is_some()).count();
        eprintln!("with icons: {with_icons}");
        for app in catalog.apps.iter().filter(|a| ["gimp", "inkscape", "vlc", "krita", "telegram-desktop"].contains(&a.package.as_str())) {
            eprintln!("{} | {} | {} | {:?} | {:?}", app.id, app.name, app.summary, app.icon, app.categories);
        }
        assert!(catalog.apps.len() > 500);
    }
}
