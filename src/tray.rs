//! StatusNotifierWatcher + host. All D-Bus calls and icon conversion run on a worker.
use crate::{appmenu::MenuNode, FeatherCompositor};
use futures_lite::{future, StreamExt};
use smithay::reexports::calloop::{channel, LoopHandle};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};
use zbus::{
    object_server::SignalEmitter,
    proxy::CacheProperties,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
    Connection, Proxy,
};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const LIMIT: usize = 64;

#[derive(Clone, Debug)]
pub struct Item {
    pub key: String,
    pub title: String,
    pub icon_name: String,
    pub pixels: Option<Arc<tiny_skia::Pixmap>>,
    pub menu_only: bool,
    pub attention: bool,
}
impl PartialEq for Item {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key
            && self.title == o.title
            && self.icon_name == o.icon_name
            && self.menu_only == o.menu_only
            && self.attention == o.attention
            && self.pixels.as_ref().map(|p| p.data()) == o.pixels.as_ref().map(|p| p.data())
    }
}
#[derive(Clone, Debug)]
struct Entry {
    key: String,
    owner: String,
    path: String,
    menu: Option<String>,
    item: Option<Item>,
}
type Registry = Arc<Mutex<BTreeMap<String, Entry>>>;
struct Watcher {
    registry: Registry,
    changed: async_channel::Sender<()>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Missing sender".into()))?
            .to_string();
        let (service, path) = if service.starts_with('/') {
            (sender.clone(), service)
        } else {
            (service, "/StatusNotifierItem".into())
        };
        zbus::names::BusName::try_from(service.as_str())
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        OwnedObjectPath::try_from(path.as_str())
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let owner = owner(conn, &service)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        // KDE exports each item on its own connection, but registers it through
        // the application's shared connection. Validate the process, not just
        // the unique bus name, before accepting that standard pattern.
        if owner != sender && !same_process(conn, &owner, &sender).await.unwrap_or(false) {
            return Err(zbus::fdo::Error::AccessDenied(
                "Item service belongs to another client".into(),
            ));
        }
        let key = format!("{service}{path}");
        tracing::debug!("Tray registration: {key}");
        let inserted = {
            let mut registry = self.registry.lock().unwrap();
            if registry.len() >= LIMIT && !registry.contains_key(&key) {
                return Err(zbus::fdo::Error::LimitsExceeded(
                    "Too many tray items".into(),
                ));
            }
            registry
                .insert(
                    key.clone(),
                    Entry {
                        key: key.clone(),
                        owner,
                        path,
                        menu: None,
                        item: None,
                    },
                )
                .is_none()
        };
        let _ = self.changed.try_send(());
        if inserted {
            Self::status_notifier_item_registered(&emitter, &key)
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }
        Ok(())
    }
    async fn register_status_notifier_host(
        &self,
        _service: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        Self::status_notifier_host_registered(&emitter)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.registry.lock().unwrap().keys().cloned().collect()
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_unregistered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[derive(Clone, Debug)]
pub enum Action {
    Activate,
    Secondary,
    Context,
    Scroll(i32, bool),
}
enum Command {
    Interact {
        key: String,
        action: Action,
        x: i32,
        y: i32,
        generation: u64,
        token: Option<String>,
    },
    Show {
        generation: u64,
        id: i32,
    },
    MenuClick {
        generation: u64,
        id: i32,
        time: u32,
    },
    Close,
}
enum Update {
    Items(Vec<Item>),
    Menu {
        generation: u64,
        key: String,
        root: Option<Arc<MenuNode>>,
    },
}
pub struct Tray {
    tx: async_channel::Sender<Command>,
    pub generation: u64,
    menu: bool,
    opened: Vec<i32>,
}
impl Tray {
    pub fn new(
        handle: &LoopHandle<'static, FeatherCompositor>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, rx) = async_channel::bounded(64);
        let (updates, source) = channel::sync_channel(4);
        handle.insert_source(source, |event, _, state| match event {
            channel::Event::Msg(Update::Items(items)) => {
                state.shell.set_tray_items(items);
                state.needs_redraw = true;
            }
            channel::Event::Msg(Update::Menu {
                generation,
                key,
                root,
            }) => {
                if state
                    .tray
                    .as_ref()
                    .is_some_and(|t| t.generation == generation)
                    && state.shell.install_tray_menu(&key, generation, root)
                {
                    state.needs_redraw = true;
                }
            }
            channel::Event::Closed => {
                state.shell.set_tray_items(Vec::new());
                state.needs_redraw = true;
            }
        })?;
        std::thread::Builder::new()
            .name("feather-tray".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                future::block_on(async move {
                    if let Err(e) = worker(rx, updates).await {
                        tracing::warn!("Трей: {e}");
                    }
                })
            })?;
        Ok(Self {
            tx,
            generation: 0,
            menu: false,
            opened: Vec::new(),
        })
    }
    pub fn interact(
        &mut self,
        key: String,
        action: Action,
        x: i32,
        y: i32,
        token: Option<String>,
    ) -> u64 {
        if matches!(action, Action::Context) {
            self.generation = self.generation.wrapping_add(1);
            self.menu = true;
            self.opened.clear();
        }
        let _ = self.tx.try_send(Command::Interact {
            key,
            action,
            x,
            y,
            generation: self.generation,
            token,
        });
        self.generation
    }
    pub fn click(&self, generation: u64, id: i32, time: u32) {
        let _ = self.tx.try_send(Command::MenuClick {
            generation,
            id,
            time,
        });
    }
    pub fn sync_menu(&mut self, ids: Vec<i32>) {
        if self.menu && ids.is_empty() {
            self.menu = false;
            self.generation = self.generation.wrapping_add(1);
            let _ = self.tx.try_send(Command::Close);
        }
        for id in ids.iter().filter(|id| !self.opened.contains(id)) {
            let _ = self.tx.try_send(Command::Show {
                generation: self.generation,
                id: *id,
            });
        }
        self.opened = ids;
    }
}

