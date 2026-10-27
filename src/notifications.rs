//! org.freedesktop.Notifications server. D-Bus and image conversion run on a worker; the
//! event loop only receives parsed, size-limited notifications and answers with
//! close/action events. The name is never taken from an already running daemon.
use crate::FeatherCompositor;
use futures_lite::future;
use smithay::reexports::calloop::{channel, LoopHandle};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};
use zbus::{object_server::SignalEmitter, zvariant::OwnedValue, Connection};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Largest side of a hint image kept for drawing (36 logical px at ×2.5 and a margin)
const IMAGE_PX: u32 = 96;
const MAX_ACTIONS: usize = 3;

/// How one program's notifications reach the user (Системные настройки → Уведомления)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delivery {
    #[default]
    Banners,
    /// Straight to the notification center, no banner
    CenterOnly,
    /// Dropped (the client is told it was dismissed)
    Off,
}

impl Delivery {
    pub const ALL: [Self; 3] = [Self::Banners, Self::CenterOnly, Self::Off];
    pub fn next(self) -> Self {
        match self { Self::Banners => Self::CenterOnly, Self::CenterOnly => Self::Off, Self::Off => Self::Banners }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Banners => "Баннеры и центр уведомлений",
            Self::CenterOnly => "Только центр уведомлений",
            Self::Off => "Выключены",
        }
    }
    pub fn id(self) -> &'static str {
        match self { Self::Banners => "banners", Self::CenterOnly => "center", Self::Off => "off" }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Banners, Self::CenterOnly, Self::Off].into_iter().find(|d| d.id() == s)
    }
}

/// A program that has sent notifications; `key` is its desktop entry or app name
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPolicy {
    pub key: String,
    pub label: String,
    pub delivery: Delivery,
}

/// Programs remembered for per-app settings; new ones beyond it get the default
pub const MAX_APPS: usize = 64;

/// Safe for one `key|label|mode` line of settings.conf
pub fn clean_field(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && *c != '|').take(64).collect::<String>().trim().to_string()
}

/// Default banner time when the client leaves it to the server (expire_timeout = -1)
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct Incoming {
    pub id: u32,
    /// The client asked to replace this id (it is reused as `id`)
    pub replaces: bool,
    pub app_name: String,
    pub app_icon: String,
    pub desktop_entry: String,
    pub image: Option<Arc<tiny_skia::Pixmap>>,
    pub summary: String,
    pub body: String,
    /// (key, label); "default" is the click on the banner itself
    pub actions: Vec<(String, String)>,
    /// None: stays until the user closes it
    pub timeout: Option<Duration>,
    pub critical: bool,
    /// Stays after an action was invoked
    pub resident: bool,
    /// Not kept in the notification center
    pub transient: bool,
    /// Resolved sound (hint or theme default); None with `suppress-sound`
    pub sound: Option<crate::sounds::Sound>,
}

pub enum Update {
    Show(Box<Incoming>),
    Close(u32),
}

/// NotificationClosed reasons from the specification
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

enum Command {
    Closed(u32, Reason),
    Invoked { id: u32, key: String, token: Option<String> },
}

pub struct Notifications {
    tx: async_channel::Sender<Command>,
}

impl Notifications {
    pub fn new(handle: &LoopHandle<'static, FeatherCompositor>) -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, rx) = async_channel::bounded(64);
        let (updates, source) = channel::sync_channel(32);
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(update) = event {
                state.notification_update(update);
            }
        })?;
        std::thread::Builder::new()
            .name("feather-notify".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                future::block_on(async move {
                    if let Err(e) = worker(rx, updates).await {
                        tracing::warn!("Уведомления: {e}");
                    }
                })
            })?;
        Ok(Self { tx })
    }

    pub fn closed(&self, id: u32, reason: Reason) {
        let _ = self.tx.try_send(Command::Closed(id, reason));
    }

    pub fn invoked(&self, id: u32, key: String, token: Option<String>) {
        let _ = self.tx.try_send(Command::Invoked { id, key, token });
    }
}

