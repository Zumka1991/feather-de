//! KDE Wayland appmenu endpoints and an event-driven DBusMenu client.
//! All bus traffic, decoding and waits live on a worker, never the compositor thread.
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use futures_lite::{future, StreamExt};
use smithay::{
    reexports::{
        calloop::{channel, LoopHandle},
        wayland_server::{
            backend::{ClientId, ObjectId},
            protocol::wl_surface::WlSurface,
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    wayland::compositor::with_states,
};
use wayland_protocols_plasma::appmenu::server::{
    org_kde_kwin_appmenu::{self, OrgKdeKwinAppmenu},
    org_kde_kwin_appmenu_manager::{self, OrgKdeKwinAppmenuManager},
};
use zbus::{
    proxy::CacheProperties,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
    Proxy,
};

use crate::FeatherCompositor;

const INTERFACE: &str = "com.canonical.dbusmenu";
const REGISTRAR: &str = "com.canonical.AppMenu.Registrar";
const REGISTRAR_PATH: &str = "/com/canonical/AppMenu/Registrar";
const PROPERTIES: &[&str] = &[
    "label",
    "enabled",
    "visible",
    "type",
    "toggle-type",
    "toggle-state",
    "children-display",
    "shortcut",
];
const MAX_ITEMS: usize = 4096;
const MAX_DEPTH: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub service: String,
    /// com.canonical.dbusmenu object, or the org.gtk.Menus menubar
    pub path: String,
    /// Set for GTK: where `app.` and `win.` actions live
    pub gtk: Option<GtkActions>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GtkActions {
    pub app: Option<String>,
    pub window: Option<String>,
}

impl Endpoint {
    fn new(service: String, path: String) -> Option<Self> {
        zbus::names::BusName::try_from(service.as_str()).ok()?;
        OwnedObjectPath::try_from(path.as_str()).ok()?;
        Some(Self { service, path, gtk: None })
    }

    pub fn gtk(service: String, menubar: String, app: Option<String>, window: Option<String>) -> Option<Self> {
        let valid = |p: Option<String>| p.filter(|p| OwnedObjectPath::try_from(p.as_str()).is_ok());
        let mut e = Self::new(service, menubar)?;
        e.gtk = Some(GtkActions { app: valid(app), window: valid(window) });
        Some(e)
    }
}

pub(crate) type SurfaceMenu = Mutex<Option<(ObjectId, Endpoint)>>;

pub fn endpoint(surface: &WlSurface) -> Option<Endpoint> {
    with_states(surface, |states| {
        states.data_map.get::<SurfaceMenu>().and_then(|m| {
            m.lock()
                .unwrap()
                .as_ref()
                .map(|(_, endpoint)| endpoint.clone())
        })
    })
}

impl GlobalDispatch<OrgKdeKwinAppmenuManager, ()> for FeatherCompositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<OrgKdeKwinAppmenuManager>,
        _: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<OrgKdeKwinAppmenuManager, ()> for FeatherCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &OrgKdeKwinAppmenuManager,
        request: org_kde_kwin_appmenu_manager::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let org_kde_kwin_appmenu_manager::Request::Create { id, surface } = request {
            data_init.init(id, surface);
        }
    }
}