async fn owner(conn: &Connection, service: &str) -> zbus::Result<String> {
    let proxy = Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await?;
    proxy.call("GetNameOwner", &(service,)).await
}

async fn same_process(conn: &Connection, a: &str, b: &str) -> zbus::Result<bool> {
    let p = Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await?;
    let a: u32 = p.call("GetConnectionUnixProcessID", &(a,)).await?;
    let b: u32 = p.call("GetConnectionUnixProcessID", &(b,)).await?;
    Ok(a != 0 && a == b)
}
async fn proxy(
    conn: &Connection,
    entry: &Entry,
    interface: &str,
    path: &str,
) -> zbus::Result<Proxy<'static>> {
    Proxy::new_owned(
        conn.clone(),
        entry.owner.clone(),
        path.to_string(),
        interface.to_string(),
    )
    .await
}
fn text(props: &HashMap<String, OwnedValue>, name: &str) -> String {
    props
        .get(name)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("")
        .chars()
        .take(512)
        .collect()
}
fn bool_prop(props: &HashMap<String, OwnedValue>, name: &str) -> bool {
    props
        .get(name)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn pixmap(value: &OwnedValue) -> Option<Arc<tiny_skia::Pixmap>> {
    let images = Vec::<(i32, i32, Vec<u8>)>::try_from(value.try_clone().ok()?).ok()?;
    let (w, h, data) = images
        .into_iter()
        .filter(|(w, h, b)| {
            *w > 0 && *h > 0 && *w <= 512 && *h <= 512 && b.len() == *w as usize * *h as usize * 4
        })
        .min_by_key(|(w, h, _)| (w - 40).abs() + (h - 40).abs())?;
    let mut image = tiny_skia::Pixmap::new(w as u32, h as u32)?;
    for (argb, rgba) in data
        .chunks_exact(4)
        .zip(image.data_mut().chunks_exact_mut(4))
    {
        let a = argb[0] as u16;
        rgba.copy_from_slice(&[
            ((argb[1] as u16 * a + 127) / 255) as u8,
            ((argb[2] as u16 * a + 127) / 255) as u8,
            ((argb[3] as u16 * a + 127) / 255) as u8,
            argb[0],
        ]);
    }
    let mut small = tiny_skia::Pixmap::new(40, 40)?;
    small.draw_pixmap(
        0,
        0,
        image.as_ref(),
        &tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        },
        tiny_skia::Transform::from_scale(40.0 / w as f32, 40.0 / h as f32),
        None,
    );
    Some(Arc::new(small))
}
async fn load_item(conn: &Connection, entry: &mut Entry) -> zbus::Result<()> {
    let p = proxy(conn, entry, "org.freedesktop.DBus.Properties", &entry.path).await?;
    let props: HashMap<String, OwnedValue> = p.call("GetAll", &(ITEM,)).await?;
    let status = text(&props, "Status");
    entry.menu = props
        .get("Menu")
        .and_then(|v| OwnedObjectPath::try_from(v.try_clone().ok()?).ok())
        .map(|p| p.to_string())
        .filter(|p| p != "/");
    if status == "Passive" {
        entry.item = None;
        return Ok(());
    }
    let attention = status == "NeedsAttention";
    let mut icon_name = text(
        &props,
        if attention {
            "AttentionIconName"
        } else {
            "IconName"
        },
    );
    let mut pixels = props
        .get(if attention {
            "AttentionIconPixmap"
        } else {
            "IconPixmap"
        })
        .and_then(pixmap);
    if attention && icon_name.is_empty() && pixels.is_none() {
        icon_name = text(&props, "IconName");
        pixels = props.get("IconPixmap").and_then(pixmap);
    }
    if !icon_name.is_empty() {
        let theme = text(&props, "IconThemePath");
        if !theme.is_empty() && !icon_name.contains('/') {
            for ext in ["svg", "png"] {
                let path = std::path::Path::new(&theme).join(format!("{icon_name}.{ext}"));
                if path.is_file() {
                    icon_name = path.to_string_lossy().into_owned();
                    break;
                }
            }
        }
    }
    entry.item = Some(Item {
        key: entry.key.clone(),
        title: text(&props, "Title"),
        icon_name,
        pixels,
        menu_only: bool_prop(&props, "ItemIsMenu"),
        attention,
    });
    Ok(())
}
async fn subscribe(conn: &Connection, interface: &str) -> zbus::Result<zbus::MessageStream> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(interface)?
        .build();
    zbus::MessageStream::for_match_rule(rule, conn, Some(64)).await
}

