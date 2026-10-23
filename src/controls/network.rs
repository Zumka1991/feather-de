use super::*;
const NM: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings.Connection";
type Props = HashMap<String, OwnedValue>;
type Settings = HashMap<String, Props>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Security {
    Open,
    Psk,
    Sae,
    Transition,
    Owe,
    Other,
}
impl Security {
    pub fn password(self) -> bool {
        matches!(self, Self::Psk | Self::Sae | Self::Transition)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: Vec<u8>,
    pub label: String,
    pub ap: String,
    pub device: String,
    pub saved: Option<String>,
    pub security: Security,
    pub strength: u8,
    pub active: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wifi {
    pub available: bool,
    pub enabled: bool,
    pub hardware: bool,
    pub connected: Option<String>,
    pub connecting: bool,
    pub networks: Vec<Network>,
    pub error: Option<String>,
}
impl Wifi {
    pub fn subtitle(&self) -> &str {
        if self.error.is_some() {
            "Служба недоступна"
        } else if !self.available {
            "Нет адаптера"
        } else if !self.hardware {
            "Заблокирован"
        } else if !self.enabled {
            "Выключен"
        } else if self.connecting {
            "Подключение…"
        } else {
            self.connected.as_deref().unwrap_or("Не подключён")
        }
    }
}
async fn proxy<'a>(
    conn: &'a zbus::Connection,
    path: &'a str,
    iface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    Proxy::new(conn, NM, path, iface).await
}
async fn props(conn: &zbus::Connection, path: &str, iface: &str) -> zbus::Result<Props> {
    let p = proxy(conn, path, "org.freedesktop.DBus.Properties").await?;
    p.call("GetAll", &(iface,)).await
}
fn number(p: &Props, key: &str) -> u32 {
    p.get(key).and_then(|v| u32::try_from(v).ok()).unwrap_or(0)
}
fn boolean(p: &Props, key: &str) -> bool {
    p.get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}
fn path(p: &Props, key: &str) -> Option<String> {
    p.get(key)
        .and_then(|v| <&zbus::zvariant::ObjectPath>::try_from(v).ok())
        .map(|p| p.as_str().to_string())
}
fn paths(p: &Props, key: &str) -> Vec<OwnedObjectPath> {
    p.get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::try_from(v).ok())
        .unwrap_or_default()
}
fn bytes(p: &Props, key: &str) -> Vec<u8> {
    p.get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::try_from(v).ok())
        .unwrap_or_default()
}
fn text<'a>(p: &'a Props, key: &str) -> Option<&'a str> {
    p.get(key).and_then(|v| <&str>::try_from(v).ok())
}
fn security(p: &Props) -> Security {
    let flags = number(p, "RsnFlags") | number(p, "WpaFlags");
    if flags & 0x500 == 0x500 {
        Security::Transition
    } else if flags & 0x100 != 0 {
        Security::Psk
    } else if flags & 0x400 != 0 {
        Security::Sae
    } else if flags & 0x1800 != 0 {
        Security::Owe
    } else if flags == 0 && number(p, "Flags") & 1 == 0 {
        Security::Open
    } else {
        Security::Other
    }
}
fn saved_matches(settings: &Settings, ssid: &[u8], sec: Security) -> bool {
    let Some(w) = settings.get("802-11-wireless") else {
        return false;
    };
    if bytes(w, "ssid") != ssid {
        return false;
    }
    let key = settings
        .get("802-11-wireless-security")
        .and_then(|s| text(s, "key-mgmt"));
    matches!(
        (sec, key),
        (Security::Open, None)
            | (Security::Psk, Some("wpa-psk"))
            | (Security::Sae, Some("sae"))
            | (Security::Transition, Some("wpa-psk" | "sae"))
            | (Security::Owe, Some("owe"))
            | (Security::Other, Some("wpa-eap" | "ieee8021x"))
    )
}
fn normalize(networks: &mut Vec<Network>) {
    networks.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then(b.strength.cmp(&a.strength))
            .then(a.label.cmp(&b.label))
    });
    let mut seen = std::collections::HashSet::new();
    networks.retain(|n| seen.insert((n.ssid.clone(), n.security)));
    networks.truncate(128);
}
async fn snapshot(conn: &zbus::Connection, detailed: bool) -> zbus::Result<Wifi> {
    let root = props(conn, ROOT, NM).await?;
    let mut wifi = Wifi {
        enabled: boolean(&root, "WirelessEnabled"),
        hardware: boolean(&root, "WirelessHardwareEnabled"),
        ..Wifi::default()
    };
    let devices: Vec<OwnedObjectPath> =
        proxy(conn, ROOT, NM).await?.call("GetDevices", &()).await?;
    for device in devices.into_iter().take(32) {
        let d = props(conn, device.as_str(), DEVICE).await?;
        if number(&d, "DeviceType") != 2 {
            continue;
        }
        wifi.available = true;
        wifi.connecting |= (40..100).contains(&number(&d, "State"));
        let wireless = props(conn, device.as_str(), WIRELESS).await?;
        let active = path(&wireless, "ActiveAccessPoint").unwrap_or_else(|| "/".into());
        let aps = if detailed {
            paths(&wireless, "AccessPoints")
        } else {
            OwnedObjectPath::try_from(active.clone())
                .ok()
                .filter(|p| p.as_str() != "/")
                .into_iter()
                .collect()
        };
        let mut saved = Vec::new();
        if detailed {
            for p in paths(&d, "AvailableConnections").into_iter().take(128) {
                if let Ok(pxy) = proxy(conn, p.as_str(), SETTINGS).await {
                    if let Ok(s) = pxy.call::<_, _, Settings>("GetSettings", &()).await {
                        saved.push((p.to_string(), s));
                    }
                }
            }
        }
        for ap in aps.into_iter().take(256) {
            let Ok(a) = props(conn, ap.as_str(), AP).await else {
                continue;
            };
            let ssid = bytes(&a, "Ssid");
            if ssid.is_empty() {
                continue;
            }
            let label: String = String::from_utf8_lossy(&ssid)
                .chars()
                .filter(|c| !c.is_control())
                .take(64)
                .collect();
            let is_active = ap.as_str() == active && number(&d, "State") == 100;
            if is_active {
                wifi.connected = Some(label.clone());
            }
            let sec = security(&a);
            wifi.networks.push(Network {
                saved: saved
                    .iter()
                    .find(|(_, s)| saved_matches(s, &ssid, sec))
                    .map(|(p, _)| p.clone()),
                ssid,
                label,
                security: sec,
                strength: a
                    .get("Strength")
                    .and_then(|v| u8::try_from(v).ok())
                    .unwrap_or(0),
                active: is_active,
                device: device.to_string(),
                ap: ap.to_string(),
            });
        }
    }
    normalize(&mut wifi.networks);
    Ok(wifi)
}
#[cfg(test)]
pub(super) async fn read_only_snapshot(conn: &zbus::Connection) -> zbus::Result<Wifi> {
    snapshot(conn, false).await
}
pub fn valid_password(sec: Security, p: &str) -> bool {
    if sec == Security::Sae {
        !p.is_empty() && p.len() <= 63
    } else {
        (8..=63).contains(&p.len()) || (p.len() == 64 && p.bytes().all(|b| b.is_ascii_hexdigit()))
    }
}
fn connection<'a>(
    n: &Network,
    password: Option<&'a str>,
) -> Result<HashMap<&'static str, HashMap<&'static str, Value<'a>>>, &'static str> {
    let mut settings = HashMap::new();
    settings.insert(
        "connection",
        HashMap::from([
            ("id", Value::from(n.label.clone())),
            ("type", Value::from("802-11-wireless")),
            ("autoconnect", Value::from(true)),
        ]),
    );
    settings.insert(
        "802-11-wireless",
        HashMap::from([
            ("ssid", Value::from(n.ssid.clone())),
            ("mode", Value::from("infrastructure")),
        ]),
    );
    let key = match n.security {
        Security::Open => None,
        Security::Owe => Some("owe"),
        Security::Psk | Security::Transition => Some("wpa-psk"),
        Security::Sae => Some("sae"),
        Security::Other => return Err("Для этой сети откройте расширенные настройки"),
    };
    if let Some(key) = key {
        let mut sec = HashMap::from([("key-mgmt", Value::from(key))]);
        if n.security.password() {
            let p = password.ok_or("Нужен пароль сети")?;
            if !valid_password(n.security, p) {
                return Err("Некорректная длина пароля Wi‑Fi");
            }
            sec.insert("psk", Value::from(p));
        }
        settings.insert("802-11-wireless-security", sec);
    }
    Ok(settings)
}
async fn action(conn: &zbus::Connection, command: Control, current: &Wifi) -> Result<(), String> {
    let root = proxy(conn, ROOT, NM)
        .await
        .map_err(|_| "NetworkManager недоступен")?;
    let result: zbus::Result<()> = match command {
        Control::Wifi(enabled) => {
            if !current.available || !current.hardware {
                return Err("Wi‑Fi-адаптер недоступен или заблокирован".into());
            }
            root.set_property("WirelessEnabled", enabled)
                .await
                .map_err(Into::into)
        }
        Control::Scan => {
            if !current.enabled {
                return Ok(());
            }
            let devices: Vec<OwnedObjectPath> = root
                .call("GetDevices", &())
                .await
                .map_err(|_| "Не удалось найти Wi‑Fi")?;
            for dev in devices.into_iter().take(32) {
                if props(conn, dev.as_str(), DEVICE)
                    .await
                    .is_ok_and(|p| number(&p, "DeviceType") == 2)
                {
                    // NM rate-limits scans; cached networks stay usable on ScanNotAllowed.
                    let _ = proxy(conn, dev.as_str(), WIRELESS)
                        .await
                        .map_err(|_| "Нет Wi‑Fi")?
                        .call::<_, _, ()>("RequestScan", &(HashMap::<String, Value>::new(),))
                        .await;
                }
            }
            Ok(())
        }
        Control::Disconnect(device) => {
            if !current
                .networks
                .iter()
                .any(|n| n.device == device && n.active)
            {
                return Err("Подключение уже изменилось".into());
            }
            proxy(conn, &device, DEVICE)
                .await
                .map_err(|_| "Адаптер отключён")?
                .call("Disconnect", &())
                .await
        }
        Control::Connect {
            network: n,
            password,
        } => {
            if !current.enabled
                || !current.networks.iter().any(|v| {
                    v.ap == n.ap
                        && v.device == n.device
                        && v.ssid == n.ssid
                        && v.security == n.security
                        && v.saved == n.saved
                })
            {
                return Err("Список сетей изменился. Выберите сеть снова.".into());
            }
            let device =
                OwnedObjectPath::try_from(n.device.as_str()).map_err(|_| "Адаптер отключён")?;
            let ap = OwnedObjectPath::try_from(n.ap.as_str()).map_err(|_| "Сеть исчезла")?;
            if let Some(saved) = &n.saved {
                let profile =
                    OwnedObjectPath::try_from(saved.as_str()).map_err(|_| "Профиль удалён")?;
                root.call::<_, _, OwnedObjectPath>("ActivateConnection", &(profile, device, ap))
                    .await
                    .map(|_| ())
            } else {
                let settings = connection(&n, password.as_ref().map(|p| p.as_str()))
                    .map_err(str::to_string)?;
                root.call::<_, _, (OwnedObjectPath, OwnedObjectPath)>(
                    "AddAndActivateConnection",
                    &(settings, device, ap),
                )
                .await
                .map(|_| ())
            }
        }
        _ => Ok(()),
    };
    result.map_err(|_| {
        "NetworkManager отклонил действие. Проверьте пароль и разрешения в настройках сети.".into()
    })
}
fn relevant(message: &zbus::Message, detailed: bool) -> bool {
    let h = message.header();
    if h.member()
        .is_some_and(|m| m.as_str() == "PropertiesChanged")
    {
        if let Ok((iface, changed, invalidated)) =
            message.body().deserialize::<(String, Props, Vec<String>)>()
        {
            let watched: &[&str] = if iface == NM {
                &["WirelessEnabled", "WirelessHardwareEnabled", "Devices"]
            } else if iface == DEVICE {
                &["State", "ActiveConnection", "AvailableConnections"]
            } else if iface == WIRELESS && detailed {
                &["ActiveAccessPoint", "LastScan", "AccessPoints"]
            } else if iface == WIRELESS {
                &["ActiveAccessPoint"]
            } else if iface == AP && detailed {
                &["Ssid", "Strength", "Flags", "WpaFlags", "RsnFlags"]
            } else {
                &[]
            };
            return watched
                .iter()
                .any(|p| changed.contains_key(*p) || invalidated.iter().any(|v| v == p));
        }
        return false;
    }
    h.member().is_some_and(|m| {
        matches!(
            m.as_str(),
            "DeviceAdded"
                | "DeviceRemoved"
                | "StateChanged"
                | "NewConnection"
                | "Removed"
                | "Updated"
        ) || (detailed && matches!(m.as_str(), "AccessPointAdded" | "AccessPointRemoved"))
    })
}
pub async fn run(commands: async_channel::Receiver<Control>, updates: channel::SyncSender<Update>) {
    let mut detailed = false;
    loop {
        let result = connected(&commands, &updates, &mut detailed).await;
        if result.is_err() {
            if updates
                .send(Update::Wifi(Wifi {
                    error: Some("NetworkManager недоступен".into()),
                    ..Wifi::default()
                }))
                .is_err()
            {
                return;
            }
        } else {
            return;
        }
        // Explicit panel reopening retries a failed bus connection. No idle retry timer.
        while let Ok(command) = commands.recv().await {
            match command {
                Control::WatchWifi(watch) => detailed = watch,
                Control::Refresh => break,
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
        .method_timeout(Duration::from_secs(3))
        .build()
        .await?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(NM)?
        .build();
    let mut events = zbus::MessageStream::for_match_rule(rule, &conn, Some(64)).await?;
    let owner_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(NM)?
        .build();
    let mut owners = zbus::MessageStream::for_match_rule(owner_rule, &conn, Some(4)).await?;
    let mut state = Wifi::default();
    let mut command = None;
    loop {
        if let Some(c) = command.take() {
            if let Control::WatchWifi(watch) = c {
                *detailed = watch;
            } else if let Err(e) = action(&conn, c, &state).await {
                if updates.send(Update::Error("Wi‑Fi", e)).is_err() {
                    return Ok(());
                }
            }
        }
        let next = snapshot(&conn, *detailed).await.unwrap_or_else(|_| Wifi {
            error: Some("NetworkManager недоступен".into()),
            ..Wifi::default()
        });
        if next != state {
            state = next;
            if updates.send(Update::Wifi(state.clone())).is_err() {
                return Ok(());
            }
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
                Event::Command(Err(_)) => return Ok(()),
                Event::Owner(Some(Ok(_))) => break,
                Event::Owner(_) => {
                    return Err(zbus::Error::Failure("Bus signal stream closed".into()))
                }
                Event::Signal(Some(Ok(message))) if relevant(&message, *detailed) => break,
                Event::Signal(None | Some(Err(_))) => {
                    return Err(zbus::Error::Failure(
                        "NetworkManager signal stream closed".into(),
                    ))
                }
                _ => {}
            }
        }
        // Drain an already queued signal burst, then collect a single fresh snapshot.
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
    fn net(ssid: &[u8], strength: u8, security: Security, active: bool) -> Network {
        Network {
            ssid: ssid.into(),
            label: String::from_utf8_lossy(ssid).into(),
            ap: "/ap/1".into(),
            device: "/device/1".into(),
            saved: None,
            security,
            strength,
            active,
        }
    }
    #[test]
    fn network_grouping_retains_active_and_separates_security() {
        let mut networks = vec![
            net(b"office", 99, Security::Psk, false),
            net(b"office", 30, Security::Psk, true),
            net(b"office", 80, Security::Open, false),
        ];
        normalize(&mut networks);
        assert_eq!(networks.len(), 2);
        assert!(networks[0].active);
        assert_eq!(networks[1].security, Security::Open);
    }
    #[test]
    fn connection_credentials_and_ssid_keep_protocol_types() {
        let n = net(b"space: slash\\ \xff", 50, Security::Psk, false);
        assert!(connection(&n, Some("short")).is_err());
        let s = connection(&n, Some("correct horse")).unwrap();
        assert_eq!(
            Vec::<u8>::try_from(s["802-11-wireless"]["ssid"].try_clone().unwrap()).unwrap(),
            n.ssid
        );
        assert_eq!(
            s["802-11-wireless-security"]["key-mgmt"]
                .downcast_ref::<&str>()
                .unwrap(),
            "wpa-psk"
        );
        assert!(connection(&net(b"corp", 50, Security::Other, false), None).is_err());
    }

    struct MockRoot {
        calls: Arc<Mutex<Vec<String>>>,
        enabled: bool,
    }
    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl MockRoot {
        fn get_devices(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from("/test/device").unwrap()]
        }
        #[zbus(property)]
        fn wireless_enabled(&self) -> bool {
            self.enabled
        }
        #[zbus(property)]
        fn set_wireless_enabled(&mut self, value: bool) {
            self.enabled = value;
            self.calls.lock().unwrap().push(format!("wifi:{value}"));
        }
        #[zbus(property)]
        fn wireless_hardware_enabled(&self) -> bool {
            true
        }
        fn activate_connection(
            &self,
            profile: OwnedObjectPath,
            device: OwnedObjectPath,
            ap: OwnedObjectPath,
        ) -> OwnedObjectPath {
            self.calls
                .lock()
                .unwrap()
                .push(format!("saved:{profile}:{device}:{ap}"));
            OwnedObjectPath::try_from("/test/active").unwrap()
        }
        fn add_and_activate_connection(
            &self,
            settings: Settings,
            device: OwnedObjectPath,
            ap: OwnedObjectPath,
        ) -> (OwnedObjectPath, OwnedObjectPath) {
            assert_eq!(bytes(&settings["802-11-wireless"], "ssid"), b"test wifi");
            assert_eq!(
                text(&settings["802-11-wireless-security"], "psk"),
                Some("test password")
            );
            self.calls
                .lock()
                .unwrap()
                .push(format!("new:{device}:{ap}"));
            (
                OwnedObjectPath::try_from("/test/new").unwrap(),
                OwnedObjectPath::try_from("/test/active").unwrap(),
            )
        }
    }
    struct MockDevice;
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
    impl MockDevice {
        #[zbus(property)]
        fn device_type(&self) -> u32 {
            2
        }
        #[zbus(property)]
        fn state(&self) -> u32 {
            100
        }
        #[zbus(property)]
        fn available_connections(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from("/test/saved").unwrap()]
        }
    }
    struct MockWireless;
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
    impl MockWireless {
        #[zbus(property)]
        fn active_access_point(&self) -> OwnedObjectPath {
            OwnedObjectPath::try_from("/test/ap").unwrap()
        }
        #[zbus(property)]
        fn access_points(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from("/test/ap").unwrap()]
        }
    }
    struct MockAp;
    #[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
    impl MockAp {
        #[zbus(property)]
        fn ssid(&self) -> Vec<u8> {
            b"test wifi".to_vec()
        }
        #[zbus(property)]
        fn rsn_flags(&self) -> u32 {
            0x100
        }
        #[zbus(property)]
        fn wpa_flags(&self) -> u32 {
            0
        }
        #[zbus(property)]
        fn flags(&self) -> u32 {
            1
        }
        #[zbus(property)]
        fn strength(&self) -> u8 {
            72
        }
    }
    struct MockProfile;
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
    impl MockProfile {
        fn get_settings(&self) -> Settings {
            HashMap::from([
                (
                    "802-11-wireless".into(),
                    HashMap::from([(
                        "ssid".into(),
                        OwnedValue::try_from(Value::from(b"test wifi".to_vec())).unwrap(),
                    )]),
                ),
                (
                    "802-11-wireless-security".into(),
                    HashMap::from([(
                        "key-mgmt".into(),
                        OwnedValue::from(zbus::zvariant::Str::from("wpa-psk")),
                    )]),
                ),
            ])
        }
    }
    #[test]
    fn private_bus_snapshot_and_actions_use_real_nm_wire_types_and_reject_stale_networks() {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return;
        }
        future::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let server = zbus::connection::Builder::session()
                .unwrap()
                .name(NM)
                .unwrap()
                .serve_at(
                    ROOT,
                    MockRoot {
                        calls: calls.clone(),
                        enabled: true,
                    },
                )
                .unwrap()
                .serve_at("/test/device", MockDevice)
                .unwrap()
                .serve_at("/test/device", MockWireless)
                .unwrap()
                .serve_at("/test/ap", MockAp)
                .unwrap()
                .serve_at("/test/saved", MockProfile)
                .unwrap()
                .build()
                .await
                .unwrap();
            let conn = zbus::connection::Builder::session()
                .unwrap()
                .method_timeout(Duration::from_millis(500))
                .build()
                .await
                .unwrap();
            let mut state = snapshot(&conn, true).await.unwrap();
            assert_eq!(state.connected.as_deref(), Some("test wifi"));
            assert_eq!(state.networks.len(), 1);
            let saved = state.networks[0].clone();
            assert!(saved.saved.is_some());
            action(
                &conn,
                Control::Connect {
                    network: saved.clone(),
                    password: None,
                },
                &state,
            )
            .await
            .unwrap();
            let mut new = saved.clone();
            new.saved = None;
            state.networks[0].saved = None;
            action(
                &conn,
                Control::Connect {
                    network: new.clone(),
                    password: Some("test password".to_string().into()),
                },
                &state,
            )
            .await
            .unwrap();
            new.ap = "/test/disappeared".into();
            assert!(action(
                &conn,
                Control::Connect {
                    network: new,
                    password: Some("test password".to_string().into())
                },
                &state
            )
            .await
            .is_err());
            action(&conn, Control::Wifi(false), &state).await.unwrap();
            assert_eq!(
                calls.lock().unwrap().as_slice(),
                [
                    "saved:/test/saved:/test/device:/test/ap",
                    "new:/test/device:/test/ap",
                    "wifi:false"
                ]
            );
            assert!(!snapshot(&conn, false).await.unwrap().enabled);
            drop(server);
        });
    }
}