impl Dispatch<OrgKdeKwinAppmenu, WlSurface> for FeatherCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &OrgKdeKwinAppmenu,
        request: org_kde_kwin_appmenu::Request,
        surface: &WlSurface,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let org_kde_kwin_appmenu::Request::SetAddress {
            service_name,
            object_path,
        } = request
        {
            let endpoint = Endpoint::new(service_name, object_path);
            with_states(surface, |states| {
                states
                    .data_map
                    .insert_if_missing_threadsafe(|| SurfaceMenu::new(None));
                *states
                    .data_map
                    .get::<SurfaceMenu>()
                    .unwrap()
                    .lock()
                    .unwrap() = endpoint.map(|e| (resource.id(), e));
            });
            state.sync_appmenu_focus();
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, resource: &OrgKdeKwinAppmenu, surface: &WlSurface) {
        if surface.is_alive() {
            with_states(surface, |states| {
                if let Some(data) = states.data_map.get::<SurfaceMenu>() {
                    let mut data = data.lock().unwrap();
                    if data.as_ref().is_some_and(|(id, _)| *id == resource.id()) {
                        *data = None;
                    }
                }
            });
        }
        state.sync_appmenu_focus();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuNode {
    pub id: i32,
    pub label: String,
    pub shortcut: String,
    pub enabled: bool,
    pub visible: bool,
    pub separator: bool,
    pub checked: bool,
    pub radio: bool,
    pub submenu: bool,
    pub children: Vec<MenuNode>,
}

impl MenuNode {
    pub fn find(&self, id: i32) -> Option<&Self> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|n| n.find(id))
    }
}

type WireNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

pub(crate) fn label_without_mnemonic(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().take(512).peekable();
    while let Some(c) = chars.next() {
        if c != '&' && c != '_' {
            result.push(c);
        } else if chars.peek() == Some(&c) {
            result.push(c);
            chars.next();
        }
    }
    result
}

fn parse_node(
    (id, props, children): WireNode,
    depth: usize,
    remaining: &mut usize,
) -> Result<MenuNode, &'static str> {
    if depth > MAX_DEPTH || *remaining == 0 {
        return Err("menu layout exceeds limits");
    }
    *remaining -= 1;
    let string = |name: &str| {
        props
            .get(name)
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("")
    };
    let boolean = |name: &str| {
        props
            .get(name)
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(true)
    };
    let shortcut = props
        .get("shortcut")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<Vec<String>>::try_from(v).ok())
        .and_then(|v| v.into_iter().next())
        .unwrap_or_default()
        .into_iter()
        .take(8)
        .map(|key| match key.as_str() {
            "Control" => "⌃".into(),
            "Alt" => "⌥".into(),
            "Shift" => "⇧".into(),
            "Super" | "Meta" => "⌘".into(),
            _ => key.chars().take(32).collect(),
        })
        .collect::<Vec<String>>()
        .join("");
    let mut node = MenuNode {
        id,
        label: label_without_mnemonic(string("label")),
        shortcut,
        enabled: boolean("enabled"),
        visible: boolean("visible"),
        separator: string("type") == "separator",
        checked: props
            .get("toggle-state")
            .and_then(|v| i32::try_from(v).ok())
            == Some(1),
        radio: string("toggle-type") == "radio",
        submenu: string("children-display") == "submenu",
        children: Vec::with_capacity(children.len().min(MAX_ITEMS)),
    };
    for child in children {
        let raw = WireNode::try_from(child).map_err(|_| "invalid menu child")?;
        node.children.push(parse_node(raw, depth + 1, remaining)?);
    }
    node.submenu |= !node.children.is_empty();
    Ok(node)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    window: u64,
    endpoint: Endpoint,
}

enum Command {
    Focus {
        generation: u64,
        target: Option<Target>,
    },
    Show {
        generation: u64,
        id: i32,
    },
    Close {
        generation: u64,
        id: i32,
    },
    Click {
        generation: u64,
        id: i32,
        timestamp: u32,
    },
}

enum Update {
    Ready,
    Layout {
        generation: u64,
        root: Option<Arc<MenuNode>>,
    },
}

type UpdateSender = channel::SyncSender<Update>;

pub struct AppMenus {
    commands: async_channel::Sender<Command>,
    focus_changed: async_channel::Sender<()>,
    target: Option<Target>,
    pub generation: u64,
    latest_generation: Arc<AtomicU64>,
    opened: Vec<i32>,
}

