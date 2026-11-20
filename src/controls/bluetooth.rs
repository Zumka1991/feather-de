use super::*;
use std::collections::{HashSet, VecDeque};
use zbus::message::Header;

const BLUEZ: &str = "org.bluez";
const ROOT: &str = "/";
const BLUEZ_PATH: &str = "/org/bluez";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const AGENT_MANAGER: &str = "org.bluez.AgentManager1";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const BATTERY: &str = "org.bluez.Battery1";
const AGENT_PATH: &str = "/org/feather/BluetoothAgent";
const AGENT_CAPABILITY: &str = "DisplayYesNo";

type Props = HashMap<String, OwnedValue>;
type Interfaces = HashMap<String, Props>;
type ManagedObjects = HashMap<OwnedObjectPath, Interfaces>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BtDevice {
    pub path: String,
    pub adapter: String,
    pub address: String,
    pub name: String,
    pub icon: String,
    pub paired: bool,
    /// `Paired == true` while BlueZ explicitly reports `Bonded == false` (ephemeral session key
    /// in RAM with no persistent LinkKey/LTK stored on disk; would be lost across reboots).
    pub unbonded: bool,
    pub trusted: bool,
    pub connected: bool,
    pub battery: Option<u8>,
    pub rssi: Option<i16>,
}

impl BtDevice {
    /// BlueZ keeps a record of it: paired, trusted (also when the bond was lost) or connected.
    pub fn remembered(&self) -> bool {
        self.paired || self.trusted || self.connected
    }

    /// Persistent cryptographic bond exists on disk (or `Bonded` is omitted by an older daemon).
    pub fn has_bond(&self) -> bool {
        self.paired && !self.unbonded
    }