fn lost_entry(entry: &Entry, name: &str, old: &str) -> bool {
    entry.owner == old
        && (name.starts_with(':')
            || entry
                .key
                .strip_prefix(name)
                .is_some_and(|p| p.starts_with('/')))
}

async fn worker(
    commands: async_channel::Receiver<Command>,
    updates: channel::SyncSender<Update>,
) -> zbus::Result<()> {
    let conn = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(2))
        .build()
        .await?;
    // A full signal stream blocks zbus's socket reader, including RPC replies
    // and AddMatch/RemoveMatch. Keep it off the connection used for GetAll,
    // actions and the watcher object while this worker awaits those calls.
    let signals = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(2))
        .build()
        .await?;
    let registry: Registry = Arc::new(Mutex::new(BTreeMap::new()));
    let (changed, changes) = async_channel::bounded(1);
    conn.object_server()
        .at(
            PATH,
            Watcher {
                registry: registry.clone(),
                changed,
            },
        )
        .await?;
    let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    conn.request_name(host.as_str()).await?;
    let _ = conn
        .request_name_with_flags(WATCHER, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await;
    let watcher: Proxy<'_> = zbus::proxy::Builder::new(&conn)
        .destination(WATCHER)?
        .path(PATH)?
        .interface(WATCHER)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let mut watcher_signals = subscribe(&signals, WATCHER).await?;
    let mut item_signals = subscribe(&signals, ITEM).await?;
    let properties_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .arg(0, ITEM)?
        .build();
    let mut properties =
        zbus::MessageStream::for_match_rule(properties_rule, &signals, Some(64)).await?;
    let owner_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .build();
    let mut owners = zbus::MessageStream::for_match_rule(owner_rule, &signals, Some(64)).await?;
    let _: zbus::Result<()> = watcher
        .call("RegisterStatusNotifierHost", &(host.as_str(),))
        .await;
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let mut rescan = true;
    let mut menu: Option<(String, u64, Arc<MenuNode>)> = None;
    let menu_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("com.canonical.dbusmenu")?
        .build();
    let mut menu_signals =
        zbus::MessageStream::for_match_rule(menu_rule, &signals, Some(32)).await?;
    loop {
        if rescan {
            let own = owner(&conn, WATCHER).await.ok().as_deref()
                == conn.unique_name().map(|n| n.as_str());
            let mut next = if own {
                registry.lock().unwrap().clone()
            } else {
                let mut next = BTreeMap::new();
                if let Ok(items) = watcher
                    .get_property::<Vec<String>>("RegisteredStatusNotifierItems")
                    .await
                {
                    for key in items.into_iter().take(LIMIT) {
                        let (service, path) = key
                            .find('/')
                            .map_or((key.as_str(), "/StatusNotifierItem"), |i| {
                                (&key[..i], &key[i..])
                            });
                        if let Ok(owner) = owner(&conn, service).await {
                            next.insert(
                                key.clone(),
                                Entry {
                                    key: key.clone(),
                                    owner,
                                    path: path.into(),
                                    menu: None,
                                    item: None,
                                },
                            );
                        }
                    }
                }
                next
            };
            for entry in next.values_mut() {
                if let Some(previous) = entries
                    .get(&entry.key)
                    .filter(|previous| previous.owner == entry.owner && previous.path == entry.path)
                {
                    entry.item = previous.item.clone();
                    entry.menu = previous.menu.clone();
                }
                if let Err(err) = load_item(&conn, entry).await {
                    tracing::debug!("Tray item {}: {err}", entry.key);
                    // A failed refresh is not an unregistration or Passive.
                    // Preserve the last successful snapshot for the same owner.
                }
            }
            entries = next;
            let _ = updates.send(Update::Items(
                entries.values().filter_map(|e| e.item.clone()).collect(),
            ));
            rescan = false;
        }
        enum Wake {
            Command(Result<Command, async_channel::RecvError>),
            Changed,
            Signal(Option<zbus::Result<zbus::Message>>),
            Watcher(Option<zbus::Result<zbus::Message>>),
            Owner(Option<zbus::Result<zbus::Message>>),
            Menu(Option<zbus::Result<zbus::Message>>),
        }
        let wake = future::or(
            async { Wake::Command(commands.recv().await) },
            future::or(
                async {
                    let _ = changes.recv().await;
                    Wake::Changed
                },
                future::or(
                    async { Wake::Signal(item_signals.next().await) },
                    future::or(
                        async { Wake::Signal(properties.next().await) },
                        future::or(
                            async { Wake::Watcher(watcher_signals.next().await) },
                            future::or(async { Wake::Owner(owners.next().await) }, async {
                                Wake::Menu(menu_signals.next().await)
                            }),
                        ),
                    ),
                ),
            ),
        )
        .await;
        match wake {
            Wake::Command(Err(_)) => return Ok(()),
            Wake::Changed | Wake::Watcher(Some(Ok(_))) => rescan = true,
            Wake::Signal(Some(Ok(message))) => {
                let header = message.header();
                let sender = header.sender().map(|s| s.as_str());
                let path = header.path().map(|p| p.as_str());
                if let Some(entry) = entries
                    .values_mut()
                    .find(|e| Some(e.owner.as_str()) == sender && Some(e.path.as_str()) == path)
                {
                    let _ = load_item(&conn, entry).await;
                    let _ = updates.send(Update::Items(
                        entries.values().filter_map(|e| e.item.clone()).collect(),
                    ));
                }
            }
            Wake::Owner(Some(Ok(message))) => {
                if let Ok((name, old, new)) =
                    message.body().deserialize::<(String, String, String)>()
                {
                    if name == WATCHER {
                        if new.is_empty() {
                            let _ = conn
                                .request_name_with_flags(
                                    WATCHER,
                                    zbus::fdo::RequestNameFlags::DoNotQueue.into(),
                                )
                                .await;
                        }
                        let _: zbus::Result<()> = watcher
                            .call("RegisterStatusNotifierHost", &(host.as_str(),))
                            .await;
                        rescan = true;
                    }
                    if new.is_empty() && !old.is_empty() {
                        let removed: Vec<_> = {
                            let mut r = registry.lock().unwrap();
                            let keys: Vec<_> = r
                                .values()
                                .filter(|e| lost_entry(e, &name, &old))
                                .map(|e| e.key.clone())
                                .collect();
                            for k in &keys {
                                r.remove(k);
                            }
                            keys
                        };
                        for key in removed {
                            let emitter = SignalEmitter::new(&conn, PATH)?;
                            let _ =
                                Watcher::status_notifier_item_unregistered(&emitter, &key).await;
                        }
                        if entries.values().any(|e| lost_entry(e, &name, &old)) {
                            rescan = true;
                        }
                    }
                }
            }
            Wake::Command(Ok(Command::Interact {
                key,
                action,
                x,
                y,
                generation,
                token,
            })) => {
                let Some(entry) = entries.get(&key) else {
                    continue;
                };
                let p = proxy(&conn, entry, ITEM, &entry.path).await?;
                if let Some(token) = token {
                    let _: zbus::Result<()> = p
                        .call("ProvideXdgActivationToken", &(token.as_str(),))
                        .await;
                }
                match action {
                    Action::Activate => {
                        let _: zbus::Result<()> = p.call("Activate", &(x, y)).await;
                    }
                    Action::Secondary => {
                        let _: zbus::Result<()> = p.call("SecondaryActivate", &(x, y)).await;
                    }
                    Action::Scroll(delta, horizontal) => {
                        let _: zbus::Result<()> = p
                            .call(
                                "Scroll",
                                &(delta, if horizontal { "horizontal" } else { "vertical" }),
                            )
                            .await;
                    }
                    Action::Context => {
                        if let Some(path) = &entry.menu {
                            let p = proxy(&conn, entry, "com.canonical.dbusmenu", path).await?;
                            let _: zbus::Result<bool> = p.call("AboutToShow", &(0i32,)).await;
                            let root = crate::appmenu::load(&p).await.ok();
                            menu = root.clone().map(|r| (key.clone(), generation, r));
                            let _ = updates.send(Update::Menu {
                                generation,
                                key,
                                root,
                            });
                        } else {
                            let _: zbus::Result<()> = p.call("ContextMenu", &(x, y)).await;
                            let _ = updates.send(Update::Menu {
                                generation,
                                key,
                                root: None,
                            });
                        }
                    }
                }
            }
            Wake::Command(Ok(Command::Show { generation, id })) => {
                if let Some((key, g, _)) = &menu {
                    if *g == generation {
                        if let Some(entry) = entries.get(key) {
                            if let Some(path) = &entry.menu {
                                let p = proxy(&conn, entry, "com.canonical.dbusmenu", path).await?;
                                let _: zbus::Result<bool> = p.call("AboutToShow", &(id,)).await;
                                if let Ok(root) = crate::appmenu::load(&p).await {
                                    let _ = updates.send(Update::Menu {
                                        generation,
                                        key: key.clone(),
                                        root: Some(root.clone()),
                                    });
                                    menu = Some((key.clone(), generation, root));
                                }
                            }
                        }
                    }
                }
            }
            Wake::Command(Ok(Command::MenuClick {
                generation,
                id,
                time,
            })) => {
                if let Some((key, g, root)) = &menu {
                    if *g == generation
                        && root
                            .find(id)
                            .is_some_and(|n| n.enabled && n.visible && !n.separator && !n.submenu)
                    {
                        if let Some(entry) = entries.get(key) {
                            if let Some(path) = &entry.menu {
                                let p = proxy(&conn, entry, "com.canonical.dbusmenu", path).await?;
                                let _: zbus::Result<()> = p
                                    .call("Event", &(id, "clicked", Value::from(0i32), time))
                                    .await;
                            }
                        }
                    }
                }
            }
            Wake::Command(Ok(Command::Close)) => {
                menu = None;
            }
            Wake::Menu(Some(Ok(message))) => {
                if let Some((key, g, _)) = &menu {
                    if let Some(entry) = entries.get(key) {
                        let header = message.header();
                        if header.sender().is_some_and(|s| s.as_str() == entry.owner)
                            && header
                                .path()
                                .is_some_and(|p| Some(p.as_str()) == entry.menu.as_deref())
                        {
                            let p = proxy(
                                &conn,
                                entry,
                                "com.canonical.dbusmenu",
                                entry.menu.as_deref().unwrap(),
                            )
                            .await?;
                            if let Ok(root) = crate::appmenu::load(&p).await {
                                let _ = updates.send(Update::Menu {
                                    generation: *g,
                                    key: key.clone(),
                                    root: Some(root.clone()),
                                });
                                menu = Some((key.clone(), *g, root));
                            }
                        }
                    }
                }
            }
            Wake::Signal(None) | Wake::Owner(None) | Wake::Menu(None) | Wake::Watcher(None) => {
                return Ok(())
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use zbus::zvariant::Str;
    type WireNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
    fn wire(id: i32, label: &str, children: Vec<WireNode>) -> WireNode {
        (
            id,
            HashMap::from([("label".into(), OwnedValue::from(Str::from(label)))]),
            children
                .into_iter()
                .map(|n| OwnedValue::try_from(Value::from(n)).unwrap())
                .collect(),
        )
    }
    #[test]
    fn validates_network_argb_pixmaps_and_tracks_the_registered_service_only() {
        let value = OwnedValue::try_from(Value::from(vec![(1i32, 1i32, vec![128u8, 255, 0, 100])]))
            .unwrap();
        let image = pixmap(&value).unwrap();
        assert_eq!(&image.data()[..4], &[128, 0, 50, 128]);
        assert_eq!(image.width(), 40);
        for (w, h, data) in [(0, 2, vec![]), (513, 1, vec![]), (2, 2, vec![0; 15])] {
            assert!(
                pixmap(&OwnedValue::try_from(Value::from(vec![(w, h, data)])).unwrap()).is_none()
            );
        }
        let e = Entry {
            key: "org.example.Tray/StatusNotifierItem".into(),
            owner: ":1.5".into(),
            path: "/StatusNotifierItem".into(),
            menu: None,
            item: None,
        };
        assert!(lost_entry(&e, ":1.5", ":1.5"));
        assert!(lost_entry(&e, "org.example.Tray", ":1.5"));
        assert!(!lost_entry(&e, "org.example.Other", ":1.5"));
        assert!(!lost_entry(&e, "org.example.Tray", ":1.6"));
    }
    struct TestItem {
        passive: Arc<AtomicBool>,
        calls: Arc<Mutex<Vec<String>>>,
    }
    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl TestItem {
        #[zbus(property)]
        fn title(&self) -> &str {
            "Tray fixture"
        }
        #[zbus(property)]
        fn status(&self) -> &str {
            if self.passive.load(Ordering::Relaxed) {
                "Passive"
            } else {
                "Active"
            }
        }
        #[zbus(property)]
        fn icon_name(&self) -> &str {
            ""
        }
        #[zbus(property)]
        fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
            vec![(1, 1, vec![255, 10, 20, 30])]
        }
        #[zbus(property)]
        fn menu(&self) -> OwnedObjectPath {
            OwnedObjectPath::try_from("/Menu").unwrap()
        }
        #[zbus(property)]
        fn item_is_menu(&self) -> bool {
            false
        }
        fn provide_xdg_activation_token(&self, token: &str) {
            self.calls.lock().unwrap().push(format!("token:{token}"));
        }
        fn activate(&self, _x: i32, _y: i32) {
            self.calls.lock().unwrap().push("activate".into());
        }
        fn secondary_activate(&self, _x: i32, _y: i32) {
            self.calls.lock().unwrap().push("secondary".into());
        }
        fn scroll(&self, delta: i32, orientation: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("scroll:{delta}:{orientation}"));
        }
        #[zbus(signal)]
        async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
    }
    struct TestMenu {
        calls: Arc<Mutex<Vec<String>>>,
        label: Arc<Mutex<String>>,
    }
    #[zbus::interface(name = "com.canonical.dbusmenu")]
    impl TestMenu {
        fn get_layout(
            &self,
            _parent_id: i32,
            _recursion_depth: i32,
            _property_names: Vec<String>,
        ) -> (u32, WireNode) {
            let mut checked = wire(1, &self.label.lock().unwrap(), vec![]);
            checked
                .1
                .insert("toggle-state".into(), OwnedValue::from(1i32));
            checked.1.insert(
                "toggle-type".into(),
                OwnedValue::from(Str::from("checkmark")),
            );
            let mut disabled = wire(2, "Disabled", vec![]);
            disabled.1.insert("enabled".into(), OwnedValue::from(false));
            (
                1,
                wire(
                    0,
                    "",
                    vec![
                        checked,
                        disabled,
                        wire(3, "Submenu", vec![wire(4, "Child", vec![])]),
                    ],
                ),
            )
        }
        fn about_to_show(&self, id: i32) -> bool {
            self.calls.lock().unwrap().push(format!("show:{id}"));
            true
        }
        fn event(&self, id: i32, event_id: &str, _data: OwnedValue, _timestamp: u32) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("event:{id}:{event_id}"));
        }
        #[zbus(signal)]
        async fn layout_updated(
            emitter: &SignalEmitter<'_>,
            revision: u32,
            parent: i32,
        ) -> zbus::Result<()>;
    }
    fn wait_update(source: &channel::Channel<Update>, mut predicate: impl FnMut(Update) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match source.try_recv() {
                Ok(u) => {
                    if predicate(u) {
                        return;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(e) => panic!("Tray worker closed: {e}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Tray update timed out"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    struct BusyItem {
        burst: Arc<AtomicBool>,
        refreshed: AtomicBool,
    }
    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl BusyItem {
        #[zbus(property)]
        async fn title(&self, #[zbus(connection)] conn: &Connection) -> String {
            if self.burst.swap(false, Ordering::Relaxed) {
                // More than the tray's bounded NameOwnerChanged queue, while
                // GetAll is pending. None of these names belongs to a tray item.
                for _ in 0..80 {
                    conn.request_name("org.example.TrayBurst").await.unwrap();
                    conn.release_name("org.example.TrayBurst").await.unwrap();
                }
                self.refreshed.store(true, Ordering::Relaxed);
            }
            if self.refreshed.load(Ordering::Relaxed) {
                "Refreshed tray fixture".into()
            } else {
                "Busy tray fixture".into()
            }
        }
        #[zbus(property)]
        fn status(&self) -> &str {
            "Active"
        }
    }

    #[test]
    #[ignore = "requires an isolated session bus"]
    fn isolated_signal_burst_during_get_all_keeps_the_tray_visible() {
        let burst = Arc::new(AtomicBool::new(false));
        let client = future::block_on(
            zbus::connection::Builder::session()
                .unwrap()
                .serve_at(
                    "/Item",
                    BusyItem {
                        burst: burst.clone(),
                        refreshed: AtomicBool::new(false),
                    },
                )
                .unwrap()
                .build(),
        )
        .unwrap();
        let (tx, rx) = async_channel::bounded(64);
        let (updates, source) = channel::sync_channel(4);
        let thread = std::thread::spawn(move || future::block_on(worker(rx, updates)).unwrap());
        wait_update(&source, |u| matches!(u, Update::Items(v) if v.is_empty()));
        let watcher = future::block_on(Proxy::new(&client, WATCHER, PATH, WATCHER)).unwrap();
        future::block_on(watcher.call::<_, _, ()>("RegisterStatusNotifierItem", &("/Item",)))
            .unwrap();
        wait_update(&source, |u| matches!(u, Update::Items(v) if v.len() == 1));
        burst.store(true, Ordering::Relaxed);
        future::block_on(watcher.call::<_, _, ()>(
            "RegisterStatusNotifierHost",
            &(client.unique_name().unwrap().as_str(),),
        ))
        .unwrap();
        wait_update(&source, |u| {
            if let Update::Items(items) = u {
                assert_eq!(
                    items.len(),
                    1,
                    "signal burst made an active tray item disappear"
                );
                items[0].title == "Refreshed tray fixture"
            } else {
                false
            }
        });
        // A second round also checks that the reader recovered and still serves registrations.
        future::block_on(watcher.call::<_, _, ()>(
            "RegisterStatusNotifierHost",
            &(client.unique_name().unwrap().as_str(),),
        ))
        .unwrap();
        wait_update(&source, |u| matches!(u, Update::Items(v) if v.len() == 1));
        drop(tx);
        drop(source);
        thread.join().unwrap();
    }
    #[test]
    #[ignore = "requires an isolated session bus: dbus-run-session -- cargo test -- --include-ignored"]
    fn isolated_worker_respects_existing_watcher_loads_items_menus_actions_and_removal() {
        let passive = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let label = Arc::new(Mutex::new("_Run".into()));
        let (changed, _) = async_channel::bounded(1);
        let server = future::block_on(
            zbus::connection::Builder::session()
                .unwrap()
                .serve_at(
                    PATH,
                    Watcher {
                        registry: Arc::new(Mutex::new(BTreeMap::new())),
                        changed,
                    },
                )
                .unwrap()
                .serve_at(
                    "/Item",
                    TestItem {
                        passive: passive.clone(),
                        calls: calls.clone(),
                    },
                )
                .unwrap()
                .serve_at(
                    "/Menu",
                    TestMenu {
                        calls: calls.clone(),
                        label: label.clone(),
                    },
                )
                .unwrap()
                .build(),
        )
        .unwrap();
        future::block_on(server.request_name_with_flags(
            WATCHER,
            zbus::fdo::RequestNameFlags::AllowReplacement.into(),
        ))
        .unwrap();
        let server_name = server.unique_name().unwrap().to_string();
        let (tx, rx) = async_channel::bounded(64);
        let (updates, source) = channel::sync_channel(4);
        let thread = std::thread::spawn(move || future::block_on(worker(rx, updates)).unwrap());
        wait_update(&source, |u| matches!(u,Update::Items(v)if v.is_empty()));
        assert_eq!(
            future::block_on(owner(&server, WATCHER)).unwrap(),
            server_name,
            "replaced the existing watcher"
        );
        future::block_on(server.release_name(WATCHER)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while future::block_on(owner(&server, WATCHER))
            .ok()
            .as_deref()
            .is_none_or(|n| n == server_name)
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        future::block_on(async {
            let p = Proxy::new(&server, WATCHER, PATH, WATCHER).await.unwrap();
            p.call::<_, _, ()>("RegisterStatusNotifierItem", &("/Item",))
                .await
                .unwrap();
        });
        let key = format!("{server_name}/Item");
        wait_update(&source, |u| {
            if let Update::Items(items) = u {
                if let Some(i) = items.first() {
                    assert_eq!(i.key, key);
                    assert_eq!(&i.pixels.as_ref().unwrap().data()[..4], &[10, 20, 30, 255]);
                    return true;
                }
                false
            } else {
                false
            }
        });
        for action in [
            Action::Activate,
            Action::Secondary,
            Action::Scroll(120, false),
        ] {
            tx.try_send(Command::Interact {
                key: key.clone(),
                action,
                x: 3,
                y: 4,
                generation: 1,
                token: Some("wayland-token".into()),
            })
            .unwrap();
        }
        tx.try_send(Command::Interact {
            key: key.clone(),
            action: Action::Context,
            x: 3,
            y: 4,
            generation: 2,
            token: None,
        })
        .unwrap();
        wait_update(&source, |u| {
            if let Update::Menu {
                generation,
                root: Some(root),
                ..
            } = u
            {
                assert_eq!(generation, 2);
                assert!(root.find(1).unwrap().checked);
                assert!(!root.find(2).unwrap().enabled);
                assert!(root.find(3).unwrap().submenu);
                true
            } else {
                false
            }
        });
        assert!(calls.lock().unwrap().iter().any(|c| c == "activate"));
        assert!(calls.lock().unwrap().iter().any(|c| c == "secondary"));
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "scroll:120:vertical"));
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "token:wayland-token"));
        for (generation, id) in [(1, 1), (2, 2), (2, 3), (2, 1)] {
            tx.try_send(Command::MenuClick {
                generation,
                id,
                time: 7,
            })
            .unwrap();
        }
        tx.try_send(Command::Show {
            generation: 2,
            id: 3,
        })
        .unwrap();
        wait_update(&source, |u| matches!(u, Update::Menu { generation: 2, .. }));
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("event:"))
                .cloned()
                .collect::<Vec<_>>(),
            ["event:1:clicked"]
        );
        *label.lock().unwrap() = "Updated".into();
        future::block_on(TestMenu::layout_updated(
            &SignalEmitter::new(&server, "/Menu").unwrap(),
            2,
            0,
        ))
        .unwrap();
        wait_update(
            &source,
            |u| matches!(u,Update::Menu{root:Some(root),..}if root.find(1).unwrap().label=="Updated"),
        );
        passive.store(true, Ordering::Relaxed);
        future::block_on(TestItem::new_status(
            &SignalEmitter::new(&server, "/Item").unwrap(),
            "Passive",
        ))
        .unwrap();
        wait_update(
            &source,
            |u| matches!(u,Update::Items(items)if items.is_empty()),
        );
        passive.store(false, Ordering::Relaxed);
        future::block_on(TestItem::new_status(
            &SignalEmitter::new(&server, "/Item").unwrap(),
            "Active",
        ))
        .unwrap();
        wait_update(
            &source,
            |u| matches!(u,Update::Items(items)if items.len()==1),
        );
        future::block_on(server.close()).unwrap();
        wait_update(
            &source,
            |u| matches!(u,Update::Items(items)if items.is_empty()),
        );
        // KDE registers an item exported on another connection of the same process.
        let exported = future::block_on(
            zbus::connection::Builder::session()
                .unwrap()
                .serve_at(
                    "/StatusNotifierItem",
                    TestItem {
                        passive: passive.clone(),
                        calls: calls.clone(),
                    },
                )
                .unwrap()
                .build(),
        )
        .unwrap();
        let caller = future::block_on(Connection::session()).unwrap();
        future::block_on(async {
            let p = Proxy::new(&caller, WATCHER, PATH, WATCHER).await.unwrap();
            assert!(
                p.call::<_, _, ()>("RegisterStatusNotifierItem", &("org.freedesktop.DBus",))
                    .await
                    .is_err(),
                "accepted another process's item"
            );
            p.call::<_, _, ()>(
                "RegisterStatusNotifierItem",
                &(exported.unique_name().unwrap().as_str(),),
            )
            .await
            .unwrap();
        });
        wait_update(
            &source,
            |u| matches!(u,Update::Items(items)if items.len()==1),
        );
        future::block_on(exported.close()).unwrap();
        wait_update(
            &source,
            |u| matches!(u,Update::Items(items)if items.is_empty()),
        );
        drop(caller);
        drop(tx);
        drop(source);
        thread.join().unwrap();
    }
}