impl AppMenus {
    pub fn new(
        dh: &DisplayHandle,
        handle: &LoopHandle<'static, FeatherCompositor>,
        advertise_registrar: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        dh.create_global::<FeatherCompositor, OrgKdeKwinAppmenuManager, _>(2, ());
        let (tx, rx) = async_channel::bounded(64);
        let (focus_changed, focus_changes) = async_channel::bounded(1);
        let (updates, source) = channel::sync_channel(4);
        let latest_generation = Arc::new(AtomicU64::new(0));
        let worker_generation = latest_generation.clone();
        handle.insert_source(source, |event, _, state| match event {
            channel::Event::Msg(Update::Ready) => {
                state.appmenu_ready = true;
                state.maybe_launch_autostart();
            }
            channel::Event::Msg(Update::Layout { generation, root }) => {
                if generation == state.appmenus.generation {
                    if let Some(root) = &root {
                        tracing::debug!(
                            generation,
                            items = root.children.len(),
                            "DBusMenu: обновлено меню активного окна"
                        );
                    }
                    if state.shell.set_client_menu(generation, root) {
                        state.needs_redraw = true;
                    }
                }
            }
            channel::Event::Closed => {
                if state.shell.set_client_menu(state.appmenus.generation, None) {
                    state.needs_redraw = true;
                }
                state.appmenu_ready = true;
                state.maybe_launch_autostart();
            }
        })?;
        std::thread::Builder::new()
            .name("feather-appmenu".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                future::block_on(async {
                    if let Err(err) = worker(
                        rx,
                        updates.clone(),
                        advertise_registrar,
                        worker_generation,
                        focus_changes,
                    )
                    .await
                    {
                        tracing::warn!("DBusMenu недоступен: {err}");
                        let _ = updates.send(Update::Ready);
                    }
                });
            })?;
        Ok(Self {
            commands: tx,
            focus_changed,
            target: None,
            generation: 0,
            latest_generation,
            opened: Vec::new(),
        })
    }

    fn send(&self, command: Command) {
        if matches!(command, Command::Focus { .. }) {
            if let Err(err) = self.commands.force_send(command) {
                tracing::debug!("DBusMenu: поток недоступен: {err}");
            }
        } else if let Err(err) = self.commands.try_send(command) {
            tracing::warn!("DBusMenu: команда не поставлена в очередь: {err}");
        }
    }

    pub fn click(&self, generation: u64, id: i32, timestamp: u32) {
        if generation == self.generation {
            self.send(Command::Click {
                generation,
                id,
                timestamp,
            });
        }
    }
}

impl FeatherCompositor {
    pub fn sync_appmenu_focus(&mut self) {
        let target = self
            .windows
            .iter()
            .rev()
            .find(|w| w.is_visible())
            .and_then(|w| {
                endpoint(w.wl_surface()?).map(|endpoint| Target {
                    window: w.id,
                    endpoint,
                })
            });
        if self.appmenus.target == target {
            return;
        }
        self.appmenus.generation = self.appmenus.generation.wrapping_add(1);
        self.appmenus
            .latest_generation
            .store(self.appmenus.generation, Ordering::Release);
        let _ = self.appmenus.focus_changed.try_send(());
        self.appmenus.target = target.clone();
        self.appmenus.opened.clear();
        self.shell.set_client_menu(self.appmenus.generation, None);
        self.appmenus.send(Command::Focus {
            generation: self.appmenus.generation,
            target,
        });
        self.needs_redraw = true;
    }

    /// Called after input filters: dispatch only changed open/closed menu ids, never poll D-Bus.
    pub fn flush_appmenu_commands(&mut self) {
        let opened = self.shell.client_open_ids();
        if opened == self.appmenus.opened {
            return;
        }
        let generation = self.appmenus.generation;
        for id in self
            .appmenus
            .opened
            .iter()
            .filter(|id| !opened.contains(id))
        {
            self.appmenus.send(Command::Close {
                generation,
                id: *id,
            });
        }
        for id in opened
            .iter()
            .filter(|id| !self.appmenus.opened.contains(id))
        {
            self.appmenus.send(Command::Show {
                generation,
                id: *id,
            });
        }
        self.appmenus.opened = opened;
    }
}