    pub fn kind_label(&self) -> &'static str {
        match self.icon.as_str() {
            s if s.starts_with("audio-headphones") || s.starts_with("audio-headset") => "Наушники",
            s if s.starts_with("audio-") => "Аудиоустройство",
            "input-keyboard" => "Клавиатура",
            "input-mouse" | "input-tablet" => "Мышь или трекпад",
            "input-gaming" => "Геймпад",
            s if s.starts_with("phone") => "Телефон",
            "computer" => "Компьютер",
            _ => "Устройство Bluetooth",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bluetooth {
    pub available: bool,
    pub powered: bool,
    pub discovering: bool,
    pub adapter_name: String,
    pub connected: Option<String>,
    pub connecting: Option<String>,
    pub devices: Vec<BtDevice>,
    pub error: Option<String>,
}

impl Bluetooth {
    pub fn subtitle(&self) -> &str {
        if self.error.is_some() {
            "Служба недоступна"
        } else if !self.available {
            "Нет адаптера"
        } else if !self.powered {
            "Выкл."
        } else if self.connecting.is_some() {
            "Подключение…"
        } else if let Some(name) = &self.connected {
            name.as_str()
        } else if self.discovering {
            "Поиск…"
        } else {
            "Вкл."
        }
    }
}

async fn proxy<'a>(
    conn: &'a zbus::Connection,
    path: &'a str,
    iface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    Proxy::new(conn, BLUEZ, path, iface).await
}

fn boolean(p: &Props, key: &str) -> bool {
    p.get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn opt_bool(p: &Props, key: &str) -> Option<bool> {
    p.get(key).and_then(|v| bool::try_from(v).ok())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AdapterMeta {
    path: String,
    powered: bool,
    connectable: Option<bool>,
    pairable: Option<bool>,
}

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.bluez.Error")]
enum BtAgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
}

#[derive(Default)]
struct AgentPolicy {
    bluez_owner: Option<String>,
    active_target: Option<String>,
    remembered: HashSet<String>,
}

struct PairingGuard {
    policy: Arc<Mutex<AgentPolicy>>,
}

impl PairingGuard {
    fn new(policy: &Arc<Mutex<AgentPolicy>>, device_path: &str) -> Self {
        if let Ok(mut p) = policy.lock() {
            p.active_target = Some(device_path.to_string());
        }
        Self {
            policy: policy.clone(),
        }
    }
}

impl Drop for PairingGuard {
    fn drop(&mut self) {
        if let Ok(mut p) = self.policy.lock() {
            p.active_target = None;
        }
    }
}

struct BtAgent(Arc<Mutex<AgentPolicy>>);

impl BtAgent {
    async fn authorize(
        &self,
        device: &OwnedObjectPath,
        header: &Header<'_>,
        conn: &zbus::Connection,
    ) -> Result<(), BtAgentError> {
        let Some(sender) = header.sender().map(|s| s.as_str().to_string()) else {
            return Err(BtAgentError::Rejected("missing sender".into()));
        };
        let cached_matches = self
            .0
            .lock()
            .ok()
            .and_then(|p| p.bluez_owner.clone())
            .is_some_and(|owner| owner == sender);
        if !cached_matches {
            let owner = zbus::fdo::DBusProxy::new(conn)
                .await?
                .get_name_owner(BLUEZ.try_into().map_err(zbus::Error::from)?)
                .await
                .map_err(|_| BtAgentError::Rejected("org.bluez has no owner".into()))?;
            if owner.as_str() != sender {
                return Err(BtAgentError::Rejected("only org.bluez may call Agent1".into()));
            }
            if let Ok(mut p) = self.0.lock() {
                p.bluez_owner = Some(owner.to_string());
            }
        }
        let dev = device.as_str();
        let allowed = self
            .0
            .lock()
            .map(|p| p.active_target.as_deref() == Some(dev) || p.remembered.contains(dev))
            .unwrap_or(false);
        if allowed {
            Ok(())
        } else {
            Err(BtAgentError::Rejected("unsolicited pairing rejected".into()))
        }
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl BtAgent {
    fn release(&self) {}

    async fn request_pin_code(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<String, BtAgentError> {
        self.authorize(&device, &header, conn).await?;
        Ok("0000".into())
    }

    async fn display_pin_code(
        &self,
        device: OwnedObjectPath,
        pincode: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), BtAgentError> {
        let _ = pincode;
        self.authorize(&device, &header, conn).await
    }

    async fn request_passkey(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<u32, BtAgentError> {
        self.authorize(&device, &header, conn).await?;
        Ok(0)
    }

    async fn display_passkey(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        entered: u16,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), BtAgentError> {
        let _ = (passkey, entered);
        self.authorize(&device, &header, conn).await
    }

    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), BtAgentError> {
        let _ = passkey;
        self.authorize(&device, &header, conn).await
    }

    async fn request_authorization(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), BtAgentError> {
        self.authorize(&device, &header, conn).await
    }

    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        uuid: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), BtAgentError> {
        let _ = uuid;
        self.authorize(&device, &header, conn).await
    }

    fn cancel(&self) {}
}

async fn register_default_agent(conn: &zbus::Connection, policy: &Arc<Mutex<AgentPolicy>>) {
    if let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await {
        if let Ok(name) = BLUEZ.try_into() {
            let owner = dbus.get_name_owner(name).await.ok().map(|o| o.to_string());
            if let Ok(mut p) = policy.lock() {
                p.bluez_owner = owner;
            }
        }
    }
    let Ok(agent_path) = OwnedObjectPath::try_from(AGENT_PATH) else {
        return;
    };
    let Ok(mgr) = proxy(conn, BLUEZ_PATH, AGENT_MANAGER).await else {
        return;
    };
    let _ = mgr
        .call::<_, _, ()>("RegisterAgent", &(agent_path.clone(), AGENT_CAPABILITY))
        .await;
    let _ = mgr
        .call::<_, _, ()>("RequestDefaultAgent", &(agent_path,))
        .await;
}

/// Ensures the powered adapter accepts incoming reconnections from trusted accessories
/// (`Connectable = true`) and saves persistent link keys during pairing (`Pairable = true`,
/// which maps to `MGMT_OP_SET_BONDABLE` in the Linux kernel).
async fn ensure_adapter_ready(conn: &zbus::Connection, meta: &AdapterMeta) -> bool {
    if !meta.powered || meta.path.is_empty() {
        return false;
    }
    let need_connectable = meta.connectable == Some(false);
    let need_pairable = meta.pairable == Some(false);
    if !need_connectable && !need_pairable {
        return false;
    }
    let Ok(pxy) = proxy(conn, &meta.path, ADAPTER).await else {
        return false;
    };
    let mut changed = false;
    if need_connectable && pxy.set_property("Connectable", true).await.is_ok() {
        changed = true;
    }
    if need_pairable && pxy.set_property("Pairable", true).await.is_ok() {
        changed = true;
    }
    changed
}

fn text<'a>(p: &'a Props, key: &str) -> Option<&'a str> {
    p.get(key).and_then(|v| <&str>::try_from(v).ok())
}

fn path(p: &Props, key: &str) -> Option<String> {
    p.get(key)
        .and_then(|v| <&zbus::zvariant::ObjectPath>::try_from(v).ok())
        .map(|p| p.as_str().to_string())
}

fn byte_val(p: &Props, key: &str) -> Option<u8> {
    p.get(key).and_then(|v| u8::try_from(v).ok())
}

fn rssi_val(p: &Props, key: &str) -> Option<i16> {
    let v = p.get(key)?;
    i16::try_from(v)
        .ok()
        .or_else(|| i32::try_from(v).ok().and_then(|n| i16::try_from(n).ok()))
}

fn is_mac_placeholder(name: &str, address: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return true;
    }
    if !address.is_empty() && n.replace('-', ":").eq_ignore_ascii_case(address.trim()) {
        return true;
    }
    // Check AA:BB:CC:DD:EE:FF or AA-BB-CC-DD-EE-FF pattern
    let bytes = n.as_bytes();
    bytes.len() == 17
        && bytes.iter().enumerate().all(|(i, &b)| {
            if i % 3 == 2 {
                b == b':' || b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn parse_managed(objects: &ManagedObjects) -> (Bluetooth, AdapterMeta) {
    let mut adapters: Vec<(&str, &Props)> = objects
        .iter()
        .filter_map(|(p, ifaces)| Some((p.as_str(), ifaces.get(ADAPTER)?)))
        .collect();
    if adapters.is_empty() {
        return (Bluetooth::default(), AdapterMeta::default());
    }
    adapters.sort_by(|(pa, a), (pb, b)| {
        boolean(b, "Powered")
            .cmp(&boolean(a, "Powered"))
            .then(pa.cmp(pb))
    });
    let (adapter_path, adapter_props) = adapters[0];
    let powered = boolean(adapter_props, "Powered");
    let discovering = powered && boolean(adapter_props, "Discovering");
    let adapter_name = text(adapter_props, "Alias")
        .or_else(|| text(adapter_props, "Name"))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Bluetooth")
        .to_string();
    let meta = AdapterMeta {
        path: adapter_path.to_string(),
        powered,
        connectable: opt_bool(adapter_props, "Connectable"),
        pairable: opt_bool(adapter_props, "Pairable"),
    };

    let mut devices = Vec::new();
    for (obj_path, ifaces) in objects {
        let Some(dev) = ifaces.get(DEVICE) else {
            continue;
        };
        let dev_adapter = path(dev, "Adapter").unwrap_or_else(|| adapter_path.to_string());
        if dev_adapter != adapter_path && !obj_path.as_str().starts_with(adapter_path) {
            continue;
        }
        let address = text(dev, "Address").unwrap_or("").trim().to_string();
        let raw_name = text(dev, "Name").unwrap_or("").trim();
        let alias = text(dev, "Alias").unwrap_or("").trim();
        let paired = boolean(dev, "Paired");
        let unbonded = paired && opt_bool(dev, "Bonded") == Some(false);
        let trusted = boolean(dev, "Trusted");
        let connected = powered && boolean(dev, "Connected");
        let icon = text(dev, "Icon").unwrap_or("").trim().to_string();
        let name = if !alias.is_empty() {
            alias.to_string()
        } else if !raw_name.is_empty() {
            raw_name.to_string()
        } else {
            address.clone()
        };
        if name.is_empty() {
            continue;
        }
        if !paired
            && !connected
            && raw_name.is_empty()
            && icon.is_empty()
            && is_mac_placeholder(&name, &address)
        {
            continue;
        }
        let battery = ifaces
            .get(BATTERY)
            .and_then(|b| byte_val(b, "Percentage"))
            .map(|p| p.min(100));
        let rssi = rssi_val(dev, "RSSI");
        devices.push(BtDevice {
            path: obj_path.as_str().to_string(),
            adapter: adapter_path.to_string(),
            address,
            name,
            icon,
            paired,
            unbonded,
            trusted,
            connected,
            battery,
            rssi,
        });
    }

    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.has_bond().cmp(&a.has_bond()))
            .then(b.paired.cmp(&a.paired))
            .then(b.rssi.unwrap_or(i16::MIN).cmp(&a.rssi.unwrap_or(i16::MIN)))
            .then(a.name.cmp(&b.name))
            .then(a.path.cmp(&b.path))
    });
    devices.truncate(64);

    let connected = if powered {
        let mut active = devices.iter().filter(|d| d.connected);
        if let Some(first) = active.next() {
            let extra = active.count();
            if extra > 0 {
                Some(format!("{} (+{extra})", first.name))
            } else {
                Some(first.name.clone())
            }
        } else {
            None
        }
    } else {
        None
    };

    (
        Bluetooth {
            available: true,
            powered,
            discovering,
            adapter_name,
            connected,
            connecting: None,
            devices,
            error: None,
        },
        meta,
    )
}

#[cfg(test)]
fn parse_objects(objects: &ManagedObjects) -> Bluetooth {
    parse_managed(objects).0
}

async fn snapshot_with_meta(conn: &zbus::Connection) -> zbus::Result<(Bluetooth, AdapterMeta)> {
    let om = proxy(conn, ROOT, OBJECT_MANAGER).await?;
    let objects: ManagedObjects = om.call("GetManagedObjects", &()).await?;
    Ok(parse_managed(&objects))
}

#[cfg(test)]
pub(super) async fn snapshot(conn: &zbus::Connection) -> zbus::Result<Bluetooth> {
    Ok(snapshot_with_meta(conn).await?.0)
}

fn sync_policy_remembered(policy: &Arc<Mutex<AgentPolicy>>, state: &Bluetooth) {
    if let Ok(mut p) = policy.lock() {
        p.remembered.clear();
        for d in &state.devices {
            if d.remembered() {
                p.remembered.insert(d.path.clone());
            }
        }
    }
}

async fn primary_adapter_path(conn: &zbus::Connection, current: &Bluetooth) -> Result<String, String> {
    if let Some(dev) = current.devices.first() {
        if !dev.adapter.is_empty() {
            return Ok(dev.adapter.clone());
        }
    }
    let om = proxy(conn, ROOT, OBJECT_MANAGER)
        .await
        .map_err(|_| "Служба Bluetooth (BlueZ) недоступна".to_string())?;
    let objects: ManagedObjects = om
        .call("GetManagedObjects", &())
        .await
        .map_err(|_| "Служба Bluetooth (BlueZ) недоступна".to_string())?;
    let mut paths: Vec<String> = objects
        .iter()
        .filter_map(|(p, ifaces)| ifaces.contains_key(ADAPTER).then(|| p.as_str().to_string()))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .next()
        .ok_or_else(|| "Bluetooth-адаптер не найден".into())
}

async fn stop_discovery_if_needed(
    conn: &zbus::Connection,
    current: &Bluetooth,
    started_discovery: &mut bool,
) {
    if !*started_discovery {
        return;
    }
    *started_discovery = false;
    if let Ok(adapter) = primary_adapter_path(conn, current).await {
        if let Ok(pxy) = proxy(conn, &adapter, ADAPTER).await {
            let _ = pxy.call::<_, _, ()>("StopDiscovery", &()).await;
        }
    }
}

async fn action_with_context(
    conn: &zbus::Connection,
    command: Control,
    current: &Bluetooth,
    meta: &AdapterMeta,
    policy: &Arc<Mutex<AgentPolicy>>,
    started_discovery: &mut bool,
) -> Result<(), String> {
    let result: zbus::Result<()> = match command {
        Control::Bluetooth(enabled) => {
            if !current.available {
                return Err("Bluetooth-адаптер недоступен".into());
            }
            if !enabled {
                stop_discovery_if_needed(conn, current, started_discovery).await;
            }
            let adapter = primary_adapter_path(conn, current).await?;
            let pxy = proxy(conn, &adapter, ADAPTER)
                .await
                .map_err(|_| "Bluetooth-адаптер недоступен")?;
            match pxy.set_property("Powered", enabled).await {
                Ok(()) => {
                    if enabled {
                        if meta.connectable.is_some() {
                            let _ = pxy.set_property("Connectable", true).await;
                        }
                        if meta.pairable.is_some() {
                            let _ = pxy.set_property("Pairable", true).await;
                        }
                    }
                    Ok(())
                }
                Err(e) => Err(e.into()),
            }
        }
        Control::ScanBluetooth => {
            if !current.available || !current.powered {
                return Ok(());
            }
            let adapter = primary_adapter_path(conn, current).await?;
            let pxy = proxy(conn, &adapter, ADAPTER)
                .await
                .map_err(|_| "Bluetooth-адаптер недоступен")?;
            if current.discovering && *started_discovery {
                *started_discovery = false;
                let _ = pxy.call::<_, _, ()>("StopDiscovery", &()).await;
            } else {
                *started_discovery = true;
                // Ignore InProgress if discovery is already active on the adapter.
                let _ = pxy.call::<_, _, ()>("StartDiscovery", &()).await;
            }
            Ok(())
        }
        Control::ConnectBluetooth(device_path) => {
            if !current.available || !current.powered {
                return Err("Включите Bluetooth перед подключением".into());
            }
            let Some(dev) = current
                .devices
                .iter()
                .find(|d| d.path == device_path && !d.connected)
            else {
                return Err("Список устройств Bluetooth изменился. Выберите устройство снова.".into());
            };
            let _pairing_guard = PairingGuard::new(policy, &device_path);
            let adapter_path = if !dev.adapter.is_empty() {
                dev.adapter.as_str()
            } else {
                meta.path.as_str()
            };
            if !adapter_path.is_empty() && (meta.connectable.is_some() || meta.pairable.is_some()) {
                if let Ok(adapter_pxy) = proxy(conn, adapter_path, ADAPTER).await {
                    if meta.connectable == Some(false) || (!dev.has_bond() && meta.connectable.is_some()) {
                        let _ = adapter_pxy.set_property("Connectable", true).await;
                    }
                    if !dev.has_bond() && meta.pairable.is_some() {
                        // Forces MGMT_OP_SET_BONDABLE = 1 before Pair() even if PairableTimeout
                        // previously flipped Pairable back to false in the background.
                        let _ = adapter_pxy.set_property("Pairable", true).await;
                    }
                }
            }
            let pxy = proxy(conn, &device_path, DEVICE)
                .await
                .map_err(|_| "Устройство недоступно")?;
            if !dev.paired {
                if let Err(err) = pxy.call::<_, _, ()>("Pair", &()).await {
                    let msg = err.to_string();
                    if !msg.contains("AlreadyExists") {
                        return Err("Не удалось выполнить сопряжение с устройством Bluetooth.".into());
                    }
                }
                let _ = pxy.set_property("Trusted", true).await;
                pxy.call("Connect", &()).await
            } else if dev.unbonded {
                if !dev.trusted {
                    let _ = pxy.set_property("Trusted", true).await;
                }
                match pxy.call::<_, _, ()>("Connect", &()).await {
                    Ok(()) => Ok(()),
                    Err(_) => {
                        if let Err(err) = pxy.call::<_, _, ()>("Pair", &()).await {
                            let msg = err.to_string();
                            if !msg.contains("AlreadyExists") {
                                return Err("Не удалось выполнить сопряжение с устройством Bluetooth.".into());
                            }
                        }
                        let _ = pxy.set_property("Trusted", true).await;
                        pxy.call("Connect", &()).await
                    }
                }
            } else {
                if !dev.trusted {
                    let _ = pxy.set_property("Trusted", true).await;
                }
                pxy.call("Connect", &()).await
            }
        }
        Control::DisconnectBluetooth(device_path) => {
            let Some(dev) = current.devices.iter().find(|d| d.path == device_path) else {
                return Err("Устройство уже отключено".into());
            };
            if !dev.connected {
                // «Отменить» for a pairing or connection attempt that is still pending in BlueZ.
                cancel_connect(conn, &device_path).await;
                return Ok(());
            }
            proxy(conn, &device_path, DEVICE)
                .await
                .map_err(|_| "Устройство уже отключено")?
                .call("Disconnect", &())
                .await
        }
        Control::ForgetBluetooth(device_path) => {
            // Any remembered device can be forgotten, including a Trusted one whose bond was lost
            // (Paired = false): otherwise it stays in BlueZ and blocks a fresh pairing.
            let Some(dev) = current
                .devices
                .iter()
                .find(|d| d.path == device_path && d.remembered())
            else {
                return Err("Сохранённое устройство уже удалено".into());
            };
            let dev_path =
                OwnedObjectPath::try_from(device_path.as_str()).map_err(|_| "Некорректный путь")?;
            proxy(conn, &dev.adapter, ADAPTER)
                .await
                .map_err(|_| "Bluetooth-адаптер недоступен")?
                .call("RemoveDevice", &(dev_path,))
                .await
        }
        _ => Ok(()),
    };
    result.map_err(|err| {
        let msg = err.to_string();
        if msg.contains("AuthenticationFailed")
            || msg.contains("AuthenticationRejected")
            || msg.contains("KeyMissing")
            || msg.contains("Key Missing")
        {
            "Ключ сопряжения отклонён устройством. Нажмите «Забыть», переведите аксессуар в режим сопряжения и подключите заново.".into()
        } else {
            "BlueZ отклонил действие. Проверьте, что устройство включено и находится рядом.".into()
        }
    })
}

#[cfg(test)]
async fn action(
    conn: &zbus::Connection,
    command: Control,
    current: &Bluetooth,
    started_discovery: &mut bool,
) -> Result<(), String> {
    let policy = Arc::new(Mutex::new(AgentPolicy::default()));
    sync_policy_remembered(&policy, current);
    action_with_context(
        conn,
        command,
        current,
        &AdapterMeta::default(),
        &policy,
        started_discovery,
    )
    .await
}

/// Best effort: abort a pending `Pair` and a pending `Connect` (BlueZ cancels it on `Disconnect`).
async fn cancel_connect(conn: &zbus::Connection, device_path: &str) {
    if let Ok(pxy) = proxy(conn, device_path, DEVICE).await {
        let _ = pxy.call::<_, _, ()>("CancelPairing", &()).await;
        let _ = pxy.call::<_, _, ()>("Disconnect", &()).await;
    }
}

fn cancels_connect(command: &Control, device_path: &str) -> bool {
    match command {
        Control::DisconnectBluetooth(p) | Control::ForgetBluetooth(p) => p == device_path,
        Control::Bluetooth(enabled) => !enabled,
        _ => false,
    }
}

/// `Pair` + `Connect` wait for the accessory. Meanwhile the worker keeps reading commands: a
/// Disconnect («Отменить»), Forget or power-off of this device aborts the attempt, and any other
/// command waits in `pending` until the attempt finishes.
async fn connect_cancellable_with_context(
    conn: &zbus::Connection,
    device_path: String,
    current: &Bluetooth,
    meta: &AdapterMeta,
    policy: &Arc<Mutex<AgentPolicy>>,
    started_discovery: &mut bool,
    commands: &async_channel::Receiver<Control>,
    pending: &mut VecDeque<Control>,
) -> Result<(), String> {
    enum Outcome {
        Done(Result<(), String>),
        Cancel(Control),
    }
    let outcome = future::race(
        async {
            let command = Control::ConnectBluetooth(device_path.clone());
            Outcome::Done(
                action_with_context(conn, command, current, meta, policy, started_discovery).await,
            )
        },
        async {
            loop {
                match commands.recv().await {
                    Ok(c) if cancels_connect(&c, &device_path) => return Outcome::Cancel(c),
                    Ok(c) => pending.push_back(c),
                    // Closed: let the attempt finish; the main loop notices the closed channel.
                    Err(_) => future::pending::<()>().await,
                }
            }
        },
    )
    .await;
    match outcome {
        Outcome::Done(result) => result,
        Outcome::Cancel(command) => {
            cancel_connect(conn, &device_path).await;
            if matches!(command, Control::DisconnectBluetooth(_)) {
                Ok(())
            } else {
                action_with_context(conn, command, current, meta, policy, started_discovery).await
            }
        }
    }
}

#[cfg(test)]
async fn connect_cancellable(
    conn: &zbus::Connection,
    device_path: String,
    current: &Bluetooth,
    started_discovery: &mut bool,
    commands: &async_channel::Receiver<Control>,
    pending: &mut VecDeque<Control>,
) -> Result<(), String> {
    let policy = Arc::new(Mutex::new(AgentPolicy::default()));
    sync_policy_remembered(&policy, current);
    connect_cancellable_with_context(
        conn,
        device_path,
        current,
        &AdapterMeta::default(),
        &policy,
        started_discovery,
        commands,
        pending,
    )
    .await
}

fn relevant(message: &zbus::Message, detailed: bool) -> bool {
    let h = message.header();
    let Some(member) = h.member().map(|m| m.as_str()) else {
        return false;
    };
    if matches!(member, "InterfacesAdded" | "InterfacesRemoved") {
        return true;
    }
    if member == "PropertiesChanged" {
        if let Ok((iface, changed, invalidated)) =
            message.body().deserialize::<(String, Props, Vec<String>)>()
        {
            let watched: &[&str] = if iface == ADAPTER && detailed {
                &["Powered", "Discovering", "Alias", "Name"]
            } else if iface == ADAPTER {
                &["Powered", "Discovering"]
            } else if iface == DEVICE && detailed {
                &[
                    "Connected",
                    "Paired",
                    "Bonded",
                    "Trusted",
                    "Alias",
                    "Name",
                    "Icon",
                    "RSSI",
                ]
            } else if iface == DEVICE {
                &["Connected", "Paired", "Bonded", "Alias", "Name"]
            } else if iface == BATTERY && detailed {
                &["Percentage"]
            } else {
                &[]
            };
            return watched
                .iter()
                .any(|p| changed.contains_key(*p) || invalidated.iter().any(|v| v == p));
        }
    }
    false
}

pub async fn run(commands: async_channel::Receiver<Control>, updates: channel::SyncSender<Update>) {
    let mut detailed = false;
    loop {
        let result = connected(&commands, &updates, &mut detailed).await;
        if result.is_err() {
            if updates
                .send(Update::Bluetooth(Bluetooth {
                    error: Some("Служба Bluetooth недоступна".into()),
                    ..Bluetooth::default()
                }))
                .is_err()
            {
                return;
            }
        } else {
            return;
        }
        while let Ok(command) = commands.recv().await {
            match command {
                Control::WatchBluetooth(watch) => {
                    detailed = watch;
                    if watch {
                        break;
                    }
                }
                Control::Refresh | Control::RefreshBluetooth => break,
                _ => {}
            }
        }
        if commands.is_closed() {
            return;
        }
    }
}

async fn connected(
    commands: &async_channel::Receiver<Control>,
    updates: &channel::SyncSender<Update>,
    detailed: &mut bool,
) -> zbus::Result<()> {
    let policy = Arc::new(Mutex::new(AgentPolicy::default()));
    let conn = zbus::connection::Builder::system()?
        .method_timeout(Duration::from_secs(25))
        .serve_at(AGENT_PATH, BtAgent(policy.clone()))?
        .build()
        .await?;
    register_default_agent(&conn, &policy).await;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(BLUEZ)?
        .build();
    let mut events = zbus::MessageStream::for_match_rule(rule, &conn, Some(64)).await?;
    let owner_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(BLUEZ)?
        .build();
    let mut owners = zbus::MessageStream::for_match_rule(owner_rule, &conn, Some(4)).await?;
    let mut state = Bluetooth::default();
    let mut meta = AdapterMeta::default();
    let mut configured_adapter: Option<String> = None;
    let mut started_discovery = false;
    let mut command = None;
    let mut pending = VecDeque::new();
    loop {
        // The UI marks a device «Подключение…» itself; it learns the attempt is over only from an
        // update, so one is sent after every action even if BlueZ state did not change.
        let mut acted = false;
        if let Some(c) = command.take() {
            if let Control::WatchBluetooth(watch) = c {
                *detailed = watch;
                if !watch {
                    stop_discovery_if_needed(&conn, &state, &mut started_discovery).await;
                }
            } else {
                acted = !matches!(c, Control::Refresh | Control::RefreshBluetooth);
                let result = if let Control::ConnectBluetooth(path) = c {
                    let (s, p) = (&mut started_discovery, &mut pending);
                    connect_cancellable_with_context(&conn, path, &state, &meta, &policy, s, commands, p).await
                } else {
                    action_with_context(&conn, c, &state, &meta, &policy, &mut started_discovery).await
                };
                if let Err(e) = result {
                    if updates.send(Update::Error("Bluetooth", e)).is_err() {
                        return Ok(());
                    }
                }
            }
        }
        // If BlueZ is not running on the system, treat it as "no adapter" unless detailed view asked.
        let (mut next, mut next_meta) = snapshot_with_meta(&conn).await.unwrap_or_default();
        sync_policy_remembered(&policy, &next);
        if !next_meta.powered || next_meta.path.is_empty() {
            configured_adapter = None;
        } else if configured_adapter.as_deref() != Some(next_meta.path.as_str()) {
            configured_adapter = Some(next_meta.path.clone());
            if ensure_adapter_ready(&conn, &next_meta).await {
                if let Ok((refreshed, refreshed_meta)) = snapshot_with_meta(&conn).await {
                    next = refreshed;
                    next_meta = refreshed_meta;
                    sync_policy_remembered(&policy, &next);
                }
            }
        }
        meta = next_meta;
        if !next.discovering {
            started_discovery = false;
        }
        if next != state || acted {
            state = next;
            if updates.send(Update::Bluetooth(state.clone())).is_err() {
                return Ok(());
            }
        }
        if let Some(c) = pending.pop_front() {
            command = Some(c);
            continue;
        }
        enum Event {
            Command(Result<Control, async_channel::RecvError>),
            Signal(Option<zbus::Result<zbus::Message>>),
            Owner(Option<zbus::Result<zbus::Message>>),
        }
        loop {
            let event = future::race(
                async { Event::Command(commands.recv().await) },
                future::race(async { Event::Signal(events.next().await) }, async {
                    Event::Owner(owners.next().await)
                }),
            )
            .await;
            match event {
                Event::Command(Ok(c)) => {
                    command = Some(c);
                    break;
                }
                Event::Command(Err(_)) => {
                    stop_discovery_if_needed(&conn, &state, &mut started_discovery).await;
                    return Ok(());
                }
                Event::Owner(Some(Ok(_))) => {
                    configured_adapter = None;
                    register_default_agent(&conn, &policy).await;
                    break;
                }
                Event::Owner(_) => {
                    return Err(zbus::Error::Failure("Bus signal stream closed".into()))
                }
                Event::Signal(Some(Ok(message))) if relevant(&message, *detailed) => break,
                Event::Signal(None | Some(Err(_))) => {
                    return Err(zbus::Error::Failure("BlueZ signal stream closed".into()))
                }
                Event::Signal(_) => {}
            }
        }
        for _ in 0..64 {
            if !matches!(future::poll_once(events.next()).await, Some(Some(Ok(_)))) {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_placeholder_filter_hides_unnamed_ble_beacons_but_keeps_named_or_paired_devices() {
        assert!(is_mac_placeholder("AA:BB:CC:DD:EE:FF", "AA:BB:CC:DD:EE:FF"));
        assert!(is_mac_placeholder("AA-BB-CC-DD-EE-FF", "AA:BB:CC:DD:EE:FF"));
        assert!(is_mac_placeholder("12-34-56-78-9A-BC", ""));
        assert!(!is_mac_placeholder("AirPods Pro", "AA:BB:CC:DD:EE:FF"));
        assert!(!is_mac_placeholder("Keychron K2", "12:34:56:78:9A:BC"));

        let mut objects = ManagedObjects::new();
        objects.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0").unwrap(),
            HashMap::from([(
                ADAPTER.to_string(),
                HashMap::from([
                    ("Powered".into(), OwnedValue::from(true)),
                    ("Discovering".into(), OwnedValue::from(true)),
                    ("Alias".into(), Value::from("Feather Host").try_into().unwrap()),
                ]),
            )]),
        );
        // Unnamed BLE beacon -> filtered out
        objects.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0/dev_11_22_33_44_55_66").unwrap(),
            HashMap::from([(
                DEVICE.to_string(),
                HashMap::from([
                    ("Address".into(), Value::from("11:22:33:44:55:66").try_into().unwrap()),
                    ("Alias".into(), Value::from("11-22-33-44-55-66").try_into().unwrap()),
                    ("Paired".into(), OwnedValue::from(false)),
                    ("Connected".into(), OwnedValue::from(false)),
                ]),
            )]),
        );
        // Paired unnamed device -> kept
        objects.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01").unwrap(),
            HashMap::from([(
                DEVICE.to_string(),
                HashMap::from([
                    ("Address".into(), Value::from("AA:BB:CC:DD:EE:01").try_into().unwrap()),
                    ("Alias".into(), Value::from("AA-BB-CC-DD-EE-01").try_into().unwrap()),
                    ("Paired".into(), OwnedValue::from(true)),
                    ("Connected".into(), OwnedValue::from(false)),
                ]),
            )]),
        );
        // Connected headphones with battery
        objects.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02").unwrap(),
            HashMap::from([
                (
                    DEVICE.to_string(),
                    HashMap::from([
                        ("Address".into(), Value::from("AA:BB:CC:DD:EE:02").try_into().unwrap()),
                        ("Name".into(), Value::from("AirPods Pro").try_into().unwrap()),
                        ("Alias".into(), Value::from("AirPods Pro").try_into().unwrap()),
                        ("Icon".into(), Value::from("audio-headphones").try_into().unwrap()),
                        ("Paired".into(), OwnedValue::from(true)),
                        ("Trusted".into(), OwnedValue::from(true)),
                        ("Connected".into(), OwnedValue::from(true)),
                    ]),
                ),
                (
                    BATTERY.to_string(),
                    HashMap::from([("Percentage".into(), OwnedValue::from(85u8))]),
                ),
            ]),
        );

        let bt = parse_objects(&objects);
        assert!(bt.available);
        assert!(bt.powered);
        assert!(bt.discovering);
        assert_eq!(bt.connected.as_deref(), Some("AirPods Pro"));
        assert_eq!(bt.subtitle(), "AirPods Pro");
        assert_eq!(bt.devices.len(), 2);
        assert_eq!(bt.devices[0].name, "AirPods Pro");
        assert_eq!(bt.devices[0].battery, Some(85));
        assert_eq!(bt.devices[0].kind_label(), "Наушники");
        assert_eq!(bt.devices[1].address, "AA:BB:CC:DD:EE:01");
    }

    #[test]
    fn signal_filter_ignores_background_rssi_when_bluetooth_view_is_closed() {
        let mut changed = Props::new();
        changed.insert("RSSI".into(), OwnedValue::from(-58i16));
        let msg = zbus::Message::signal(
            "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .unwrap()
        .build(&(DEVICE.to_string(), changed, Vec::<String>::new()))
        .unwrap();
        assert!(!relevant(&msg, false));
        assert!(relevant(&msg, true));
    }

    /// Both mock tests own `org.bluez` on the private session bus, one at a time.
    static BUS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn private_bus_bluez_object_manager_and_device_actions() {
        use std::sync::{Arc, Mutex};
        let _bus = BUS.lock().unwrap_or_else(|e| e.into_inner());
        #[derive(Default)]
        struct State {
            powered: bool,
            discovering: bool,
            paired_mouse: bool,
            trusted_mouse: bool,
            connected_headphones: bool,
            connected_mouse: bool,
            removed: Vec<String>,
        }
        struct MockRoot(Arc<Mutex<State>>);
        #[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
        impl MockRoot {
            fn get_managed_objects(&self) -> ManagedObjects {
                let s = self.0.lock().unwrap();
                let mut out = ManagedObjects::new();
                out.insert(
                    OwnedObjectPath::try_from("/org/bluez/hci0").unwrap(),
                    HashMap::from([(
                        ADAPTER.to_string(),
                        HashMap::from([
                            ("Powered".into(), OwnedValue::from(s.powered)),
                            ("Discovering".into(), OwnedValue::from(s.discovering)),
                            ("Alias".into(), Value::from("Feather BT").try_into().unwrap()),
                        ]),
                    )]),
                );
                if !s.removed.iter().any(|r| r == "/org/bluez/hci0/dev_AA_BB_CC_11_22_33") {
                    out.insert(
                        OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_11_22_33").unwrap(),
                        HashMap::from([
                            (
                                DEVICE.to_string(),
                                HashMap::from([
                                    ("Adapter".into(), Value::from(OwnedObjectPath::try_from("/org/bluez/hci0").unwrap()).try_into().unwrap()),
                                    ("Address".into(), Value::from("AA:BB:CC:11:22:33").try_into().unwrap()),
                                    ("Name".into(), Value::from("Sony WH-1000XM5").try_into().unwrap()),
                                    ("Alias".into(), Value::from("Sony WH-1000XM5").try_into().unwrap()),
                                    ("Icon".into(), Value::from("audio-headphones").try_into().unwrap()),
                                    ("Paired".into(), OwnedValue::from(true)),
                                    ("Trusted".into(), OwnedValue::from(true)),
                                    ("Connected".into(), OwnedValue::from(s.connected_headphones)),
                                ]),
                            ),
                            (
                                BATTERY.to_string(),
                                HashMap::from([("Percentage".into(), OwnedValue::from(90u8))]),
                            ),
                        ]),
                    );
                }
                out.insert(
                    OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_44_55_66").unwrap(),
                    HashMap::from([(
                        DEVICE.to_string(),
                        HashMap::from([
                            ("Adapter".into(), Value::from(OwnedObjectPath::try_from("/org/bluez/hci0").unwrap()).try_into().unwrap()),
                            ("Address".into(), Value::from("AA:BB:CC:44:55:66").try_into().unwrap()),
                            ("Name".into(), Value::from("Magic Trackpad").try_into().unwrap()),
                            ("Alias".into(), Value::from("Magic Trackpad").try_into().unwrap()),
                            ("Icon".into(), Value::from("input-mouse").try_into().unwrap()),
                            ("Paired".into(), OwnedValue::from(s.paired_mouse)),
                            ("Trusted".into(), OwnedValue::from(s.trusted_mouse)),
                            ("Connected".into(), OwnedValue::from(s.connected_mouse)),
                            ("RSSI".into(), OwnedValue::from(-52i16)),
                        ]),
                    )]),
                );
                out
            }
        }
        struct MockAdapter(Arc<Mutex<State>>);
        #[zbus::interface(name = "org.bluez.Adapter1")]
        impl MockAdapter {
            #[zbus(property)]
            fn powered(&self) -> bool {
                self.0.lock().unwrap().powered
            }
            #[zbus(property)]
            fn set_powered(&mut self, v: bool) {
                self.0.lock().unwrap().powered = v;
            }
            #[zbus(property)]
            fn discovering(&self) -> bool {
                self.0.lock().unwrap().discovering
            }
            fn start_discovery(&mut self) {
                self.0.lock().unwrap().discovering = true;
            }
            fn stop_discovery(&mut self) {
                self.0.lock().unwrap().discovering = false;
            }
            fn remove_device(&mut self, device: OwnedObjectPath) {
                self.0.lock().unwrap().removed.push(device.to_string());
            }
        }
        struct MockDevice {
            state: Arc<Mutex<State>>,
            is_mouse: bool,
        }
        #[zbus::interface(name = "org.bluez.Device1")]
        impl MockDevice {
            fn connect(&mut self) {
                let mut s = self.state.lock().unwrap();
                if self.is_mouse {
                    s.connected_mouse = true;
                } else {
                    s.connected_headphones = true;
                }
            }
            fn disconnect(&mut self) {
                let mut s = self.state.lock().unwrap();
                if self.is_mouse {
                    s.connected_mouse = false;
                } else {
                    s.connected_headphones = false;
                }
            }
            fn pair(&mut self) {
                if self.is_mouse {
                    self.state.lock().unwrap().paired_mouse = true;
                }
            }
            #[zbus(property)]
            fn trusted(&self) -> bool {
                let s = self.state.lock().unwrap();
                if self.is_mouse { s.trusted_mouse } else { true }
            }
            #[zbus(property)]
            fn set_trusted(&mut self, v: bool) {
                if self.is_mouse {
                    self.state.lock().unwrap().trusted_mouse = v;
                }
            }
        }

        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return;
        }
        let state = Arc::new(Mutex::new(State {
            powered: true,
            discovering: false,
            paired_mouse: false,
            trusted_mouse: false,
            connected_headphones: true,
            connected_mouse: false,
            removed: Vec::new(),
        }));
        future::block_on(async {
            let _server = zbus::connection::Builder::session()
                .unwrap()
                .name(BLUEZ)
                .unwrap()
                .serve_at("/", MockRoot(state.clone()))
                .unwrap()
                .serve_at("/org/bluez/hci0", MockAdapter(state.clone()))
                .unwrap()
                .serve_at(
                    "/org/bluez/hci0/dev_AA_BB_CC_11_22_33",
                    MockDevice {
                        state: state.clone(),
                        is_mouse: false,
                    },
                )
                .unwrap()
                .serve_at(
                    "/org/bluez/hci0/dev_AA_BB_CC_44_55_66",
                    MockDevice {
                        state: state.clone(),
                        is_mouse: true,
                    },
                )
                .unwrap()
                .build()
                .await
                .unwrap();
            let conn = zbus::Connection::session().await.unwrap();

            let snap = snapshot(&conn).await.unwrap();
            assert!(snap.available && snap.powered && !snap.discovering);
            assert_eq!(snap.connected.as_deref(), Some("Sony WH-1000XM5"));
            assert_eq!(snap.devices.len(), 2);
            assert_eq!(snap.devices[0].name, "Sony WH-1000XM5");
            assert_eq!(snap.devices[0].battery, Some(90));
            assert!(snap.devices[0].connected && snap.devices[0].paired);
            assert_eq!(snap.devices[1].name, "Magic Trackpad");
            assert!(!snap.devices[1].paired && !snap.devices[1].connected);

            let mut started_discovery = false;
            // Start discovery
            action(&conn, Control::ScanBluetooth, &snap, &mut started_discovery)
                .await
                .unwrap();
            assert!(started_discovery);
            assert!(state.lock().unwrap().discovering);

            // Connect unpaired device -> pairs, trusts, and connects
            action(
                &conn,
                Control::ConnectBluetooth("/org/bluez/hci0/dev_AA_BB_CC_44_55_66".into()),
                &snap,
                &mut started_discovery,
            )
            .await
            .unwrap();
            {
                let s = state.lock().unwrap();
                assert!(s.paired_mouse && s.trusted_mouse && s.connected_mouse);
            }

            // Disconnect headphones
            action(
                &conn,
                Control::DisconnectBluetooth("/org/bluez/hci0/dev_AA_BB_CC_11_22_33".into()),
                &snap,
                &mut started_discovery,
            )
            .await
            .unwrap();
            assert!(!state.lock().unwrap().connected_headphones);

            // Forget paired device
            action(
                &conn,
                Control::ForgetBluetooth("/org/bluez/hci0/dev_AA_BB_CC_11_22_33".into()),
                &snap,
                &mut started_discovery,
            )
            .await
            .unwrap();
            assert_eq!(
                state.lock().unwrap().removed,
                vec!["/org/bluez/hci0/dev_AA_BB_CC_11_22_33".to_string()]
            );

            // Power off stops discovery and sets Powered=false
            let snap2 = snapshot(&conn).await.unwrap();
            action(&conn, Control::Bluetooth(false), &snap2, &mut started_discovery)
                .await
                .unwrap();
            assert!(!started_discovery);
            assert!(!state.lock().unwrap().discovering);
            assert!(!state.lock().unwrap().powered);

            // Stale device path is rejected before calling D-Bus
            assert!(action(
                &conn,
                Control::ConnectBluetooth("/org/bluez/hci0/dev_STALE".into()),
                &snap,
                &mut started_discovery,
            )
            .await
            .is_err());
        });
    }

    #[test]
    fn private_bus_forget_trusted_unpaired_device_cancels_pending_pairing() {
        use std::sync::{Arc, Mutex};
        const DEV: &str = "/org/bluez/hci0/dev_2C_BE_EE_00_00_01";
        #[derive(Default)]
        struct State {
            cancelled: u32,
            disconnects: u32,
            removed: Vec<String>,
        }
        struct MockRoot(Arc<Mutex<State>>);
        #[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
        impl MockRoot {
            fn get_managed_objects(&self) -> ManagedObjects {
                let adapter = OwnedObjectPath::try_from("/org/bluez/hci0").unwrap();
                let mut out = ManagedObjects::new();
                out.insert(
                    adapter.clone(),
                    HashMap::from([(
                        ADAPTER.to_string(),
                        HashMap::from([("Powered".into(), OwnedValue::from(true))]),
                    )]),
                );
                if self.0.lock().unwrap().removed.is_empty() {
                    // The stuck state seen on real hardware: remembered and trusted, bond lost.
                    out.insert(
                        OwnedObjectPath::try_from(DEV).unwrap(),
                        HashMap::from([(
                            DEVICE.to_string(),
                            HashMap::from([
                                ("Adapter".into(), Value::from(adapter).try_into().unwrap()),
                                ("Address".into(), Value::from("2C:BE:EE:00:00:01").try_into().unwrap()),
                                ("Alias".into(), Value::from("CMF Buds Pro 2").try_into().unwrap()),
                                ("Icon".into(), Value::from("audio-headset").try_into().unwrap()),
                                ("Paired".into(), OwnedValue::from(false)),
                                ("Trusted".into(), OwnedValue::from(true)),
                                ("Connected".into(), OwnedValue::from(false)),
                            ]),
                        )]),
                    );
                }
                out
            }
        }
        struct MockAdapter(Arc<Mutex<State>>);
        #[zbus::interface(name = "org.bluez.Adapter1")]
        impl MockAdapter {
            fn remove_device(&self, device: OwnedObjectPath) {
                self.0.lock().unwrap().removed.push(device.to_string());
            }
        }
        struct MockDevice {
            state: Arc<Mutex<State>>,
            cancel: (async_channel::Sender<()>, async_channel::Receiver<()>),
        }
        #[zbus::interface(name = "org.bluez.Device1")]
        impl MockDevice {
            /// Headphones that never answer: only CancelPairing ends the call.
            async fn pair(&self) -> zbus::fdo::Result<()> {
                let _ = self.cancel.1.recv().await;
                Err(zbus::fdo::Error::Failed("AuthenticationCanceled".into()))
            }
            fn cancel_pairing(&self) {
                self.state.lock().unwrap().cancelled += 1;
                let _ = self.cancel.0.try_send(());
            }
            fn disconnect(&self) {
                self.state.lock().unwrap().disconnects += 1;
            }
            fn connect(&self) {}
        }

        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return;
        }
        let _bus = BUS.lock().unwrap_or_else(|e| e.into_inner());
        let state = Arc::new(Mutex::new(State::default()));
        let cancel = async_channel::bounded(1);
        let drain = cancel.1.clone();
        future::block_on(async {
            let _server = zbus::connection::Builder::session()
                .unwrap()
                .name(BLUEZ)
                .unwrap()
                .serve_at("/", MockRoot(state.clone()))
                .unwrap()
                .serve_at("/org/bluez/hci0", MockAdapter(state.clone()))
                .unwrap()
                .serve_at(DEV, MockDevice { state: state.clone(), cancel })
                .unwrap()
                .build()
                .await
                .unwrap();
            let conn = zbus::Connection::session().await.unwrap();
            let snap = snapshot(&conn).await.unwrap();
            assert_eq!(snap.devices.len(), 1);
            let dev = &snap.devices[0];
            assert!(!dev.paired && dev.trusted && dev.remembered());

            // «Отменить» for an attempt that is not running in this worker is a harmless no-op.
            let mut started_discovery = false;
            action(&conn, Control::DisconnectBluetooth(DEV.into()), &snap, &mut started_discovery)
                .await
                .unwrap();
            assert_eq!(state.lock().unwrap().cancelled, 1);
            while drain.try_recv().is_ok() {}

            // Pair hangs; a Refresh queues behind it and Forget aborts it and removes the device.
            let (tx, rx) = async_channel::bounded(16);
            let mut pending = VecDeque::new();
            let (result, ()) = future::zip(
                connect_cancellable(&conn, DEV.into(), &snap, &mut started_discovery, &rx, &mut pending),
                async {
                    tx.send(Control::RefreshBluetooth).await.unwrap();
                    tx.send(Control::ForgetBluetooth(DEV.into())).await.unwrap();
                },
            )
            .await;
            result.unwrap();
            {
                let s = state.lock().unwrap();
                assert_eq!(s.cancelled, 2);
                assert_eq!(s.disconnects, 2);
                assert_eq!(s.removed, vec![DEV.to_string()]);
            }
            assert!(matches!(pending.pop_front(), Some(Control::RefreshBluetooth)));
            assert!(pending.is_empty());
            assert!(snapshot(&conn).await.unwrap().devices.is_empty());
        });
    }

