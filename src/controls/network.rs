use super::*;
const NM: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const SETTINGS_IFACE: &str = "org.freedesktop.NetworkManager.Settings";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRED: &str = "org.freedesktop.NetworkManager.Device.Wired";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const ACTIVE_CONN: &str = "org.freedesktop.NetworkManager.Connection.Active";
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
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Открытая",
            Self::Psk => "WPA2",
            Self::Sae => "WPA3",
            Self::Transition => "WPA2/WPA3",
            Self::Owe => "OWE",
            Self::Other => "802.1X",
        }
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedWifi {
    pub profile: String,
    pub ssid: Vec<u8>,
    pub label: String,
    pub security: Security,
    pub autoconnect: bool,
    pub active: bool,
    pub in_range: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ethernet {
    pub device: String,
    pub interface: String,
    pub label: String,
    pub profile: Option<String>,
    pub carrier: bool,
    pub speed: u32,
    pub active: bool,
    pub connecting: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vpn {
    pub profile: String,
    pub label: String,
    pub kind: String,
    pub active_path: Option<String>,
    pub active: bool,
    pub connecting: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wifi {
    pub available: bool,
    pub enabled: bool,
    pub hardware: bool,
    pub connected: Option<String>,
    pub connecting: bool,
    pub networks: Vec<Network>,
    pub saved_wifi: Vec<SavedWifi>,
    pub ethernets: Vec<Ethernet>,
    pub vpns: Vec<Vpn>,
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
fn profile_security(settings: &Settings) -> Security {
    match settings
        .get("802-11-wireless-security")
        .and_then(|s| text(s, "key-mgmt"))
    {
        None => Security::Open,
        Some("owe") => Security::Owe,
        Some("wpa-psk") => Security::Psk,
        Some("sae") => Security::Sae,
        Some(_) => Security::Other,
    }
}
fn vpn_kind(ctype: &str, settings: &Settings) -> String {
    if ctype == "wireguard" {
        return "WireGuard".into();
    }
    let service = settings
        .get("vpn")
        .and_then(|v| text(v, "service-type"))
        .unwrap_or("");
    let tail = service
        .rsplit('.')
        .next()
        .unwrap_or(service)
        .to_ascii_lowercase();
    match tail.as_str() {
        "openvpn" => "OpenVPN".into(),
        "wireguard" => "WireGuard".into(),
        "openconnect" => "OpenConnect".into(),
        "l2tp" => "L2TP".into(),
        "pptp" => "PPTP".into(),
        "strongswan" | "libreswan" => "IPsec".into(),
        "fortisslvpn" => "FortiSSL".into(),
        "vpnc" => "Cisco VPNC".into(),
        "sstp" => "SSTP".into(),
        _ => "VPN".into(),
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
    struct ActiveConn {
        path: String,
        profile: String,
        state: u32,
    }
    let root = props(conn, ROOT, NM).await?;
    let mut wifi = Wifi {
        enabled: boolean(&root, "WirelessEnabled"),
        hardware: boolean(&root, "WirelessHardwareEnabled"),
        ..Wifi::default()
    };
    let mut active_conns = Vec::new();
    let mut saved: Vec<(String, Settings)> = Vec::new();
    if detailed {
        for ac in paths(&root, "ActiveConnections").into_iter().take(64) {
            if let Ok(p) = props(conn, ac.as_str(), ACTIVE_CONN).await {
                if let Some(profile) = path(&p, "Connection").filter(|s| s != "/") {
                    active_conns.push(ActiveConn {
                        path: ac.to_string(),
                        profile,
                        state: number(&p, "State"),
                    });
                }
            }
        }
        if let Ok(pxy) = proxy(conn, SETTINGS_PATH, SETTINGS_IFACE).await {
            let list = match pxy
                .call::<_, _, Vec<OwnedObjectPath>>("ListConnections", &())
                .await
            {
                Ok(v) => v,
                Err(_) => props(conn, SETTINGS_PATH, SETTINGS_IFACE)
                    .await
                    .map(|p| paths(&p, "Connections"))
                    .unwrap_or_default(),
            };
            for p in list.into_iter().take(128) {
                if let Ok(cpxy) = proxy(conn, p.as_str(), SETTINGS).await {
                    if let Ok(s) = cpxy.call::<_, _, Settings>("GetSettings", &()).await {
                        saved.push((p.to_string(), s));
                    }
                }
            }
        }
    }
    let devices: Vec<OwnedObjectPath> =
        proxy(conn, ROOT, NM).await?.call("GetDevices", &()).await?;
    for device in devices.into_iter().take(32) {
        let d = props(conn, device.as_str(), DEVICE).await?;
        let dev_type = number(&d, "DeviceType");
        let dev_state = number(&d, "State");
        if detailed && (dev_type == 1 || dev_type == 2) {
            for p in paths(&d, "AvailableConnections").into_iter().take(128) {
                if !saved.iter().any(|(k, _)| k == p.as_str()) {
                    if let Ok(pxy) = proxy(conn, p.as_str(), SETTINGS).await {
                        if let Ok(s) = pxy.call::<_, _, Settings>("GetSettings", &()).await {
                            saved.push((p.to_string(), s));
                        }
                    }
                }
            }
        }
        if dev_type == 1 && detailed {
            let wired = props(conn, device.as_str(), WIRED).await.unwrap_or_default();
            let iface: String = text(&d, "Interface")
                .unwrap_or("")
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(32)
                .collect();
            let avail = paths(&d, "AvailableConnections");
            let profile = avail.first().map(|p| p.to_string());
            let profile_label = profile
                .as_ref()
                .and_then(|p| saved.iter().find(|(k, _)| k == p))
                .and_then(|(_, s)| s.get("connection"))
                .and_then(|c| text(c, "id"))
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().filter(|c| !c.is_control()).take(64).collect::<String>());
            let label = match (profile_label, iface.is_empty()) {
                (Some(name), false) if !name.contains(&iface) => format!("{name} ({iface})"),
                (Some(name), _) => name,
                (None, false) => format!("Ethernet ({iface})"),
                (None, true) => "Ethernet".into(),
            };
            let active = dev_state == 100;
            let connecting = (40..100).contains(&dev_state);
            let carrier = wired
                .get("Carrier")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(dev_state >= 30)
                || active
                || connecting;
            wifi.ethernets.push(Ethernet {
                device: device.to_string(),
                interface: iface,
                label,
                profile,
                carrier,
                speed: number(&wired, "Speed"),
                active,
                connecting,
            });
            continue;
        }
        if dev_type != 2 {
            continue;
        }
        wifi.available = true;
        wifi.connecting |= (40..100).contains(&dev_state);
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
            let is_active = ap.as_str() == active && dev_state == 100;
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
    if detailed {
        for (profile_path, settings) in &saved {
            let conn_sec = settings.get("connection");
            let ctype = conn_sec.and_then(|c| text(c, "type")).or_else(|| {
                if settings.contains_key("802-11-wireless") {
                    Some("802-11-wireless")
                } else if settings.contains_key("vpn") {
                    Some("vpn")
                } else if settings.contains_key("wireguard") {
                    Some("wireguard")
                } else {
                    None
                }
            });
            if ctype == Some("802-11-wireless") {
                let Some(w) = settings.get("802-11-wireless") else {
                    continue;
                };
                let ssid = bytes(w, "ssid");
                if ssid.is_empty() {
                    continue;
                }
                let label: String = conn_sec
                    .and_then(|c| text(c, "id"))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| String::from_utf8_lossy(&ssid).into_owned())
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(64)
                    .collect();
                let sec = profile_security(settings);
                let autoconnect = conn_sec
                    .and_then(|c| c.get("autoconnect"))
                    .and_then(|v| bool::try_from(v).ok())
                    .unwrap_or(true);
                let active = active_conns
                    .iter()
                    .any(|ac| ac.profile == *profile_path && ac.state == 2)
                    || wifi.networks.iter().any(|n| {
                        n.active
                            && (n.saved.as_deref() == Some(profile_path.as_str())
                                || (n.ssid == ssid && saved_matches(settings, &n.ssid, n.security)))
                    });
                let in_range = active
                    || wifi.networks.iter().any(|n| {
                        n.saved.as_deref() == Some(profile_path.as_str())
                            || (n.ssid == ssid && saved_matches(settings, &n.ssid, n.security))
                    });
                wifi.saved_wifi.push(SavedWifi {
                    profile: profile_path.clone(),
                    ssid,
                    label,
                    security: sec,
                    autoconnect,
                    active,
                    in_range,
                });
            } else if matches!(ctype, Some("vpn" | "wireguard")) {
                let label: String = conn_sec
                    .and_then(|c| text(c, "id"))
                    .filter(|s| !s.is_empty())
                    .unwrap_or("VPN")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(64)
                    .collect();
                let kind = vpn_kind(ctype.unwrap_or("vpn"), settings);
                let ac = active_conns
                    .iter()
                    .find(|ac| ac.profile == *profile_path && matches!(ac.state, 1 | 2));
                wifi.vpns.push(Vpn {
                    profile: profile_path.clone(),
                    label,
                    kind,
                    active_path: ac.map(|a| a.path.clone()),
                    active: ac.is_some_and(|a| a.state == 2),
                    connecting: ac.is_some_and(|a| a.state == 1),
                });
            }
        }
        wifi.saved_wifi.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then(b.in_range.cmp(&a.in_range))
                .then(a.label.cmp(&b.label))
                .then(a.profile.cmp(&b.profile))
        });
        wifi.saved_wifi.truncate(128);
        wifi.ethernets.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then(b.connecting.cmp(&a.connecting))
                .then(b.carrier.cmp(&a.carrier))
                .then(a.interface.cmp(&b.interface))
        });
        wifi.ethernets.truncate(32);
        wifi.vpns.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then(b.connecting.cmp(&a.connecting))
                .then(a.label.cmp(&b.label))
                .then(a.profile.cmp(&b.profile))
        });
        wifi.vpns.truncate(64);
    }
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
            let active_wifi = current
                .networks
                .iter()
                .any(|n| n.device == device && n.active);
            let active_eth = current
                .ethernets
                .iter()
                .any(|e| e.device == device && (e.active || e.connecting));
            if !active_wifi && !active_eth {
                return Err("Подключение уже изменилось".into());
            }
            proxy(conn, &device, DEVICE)
                .await
                .map_err(|_| "Адаптер отключён")?
                .call("Disconnect", &())
                .await
        }
        Control::ConnectEthernet { device, profile } => {
            if !current
                .ethernets
                .iter()
                .any(|e| e.device == device && e.carrier && !e.active)
            {
                return Err("Кабель не подключён или состояние адаптера изменилось".into());
            }
            let dev =
                OwnedObjectPath::try_from(device.as_str()).map_err(|_| "Адаптер отключён")?;
            let prof = OwnedObjectPath::try_from(profile.as_deref().unwrap_or("/"))
                .map_err(|_| "Профиль удалён")?;
            let spec = OwnedObjectPath::try_from("/").unwrap();
            root.call::<_, _, OwnedObjectPath>("ActivateConnection", &(prof, dev, spec))
                .await
                .map(|_| ())
        }
        Control::SetAutoconnect {
            profile,
            autoconnect,
        } => {
            if !current.saved_wifi.iter().any(|s| s.profile == profile) {
                return Err("Сохранённый профиль изменился".into());
            }
            let pxy = proxy(conn, &profile, SETTINGS)
                .await
                .map_err(|_| "Профиль удалён")?;
            let mut settings: Settings = pxy
                .call("GetSettings", &())
                .await
                .map_err(|_| "Не удалось прочитать профиль")?;
            if settings.contains_key("802-11-wireless-security") {
                if let Ok(secrets) = pxy
                    .call::<_, _, Settings>("GetSecrets", &("802-11-wireless-security",))
                    .await
                {
                    if let (Some(src), Some(dst)) = (
                        secrets.get("802-11-wireless-security"),
                        settings.get_mut("802-11-wireless-security"),
                    ) {
                        for (k, v) in src {
                            dst.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            settings
                .entry("connection".into())
                .or_default()
                .insert("autoconnect".into(), OwnedValue::from(autoconnect));
            pxy.call::<_, _, ()>("Update", &(settings,)).await
        }
        Control::ForgetConnection(profile) => {
            if !current.saved_wifi.iter().any(|s| s.profile == profile) {
                return Err("Сохранённый профиль уже удалён".into());
            }
            proxy(conn, &profile, SETTINGS)
                .await
                .map_err(|_| "Профиль уже удалён")?
                .call("Delete", &())
                .await
        }
        Control::ConnectVpn(profile) => {
            if !current
                .vpns
                .iter()
                .any(|v| v.profile == profile && !v.active && !v.connecting)
            {
                return Err("Состояние VPN уже изменилось".into());
            }
            let prof =
                OwnedObjectPath::try_from(profile.as_str()).map_err(|_| "Профиль VPN удалён")?;
            let none = OwnedObjectPath::try_from("/").unwrap();
            root.call::<_, _, OwnedObjectPath>("ActivateConnection", &(prof, none.clone(), none))
                .await
                .map(|_| ())
        }
        Control::DisconnectVpn(active_path) => {
            if !current
                .vpns
                .iter()
                .any(|v| v.active_path.as_deref() == Some(active_path.as_str()))
            {
                return Err("Подключение VPN уже закрыто".into());
            }
            let act = OwnedObjectPath::try_from(active_path.as_str())
                .map_err(|_| "Подключение VPN уже закрыто")?;
            root.call::<_, _, ()>("DeactivateConnection", &(act,)).await
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
            let watched: &[&str] = if iface == NM && detailed {
                &[
                    "WirelessEnabled",
                    "WirelessHardwareEnabled",
                    "Devices",
                    "ActiveConnections",
                ]
            } else if iface == NM {
                &["WirelessEnabled", "WirelessHardwareEnabled", "Devices"]
            } else if iface == DEVICE {
                &["State", "ActiveConnection", "AvailableConnections"]
            } else if iface == WIRED && detailed {
                &["Carrier", "Speed"]
            } else if iface == ACTIVE_CONN && detailed {
                &["State", "VpnState"]
            } else if iface == SETTINGS_IFACE && detailed {
                &["Connections"]
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
                | "ConnectionRemoved"
        ) || (detailed
            && matches!(
                m.as_str(),
                "AccessPointAdded" | "AccessPointRemoved" | "VpnStateChanged"
            ))
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
                Control::WatchWifi(watch) => {
                    detailed = watch;
                    if watch {
                        break;
                    }
                }
                Control::Refresh | Control::RefreshNetwork => break,
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
            vec![
                OwnedObjectPath::try_from("/test/device").unwrap(),
                OwnedObjectPath::try_from("/test/eth0").unwrap(),
            ]
        }
        #[zbus(property)]
        fn active_connections(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from("/test/active_vpn").unwrap()]
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
        fn deactivate_connection(&self, active: OwnedObjectPath) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("deactivate:{active}"));
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
    struct MockSettings;
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
    impl MockSettings {
        fn list_connections(&self) -> Vec<OwnedObjectPath> {
            vec![
                OwnedObjectPath::try_from("/test/saved").unwrap(),
                OwnedObjectPath::try_from("/test/eth_profile").unwrap(),
                OwnedObjectPath::try_from("/test/vpn0").unwrap(),
            ]
        }
    }
    struct MockDevice {
        calls: Arc<Mutex<Vec<String>>>,
        kind: u32,
        iface: &'static str,
        profile: &'static str,
    }
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
    impl MockDevice {
        #[zbus(property)]
        fn device_type(&self) -> u32 {
            self.kind
        }
        #[zbus(property)]
        fn state(&self) -> u32 {
            100
        }
        #[zbus(property)]
        fn interface(&self) -> String {
            self.iface.into()
        }
        #[zbus(property)]
        fn active_connection(&self) -> OwnedObjectPath {
            OwnedObjectPath::try_from("/test/active").unwrap()
        }
        #[zbus(property)]
        fn available_connections(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from(self.profile).unwrap()]
        }
        fn disconnect(&self) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("disconnect:{}", self.iface));
        }
    }
    struct MockWired;
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wired")]
    impl MockWired {
        #[zbus(property)]
        fn carrier(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn speed(&self) -> u32 {
            1000
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
    struct MockActiveVpn;
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
    impl MockActiveVpn {
        #[zbus(property)]
        fn connection(&self) -> OwnedObjectPath {
            OwnedObjectPath::try_from("/test/vpn0").unwrap()
        }
        #[zbus(property)]
        fn state(&self) -> u32 {
            2
        }
        #[zbus(property)]
        fn vpn_state(&self) -> u32 {
            5
        }
    }
    struct MockProfile {
        calls: Arc<Mutex<Vec<String>>>,
    }
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
    impl MockProfile {
        fn get_settings(&self) -> Settings {
            HashMap::from([
                (
                    "connection".into(),
                    HashMap::from([
                        (
                            "id".into(),
                            OwnedValue::from(zbus::zvariant::Str::from("test wifi")),
                        ),
                        (
                            "type".into(),
                            OwnedValue::from(zbus::zvariant::Str::from("802-11-wireless")),
                        ),
                        ("autoconnect".into(), OwnedValue::from(true)),
                    ]),
                ),
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
        fn get_secrets(&self, setting_name: &str) -> Settings {
            assert_eq!(setting_name, "802-11-wireless-security");
            HashMap::from([(
                "802-11-wireless-security".into(),
                HashMap::from([(
                    "psk".into(),
                    OwnedValue::from(zbus::zvariant::Str::from("existing secret")),
                )]),
            )])
        }
        fn update(&self, settings: Settings) {
            let auto = settings["connection"]["autoconnect"]
                .downcast_ref::<bool>()
                .unwrap();
            let psk = text(&settings["802-11-wireless-security"], "psk").unwrap_or("");
            self.calls
                .lock()
                .unwrap()
                .push(format!("update:auto={auto}:psk={psk}"));
        }
        fn delete(&self) {
            self.calls.lock().unwrap().push("delete:/test/saved".into());
        }
    }
    struct MockSimpleProfile {
        id: &'static str,
        kind: &'static str,
    }
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
    impl MockSimpleProfile {
        fn get_settings(&self) -> Settings {
            HashMap::from([(
                "connection".into(),
                HashMap::from([
                    ("id".into(), OwnedValue::from(zbus::zvariant::Str::from(self.id))),
                    (
                        "type".into(),
                        OwnedValue::from(zbus::zvariant::Str::from(self.kind)),
                    ),
                ]),
            )])
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
                .serve_at(SETTINGS_PATH, MockSettings)
                .unwrap()
                .serve_at(
                    "/test/device",
                    MockDevice {
                        calls: calls.clone(),
                        kind: 2,
                        iface: "wlan0",
                        profile: "/test/saved",
                    },
                )
                .unwrap()
                .serve_at("/test/device", MockWireless)
                .unwrap()
                .serve_at(
                    "/test/eth0",
                    MockDevice {
                        calls: calls.clone(),
                        kind: 1,
                        iface: "enp3s0",
                        profile: "/test/eth_profile",
                    },
                )
                .unwrap()
                .serve_at("/test/eth0", MockWired)
                .unwrap()
                .serve_at("/test/ap", MockAp)
                .unwrap()
                .serve_at("/test/active_vpn", MockActiveVpn)
                .unwrap()
                .serve_at(
                    "/test/saved",
                    MockProfile {
                        calls: calls.clone(),
                    },
                )
                .unwrap()
                .serve_at(
                    "/test/eth_profile",
                    MockSimpleProfile {
                        id: "Проводное 1",
                        kind: "802-3-ethernet",
                    },
                )
                .unwrap()
                .serve_at(
                    "/test/vpn0",
                    MockSimpleProfile {
                        id: "Work VPN",
                        kind: "wireguard",
                    },
                )
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
            assert_eq!(state.saved_wifi.len(), 1);
            assert_eq!(state.saved_wifi[0].label, "test wifi");
            assert!(state.saved_wifi[0].autoconnect);
            assert!(state.saved_wifi[0].active);
            assert!(state.saved_wifi[0].in_range);
            assert_eq!(state.saved_wifi[0].security, Security::Psk);
            assert_eq!(state.ethernets.len(), 1);
            assert_eq!(state.ethernets[0].interface, "enp3s0");
            assert!(state.ethernets[0].active);
            assert!(state.ethernets[0].carrier);
            assert_eq!(state.ethernets[0].speed, 1000);
            assert_eq!(state.vpns.len(), 1);
            assert_eq!(state.vpns[0].label, "Work VPN");
            assert_eq!(state.vpns[0].kind, "WireGuard");
            assert!(state.vpns[0].active);
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
            action(
                &conn,
                Control::SetAutoconnect {
                    profile: "/test/saved".into(),
                    autoconnect: false,
                },
                &state,
            )
            .await
            .unwrap();
            action(&conn, Control::ForgetConnection("/test/saved".into()), &state)
                .await
                .unwrap();
            action(&conn, Control::Disconnect("/test/eth0".into()), &state)
                .await
                .unwrap();
            state.ethernets[0].active = false;
            action(
                &conn,
                Control::ConnectEthernet {
                    device: "/test/eth0".into(),
                    profile: Some("/test/eth_profile".into()),
                },
                &state,
            )
            .await
            .unwrap();
            action(
                &conn,
                Control::DisconnectVpn("/test/active_vpn".into()),
                &state,
            )
            .await
            .unwrap();
            state.vpns[0].active = false;
            state.vpns[0].active_path = None;
            action(&conn, Control::ConnectVpn("/test/vpn0".into()), &state)
                .await
                .unwrap();
            action(&conn, Control::Wifi(false), &state).await.unwrap();
            assert_eq!(
                calls.lock().unwrap().as_slice(),
                [
                    "saved:/test/saved:/test/device:/test/ap",
                    "new:/test/device:/test/ap",
                    "update:auto=false:psk=existing secret",
                    "delete:/test/saved",
                    "disconnect:enp3s0",
                    "saved:/test/eth_profile:/test/eth0:/",
                    "deactivate:/test/active_vpn",
                    "saved:/test/vpn0:/:/",
                    "wifi:false"
                ]
            );
            let compact = snapshot(&conn, false).await.unwrap();
            assert!(!compact.enabled);
            assert!(compact.saved_wifi.is_empty());
            assert!(compact.ethernets.is_empty());
            assert!(compact.vpns.is_empty());
            drop(server);
        });
    }
}