// Qt probes this name even on Wayland. Own it only for our standalone session (or an
// explicitly isolated debug bus), never replace a host desktop's registrar.
#[derive(Default)]
struct Registrar {
    windows: HashMap<u32, (String, OwnedObjectPath)>,
}

#[zbus::interface(name = "com.canonical.AppMenu.Registrar")]
impl Registrar {
    fn register_window(
        &mut self,
        window_id: u32,
        menu_object_path: OwnedObjectPath,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if self.windows.len() >= 256 && !self.windows.contains_key(&window_id) {
            return Err(zbus::fdo::Error::LimitsExceeded(
                "Too many registered menus".into(),
            ));
        }
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Missing sender".into()))?;
        self.windows
            .insert(window_id, (sender.to_string(), menu_object_path));
        Ok(())
    }
    fn unregister_window(
        &mut self,
        window_id: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        if self
            .windows
            .get(&window_id)
            .is_some_and(|(service, _)| header.sender().is_some_and(|s| s.as_str() == service))
        {
            self.windows.remove(&window_id);
        }
    }
    fn get_menu_for_window(&self, window_id: u32) -> zbus::fdo::Result<(String, OwnedObjectPath)> {
        self.windows
            .get(&window_id)
            .cloned()
            .ok_or_else(|| zbus::fdo::Error::UnknownObject("No menu for this window".into()))
    }
    fn get_menus(&self) -> Vec<(u32, String, OwnedObjectPath)> {
        self.windows
            .iter()
            .map(|(id, (service, path))| (*id, service.clone(), path.clone()))
            .collect()
    }
}

pub(crate) async fn load(proxy: &Proxy<'_>) -> zbus::Result<Arc<MenuNode>> {
    let (_, root): (u32, WireNode) = proxy
        .call("GetLayout", &(0i32, MAX_DEPTH as i32, PROPERTIES))
        .await?;
    let mut remaining = MAX_ITEMS;
    let root = parse_node(root, 0, &mut remaining).map_err(|e| zbus::Error::Failure(e.into()))?;
    fn unique(node: &MenuNode, ids: &mut std::collections::HashSet<i32>) -> bool {
        ids.insert(node.id) && node.children.iter().all(|child| unique(child, ids))
    }
    if !unique(&root, &mut std::collections::HashSet::new()) {
        return Err(zbus::Error::Failure("duplicate menu item ids".into()));
    }
    Ok(Arc::new(root))
}

// Cancel an old provider's RPC as soon as focus changes; a frozen application must
// not delay loading the next window's menu until its method timeout expires.
async fn while_current<T>(
    call: impl std::future::Future<Output = T>,
    generation: u64,
    latest: &AtomicU64,
    changes: &async_channel::Receiver<()>,
) -> Option<T> {
    if generation != latest.load(Ordering::Acquire) {
        return None;
    }
    future::or(async { Some(call.await) }, async {
        while generation == latest.load(Ordering::Acquire) {
            if changes.recv().await.is_err() {
                return None;
            }
        }
        None
    })
    .await
}