    #[test]
    fn private_bus_registers_agent_enables_bondable_and_persists_bond() {
        use std::sync::{Arc, Mutex};
        const DEV: &str = "/org/bluez/hci0/dev_AC_47_1B_7C_85_D2";
        const STRANGER: &str = "/org/bluez/hci0/dev_FF_EE_DD_CC_BB_AA";

        #[derive(Default)]
        struct State {
            connectable: bool,
            pairable: bool,
            agent_sender: Option<String>,
            agent_path: Option<String>,
            agent_cap: Option<String>,
            default_agent: Option<String>,
            paired: bool,
            bonded: bool,
            trusted: bool,
            connected: bool,
            confirmed: u32,
            authorized_services: Vec<String>,
        }

        struct MockRoot(Arc<Mutex<State>>);
        #[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
        impl MockRoot {
            fn get_managed_objects(&self) -> ManagedObjects {
                let s = self.0.lock().unwrap();
                let adapter = OwnedObjectPath::try_from("/org/bluez/hci0").unwrap();
                let mut out = ManagedObjects::new();
                out.insert(
                    adapter.clone(),
                    HashMap::from([(
                        ADAPTER.to_string(),
                        HashMap::from([
                            ("Powered".into(), OwnedValue::from(true)),
                            ("Discovering".into(), OwnedValue::from(false)),
                            ("Connectable".into(), OwnedValue::from(s.connectable)),
                            ("Pairable".into(), OwnedValue::from(s.pairable)),
                            ("Alias".into(), Value::from("napsy").try_into().unwrap()),
                        ]),
                    )]),
                );
                out.insert(
                    OwnedObjectPath::try_from(DEV).unwrap(),
                    HashMap::from([(
                        DEVICE.to_string(),
                        HashMap::from([
                            ("Adapter".into(), Value::from(adapter).try_into().unwrap()),
                            ("Address".into(), Value::from("AC:47:1B:7C:85:D2").try_into().unwrap()),
                            ("Name".into(), Value::from("HUAWEI FreeBuds SE 2").try_into().unwrap()),
                            ("Alias".into(), Value::from("HUAWEI FreeBuds SE 2").try_into().unwrap()),
                            ("Icon".into(), Value::from("audio-headset").try_into().unwrap()),
                            ("Paired".into(), OwnedValue::from(s.paired)),
                            ("Bonded".into(), OwnedValue::from(s.bonded)),
                            ("Trusted".into(), OwnedValue::from(s.trusted)),
                            ("Connected".into(), OwnedValue::from(s.connected)),
                        ]),
                    )]),
                );
                out
            }
        }

        struct MockAgentManager(Arc<Mutex<State>>);
        #[zbus::interface(name = "org.bluez.AgentManager1")]
        impl MockAgentManager {
            fn register_agent(
                &self,
                agent: OwnedObjectPath,
                capability: String,
                #[zbus(header)] header: Header<'_>,
            ) {
                let mut s = self.0.lock().unwrap();
                s.agent_sender = header.sender().map(|u| u.as_str().to_string());
                s.agent_path = Some(agent.to_string());
                s.agent_cap = Some(capability);
            }
            fn request_default_agent(&self, agent: OwnedObjectPath) {
                self.0.lock().unwrap().default_agent = Some(agent.to_string());
            }
            fn unregister_agent(&self, _agent: OwnedObjectPath) {}
        }

        struct MockAdapter(Arc<Mutex<State>>);
        #[zbus::interface(name = "org.bluez.Adapter1")]
        impl MockAdapter {
            #[zbus(property)]
            fn connectable(&self) -> bool {
                self.0.lock().unwrap().connectable
            }
            #[zbus(property)]
            fn set_connectable(&mut self, v: bool) {
                self.0.lock().unwrap().connectable = v;
            }
            #[zbus(property)]
            fn pairable(&self) -> bool {
                self.0.lock().unwrap().pairable
            }
            #[zbus(property)]
            fn set_pairable(&mut self, v: bool) {
                self.0.lock().unwrap().pairable = v;
            }
        }

        struct MockDevice(Arc<Mutex<State>>);
        #[zbus::interface(name = "org.bluez.Device1")]
        impl MockDevice {
            async fn pair(&self, #[zbus(connection)] conn: &zbus::Connection) -> zbus::fdo::Result<()> {
                let (pairable, sender, path) = {
                    let s = self.0.lock().unwrap();
                    (
                        s.pairable,
                        s.agent_sender.clone(),
                        s.default_agent.clone(),
                    )
                };
                let mut confirmed_ok = false;
                if let (Some(dest), Some(agent_path)) = (sender, path) {
                    let dev = OwnedObjectPath::try_from(DEV).unwrap();
                    confirmed_ok = conn
                        .call_method(
                            Some(dest.as_str()),
                            agent_path.as_str(),
                            Some("org.bluez.Agent1"),
                            "RequestConfirmation",
                            &(dev, 482910u32),
                        )
                        .await
                        .is_ok();
                }
                let mut s = self.0.lock().unwrap();
                s.paired = true;
                // Mirrors Linux kernel HCI_BONDABLE + BlueZ agent behavior:
                // persistent bond is saved only when Pairable (Bondable) is true and Agent1 confirms.
                s.bonded = pairable && confirmed_ok;
                if confirmed_ok {
                    s.confirmed += 1;
                }
                Ok(())
            }
            async fn connect(&self, #[zbus(connection)] conn: &zbus::Connection) -> zbus::fdo::Result<()> {
                let (sender, path) = {
                    let s = self.0.lock().unwrap();
                    (s.agent_sender.clone(), s.default_agent.clone())
                };
                let (Some(dest), Some(agent_path)) = (sender, path) else {
                    return Err(zbus::fdo::Error::Failed("No agent".into()));
                };
                let dev = OwnedObjectPath::try_from(DEV).unwrap();
                let uuid = "0000110b-0000-1000-8000-00805f9b34fb".to_string();
                conn.call_method(
                    Some(dest.as_str()),
                    agent_path.as_str(),
                    Some("org.bluez.Agent1"),
                    "AuthorizeService",
                    &(dev, uuid.clone()),
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                let mut s = self.0.lock().unwrap();
                s.authorized_services.push(uuid);
                s.connected = true;
                Ok(())
            }
            #[zbus(property)]
            fn trusted(&self) -> bool {
                self.0.lock().unwrap().trusted
            }
            #[zbus(property)]
            fn set_trusted(&mut self, v: bool) {
                self.0.lock().unwrap().trusted = v;
            }
        }

        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return;
        }
        let _bus = BUS.lock().unwrap_or_else(|e| e.into_inner());
        let state = Arc::new(Mutex::new(State::default()));
        future::block_on(async {
            let server = zbus::connection::Builder::session()
                .unwrap()
                .name(BLUEZ)
                .unwrap()
                .serve_at("/", MockRoot(state.clone()))
                .unwrap()
                .serve_at(BLUEZ_PATH, MockAgentManager(state.clone()))
                .unwrap()
                .serve_at("/org/bluez/hci0", MockAdapter(state.clone()))
                .unwrap()
                .serve_at(DEV, MockDevice(state.clone()))
                .unwrap()
                .build()
                .await
                .unwrap();

            let policy = Arc::new(Mutex::new(AgentPolicy::default()));
            let conn = zbus::connection::Builder::session()
                .unwrap()
                .method_timeout(Duration::from_secs(5))
                .serve_at(AGENT_PATH, BtAgent(policy.clone()))
                .unwrap()
                .build()
                .await
                .unwrap();

            register_default_agent(&conn, &policy).await;
            {
                let s = state.lock().unwrap();
                assert_eq!(s.agent_path.as_deref(), Some(AGENT_PATH));
                assert_eq!(s.agent_cap.as_deref(), Some(AGENT_CAPABILITY));
                assert_eq!(s.default_agent.as_deref(), Some(AGENT_PATH));
            }

            // Initial startup brings a non-connectable / non-pairable adapter ready.
            let (_, meta0) = snapshot_with_meta(&conn).await.unwrap();
            assert_eq!(meta0.connectable, Some(false));
            assert_eq!(meta0.pairable, Some(false));
            assert!(ensure_adapter_ready(&conn, &meta0).await);
            assert!(state.lock().unwrap().connectable);
            assert!(state.lock().unwrap().pairable);

            // Unsolicited incoming pairing from an unknown device is rejected by BtAgent.
            let client_dest = state.lock().unwrap().agent_sender.clone().unwrap();
            let stranger_res = server
                .call_method(
                    Some(client_dest.as_str()),
                    AGENT_PATH,
                    Some("org.bluez.Agent1"),
                    "RequestConfirmation",
                    &(OwnedObjectPath::try_from(STRANGER).unwrap(), 123456u32),
                )
                .await;
            assert!(stranger_res.is_err(), "unsolicited stranger pairing must be rejected");

            // Simulate PairableTimeout dropping Pairable back to false right before user clicks Connect:
            // action_with_context re-enables Pairable before Pair(), answers RequestConfirmation &
            // AuthorizeService without deadlocking, and achieves Bonded == true.
            state.lock().unwrap().pairable = false;
            let (snap1, meta1) = snapshot_with_meta(&conn).await.unwrap();
            sync_policy_remembered(&policy, &snap1);
            let mut started_discovery = false;
            action_with_context(
                &conn,
                Control::ConnectBluetooth(DEV.into()),
                &snap1,
                &meta1,
                &policy,
                &mut started_discovery,
            )
            .await
            .unwrap();

            assert!(policy.lock().unwrap().active_target.is_none());
            {
                let s = state.lock().unwrap();
                assert!(s.pairable && s.connectable);
                assert!(s.paired && s.bonded && s.trusted && s.connected);
                assert_eq!(s.confirmed, 1);
                assert_eq!(s.authorized_services.len(), 1);
            }

            let (snap2, _) = snapshot_with_meta(&conn).await.unwrap();
            sync_policy_remembered(&policy, &snap2);
            let dev = &snap2.devices[0];
            assert!(dev.paired && !dev.unbonded && dev.has_bond() && dev.trusted && dev.connected);

            // Subsequent background auto-reconnect of this remembered headset is authorized,
            // while an unknown stranger is still rejected.
            let reconnect_auth = server
                .call_method(
                    Some(client_dest.as_str()),
                    AGENT_PATH,
                    Some("org.bluez.Agent1"),
                    "AuthorizeService",
                    &(
                        OwnedObjectPath::try_from(DEV).unwrap(),
                        "0000111e-0000-1000-8000-00805f9b34fb".to_string(),
                    ),
                )
                .await;
            assert!(reconnect_auth.is_ok());
        });
    }
}
