//! «Сейчас исполняется»: MPRIS2 players on the session bus (Rhythmbox, browsers, mpv…).
//! A worker thread follows their names and PropertiesChanged signals; the compositor only
//! receives snapshots of the current player and sends commands, never touching the bus.
use futures_lite::{future, StreamExt};
use smithay::reexports::calloop::{channel, LoopHandle};
use std::collections::HashMap;
use zbus::zvariant::{OwnedValue, Value};

const NAMESPACE: &str = "org.mpris.MediaPlayer2";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Player {
    /// org.mpris.MediaPlayer2.rhythmbox
    pub bus: String,
    /// «Rhythmbox»
    pub identity: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_raise: bool,
}
impl Player {
    /// Second line of the card: the artist, else the program
    pub fn subtitle(&self) -> &str {
        if self.artist.is_empty() { &self.identity } else { &self.artist }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    Pause,
    Stop,
    Next,
    Previous,
    /// Bring the player's window back (Rhythmbox hides itself when closed while playing)
    Raise,
}
impl Command {
    pub fn from_keysym(sym: u32) -> Option<Self> {
        use smithay::input::keyboard::keysyms as k;
        match sym {
            k::KEY_XF86AudioPlay => Some(Self::PlayPause),
            k::KEY_XF86AudioPause => Some(Self::Pause),
            k::KEY_XF86AudioStop => Some(Self::Stop),
            k::KEY_XF86AudioNext => Some(Self::Next),
            k::KEY_XF86AudioPrev => Some(Self::Previous),
            _ => None,
        }
    }
    fn method(self) -> (&'static str, &'static str) {
        match self {
            Self::PlayPause => (PLAYER, "PlayPause"),
            Self::Pause => (PLAYER, "Pause"),
            Self::Stop => (PLAYER, "Stop"),
            Self::Next => (PLAYER, "Next"),
            Self::Previous => (PLAYER, "Previous"),
            Self::Raise => (NAMESPACE, "Raise"),
        }
    }
}

pub struct Mpris {
    commands: async_channel::Sender<Command>,
}
impl Mpris {
    pub fn new(handle: &LoopHandle<'static, crate::FeatherCompositor>) -> Result<Self, Box<dyn std::error::Error>> {
        let (updates, source) = channel::channel::<Option<Player>>();
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(player) = event {
                if state.shell.cc.media != player {
                    state.shell.cc.media = player;
                    if state.shell.control_center_open {
                        state.shell.overlay_dirty = true;
                        state.needs_redraw = true;
                    }
                }
            }
        })?;
        let (commands, rx) = async_channel::bounded(8);
        std::thread::Builder::new()
            .name("feather-mpris".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                future::block_on(async {
                    if let Err(e) = run(rx, updates).await {
                        tracing::warn!("MPRIS: {e}");
                    }
                })
            })?;
        Ok(Self { commands })
    }
    pub fn send(&self, command: Command) {
        let _ = self.commands.try_send(command);
    }
}

struct Entry {
    /// Unique name that sends this player's signals
    owner: String,
    player: Player,
    /// Raised each time it starts playing: the newest playing one is the current
    touched: u64,
}

/// Players by well-known name
type Players = HashMap<String, Entry>;

/// The card shows the player that started playing last, else the last paused one; a stopped
/// player with nothing loaded (a browser tab without media) is not shown
fn current(players: &Players) -> Option<&Player> {
    players
        .values()
        .filter(|e| e.player.playing || !e.player.title.is_empty())
        .max_by_key(|e| (e.player.playing, e.touched, std::cmp::Reverse(e.player.bus.clone())))
        .map(|e| &e.player)
}

async fn get_all(conn: &zbus::Connection, bus: &str, interface: &str) -> zbus::Result<HashMap<String, OwnedValue>> {
    conn.call_method(Some(bus), PATH, Some(PROPERTIES), "GetAll", &(interface,)).await?.body().deserialize()
}

fn text(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    props.get(key).and_then(|v| <&str>::try_from(&**v).ok()).unwrap_or_default().to_string()
}