async fn worker(
    commands: async_channel::Receiver<Command>,
    updates: UpdateSender,
    advertise_registrar: bool,
    latest_generation: Arc<AtomicU64>,
    focus_changes: async_channel::Receiver<()>,
) -> zbus::Result<()> {
    let conn = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(2))
        .build()
        .await?;
    if advertise_registrar {
        conn.object_server()
            .at(REGISTRAR_PATH, Registrar::default())
            .await?;
        if let Err(err) = conn
            .request_name_with_flags(REGISTRAR, zbus::fdo::RequestNameFlags::DoNotQueue.into())
            .await
        {
            tracing::debug!("DBusMenu: регистратор уже занят: {err}");
        }
    }
    let owner_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .path("/org/freedesktop/DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(2, "")?
        .build();
    let mut owners = zbus::MessageStream::for_match_rule(owner_rule, &conn, Some(32)).await?;
    let _ = updates.send(Update::Ready);
    let mut active_service = String::new();
    let mut generation = 0;
    let mut proxy: Option<Proxy<'static>> = None;
    let mut signals: Option<zbus::proxy::SignalStream<'static>> = None;
    let mut root: Option<Arc<MenuNode>> = None;
    // GTK windows: action paths of the focused one and its loaded menu (ids → actions)
    let mut gtk: Option<GtkActions> = None;
    let mut gtk_menu = crate::gtk_menu::GtkMenu::default();
    enum Wake {
        Command(Result<Command, async_channel::RecvError>),
        Signal(Option<zbus::Message>),
        Owner(Option<zbus::Result<zbus::Message>>),
    }
    loop {
        let wake = if let Some(stream) = signals.as_mut() {
            future::or(
                async { Wake::Command(commands.recv().await) },
                future::or(async { Wake::Signal(stream.next().await) }, async {
                    Wake::Owner(owners.next().await)
                }),
            )
            .await
        } else {
            future::or(async { Wake::Command(commands.recv().await) }, async {
                Wake::Owner(owners.next().await)
            })
            .await
        };
        let mut reload = false;
        match wake {
            Wake::Command(Ok(Command::Focus {
                generation: next,
                target,
            })) => {
                if next != latest_generation.load(Ordering::Acquire) {
                    continue;
                }
                generation = next;
                if let Some(p) = proxy.as_ref().filter(|_| gtk.is_some()) {
                    gtk_menu.end(p).await;
                }
                gtk = None;
                gtk_menu = Default::default();
                active_service.clear();
                proxy = None;
                signals = None;
                root = None;
                if let Some(target) = target {
                    let endpoint = target.endpoint;
                    active_service = endpoint.service.clone();
                    let interface = if endpoint.gtk.is_some() { crate::gtk_menu::MENUS } else { INTERFACE };
                    gtk = endpoint.gtk;
                    let created: zbus::Result<Proxy<'static>> = zbus::proxy::Builder::new(&conn)
                        .destination(endpoint.service)?
                        .path(endpoint.path)?
                        .interface(interface)?
                        .cache_properties(CacheProperties::No)
                        .build()
                        .await;
                    if let Ok(p) = created {
                        match p.receive_all_signals().await {
                            Ok(s) => {
                                signals = Some(s);
                                proxy = Some(p);
                                reload = true;
                            }
                            Err(err) => tracing::debug!("DBusMenu: подписка недоступна: {err}"),
                        }
                    }
                }
            }
            Wake::Command(Ok(Command::Show {
                generation: token,
                id,
            })) if token == generation && token == latest_generation.load(Ordering::Acquire) => {
                if gtk.is_some() {
                    // GTK has no AboutToShow: refresh enabled/checked states as it opens
                    reload = true;
                } else if let Some(p) = &proxy {
                    let shown = while_current(
                        p.call::<_, _, bool>("AboutToShow", &(id,)),
                        generation,
                        &latest_generation,
                        &focus_changes,
                    )
                    .await;
                    if !matches!(shown, Some(Ok(_))) {
                        continue;
                    }
                    let _ = p
                        .call_noreply("Event", &(id, "opened", Value::from(0i32), 0u32))
                        .await;
                    reload = true;
                }
            }
            Wake::Command(Ok(Command::Close {
                generation: token,
                id,
            })) if token == generation && token == latest_generation.load(Ordering::Acquire) => {
                if let Some(p) = proxy.as_ref().filter(|_| gtk.is_none()) {
                    let _ = p
                        .call_noreply("Event", &(id, "closed", Value::from(0i32), 0u32))
                        .await;
                }
            }
            Wake::Command(Ok(Command::Click {
                generation: token,
                id,
                timestamp,
            })) if token == generation && token == latest_generation.load(Ordering::Acquire) => {
                if root
                    .as_ref()
                    .and_then(|r| r.find(id))
                    .is_some_and(|n| n.enabled && n.visible && !n.separator && !n.submenu)
                {
                    if let Some(paths) = &gtk {
                        gtk_menu.activate(&conn, &active_service, paths, id).await;
                    } else if let Some(p) = &proxy {
                        let _ = p
                            .call_noreply("Event", &(id, "clicked", Value::from(0i32), timestamp))
                            .await;
                    }
                }
            }
            Wake::Command(Err(_)) => return Ok(()),
            Wake::Signal(Some(msg)) => {
                reload = msg.header().member().is_some_and(|m| {
                    matches!(m.as_str(), "LayoutUpdated" | "ItemsPropertiesUpdated" | "Changed")
                });
            }
            Wake::Signal(None) => {
                proxy = None;
                signals = None;
                root = None;
                let _ = updates.send(Update::Layout {
                    generation,
                    root: None,
                });
            }
            Wake::Owner(Some(Ok(message))) => {
                if let Ok((name, old, _)) = message.body().deserialize::<(String, String, String)>()
                {
                    if advertise_registrar {
                        if let Ok(interface) = conn
                            .object_server()
                            .interface::<_, Registrar>(REGISTRAR_PATH)
                            .await
                        {
                            interface
                                .get_mut()
                                .await
                                .windows
                                .retain(|_, (service, _)| service != &old);
                        }
                    }
                    if name == active_service || old == active_service {
                        active_service.clear();
                        proxy = None;
                        signals = None;
                        root = None;
                        let _ = updates.send(Update::Layout {
                            generation,
                            root: None,
                        });
                    }
                }
            }
            Wake::Owner(_) => return Err(zbus::Error::Failure("session bus disconnected".into())),
            _ => {}
        }
        if reload {
            // One snapshot for a burst of property/layout signals; no idle polling.
            if let Some(stream) = signals.as_mut() {
                for _ in 0..64 {
                    if !matches!(future::poll_once(stream.next()).await, Some(Some(_))) {
                        break;
                    }
                }
            }
            if let (Some(p), Some(paths)) = (&proxy, &gtk) {
                let loaded = while_current(crate::gtk_menu::load(&conn, p, paths), generation, &latest_generation, &focus_changes).await;
                match loaded {
                    None => continue,
                    Some(Ok(menu)) => {
                        // Start again counted every group once more: balance the old ones
                        gtk_menu.end(p).await;
                        root = menu.root.clone();
                        gtk_menu = menu;
                    }
                    Some(Err(err)) => {
                        tracing::debug!("GTK menu: {err}");
                        root = None;
                    }
                }
            } else if let Some(p) = &proxy {
                let Some(loaded) =
                    while_current(load(p), generation, &latest_generation, &focus_changes).await
                else {
                    continue;
                };
                match loaded {
                    Ok(menu) => root = Some(menu),
                    Err(err) => {
                        tracing::debug!("DBusMenu: GetLayout: {err}");
                        root = None;
                    }
                }
            }
            let _ = updates.send(Update::Layout {
                generation,
                root: root.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Str;

    fn wire(id: i32, label: &str, children: Vec<WireNode>) -> WireNode {
        let props = HashMap::from([("label".into(), OwnedValue::from(Str::from(label)))]);
        let children = children
            .into_iter()
            .map(|child| OwnedValue::try_from(Value::from(child)).unwrap())
            .collect();
        (id, props, children)
    }

    #[test]
    fn decodes_nested_variants_mnemonics_and_item_state() {
        let mut child = wire(2, "_Save __ & _Close", Vec::new());
        child.1.insert("enabled".into(), OwnedValue::from(false));
        child.1.insert("visible".into(), OwnedValue::from(false));
        child
            .1
            .insert("toggle-state".into(), OwnedValue::from(1i32));
        child
            .1
            .insert("toggle-type".into(), OwnedValue::from(Str::from("radio")));
        child.1.insert(
            "shortcut".into(),
            OwnedValue::try_from(Value::from(vec![vec!["Control", "Shift", "S"]])).unwrap(),
        );
        let root =
            parse_node(wire(0, "", vec![wire(1, "&File", vec![child])]), 0, &mut 16).unwrap();
        assert_eq!(root.children[0].label, "File");
        assert!(root.children[0].submenu);
        let child = root.find(2).unwrap();
        assert_eq!(child.label, "Save _  Close");
        assert_eq!(child.shortcut, "⌃⇧S");
        assert!(!child.enabled && !child.visible);
        assert!(child.checked && child.radio);
    }

    #[test]
    fn refuses_excessive_or_malformed_layouts_and_invalid_addresses() {
        assert!(parse_node(wire(0, "", vec![wire(1, "File", vec![])]), 0, &mut 1).is_err());
        assert!(parse_node(wire(0, "", vec![]), MAX_DEPTH + 1, &mut 10).is_err());
        assert!(parse_node(
            (0, HashMap::new(), vec![OwnedValue::from(3i32)]),
            0,
            &mut 10
        )
        .is_err());
        assert!(Endpoint::new("not a bus name".into(), "/MenuBar/1".into()).is_none());
        assert!(Endpoint::new(":1.4".into(), "not/a/path".into()).is_none());
        assert!(Endpoint::new(":1.4".into(), "/MenuBar/1".into()).is_some());
    }
    struct TestMenu {
        label: Arc<Mutex<String>>,
        clicks: Arc<AtomicU64>,
        shows: Arc<AtomicU64>,
        stall: Arc<std::sync::atomic::AtomicBool>,
    }

    #[zbus::interface(name = "com.canonical.dbusmenu")]
    impl TestMenu {
        fn get_layout(
            &self,
            _parent_id: i32,
            _recursion_depth: i32,
            _property_names: Vec<String>,
        ) -> (u32, WireNode) {
            (
                1,
                wire(
                    0,
                    "",
                    vec![wire(
                        1,
                        "_File",
                        vec![wire(2, &self.label.lock().unwrap(), vec![])],
                    )],
                ),
            )
        }
        async fn about_to_show(&self, _id: i32) -> bool {
            self.shows.fetch_add(1, Ordering::Relaxed);
            if self.stall.load(Ordering::Relaxed) {
                future::pending::<()>().await;
            }
            true
        }
        fn event(&self, id: i32, event_id: &str, _data: OwnedValue, _timestamp: u32) {
            if id == 2 && event_id == "clicked" {
                self.clicks.fetch_add(1, Ordering::Relaxed);
            }
        }
        #[zbus(signal)]
        async fn layout_updated(
            emitter: &zbus::object_server::SignalEmitter<'_>,
            revision: u32,
            parent: i32,
        ) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn items_properties_updated(
            emitter: &zbus::object_server::SignalEmitter<'_>,
            changed: Vec<(i32, HashMap<String, OwnedValue>)>,
            removed: Vec<(i32, Vec<String>)>,
        ) -> zbus::Result<()>;
    }

    fn wait_update(
        source: &channel::Channel<Update>,
        expected_generation: Option<u64>,
    ) -> Option<Arc<MenuNode>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match source.try_recv() {
                Ok(Update::Ready) if expected_generation.is_none() => return None,
                Ok(Update::Layout { generation, root })
                    if Some(generation) == expected_generation =>
                {
                    return root
                }
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(err) => panic!("worker closed unexpectedly: {err}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for DBusMenu worker"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    #[ignore = "requires an isolated session bus: dbus-run-session -- cargo test -- --include-ignored"]
    fn worker_loads_opens_clicks_updates_rejects_stale_actions_and_handles_owner_exit() {
        let label = Arc::new(Mutex::new("_Run".into()));
        let clicks = Arc::new(AtomicU64::new(0));
        let shows = Arc::new(AtomicU64::new(0));
        let stall = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = future::block_on(
            zbus::connection::Builder::session()
                .unwrap()
                .serve_at(
                    "/Menu",
                    TestMenu {
                        label: label.clone(),
                        clicks: clicks.clone(),
                        shows: shows.clone(),
                        stall: stall.clone(),
                    },
                )
                .unwrap()
                .build(),
        )
        .unwrap();
        // Even a replaceable registrar owned by the host must remain untouched.
        future::block_on(server.request_name_with_flags(
            REGISTRAR,
            zbus::fdo::RequestNameFlags::AllowReplacement.into(),
        ))
        .unwrap();
        let endpoint =
            Endpoint::new(server.unique_name().unwrap().to_string(), "/Menu".into()).unwrap();
        let (tx, rx) = async_channel::bounded(64);
        let (focus_changed, focus_changes) = async_channel::bounded(1);
        let (updates, source) = channel::sync_channel(4);
        let latest = Arc::new(AtomicU64::new(1));
        let worker_latest = latest.clone();
        let thread = std::thread::spawn(move || {
            future::block_on(worker(rx, updates, true, worker_latest, focus_changes)).unwrap()
        });
        wait_update(&source, None);
        let owner = future::block_on(async {
            zbus::fdo::DBusProxy::new(&server)
                .await
                .unwrap()
                .get_name_owner(REGISTRAR.try_into().unwrap())
                .await
                .unwrap()
        });
        assert_eq!(owner.as_str(), server.unique_name().unwrap().as_str());
        tx.try_send(Command::Focus {
            generation: 1,
            target: Some(Target {
                window: 1,
                endpoint: endpoint.clone(),
            }),
        })
        .unwrap();
        let root = wait_update(&source, Some(1)).unwrap();
        assert_eq!(root.find(2).unwrap().label, "Run");
        tx.try_send(Command::Show {
            generation: 1,
            id: 1,
        })
        .unwrap();
        wait_update(&source, Some(1)).unwrap();
        assert_eq!(shows.load(Ordering::Relaxed), 1);
        tx.try_send(Command::Click {
            generation: 1,
            id: 2,
            timestamp: 42,
        })
        .unwrap();
        tx.try_send(Command::Show {
            generation: 1,
            id: 1,
        })
        .unwrap();
        wait_update(&source, Some(1));
        assert_eq!(clicks.load(Ordering::Relaxed), 1);
        *label.lock().unwrap() = "Updated".into();
        future::block_on(async {
            let interface = server
                .object_server()
                .interface::<_, TestMenu>("/Menu")
                .await
                .unwrap();
            TestMenu::items_properties_updated(interface.signal_emitter(), vec![], vec![])
                .await
                .unwrap();
        });
        assert_eq!(
            wait_update(&source, Some(1))
                .unwrap()
                .find(2)
                .unwrap()
                .label,
            "Updated"
        );
        *label.lock().unwrap() = "Layout update".into();
        future::block_on(async {
            let interface = server
                .object_server()
                .interface::<_, TestMenu>("/Menu")
                .await
                .unwrap();
            TestMenu::layout_updated(interface.signal_emitter(), 2, 0)
                .await
                .unwrap();
        });
        assert_eq!(
            wait_update(&source, Some(1))
                .unwrap()
                .find(2)
                .unwrap()
                .label,
            "Layout update"
        );
        // Freeze AboutToShow, then switch focus while that RPC is pending.
        stall.store(true, Ordering::Relaxed);
        tx.try_send(Command::Show {
            generation: 1,
            id: 1,
        })
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while shows.load(Ordering::Relaxed) < 3 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let switched = std::time::Instant::now();
        latest.store(2, Ordering::Release);
        focus_changed.try_send(()).unwrap();
        tx.try_send(Command::Click {
            generation: 1,
            id: 2,
            timestamp: 43,
        })
        .unwrap();
        tx.try_send(Command::Focus {
            generation: 2,
            target: Some(Target {
                window: 2,
                endpoint,
            }),
        })
        .unwrap();
        wait_update(&source, Some(2)).unwrap();
        assert!(
            switched.elapsed() < Duration::from_millis(500),
            "old provider delayed the next menu"
        );
        assert_eq!(clicks.load(Ordering::Relaxed), 1);
        future::block_on(server.close()).unwrap();
        assert!(wait_update(&source, Some(2)).is_none());
        drop(tx);
        thread.join().unwrap();
    }
}