struct Server {
    next: Arc<AtomicU32>,
    updates: channel::SyncSender<Update>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 { replaces_id } else { next_id(&self.next) };
        let n = parse(id, replaces_id != 0, app_name, app_icon, summary, body, actions, &hints, expire_timeout);
        // A full queue means the loop is far behind: drop the banner, never block D-Bus.
        let _ = self.updates.try_send(Update::Show(Box::new(n)));
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.updates.try_send(Update::Close(id));
    }

    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "body-markup", "icon-static", "persistence", "sound"]
    }

    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("Пёрышко", "Feather DE", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn activation_token(emitter: &SignalEmitter<'_>, id: u32, activation_token: &str) -> zbus::Result<()>;
}

fn next_id(counter: &AtomicU32) -> u32 {
    loop {
        let id = counter.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

fn clip(mut s: String, chars: usize) -> String {
    if let Some((i, _)) = s.char_indices().nth(chars) {
        s.truncate(i);
        s.push('…');
    }
    s
}

#[allow(clippy::too_many_arguments)]
fn parse(
    id: u32,
    replaces: bool,
    app_name: String,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: &HashMap<String, OwnedValue>,
    expire_timeout: i32,
) -> Incoming {
    let hint = |k: &str| hints.get(k);
    let critical = hint("urgency").and_then(|v| u8::try_from(v).ok()) == Some(2);
    let flag = |k: &str| hint(k).and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
    let string = |k: &str| hint(k).and_then(|v| <&str>::try_from(v).ok()).unwrap_or("").to_string();
    let image = ["image-data", "image_data", "icon_data"]
        .iter()
        .find_map(|k| hint(k).and_then(image_data));
    let image_path = ["image-path", "image_path"].iter().map(|k| string(k)).find(|s| !s.is_empty());
    let icon = image_path.unwrap_or(app_icon);
    let icon = icon.strip_prefix("file://").map(str::to_string).unwrap_or(icon);
    let timeout = match expire_timeout {
        _ if critical => None,
        0 => None,
        t if t < 0 => Some(DEFAULT_TIMEOUT),
        t => Some(Duration::from_millis(t as u64).max(Duration::from_secs(1))),
    };
    let actions = actions
        .chunks_exact(2)
        .filter(|p| !p[0].is_empty())
        .map(|p| (clip(p[0].clone(), 64), clip(strip_markup(&p[1]), 32)))
        .filter(|(k, l)| k == "default" || !l.is_empty())
        .take(MAX_ACTIONS + 1)
        .collect();
    Incoming {
        id,
        replaces,
        app_name: clip(app_name, 64),
        app_icon: clip(icon, 512),
        desktop_entry: clip(string("desktop-entry"), 128),
        image,
        summary: clip(strip_markup(&summary), 200),
        body: clip(strip_markup(&body), 600),
        actions,
        timeout,
        critical,
        resident: flag("resident"),
        transient: flag("transient"),
        sound: if flag("suppress-sound") { None } else { Some(sound_path(&string("sound-file"), &string("sound-name")).map(crate::sounds::Sound::File)
            .unwrap_or(crate::sounds::Sound::Effect(crate::sounds::Effect::Notification))) },
    }
}

const SOUND_EXT: [&str; 4] = ["oga", "ogg", "wav", "flac"];

/// `sound-file` if it is a readable sound, else `sound-name` or «message-new-instant»
/// from the freedesktop theme. Runs on the worker (it touches the filesystem).
fn sound_path(file: &str, name: &str) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut dirs: Vec<PathBuf> = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .into_iter().collect();
    dirs.extend(std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into()).split(':').map(PathBuf::from));
    sound_in(file, name, &dirs)
}

fn sound_in(file: &str, name: &str, dirs: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    let file = file.strip_prefix("file://").unwrap_or(file);
    let ok = |p: &Path| p.is_file() && p.extension().and_then(|e| e.to_str()).is_some_and(|e| SOUND_EXT.contains(&e));
    if !file.is_empty() && Path::new(file).is_absolute() && ok(Path::new(file)) {
        return Some(PathBuf::from(file));
    }
    let valid = |n: &str| !n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    [name, "message-new-instant"].into_iter().filter(|n| valid(n)).find_map(|n| {
        dirs.iter().find_map(|d| SOUND_EXT.iter().map(|e| d.join(format!("sounds/freedesktop/stereo/{n}.{e}"))).find(|p| ok(p)))
    })
}

/// The body-markup subset (b, i, u, a, img) is shown as plain text: tags are removed,
/// entities decoded, whitespace runs folded so line breaks do not waste banner lines.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '<' {
            if let Some(end) = rest.find('>') {
                let tag = &rest[1..end];
                if tag.starts_with("br") || tag.starts_with("/p") { out.push(' '); }
                rest = &rest[end + 1..];
                continue;
            }
        }
        if c == '&' {
            let entities = [("&amp;", '&'), ("&lt;", '<'), ("&gt;", '>'), ("&quot;", '"'), ("&apos;", '\''), ("&#39;", '\''), ("&nbsp;", ' ')];
            if let Some((e, ch)) = entities.iter().find(|(e, _)| rest.starts_with(e)) {
                out.push(*ch);
                rest = &rest[e.len()..];
                continue;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `(iiibiiay)`: width, height, rowstride, has_alpha, bits, channels, RGB(A) bytes.
/// Converted to a premultiplied pixmap no larger than IMAGE_PX.
fn image_data(value: &OwnedValue) -> Option<Arc<tiny_skia::Pixmap>> {
    let (w, h, stride, alpha, bits, channels, data) =
        <(i32, i32, i32, bool, i32, i32, Vec<u8>)>::try_from(value.try_clone().ok()?).ok()?;
    image_from_raw(w, h, stride, alpha, bits, channels, &data)
}

fn image_from_raw(w: i32, h: i32, stride: i32, alpha: bool, bits: i32, channels: i32, data: &[u8]) -> Option<Arc<tiny_skia::Pixmap>> {
    if !(1..=1024).contains(&w) || !(1..=1024).contains(&h) || bits != 8 || channels != if alpha { 4 } else { 3 } {
        return None;
    }
    let (w, h, stride, ch) = (w as usize, h as usize, stride as usize, channels as usize);
    if stride < w * ch || data.len() < stride * (h - 1) + w * ch {
        return None;
    }
    let mut full = tiny_skia::Pixmap::new(w as u32, h as u32)?;
    for (y, row) in full.data_mut().chunks_exact_mut(w * 4).enumerate() {
        let src = &data[y * stride..y * stride + w * ch];
        for (px, s) in row.chunks_exact_mut(4).zip(src.chunks_exact(ch)) {
            let a = if alpha { s[3] as u16 } else { 255 };
            let pm = |c: u8| ((c as u16 * a + 127) / 255) as u8;
            px.copy_from_slice(&[pm(s[0]), pm(s[1]), pm(s[2]), a as u8]);
        }
    }
    let side = w.max(h) as u32;
    if side <= IMAGE_PX {
        return Some(Arc::new(full));
    }
    let k = IMAGE_PX as f32 / side as f32;
    let (sw, sh) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
    let mut small = tiny_skia::Pixmap::new(sw, sh)?;
    small.draw_pixmap(
        0,
        0,
        full.as_ref(),
        &tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() },
        tiny_skia::Transform::from_scale(sw as f32 / w as f32, sh as f32 / h as f32),
        None,
    );
    Some(Arc::new(small))
}

async fn worker(commands: async_channel::Receiver<Command>, updates: channel::SyncSender<Update>) -> zbus::Result<()> {
    let conn = zbus::connection::Builder::session()?.build().await?;
    serve(&conn, commands, updates).await
}

async fn serve(conn: &Connection, commands: async_channel::Receiver<Command>, updates: channel::SyncSender<Update>) -> zbus::Result<()> {
    conn.object_server()
        .at(PATH, Server { next: Arc::new(AtomicU32::new(1)), updates })
        .await?;
    // Never replace mako/dunst/the host desktop's daemon (nested runs share its bus).
    let reply = conn
        .request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await;
    if !matches!(reply, Ok(zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner)) {
        tracing::info!("Уведомления: {NAME} уже обслуживает другая программа");
        return Ok(());
    }
    let emitter = SignalEmitter::new(conn, PATH)?;
    while let Ok(command) = commands.recv().await {
        match command {
            Command::Closed(id, reason) => {
                let _ = Server::notification_closed(&emitter, id, reason as u32).await;
            }
            Command::Invoked { id, key, token } => {
                if let Some(token) = token {
                    let _ = Server::activation_token(&emitter, id, &token).await;
                }
                let _ = Server::action_invoked(&emitter, id, &key).await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::StreamExt;
    use zbus::zvariant::Value;

    #[test]
    fn markup_is_reduced_to_folded_plain_text() {
        assert_eq!(strip_markup("<b>Привет</b>,\n  <a href=\"x\">мир</a> &amp; &lt;3&gt;"), "Привет, мир & <3>");
        assert_eq!(strip_markup("a<br/>b"), "a b");
        assert_eq!(strip_markup("x < y"), "x < y");
        assert_eq!(strip_markup("&unknown; ok"), "&unknown; ok");
    }

    #[test]
    fn sounds_come_from_valid_files_or_theme_names_only() {
        let dir = std::env::temp_dir().join(format!("feather-sound-{}", std::process::id()));
        let theme = dir.join("sounds/freedesktop/stereo");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("message-new-instant.oga"), b"x").unwrap();
        std::fs::write(theme.join("bell.oga"), b"x").unwrap();
        std::fs::write(dir.join("ding.wav"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let dirs = [dir.clone()];
        let f = |file: &str, name: &str| sound_in(file, name, &dirs).map(|p| p.file_name().unwrap().to_string_lossy().into_owned());
        assert_eq!(f(dir.join("ding.wav").to_str().unwrap(), ""), Some("ding.wav".into()));
        assert_eq!(f(&format!("file://{}", dir.join("ding.wav").display()), ""), Some("ding.wav".into()));
        assert_eq!(f(dir.join("notes.txt").to_str().unwrap(), "bell"), Some("bell.oga".into()), "not a sound file");
        assert_eq!(f("relative.wav", "../bell"), Some("message-new-instant.oga".into()), "unsafe name falls back");
        assert_eq!(f("", "missing"), Some("message-new-instant.oga".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hint_images_are_validated_premultiplied_and_bounded() {
        let rgba = [255, 0, 0, 128, 0, 255, 0, 255];
        let img = image_from_raw(2, 1, 8, true, 8, 4, &rgba).unwrap();
        assert_eq!(&img.data()[..4], &[128, 0, 0, 128]);
        // Padded rows of RGB without alpha
        let rgb = [1, 2, 3, 0, 4, 5, 6, 0];
        let img = image_from_raw(1, 2, 4, false, 8, 3, &rgb).unwrap();
        assert_eq!(img.data(), &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(image_from_raw(2, 2, 8, true, 8, 4, &rgba).is_none(), "short data");
        assert!(image_from_raw(2, 1, 8, true, 8, 3, &rgba).is_none(), "channels mismatch");
        let big = vec![200u8; 512 * 256 * 4];
        let img = image_from_raw(512, 256, 2048, true, 8, 4, &big).unwrap();
        assert_eq!((img.width(), img.height()), (IMAGE_PX, IMAGE_PX / 2));
    }

    #[test]
    fn timeouts_urgency_and_actions_follow_the_specification() {
        let mut hints = HashMap::new();
        let p = |hints: &HashMap<String, OwnedValue>, t, actions: &[&str]| {
            parse(7, false, "app".into(), "file:///x.png".into(), "<b>S</b>".into(), "B".into(), actions.iter().map(|s| s.to_string()).collect(), hints, t)
        };
        let n = p(&hints, -1, &["default", "", "reply", "Ответить", "x", "", "odd"]);
        assert_eq!(n.timeout, Some(DEFAULT_TIMEOUT));
        assert_eq!(n.app_icon, "/x.png");
        assert_eq!(n.summary, "S");
        assert!(n.sound.is_some(),"a built-in chime is available even without a theme");
        hints.insert("suppress-sound".into(),Value::Bool(true).try_into().unwrap());
        assert!(p(&hints,-1,&[]).sound.is_none());
        hints.remove("suppress-sound");
        assert_eq!(n.actions, vec![("default".into(), "".into()), ("reply".into(), "Ответить".into())]);
        assert_eq!(p(&hints, 0, &[]).timeout, None);
        assert_eq!(p(&hints, 10, &[]).timeout, Some(Duration::from_secs(1)));
        assert_eq!(p(&hints, 8000, &[]).timeout, Some(Duration::from_secs(8)));
        hints.insert("urgency".into(), Value::U8(2).try_into().unwrap());
        hints.insert("resident".into(), Value::Bool(true).try_into().unwrap());
        hints.insert("desktop-entry".into(), Value::from("org.example.App").try_into().unwrap());
        let n = p(&hints, 3000, &[]);
        assert!(n.critical && n.resident && n.timeout.is_none());
        assert_eq!(n.desktop_entry, "org.example.App");
        let many: Vec<String> = (0..20).map(|i| format!("a{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(p(&HashMap::new(), -1, &refs).actions.len() <= MAX_ACTIONS + 1);
    }

    fn wait_update(source: &channel::Channel<Update>) -> Update {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match source.try_recv() {
                Ok(u) => return u,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(e) => panic!("Notification worker closed: {e}"),
            }
            assert!(std::time::Instant::now() < deadline, "Notification update timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Real D-Bus clients on the test's own session bus (dbus-run-session).
    #[test]
    #[ignore = "requires an isolated session bus: dbus-run-session -- cargo test -- --include-ignored"]
    fn server_round_trip_on_a_real_bus_never_replaces_an_owner() {
        let (tx, rx) = async_channel::bounded(8);
        let (updates, source) = channel::sync_channel(8);
        let thread = std::thread::spawn(move || future::block_on(worker(rx, updates)).unwrap());
        future::block_on(async {
            let conn = Connection::session().await.unwrap();
            let dbus = zbus::fdo::DBusProxy::new(&conn).await.unwrap();
            let name: zbus::names::BusName = NAME.try_into().unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !dbus.name_has_owner(name.clone()).await.unwrap() {
                assert!(std::time::Instant::now() < deadline, "server never took the name");
                std::thread::sleep(Duration::from_millis(5));
            }
            let owner = dbus.get_name_owner(name.clone()).await.unwrap();
            let proxy = zbus::Proxy::new(&conn, NAME, PATH, NAME).await.unwrap();
            let caps: Vec<String> = proxy.call("GetCapabilities", &()).await.unwrap();
            assert!(caps.iter().any(|c| c == "actions") && caps.iter().any(|c| c == "body-markup"));
            let info: (String, String, String, String) = proxy.call("GetServerInformation", &()).await.unwrap();
            assert_eq!(info.3, "1.2");
            let mut signals = proxy.receive_all_signals().await.unwrap();
            let hints: HashMap<&str, Value> = HashMap::new();
            let id: u32 = proxy
                .call("Notify", &("t", 0u32, "", "Привет", "<i>тело</i>", vec!["default", "", "ok", "OK"], hints.clone(), -1i32))
                .await
                .unwrap();
            assert!(id > 0);
            let Update::Show(n) = wait_update(&source) else { panic!("expected Show") };
            assert_eq!((n.id, n.replaces, n.summary.as_str(), n.body.as_str()), (id, false, "Привет", "тело"));
            let again: u32 = proxy.call("Notify", &("t", id, "", "Снова", "", Vec::<&str>::new(), hints.clone(), -1i32)).await.unwrap();
            assert_eq!(again, id);
            let Update::Show(n) = wait_update(&source) else { panic!("expected Show") };
            assert!(n.replaces && n.summary == "Снова");
            let other: u32 = proxy.call("Notify", &("t", 0u32, "", "Ещё", "", Vec::<&str>::new(), hints, -1i32)).await.unwrap();
            assert_ne!(other, id);
            let _ = wait_update(&source);
            let _: () = proxy.call("CloseNotification", &(id,)).await.unwrap();
            assert!(matches!(wait_update(&source), Update::Close(x) if x == id));
            tx.send(Command::Invoked { id, key: "ok".into(), token: Some("tok".into()) }).await.unwrap();
            tx.send(Command::Closed(id, Reason::Closed)).await.unwrap();
            let mut got = Vec::new();
            while got.len() < 3 {
                let m = signals.next().await.unwrap();
                let member = m.header().member().unwrap().to_string();
                match member.as_str() {
                    "ActivationToken" => assert_eq!(m.body().deserialize::<(u32, String)>().unwrap(), (id, "tok".into())),
                    "ActionInvoked" => assert_eq!(m.body().deserialize::<(u32, String)>().unwrap(), (id, "ok".into())),
                    "NotificationClosed" => assert_eq!(m.body().deserialize::<(u32, u32)>().unwrap(), (id, 3)),
                    _ => continue,
                }
                got.push(member);
            }
            assert_eq!(got, ["ActivationToken", "ActionInvoked", "NotificationClosed"]);
            // A second server (another compositor, a nested run) leaves the owner alone
            let second = Connection::session().await.unwrap();
            let (_keep, orx) = async_channel::bounded(1);
            let (oupd, _osrc) = channel::sync_channel(1);
            serve(&second, orx, oupd).await.unwrap();
            assert_eq!(dbus.get_name_owner(name).await.unwrap(), owner);
        });
        drop(tx);
        thread.join().unwrap();
    }
}