fn flag(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props.get(key).and_then(|v| bool::try_from(&**v).ok()).unwrap_or(false)
}

/// Title and artists from `Metadata` (a{sv}); a file without tags shows its name
fn metadata(props: &HashMap<String, OwnedValue>) -> (String, String) {
    let Some(Value::Dict(dict)) = props.get("Metadata").map(|v| &**v) else { return Default::default() };
    let (mut title, mut artist, mut url) = (String::new(), String::new(), String::new());
    for (key, value) in dict.iter() {
        let value = match value {
            Value::Value(inner) => &**inner,
            v => v,
        };
        match <&str>::try_from(key) {
            Ok("xesam:title") => title = <&str>::try_from(value).unwrap_or_default().to_string(),
            Ok("xesam:url") => url = <&str>::try_from(value).unwrap_or_default().to_string(),
            Ok("xesam:artist") => {
                if let Value::Array(names) = value {
                    artist = names.iter().filter_map(|n| <&str>::try_from(n).ok()).collect::<Vec<_>>().join(", ");
                }
            }
            _ => {}
        }
    }
    if title.trim().is_empty() {
        title = url.rsplit('/').next().map(decode_url).unwrap_or_default();
    }
    (title, artist)
}

/// %D0%9F… → «П…» for file names from xesam:url
fn decode_url(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) if bytes[i] == b'%' => {
                out.push(byte);
                i += 3;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn fetch(conn: &zbus::Connection, bus: &str, identity: Option<(String, bool)>) -> Option<Player> {
    let props = get_all(conn, bus, PLAYER).await.ok()?;
    let (identity, can_raise) = match identity {
        Some(known) => known,
        None => {
            let root = get_all(conn, bus, NAMESPACE).await.unwrap_or_default();
            let name = text(&root, "Identity");
            let short = bus.strip_prefix("org.mpris.MediaPlayer2.").unwrap_or(bus);
            (if name.is_empty() { short.split('.').next().unwrap_or(short).to_string() } else { name }, flag(&root, "CanRaise"))
        }
    };
    let (title, artist) = metadata(&props);
    Some(Player {
        bus: bus.to_string(),
        identity,
        title,
        artist,
        playing: text(&props, "PlaybackStatus") == "Playing",
        can_next: flag(&props, "CanGoNext"),
        can_previous: flag(&props, "CanGoPrevious"),
        can_raise,
    })
}

async fn add(conn: &zbus::Connection, players: &mut Players, clock: &mut u64, bus: String, owner: String) {
    let known = players.get(&bus).map(|e| (e.player.identity.clone(), e.player.can_raise));
    let Some(player) = fetch(conn, &bus, known).await else {
        players.remove(&bus);
        return;
    };
    let was_playing = players.get(&bus).is_some_and(|e| e.player.playing);
    let touched = if player.playing && !was_playing {
        *clock += 1;
        *clock
    } else {
        players.get(&bus).map_or(0, |e| e.touched)
    };
    players.insert(bus, Entry { owner, player, touched });
}

async fn run(commands: async_channel::Receiver<Command>, updates: channel::Sender<Option<Player>>) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let owners_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg0ns(NAMESPACE)?
        .build();
    let mut owners = zbus::MessageStream::for_match_rule(owners_rule, &conn, Some(16)).await?;
    let props_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(PROPERTIES)?
        .member("PropertiesChanged")?
        .path(PATH)?
        .build();
    let mut changes = zbus::MessageStream::for_match_rule(props_rule, &conn, Some(64)).await?;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let mut players = Players::new();
    let mut clock = 0;
    for name in dbus.list_names().await? {
        let bus = name.to_string();
        if bus.starts_with("org.mpris.MediaPlayer2.") {
            if let Ok(owner) = dbus.get_name_owner(name.as_ref()).await {
                add(&conn, &mut players, &mut clock, bus, owner.to_string()).await;
            }
        }
    }
    let mut sent = current(&players).cloned();
    if updates.send(sent.clone()).is_err() {
        return Ok(());
    }
    enum Event {
        Command(Result<Command, async_channel::RecvError>),
        Owner(Option<zbus::Result<zbus::Message>>),
        Change(Option<zbus::Result<zbus::Message>>),
    }
    loop {
        let event = future::race(
            async { Event::Command(commands.recv().await) },
            future::race(async { Event::Owner(owners.next().await) }, async { Event::Change(changes.next().await) }),
        )
        .await;
        match event {
            Event::Command(Err(_)) | Event::Owner(None) | Event::Change(None) => return Ok(()),
            Event::Command(Ok(command)) => {
                if let Some(bus) = current(&players).map(|p| p.bus.clone()) {
                    let (interface, method) = command.method();
                    if let Err(e) = conn.call_method(Some(bus.as_str()), PATH, Some(interface), method, &()).await {
                        tracing::warn!("MPRIS {bus} {method}: {e}");
                    }
                }
            }
            Event::Owner(Some(Ok(message))) => {
                let Ok((name, _, new)) = message.body().deserialize::<(String, String, String)>() else { continue };
                if !name.starts_with("org.mpris.MediaPlayer2.") {
                    continue;
                }
                if new.is_empty() {
                    players.remove(&name);
                } else {
                    add(&conn, &mut players, &mut clock, name, new).await;
                }
            }
            Event::Change(Some(Ok(message))) => {
                let header = message.header();
                let Some(sender) = header.sender().map(|s| s.to_string()) else { continue };
                let Ok((interface, _, _)) = message.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() else { continue };
                if interface != PLAYER {
                    continue;
                }
                let found = players.iter().find(|(_, e)| e.owner == sender).map(|(bus, _)| bus.clone());
                if let Some(bus) = found {
                    add(&conn, &mut players, &mut clock, bus, sender).await;
                }
            }
            Event::Owner(Some(Err(_))) | Event::Change(Some(Err(_))) => {}
        }
        let now = current(&players).cloned();
        if now != sent {
            sent = now;
            if updates.send(sent.clone()).is_err() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(bus: &str, title: &str, playing: bool, touched: u64) -> (String, Entry) {
        let player = Player { bus: bus.into(), title: title.into(), playing, ..Default::default() };
        (bus.into(), Entry { owner: String::new(), player, touched })
    }

    #[test]
    fn current_player_is_the_newest_playing_then_a_paused_one() {
        let mut players: Players = [entry("a", "Песня", false, 1), entry("b", "", false, 5)].into_iter().collect();
        // A browser without media is not a player to show
        assert_eq!(current(&players).map(|p| p.bus.as_str()), Some("a"));
        players.extend([entry("c", "Видео", true, 2), entry("d", "Подкаст", true, 3)]);
        assert_eq!(current(&players).map(|p| p.bus.as_str()), Some("d"));
        players.clear();
        players.extend([entry("b", "", false, 5)]);
        assert!(current(&players).is_none());
    }

    #[test]
    fn metadata_title_artist_and_untagged_file_name() {
        let meta = |pairs: Vec<(&str, Value<'static>)>| {
            let mut dict = zbus::zvariant::Dict::new(&zbus::zvariant::Signature::Str, &zbus::zvariant::Signature::Variant);
            for (k, v) in pairs {
                dict.append(Value::from(k), Value::Value(Box::new(v))).unwrap();
            }
            HashMap::from([("Metadata".to_string(), OwnedValue::try_from(Value::Dict(dict)).unwrap())])
        };
        let props = meta(vec![("xesam:title", Value::from("Тяжёлые времена")), ("xesam:artist", Value::from(vec!["Сифей", "Гость"]))]);
        assert_eq!(metadata(&props), ("Тяжёлые времена".into(), "Сифей, Гость".into()));
        let props = meta(vec![("xesam:url", Value::from("file:///home/u/%D0%A0%D0%B0%D0%B1%D0%BE%D1%87%D0%B8%D0%B9/track%201.mp3"))]);
        assert_eq!(metadata(&props), ("track 1.mp3".into(), String::new()));
        assert_eq!(Command::from_keysym(smithay::input::keyboard::keysyms::KEY_XF86AudioPlay), Some(Command::PlayPause));
    }
}
