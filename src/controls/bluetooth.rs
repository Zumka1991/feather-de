use super::*;
use std::collections::VecDeque;

const BLUEZ: &str = "org.bluez";
const ROOT: &str = "/";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const BATTERY: &str = "org.bluez.Battery1";

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

fn parse_objects(objects: &ManagedObjects) -> Bluetooth {
    let mut adapters: Vec<(&str, &Props)> = objects
        .iter()
        .filter_map(|(p, ifaces)| Some((p.as_str(), ifaces.get(ADAPTER)?)))
        .collect();
    if adapters.is_empty() {
        return Bluetooth::default();
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
            trusted,
            connected,
            battery,
            rssi,
        });
    }

    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
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

    Bluetooth {
        available: true,
        powered,
        discovering,
        adapter_name,
        connected,
        connecting: None,
        devices,
        error: None,
    }
}

pub(super) async fn snapshot(conn: &zbus::Connection) -> zbus::Result<Bluetooth> {
    let om = proxy(conn, ROOT, OBJECT_MANAGER).await?;
    let objects: ManagedObjects = om.call("GetManagedObjects", &()).await?;
    Ok(parse_objects(&objects))
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

async fn action(
    conn: &zbus::Connection,
    command: Control,
    current: &Bluetooth,
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
            proxy(conn, &adapter, ADAPTER)
                .await
                .map_err(|_| "Bluetooth-адаптер недоступен")?
                .set_property("Powered", enabled)
                .await
                .map_err(Into::into)
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
            }
            pxy.call("Connect", &()).await
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
    result.map_err(|_| {
        "BlueZ отклонил действие. Проверьте, что устройство включено и находится рядом.".into()
    })
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
async fn connect_cancellable(
    conn: &zbus::Connection,
    device_path: String,
    current: &Bluetooth,
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
            Outcome::Done(action(conn, command, current, started_discovery).await)
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
                action(conn, command, current, started_discovery).await
            }
        }
    }
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
                    "Trusted",
                    "Alias",
                    "Name",
                    "Icon",
                    "RSSI",
                ]
            } else if iface == DEVICE {
                &["Connected", "Paired", "Alias", "Name"]
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
    let conn = zbus::connection::Builder::system()?
        .method_timeout(Duration::from_secs(5))
        .build()
        .await?;
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
                    connect_cancellable(&conn, path, &state, s, commands, p).await
                } else {
                    action(&conn, c, &state, &mut started_discovery).await
                };
                if let Err(e) = result {
                    if updates.send(Update::Error("Bluetooth", e)).is_err() {
                        return Ok(());
                    }
                }
            }
        }
        // If BlueZ is not running on the system, treat it as "no adapter" unless detailed view asked.
        let next = snapshot(&conn).await.unwrap_or_default();
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
                Event::Owner(Some(Ok(_))) => break,
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
}
