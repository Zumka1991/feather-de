//! Event-driven system controls. No bus, subprocess, or waits on the compositor thread.
use futures_lite::{future, StreamExt};
use smithay::reexports::calloop::{channel, LoopHandle};
use std::collections::HashMap;
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use zbus::{
    zvariant::{OwnedObjectPath, OwnedValue, Value},
    Proxy,
};
pub type Secret = zeroize::Zeroizing<String>;

pub mod bluetooth;
pub mod network;

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Sink {
    pub name: String,
    pub label: String,
    pub index: u32,
    pub channels: Vec<u32>,
    /// Last known non-zero channel ratios, retained only while the device stays present.
    pub balance_channels: Vec<u32>,
    pub muted: bool,
    /// The active port (or every known port) reports "not available".
    pub unplugged: bool,
    /// Worn audio: Bluetooth output or a headphone/headset form factor (USB headsets too)
    pub headphones: bool,
}
impl Sink {
    pub fn volume(&self) -> f32 {
        self.channels.iter().copied().max().unwrap_or(0) as f32 / 65536.0
    }
}
#[derive(Clone, Default, Debug, PartialEq)]
pub struct AudioStream {
    pub index: u32,
    pub name: String,
    pub subtitle: String,
    pub binary: String,
    pub icon_name: String,
    pub sink_index: u32,
    pub sink_name: String,
    pub channels: Vec<u32>,
    pub balance_channels: Vec<u32>,
    pub muted: bool,
}
impl AudioStream {
    pub fn volume(&self) -> f32 {
        (self.channels.iter().copied().max().unwrap_or(0) as f32 / 65536.0).clamp(0.0, 1.0)
    }
}
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Audio {
    pub sinks: Vec<Sink>,
    pub default: String,
    /// Microphones and line inputs; monitors of outputs are left out.
    pub sources: Vec<Sink>,
    pub default_source: String,
    /// Active playback streams (queried while Sound UI is open or via D-Bus).
    pub streams: Vec<AudioStream>,
    pub input_error: Option<String>,
    pub error: Option<String>,
}
impl Audio {
    pub fn current(&self) -> Option<&Sink> {
        self.sinks.iter().find(|s| s.name == self.default)
    }
    pub fn current_source(&self) -> Option<&Sink> {
        self.sources.iter().find(|s| s.name == self.default_source)
    }
    pub fn retain_zero_balances(&mut self, previous: &Audio) {
        for (group, old_group) in [(&mut self.sinks, &previous.sinks), (&mut self.sources, &previous.sources)] {
            for sink in group.iter_mut() {
                if sink.channels.iter().copied().max().unwrap_or(0) == 0 {
                    if let Some(old) = old_group.iter().find(|o| o.name == sink.name && o.index == sink.index && o.channels.len() == sink.channels.len()) {
                        let cached = if old.channels.iter().copied().max().unwrap_or(0) > 0 {
                            &old.channels
                        } else if old.balance_channels.len() == sink.channels.len() && old.balance_channels.iter().copied().max().unwrap_or(0) > 0 {
                            &old.balance_channels
                        } else {
                            continue;
                        };
                        sink.balance_channels = cached.clone();
                    }
                }
            }
        }
        for stream in &mut self.streams {
            if stream.channels.iter().copied().max().unwrap_or(0) == 0 {
                if let Some(old) = previous.streams.iter().find(|o| o.index == stream.index && o.name == stream.name && o.channels.len() == stream.channels.len()) {
                    let cached = if old.channels.iter().copied().max().unwrap_or(0) > 0 {
                        &old.channels
                    } else if old.balance_channels.len() == stream.channels.len() && old.balance_channels.iter().copied().max().unwrap_or(0) > 0 {
                        &old.balance_channels
                    } else {
                        continue;
                    };
                    stream.balance_channels = cached.clone();
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioKey { Raise, Lower, Mute }
impl AudioKey {
    pub fn from_keysym(sym: u32) -> Option<Self> {
        use smithay::input::keyboard::keysyms as k;
        match sym {
            k::KEY_XF86AudioRaiseVolume => Some(Self::Raise),
            k::KEY_XF86AudioLowerVolume => Some(Self::Lower),
            k::KEY_XF86AudioMute => Some(Self::Mute),
            _ => None,
        }
    }
}

// Password commands intentionally do not implement Debug.
#[derive(Clone)]
pub enum Control {
    Refresh,
    /// Switch to headphones as they connect (from the Sound settings)
    AutoHeadphones(bool),
    RefreshAudio,
    RefreshNetwork,
    RefreshBluetooth,
    Scan,
    ScanBluetooth,
    WatchWifi(bool),
    WatchBluetooth(bool),
    WatchAudioStreams(bool),
    Wifi(bool),
    Bluetooth(bool),
    Connect {
        network: network::Network,
        password: Option<Secret>,
    },
    Disconnect(String),
    ConnectEthernet {
        device: String,
        profile: Option<String>,
    },
    SetAutoconnect {
        profile: String,
        autoconnect: bool,
    },
    ForgetConnection(String),
    ConnectVpn(String),
    DisconnectVpn(String),
    ConnectBluetooth(String),
    DisconnectBluetooth(String),
    ForgetBluetooth(String),
    Volume {
        sink: String,
        value: f32,
    },
    Mute {
        sink: String,
        muted: bool,
    },
    Output(String),
    InputVolume {
        source: String,
        value: f32,
    },
    InputMute {
        source: String,
        muted: bool,
    },
    Input(String),
    StreamVolume {
        index: u32,
        value: f32,
    },
    StreamMute {
        index: u32,
        muted: bool,
    },
    StreamOutput {
        index: u32,
        sink: String,
    },
    QueryStreams(Arc<Mutex<Option<Box<dyn FnOnce(Result<Vec<AudioStream>, String>) + Send>>>>),
    AudioKey(AudioKey),
}
impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Refresh => "Refresh",
            Self::AutoHeadphones(_) => "AutoHeadphones",
            Self::RefreshAudio => "RefreshAudio",
            Self::RefreshNetwork => "RefreshNetwork",
            Self::RefreshBluetooth => "RefreshBluetooth",
            Self::Scan => "Scan",
            Self::ScanBluetooth => "ScanBluetooth",
            Self::WatchWifi(_) => "WatchWifi",
            Self::WatchBluetooth(_) => "WatchBluetooth",
            Self::WatchAudioStreams(_) => "WatchAudioStreams",
            Self::Wifi(_) => "Wifi",
            Self::Bluetooth(_) => "Bluetooth",
            Self::Connect { .. } => "Connect(redacted)",
            Self::Disconnect(_) => "Disconnect",
            Self::ConnectEthernet { .. } => "ConnectEthernet",
            Self::SetAutoconnect { .. } => "SetAutoconnect",
            Self::ForgetConnection(_) => "ForgetConnection",
            Self::ConnectVpn(_) => "ConnectVpn",
            Self::DisconnectVpn(_) => "DisconnectVpn",
            Self::ConnectBluetooth(_) => "ConnectBluetooth",
            Self::DisconnectBluetooth(_) => "DisconnectBluetooth",
            Self::ForgetBluetooth(_) => "ForgetBluetooth",
            Self::Volume { .. } => "Volume",
            Self::Mute { .. } => "Mute",
            Self::Output(_) => "Output",
            Self::InputVolume { .. } => "InputVolume",
            Self::InputMute { .. } => "InputMute",
            Self::Input(_) => "Input",
            Self::StreamVolume { .. } => "StreamVolume",
            Self::StreamMute { .. } => "StreamMute",
            Self::StreamOutput { .. } => "StreamOutput",
            Self::QueryStreams(_) => "QueryStreams",
            Self::AudioKey(_) => "AudioKey",
        })
    }
}
pub enum Update {
    Wifi(network::Wifi),
    Bluetooth(bluetooth::Bluetooth),
    Audio(Audio),
    AudioFeedback(Audio),
    Error(&'static str, String),
}

pub struct Controls {
    wifi: async_channel::Sender<Control>,
    bluetooth: async_channel::Sender<Control>,
    audio: async_channel::Sender<Control>,
    wifi_open: bool,
    bluetooth_open: bool,
    audio_open: bool,
    detailed: bool,
    bluetooth_detailed: bool,
    audio_streams_open: bool,
}
impl Controls {
    pub fn new(
        handle: &LoopHandle<'static, crate::FeatherCompositor>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (updates, source) = channel::sync_channel(8);
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(update) = event {
                if let Update::AudioFeedback(ref audio) = update {
                    state.show_audio_feedback(audio);
                }
                let settings_visible = state
                    .windows
                    .iter()
                    .any(|w| w.app_id == "builtin.settings" && w.is_visible());
                let sound_page =
                    settings_visible && state.system_settings.page == crate::system_settings::Page::Sound;
                let network_page =
                    settings_visible && state.system_settings.page == crate::system_settings::Page::Network;
                let bluetooth_page =
                    settings_visible && state.system_settings.page == crate::system_settings::Page::Bluetooth;
                let audio_update = matches!(update, Update::Audio(_) | Update::AudioFeedback(_));
                let wifi_update = matches!(update, Update::Wifi(_));
                let bt_update = matches!(update, Update::Bluetooth(_));
                if sound_page {
                    if let Update::Error("Звук", ref error) = update {
                        state.system_settings.error = Some(error.clone());
                        state.mark_settings_dirty();
                        state.needs_redraw = true;
                        return;
                    }
                }
                if network_page && !state.shell.control_center_open {
                    if let Update::Error("Wi‑Fi", ref error) = update {
                        state.system_settings.error = Some(error.clone());
                        state.play_effect(
                            crate::sounds::Effect::Alert(state.sound_preferences.tone),
                            false,
                        );
                        state.mark_settings_dirty();
                        state.needs_redraw = true;
                        return;
                    }
                }
                if bluetooth_page && !state.shell.control_center_open {
                    if let Update::Error("Bluetooth", ref error) = update {
                        state.shell.cc.bluetooth.connecting = None;
                        state.system_settings.error = Some(error.clone());
                        state.play_effect(
                            crate::sounds::Effect::Alert(state.sound_preferences.tone),
                            false,
                        );
                        state.mark_settings_dirty();
                        state.needs_redraw = true;
                        return;
                    }
                }
                let previous = if sound_page && audio_update {
                    Some(state.shell.cc.audio.clone())
                } else {
                    None
                };
                let previous_wifi = if network_page && wifi_update {
                    Some(state.shell.cc.wifi.clone())
                } else {
                    None
                };
                let previous_bt = if bluetooth_page && bt_update {
                    Some(state.shell.cc.bluetooth.clone())
                } else {
                    None
                };
                if state.shell.install_controls(update) {
                    state.needs_redraw = true;
                }
                if previous.as_ref().is_some_and(|old| *old != state.shell.cc.audio)
                    || previous_wifi.as_ref().is_some_and(|old| *old != state.shell.cc.wifi)
                    || previous_bt.as_ref().is_some_and(|old| *old != state.shell.cc.bluetooth)
                {
                    if state.system_settings.audio_drag.as_ref().is_some_and(|(_, command)| match command {
                        Control::InputVolume { source, .. } => state.shell.cc.audio.current_source().is_none_or(|s| s.name != *source),
                        Control::Volume { sink, .. } => state.shell.cc.audio.current().is_none_or(|s| s.name != *sink),
                        Control::StreamVolume { index, .. } => !state.shell.cc.audio.streams.iter().any(|s| s.index == *index),
                        _ => true,
                    }) { state.system_settings.audio_drag = None; }
                    state.mark_settings_dirty();
                    state.needs_redraw = true;
                }
            }
        })?;
        let (wifi, rx) = async_channel::bounded(16);
        let out = updates.clone();
        std::thread::Builder::new()
            .name("feather-network".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                future::block_on(network::run(rx, out));
            })?;
        let (bluetooth, rx) = async_channel::bounded(16);
        let out = updates.clone();
        std::thread::Builder::new()
            .name("feather-bluetooth".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                future::block_on(bluetooth::run(rx, out));
            })?;
        let (audio, rx) = async_channel::bounded(16);
        std::thread::Builder::new()
            .name("feather-audio".into())
            .stack_size(512 * 1024)
            .spawn(move || audio_run(rx, updates))?;
        Ok(Self {
            wifi,
            bluetooth,
            audio,
            wifi_open: false,
            bluetooth_open: false,
            audio_open: false,
            detailed: false,
            bluetooth_detailed: false,
            audio_streams_open: false,
        })
    }
    pub fn sync(
        &mut self,
        open: bool,
        detailed: bool,
        bluetooth_detailed: bool,
        audio_streams: bool,
    ) {
        if open && !self.wifi_open && self.wifi.try_send(Control::Refresh).is_ok() {
            self.wifi_open = true;
        }
        if open && !self.bluetooth_open && self.bluetooth.try_send(Control::Refresh).is_ok() {
            self.bluetooth_open = true;
        }
        if open && !self.audio_open && self.audio.try_send(Control::Refresh).is_ok() {
            self.audio_open = true;
        }
        if !open {
            self.wifi_open = false;
            self.bluetooth_open = false;
            self.audio_open = false;
        }
        if detailed != self.detailed && self.send(Control::WatchWifi(detailed)).is_ok() {
            self.detailed = detailed;
        }
        if bluetooth_detailed != self.bluetooth_detailed
            && self.send(Control::WatchBluetooth(bluetooth_detailed)).is_ok()
        {
            self.bluetooth_detailed = bluetooth_detailed;
        }
        if audio_streams != self.audio_streams_open
            && self.send(Control::WatchAudioStreams(audio_streams)).is_ok()
        {
            self.audio_streams_open = audio_streams;
        }
    }
    pub fn send(&self, command: Control) -> Result<(), &'static str> {
        match command {
            Control::Refresh => {
                let a = self.wifi.try_send(Control::Refresh);
                let b = self.bluetooth.try_send(Control::Refresh);
                let c = self.audio.try_send(Control::Refresh);
                if a.is_err() || b.is_err() || c.is_err() {
                    Err("Служба занята. Повторите действие.")
                } else {
                    Ok(())
                }
            }
            Control::RefreshNetwork
            | Control::Scan
            | Control::WatchWifi(_)
            | Control::Wifi(_)
            | Control::Connect { .. }
            | Control::Disconnect(_)
            | Control::ConnectEthernet { .. }
            | Control::SetAutoconnect { .. }
            | Control::ForgetConnection(_)
            | Control::ConnectVpn(_)
            | Control::DisconnectVpn(_) => self
                .wifi
                .try_send(command)
                .map_err(|_| "Сеть занята. Повторите действие."),
            Control::RefreshBluetooth
            | Control::ScanBluetooth
            | Control::WatchBluetooth(_)
            | Control::Bluetooth(_)
            | Control::ConnectBluetooth(_)
            | Control::DisconnectBluetooth(_)
            | Control::ForgetBluetooth(_) => self
                .bluetooth
                .try_send(command)
                .map_err(|_| "Bluetooth занят. Повторите действие."),
            _ => self
                .audio
                .try_send(command)
                .map_err(|_| "Звук занят. Повторите действие."),
        }
    }
}

/// Commands have bounded time and bounded output; no shell interpolation, passwords or user
/// strings on a shell command line. This runs exclusively on the audio worker.
fn pactl_until(args: &[String], deadline: Instant) -> Result<String, String> {
    if Instant::now() >= deadline {
        return Err("Служба звука не отвечает".into());
    }
    let mut child = Command::new("pactl")
        .args(args)
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Не найден pactl (пакет libpulse)".to_string())?;
    let mut stdout = child.stdout.take().unwrap();
    use std::os::fd::AsRawFd;
    // Nonblocking drain avoids a full-pipe deadlock and an unbounded reader.join()
    // if a descendant inherited stdout. The deadline covers both process and pipe.
    let fd = stdout.as_raw_fd();
    if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Не удалось читать ответ звука".into());
    }
    let mut bytes = Vec::new();
    let result = loop {
        let mut buf = [0; 8192];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.len() > 1024 * 1024 {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if bytes.len() > 1024 * 1024 {
            break Err("Слишком большой ответ службы звука".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err("Служба звука отклонила действие".to_string())
                }
            }
            Err(_) => break Err("Не удалось дождаться службы звука".to_string()),
            _ if Instant::now() >= deadline => break Err("Служба звука не отвечает".to_string()),
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result?;
    let mut tail = [0; 8192];
    while let Ok(n) = stdout.read(&mut tail) {
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&tail[..n]);
        if bytes.len() > 1024 * 1024 {
            break;
        }
    }
    if bytes.len() > 1024 * 1024 {
        return Err("Слишком большой ответ службы звука".into());
    }
    String::from_utf8(bytes).map_err(|_| "Некорректный ответ службы звука".into())
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).into()).collect()
}

fn parse_sinks(json: &str) -> Result<Vec<Sink>, String> {
    parse_devices(json, false)
}
/// pactl names the monitored sink of a monitor source in its "monitor_source" field.
fn parse_sources(json: &str) -> Result<Vec<Sink>, String> {
    parse_devices(json, true)
}
fn parse_devices(json: &str, skip_monitors: bool) -> Result<Vec<Sink>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "Некорректный список устройств звука")?;
    let list = value
        .as_array()
        .ok_or("Некорректный список устройств звука")?;
    let mut result = Vec::new();
    for s in list.iter().take(128) {
        if skip_monitors && (s["monitor_source"].as_str().is_some_and(|m| !m.is_empty())
            || s["properties"]["device.class"].as_str() == Some("monitor")
            || s["monitor_of_sink"].as_u64().is_some_and(|index| index != u32::MAX as u64)) {
            continue;
        }
        let name = s["name"]
            .as_str()
            .ok_or("Устройство без имени")?
            .to_string();
        let channels = s["volume"]
            .as_object()
            .ok_or("Нет громкости устройства")?
            .values()
            .map(|v| {
                v["value"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or("Некорректная громкость")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if channels.is_empty() {
            continue;
        }
        let balance_channels = if channels.iter().copied().max().unwrap_or(0) > 0 {
            channels.clone()
        } else {
            Vec::new()
        };
        let props = &s["properties"];
        let headphones = props["device.bus"].as_str() == Some("bluetooth")
            || matches!(props["device.form_factor"].as_str(), Some("headphone" | "headset" | "hands-free" | "handset"));
        result.push(Sink {
            headphones,
            label: s["description"]
                .as_str()
                .unwrap_or(&name)
                .chars()
                .take(256)
                .collect(),
            name,
            index: s["index"].as_u64().ok_or("Нет индекса устройства")? as u32,
            channels,
            balance_channels,
            muted: s["mute"].as_bool().unwrap_or(false),
            unplugged: s["ports"].as_array().is_some_and(|ports| {
                if let Some(active) = s["active_port"].as_str().and_then(|name| ports.iter().find(|p| p["name"].as_str() == Some(name))) {
                    active["availability"].as_str() == Some("not available")
                } else {
                    !ports.is_empty() && ports.iter().all(|p| p["availability"].as_str() == Some("not available"))
                }
            }),
        });
    }
    Ok(result)
}
pub(crate) fn parse_streams(json: &str, sinks: &[Sink]) -> Result<Vec<AudioStream>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "Некорректный список потоков звука")?;
    let list = value
        .as_array()
        .ok_or("Некорректный список потоков звука")?;
    let mut result = Vec::new();
    for s in list.iter().take(64) {
        let props = &s["properties"];
        if props["media.role"].as_str() == Some("event")
            || props["application.id"].as_str() == Some("org.PulseAudio.pavucontrol")
            || props["media.name"].as_str() == Some("Peak detect")
            || props["stream.monitor"].as_str() == Some("true")
        {
            continue;
        }
        let Some(index) = s["index"].as_u64().and_then(|v| u32::try_from(v).ok()) else {
            continue;
        };
        let channels = match s["volume"].as_object() {
            Some(vol) => vol
                .values()
                .filter_map(|v| v["value"].as_u64().and_then(|n| u32::try_from(n).ok()))
                .collect::<Vec<_>>(),
            None => continue,
        };
        if channels.is_empty() {
            continue;
        }
        let app_name = ["application.name", "node.name", "application.process.binary", "media.name"]
            .into_iter()
            .find_map(|k| props[k].as_str().map(str::trim).filter(|v| !v.is_empty()))
            .unwrap_or("");
        if app_name.is_empty() {
            continue;
        }
        let name: String = app_name.chars().take(128).collect();
        let raw_media = props["media.name"].as_str().map(str::trim).unwrap_or("");
        let generic_media = matches!(
            raw_media.to_ascii_lowercase().as_str(),
            "" | "playback" | "audiostream" | "audio stream" | "output" | "simple directmedia layer"
        ) || raw_media.eq_ignore_ascii_case(&name);
        let subtitle: String = if generic_media {
            String::new()
        } else {
            raw_media.chars().take(128).collect()
        };
        let binary: String = props["application.process.binary"]
            .as_str()
            .unwrap_or("")
            .trim()
            .chars()
            .take(64)
            .collect();
        let icon_name: String = props["application.icon_name"]
            .as_str()
            .unwrap_or("")
            .trim()
            .chars()
            .take(64)
            .collect();
        let sink_index = s["sink"].as_u64().unwrap_or(0) as u32;
        let sink_name = sinks
            .iter()
            .find(|d| d.index == sink_index)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        let balance_channels = if channels.iter().copied().max().unwrap_or(0) > 0 {
            channels.clone()
        } else {
            Vec::new()
        };
        result.push(AudioStream {
            index,
            name,
            subtitle,
            binary,
            icon_name,
            sink_index,
            sink_name,
            channels,
            balance_channels,
            muted: s["mute"].as_bool().unwrap_or(false),
        });
    }
    result.sort_by_key(|s| s.index);
    Ok(result)
}
fn audio_streams_snapshot(sinks: &[Sink]) -> Result<Vec<AudioStream>, String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let json = pactl_until(&strings(&["--format=json", "list", "sink-inputs"]), deadline)?;
    parse_streams(&json, sinks)
}
fn audio_snapshot(cached_inputs: Option<&Audio>) -> Audio {
    let deadline = Instant::now() + Duration::from_secs(3);
    let run = |args: &[String]| pactl_until(args, deadline);
    match cached_inputs {
        None => audio_snapshot_with(&run),
        Some(cached) => audio_snapshot_with_cached_inputs(&run, Some(cached)),
    }
}
fn audio_snapshot_with(run: &dyn Fn(&[String]) -> Result<String, String>) -> Audio {
    audio_snapshot_with_cached_inputs(run, None)
}
fn audio_snapshot_with_cached_inputs(
    run: &dyn Fn(&[String]) -> Result<String, String>,
    cached_inputs: Option<&Audio>,
) -> Audio {
    let mut audio = Audio::default();
    match run(&strings(&["--format=json", "list", "sinks"])).and_then(|j| parse_sinks(&j)) {
        Ok(devices) => audio.sinks = devices,
        Err(error) => audio.error = Some(error),
    }
    if !audio.sinks.is_empty() {
        match run(&strings(&["get-default-sink"])) {
            Ok(name) => audio.default = name.trim().into(),
            Err(error) => audio.error = Some(error),
        }
    }
    if let Some(cached) = cached_inputs {
        audio.sources = cached.sources.clone();
        audio.default_source = cached.default_source.clone();
        audio.input_error = cached.input_error.clone();
        return audio;
    }
    // A machine can have inputs without outputs, or working outputs when input queries fail.
    match run(&strings(&["--format=json", "list", "sources"])).and_then(|j| parse_sources(&j)) {
        Ok(devices) => audio.sources = devices,
        Err(error) => audio.input_error = Some(error),
    }
    if !audio.sources.is_empty() {
        match run(&strings(&["get-default-source"])) {
            Ok(name) => audio.default_source = name.trim().into(),
            Err(error) => audio.input_error = Some(error),
        }
    }
    audio
}

fn channel_volumes(channels: &[u32], balance_channels: &[u32], value: f32) -> Vec<String> {
    let active = if channels.iter().copied().max().unwrap_or(0) > 0 {
        channels
    } else if balance_channels.len() == channels.len()
        && balance_channels.iter().copied().max().unwrap_or(0) > 0
    {
        balance_channels
    } else {
        channels
    };
    let peak = active.iter().copied().max().unwrap_or(0) as f32;
    let target = value.clamp(0.0, 1.0);
    active
        .iter()
        .map(|c| {
            format!(
                "{:.3}%",
                if peak == 0.0 {
                    target
                } else {
                    *c as f32 / peak * target
                } * 100.0
            )
        })
        .collect()
}
fn volumes(sink: &Sink, value: f32) -> Vec<String> {
    channel_volumes(&sink.channels, &sink.balance_channels, value)
}
fn audio_action(command: Control, state: &Audio) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    audio_action_with(command, state, &|args| pactl_until(args, deadline))
}
fn audio_action_with(
    command: Control,
    state: &Audio,
    run: &dyn Fn(&[String]) -> Result<String, String>,
) -> Result<(), String> {
    match command {
        Control::Volume { sink, value } => {
            let info = state
                .sinks
                .iter()
                .find(|s| s.name == sink)
                .ok_or("Устройство отключено")?;
            let mut args = vec!["set-sink-volume".into(), sink];
            args.extend(volumes(info, value));
            run(&args)?;
        }
        Control::Mute { sink, muted } => {
            if !state.sinks.iter().any(|s| s.name == sink) {
                return Err("Устройство отключено".into());
            }
            run(&vec![
                "set-sink-mute".into(),
                sink,
                if muted { "1" } else { "0" }.into(),
            ])?;
        }
        Control::AudioKey(key) => audio_keys_with(&[key], state, run)?,
        Control::InputVolume { source, value } => {
            let info = state
                .sources
                .iter()
                .find(|s| s.name == source)
                .ok_or("Микрофон отключён")?;
            let mut args = vec!["set-source-volume".into(), source];
            args.extend(volumes(info, value));
            run(&args)?;
        }
        Control::InputMute { source, muted } => {
            if !state.sources.iter().any(|s| s.name == source) {
                return Err("Микрофон отключён".into());
            }
            run(&vec![
                "set-source-mute".into(),
                source,
                if muted { "1" } else { "0" }.into(),
            ])?;
        }
        Control::Input(name) => {
            if !state.sources.iter().any(|s| s.name == name) {
                return Err("Микрофон отключён".into());
            }
            run(&vec!["set-default-source".into(), name.clone()])?;
            // Recordings from the old default follow, like playback does for outputs.
            let old = state.current_source().map(|s| s.index as u64);
            move_streams(run, "source-outputs", "source", "move-source-output", old, &name)?;
        }
        Control::Output(name) => {
            if !state.sinks.iter().any(|s| s.name == name) {
                return Err("Устройство отключено".into());
            }
            run(&vec!["set-default-sink".into(), name.clone()])?;
            // Existing streams on the old default follow an explicit output selection.
            let old = state.current().map(|s| s.index as u64);
            move_streams(run, "sink-inputs", "sink", "move-sink-input", old, &name)?;
        }
        Control::StreamVolume { index, value } => {
            let mut args = vec!["set-sink-input-volume".into(), index.to_string()];
            if let Some(stream) = state.streams.iter().find(|s| s.index == index) {
                args.extend(channel_volumes(&stream.channels, &stream.balance_channels, value));
            } else {
                args.push(format!("{:.3}%", value.clamp(0.0, 1.0) * 100.0));
            }
            run(&args)?;
        }
        Control::StreamMute { index, muted } => {
            run(&vec![
                "set-sink-input-mute".into(),
                index.to_string(),
                if muted { "1" } else { "0" }.into(),
            ])?;
        }
        Control::StreamOutput { index, sink } => {
            if !state.sinks.iter().any(|s| s.name == sink) {
                return Err("Устройство отключено".into());
            }
            run(&vec![
                "move-sink-input".into(),
                index.to_string(),
                sink,
            ])?;
        }
        _ => {}
    }
    Ok(())
}

fn move_streams(
    run: &dyn Fn(&[String]) -> Result<String, String>,
    list: &str,
    field: &str,
    command: &str,
    old: Option<u64>,
    name: &str,
) -> Result<(), String> {
    let Some(old) = old else { return Ok(()) };
    let streams: serde_json::Value =
        serde_json::from_str(&run(&strings(&["--format=json", "list", list]))?)
            .map_err(|_| "Некорректный список потоков")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    for stream in streams
        .as_array()
        .ok_or("Некорректный список потоков")?
        .iter()
        .take(64)
    {
        if Instant::now() >= deadline {
            return Err("Устройство выбрано; не все потоки успели перейти".into());
        }
        if stream[field].as_u64() == Some(old) {
            if let Some(id) = stream["index"].as_u64() {
                let _ = run(&vec![command.into(), id.to_string(), name.into()]);
            }
        }
    }
    Ok(())
}

fn audio_keys_with(keys: &[AudioKey], state: &Audio, run: &dyn Fn(&[String]) -> Result<String, String>) -> Result<(), String> {
    let sink = state.current().ok_or("Нет устройства вывода звука")?;
    let (mut value, mut muted) = (sink.volume(), sink.muted);
    let mut changed_volume = false;
    for key in keys {
        match key {
            AudioKey::Mute => muted = !muted,
            AudioKey::Raise | AudioKey::Lower => {
                value = (value + if *key == AudioKey::Raise { 0.05 } else { -0.05 }).clamp(0.0, 1.0);
                changed_volume = true;
                muted = false;
            }
        }
    }
    if changed_volume && (value - sink.volume()).abs() > 0.00001 {
        let mut args = vec!["set-sink-volume".into(), sink.name.clone()];
        args.extend(volumes(sink, value));
        run(&args)?;
    }
    if muted != sink.muted {
        run(&vec!["set-sink-mute".into(), sink.name.clone(), if muted {"1"} else {"0"}.into()])?;
    }
    Ok(())
}

struct Subscription(Arc<Mutex<Child>>, std::os::unix::net::UnixStream);
impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.1.shutdown(std::net::Shutdown::Both);
        if let Ok(mut p) = self.0.lock() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}
fn subscribe(
    wake: async_channel::Sender<()>,
    sources_dirty: Arc<std::sync::atomic::AtomicBool>,
    watch_streams: Arc<std::sync::atomic::AtomicBool>,
    streams_dirty: Arc<std::sync::atomic::AtomicBool>,
) -> Result<Subscription, String> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new("pactl");
    cmd.arg("subscribe")
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // This child belongs to our process, including when the compositor exits abruptly.
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(0);
            }
            Ok(())
        });
    }
    let (cancel, reader_cancel) =
        std::os::unix::net::UnixStream::pair().map_err(|_| "Не удалось подписаться на звук")?;
    let mut child = cmd.spawn().map_err(|_| "Не удалось подписаться на звук")?;
    let mut stdout = child.stdout.take().unwrap();
    let guard = Subscription(Arc::new(Mutex::new(child)), cancel);
    std::thread::Builder::new()
        .name("feather-audio-events".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            use std::os::fd::AsRawFd;
            let fd = stdout.as_raw_fd();
            if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
                return;
            }
            let mut fds = [
                libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: reader_cancel.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let mut pending = Vec::new();
            loop {
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                if ready < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if fds[1].revents != 0 {
                    break;
                }
                let mut buf = [0; 4096];
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                            let line = String::from_utf8_lossy(&pending[..end]);
                            if line.contains(" on source #") || line.contains(" on server #") {
                                sources_dirty.store(true, std::sync::atomic::Ordering::Release);
                                let _ = wake.try_send(());
                            } else if line.contains(" on sink #") {
                                let _ = wake.try_send(());
                            } else if line.contains(" on sink-input #")
                                && watch_streams.load(std::sync::atomic::Ordering::Acquire)
                            {
                                streams_dirty.store(true, std::sync::atomic::Ordering::Release);
                                let _ = wake.try_send(());
                            }
                            pending.drain(..=end);
                        }
                        if pending.len() > 8192 {
                            pending.clear();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
            sources_dirty.store(true, std::sync::atomic::Ordering::Release);
            let _ = wake.try_send(());
        })
        .map_err(|_| "Не удалось читать события звука")?;
    Ok(guard)
}
/// Headphones that have just appeared in the output list (not at the first listing, and not
/// a sink that vanished moments ago: a Bluetooth profile change recreates the same sink)
fn arrived_headphones(
    before: &Audio,
    after: &Audio,
    vanished: &mut std::collections::HashMap<String, Instant>,
    now: Instant,
) -> Option<String> {
    for sink in &before.sinks {
        if !after.sinks.iter().any(|s| s.name == sink.name) {
            vanished.insert(sink.name.clone(), now);
        }
    }
    vanished.retain(|_, at| now.duration_since(*at) < Duration::from_secs(10));
    if before.sinks.is_empty() || before.error.is_some() || after.error.is_some() {
        return None;
    }
    after
        .sinks
        .iter()
        .filter(|s| s.headphones && !s.unplugged && s.name != after.default)
        .find(|s| !before.sinks.iter().any(|b| b.name == s.name) && !vanished.contains_key(&s.name))
        .map(|s| s.name.clone())
}
fn audio_run(commands: async_channel::Receiver<Control>, updates: channel::SyncSender<Update>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (wake, changes) = async_channel::bounded(1);
    let sources_dirty = Arc::new(AtomicBool::new(true));
    let watch_streams = Arc::new(AtomicBool::new(false));
    let streams_dirty = Arc::new(AtomicBool::new(false));
    let mut watcher = subscribe(
        wake.clone(),
        sources_dirty.clone(),
        watch_streams.clone(),
        streams_dirty.clone(),
    )
    .ok();
    let mut state = Audio::default();
    let mut command = Some(Control::Refresh);
    let mut auto_headphones = true;
    let mut vanished = std::collections::HashMap::new();
    loop {
        let mut feedback = false;
        let mut queued = None;
        let mut refresh_inputs = sources_dirty.swap(false, Ordering::AcqRel);
        let mut force_streams = streams_dirty.swap(false, Ordering::AcqRel);
        let mut skip_snapshot = false;
        if let Some(c) = command.take() {
            if matches!(
                c,
                Control::Refresh
                    | Control::RefreshAudio
                    | Control::Input(_)
                    | Control::InputMute { .. }
                    | Control::InputVolume { .. }
            ) {
                refresh_inputs = true;
            }
            if matches!(
                c,
                Control::StreamVolume { .. }
                    | Control::StreamMute { .. }
                    | Control::StreamOutput { .. }
            ) {
                force_streams = true;
            }
            if matches!(c, Control::Refresh | Control::RefreshAudio)
                && watcher
                    .as_ref()
                    .is_none_or(|w| w.0.lock().unwrap().try_wait().ok().flatten().is_some())
            {
                watcher = subscribe(
                    wake.clone(),
                    sources_dirty.clone(),
                    watch_streams.clone(),
                    streams_dirty.clone(),
                )
                .ok();
            }
            let result = match c {
                Control::AutoHeadphones(on) => {
                    auto_headphones = on;
                    Ok(())
                }
                Control::WatchAudioStreams(on) => {
                    watch_streams.store(on, Ordering::Release);
                    if on {
                        force_streams = true;
                    } else {
                        skip_snapshot = !refresh_inputs;
                    }
                    Ok(())
                }
                Control::QueryStreams(slot) => {
                    let sinks_snap = if state.sinks.is_empty() {
                        audio_snapshot(Some(&state)).sinks
                    } else {
                        state.sinks.clone()
                    };
                    let res = audio_streams_snapshot(&sinks_snap);
                    if let Ok(ref list) = res {
                        let mut updated = list.clone();
                        let mut tmp = state.clone();
                        tmp.streams = std::mem::take(&mut updated);
                        tmp.retain_zero_balances(&state);
                        if tmp != state {
                            state = tmp;
                            let _ = updates.send(Update::Audio(state.clone()));
                        }
                    }
                    if let Ok(mut guard) = slot.lock() {
                        if let Some(cb) = guard.take() {
                            cb(res.map(|_| state.streams.clone()));
                        }
                    }
                    skip_snapshot = !refresh_inputs;
                    Ok(())
                }
                Control::AudioKey(first) => {
                    feedback = true;
                    let mut keys = vec![first];
                    // Collapse a burst without crossing a slider/output command or reordering mute.
                    for _ in 1..16 {
                        match commands.try_recv() {
                            Ok(Control::AudioKey(key)) => keys.push(key),
                            Ok(other) => { queued = Some(other); break; }
                            Err(_) => break,
                        }
                    }
                    let deadline = Instant::now() + Duration::from_secs(3);
                    audio_keys_with(&keys, &state, &|args| pactl_until(args, deadline))
                }
                other => audio_action(other, &state),
            };
            if let Err(error) = result {
                refresh_inputs = true;
                skip_snapshot = false;
                if updates.send(Update::Error("Звук", error)).is_err() {
                    break;
                }
            }
        }
        if !skip_snapshot {
            refresh_inputs |= sources_dirty.swap(false, Ordering::AcqRel);
            let mut next = audio_snapshot(if refresh_inputs { None } else { Some(&state) });
            if watch_streams.load(Ordering::Acquire) || force_streams {
                if let Ok(streams) = audio_streams_snapshot(&next.sinks) {
                    next.streams = streams;
                } else {
                    next.streams = state.streams.clone();
                }
            } else {
                next.streams = state.streams.clone();
            }
            next.retain_zero_balances(&state);
            let arrived = arrived_headphones(&state, &next, &mut vanished, Instant::now());
            if let Some(target) = arrived.filter(|_| auto_headphones) {
                // Like macOS: freshly connected headphones take over, playing streams follow
                if audio_action(Control::Output(target), &next).is_ok() {
                    let mut switched = audio_snapshot(Some(&next));
                    if watch_streams.load(Ordering::Acquire) || force_streams {
                        if let Ok(streams) = audio_streams_snapshot(&switched.sinks) {
                            switched.streams = streams;
                        } else {
                            switched.streams = next.streams.clone();
                        }
                    } else {
                        switched.streams = next.streams.clone();
                    }
                    next = switched;
                    next.retain_zero_balances(&state);
                }
            }
            if feedback || next != state {
                state = next;
                let update = if feedback { Update::AudioFeedback(state.clone()) } else { Update::Audio(state.clone()) };
                if updates.send(update).is_err() { break; }
            }
        }
        if queued.is_some() { command = queued; continue; }
        enum Event {
            Command(Result<Control, async_channel::RecvError>),
            Changed,
        }
        match future::block_on(future::race(
            async { Event::Command(commands.recv().await) },
            async {
                let _ = changes.recv().await;
                Event::Changed
            },
        )) {
            Event::Command(Ok(c)) => command = Some(c),
            Event::Command(Err(_)) => break,
            Event::Changed => while changes.try_recv().is_ok() {},
        }
    }
    drop(watcher);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_json_and_volume_keep_channels_and_balance() {
        let data = r#"[{"index":12,"name":"usb.output","description":"USB ☃","mute":true,"volume":{"left":{"value":32768},"right":{"value":65536}}}]"#;
        let sinks = parse_sinks(data).unwrap();
        assert_eq!(sinks[0].volume(), 1.0);
        assert!(sinks[0].muted);
        assert_eq!(volumes(&sinks[0], 0.5), vec!["25.000%", "50.000%"]);
        assert!(parse_sinks("not json").is_err());
        assert!(parse_sinks(r#"[{"name":"bad","volume":{}}]"#)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn multimedia_keys_apply_in_order_clamp_preserve_balance_and_unmute() {
        use AudioKey::{Raise as U, Lower as D, Mute as M};
        let sink = Sink {name:"default; literal".into(),channels:vec![32768,16384],muted:false,..Default::default()};
        let mut state = Audio {default:sink.name.clone(),sinks:vec![sink],..Default::default()};
        let calls=Mutex::new(Vec::new());
        let run=|args:&[String]|{calls.lock().unwrap().push(args.to_vec());Ok(String::new())};
        audio_keys_with(&[U,U,D],&state,&run).unwrap();
        assert_eq!(*calls.lock().unwrap(),[vec!["set-sink-volume","default; literal","55.000%","27.500%"]]);
        calls.lock().unwrap().clear();
        audio_keys_with(&[M,M],&state,&run).unwrap();assert!(calls.lock().unwrap().is_empty());
        audio_keys_with(&[U;32],&state,&run).unwrap();
        assert_eq!(calls.lock().unwrap()[0],["set-sink-volume","default; literal","100.000%","50.000%"]);
        calls.lock().unwrap().clear();
        state.sinks[0].muted=true;
        audio_keys_with(&[D],&state,&run).unwrap();
        assert_eq!(*calls.lock().unwrap(),[
            vec!["set-sink-volume","default; literal","45.000%","22.500%"],
            vec!["set-sink-mute","default; literal","0"]]);
        calls.lock().unwrap().clear();
        audio_keys_with(&[U,M],&state,&run).unwrap();
        assert_eq!(calls.lock().unwrap().len(),1,"raise then mute returns to the original mute state");
        calls.lock().unwrap().clear();
        audio_keys_with(&[D;32],&state,&run).unwrap();assert_eq!(calls.lock().unwrap()[0][2..],["0.000%","0.000%"]);
        calls.lock().unwrap().clear();state.sinks[0].channels=vec![0,0];
        audio_keys_with(&[U],&state,&run).unwrap();assert_eq!(calls.lock().unwrap()[0][2..],["5.000%","5.000%"]);
        calls.lock().unwrap().clear();state.sinks.clear();
        assert!(audio_keys_with(&[U],&state,&run).is_err());assert!(calls.lock().unwrap().is_empty());
        use smithay::input::keyboard::keysyms as k;
        assert_eq!(AudioKey::from_keysym(k::KEY_XF86AudioRaiseVolume),Some(U));
        for key in [k::KEY_F10,k::KEY_F11,k::KEY_F12] {assert_eq!(AudioKey::from_keysym(key),None);}
    }

    #[test]
    fn audio_actions_target_named_sink_and_move_only_old_default_streams() {
        let a = Sink {
            name: "old sink; literal".into(),
            index: 7,
            channels: vec![65536, 32768],
            ..Sink::default()
        };
        let b = Sink {
            name: "usb output".into(),
            index: 8,
            channels: vec![65536],
            ..Sink::default()
        };
        let state = Audio {
            default: a.name.clone(),
            sinks: vec![a.clone(), b.clone()],
            ..Audio::default()
        };
        let calls = Mutex::new(Vec::new());
        let fake = |args: &[String]| {
            calls.lock().unwrap().push(args.to_vec());
            Ok(if args[0] == "--format=json" {
                r#"[{"index":4,"sink":7},{"index":5,"sink":99}]"#.into()
            } else {
                String::new()
            })
        };
        audio_action_with(
            Control::Volume {
                sink: a.name.clone(),
                value: 0.5,
            },
            &state,
            &fake,
        )
        .unwrap();
        audio_action_with(
            Control::Mute {
                sink: a.name.clone(),
                muted: true,
            },
            &state,
            &fake,
        )
        .unwrap();
        audio_action_with(Control::Output(b.name.clone()), &state, &fake).unwrap();
        let before = calls.lock().unwrap().len();
        assert!(audio_action_with(Control::Output("removed".into()), &state, &fake).is_err());
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), before);
        assert_eq!(
            log[0],
            vec!["set-sink-volume", "old sink; literal", "50.000%", "25.000%"]
        );
        assert_eq!(log[1], vec!["set-sink-mute", "old sink; literal", "1"]);
        assert_eq!(
            log.last().unwrap(),
            &vec!["move-sink-input", "4", "usb output"]
        );
        assert!(!log.iter().any(|a| a.iter().any(|v| v == "5")));
    }

    #[test]
    fn sources_skip_monitors_and_report_unplugged_ports() {
        let data = r#"[
            {"index":1,"name":"out.monitor","monitor_source":"out","description":"Monitor","mute":false,"volume":{"mono":{"value":65536}},"ports":[]},
            {"index":2,"name":"mic","monitor_source":"","description":"Встроенный","mute":true,"volume":{"front-left":{"value":32768},"front-right":{"value":32768}},
             "ports":[{"name":"front","availability":"not available"},{"name":"rear","availability":"not available"}]},
            {"index":3,"name":"usb","monitor_source":"","description":"USB","mute":false,"volume":{"mono":{"value":65536}},
             "ports":[{"name":"mic","availability":"available"},{"name":"line","availability":"not available"}]}]"#;
        let sources = parse_sources(data).unwrap();
        assert_eq!(sources.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["mic", "usb"]);
        assert!(sources[0].unplugged && sources[0].muted && (sources[0].volume() - 0.5).abs() < 1e-6);
        assert!(!sources[1].unplugged);
        // Outputs keep their monitor-less list unchanged.
        assert_eq!(parse_sinks(data).unwrap().len(), 3);
    }

    #[test]
    fn input_failure_keeps_outputs_and_input_only_servers_are_supported() {
        let device = r#"[{"index":7,"name":"device","volume":{"mono":{"value":32768}}}]"#;
        let audio = audio_snapshot_with(&|args| match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["--format=json", "list", "sinks"] => Ok(device.into()),
            ["get-default-sink"] => Ok("device\n".into()),
            _ => Err("Input service failed".into()),
        });
        assert!(audio.current().is_some());
        assert!(audio.error.is_none());
        assert_eq!(audio.input_error.as_deref(), Some("Input service failed"));
        let audio = audio_snapshot_with(&|args| match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["--format=json", "list", "sinks"] => Ok("[]".into()),
            ["--format=json", "list", "sources"] => Ok(device.into()),
            ["get-default-source"] => Ok("device\n".into()),
            _ => panic!("unexpected query: {args:?}"),
        });
        assert!(audio.current().is_none() && audio.error.is_none());
        assert_eq!(audio.current_source().unwrap().volume(), 0.5);
        assert!(audio.input_error.is_none());
        let audio = audio_snapshot_with(&|_| Ok("not json".into()));
        assert!(audio.error.is_some() && audio.input_error.is_some());
    }
    #[test]
    fn sources_filter_pulse_monitors_and_use_active_port_availability() {
        let devices = parse_sources(r#"[
            {"index":1,"name":"monitor","monitor_of_sink":3,"volume":{"mono":{"value":65536}}},
            {"index":2,"name":"monitor2","properties":{"device.class":"monitor"},"volume":{"mono":{"value":65536}}},
            {"index":3,"name":"mic","monitor_of_sink":4294967295,"volume":{"mono":{"value":65536}},"active_port":"front",
             "ports":[{"name":"front","availability":"not available"},{"name":"rear","availability":"available"}]},
            {"index":4,"name":"usb","volume":{"mono":{"value":65536}},"ports":[]}
        ]"#).unwrap();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].unplugged);
        assert!(!devices[1].unplugged);
    }

    #[test]
    fn input_actions_target_named_source_and_move_only_old_default_recordings() {
        let mic = Sink { name: "builtin mic".into(), index: 2, channels: vec![65536, 32768], ..Sink::default() };
        let usb = Sink { name: "usb mic".into(), index: 3, channels: vec![65536], ..Sink::default() };
        let state = Audio {
            default_source: mic.name.clone(),
            sources: vec![mic.clone(), usb.clone()],
            ..Audio::default()
        };
        let calls = Mutex::new(Vec::new());
        let fake = |args: &[String]| {
            calls.lock().unwrap().push(args.to_vec());
            Ok(if args[0] == "--format=json" {
                r#"[{"index":10,"source":2},{"index":11,"source":3}]"#.into()
            } else {
                String::new()
            })
        };
        audio_action_with(Control::InputVolume { source: mic.name.clone(), value: 0.4 }, &state, &fake).unwrap();
        audio_action_with(Control::InputMute { source: usb.name.clone(), muted: true }, &state, &fake).unwrap();
        audio_action_with(Control::Input(usb.name.clone()), &state, &fake).unwrap();
        let before = calls.lock().unwrap().len();
        assert!(audio_action_with(Control::Input("gone".into()), &state, &fake).is_err());
        assert!(audio_action_with(Control::InputMute { source: "gone".into(), muted: false }, &state, &fake).is_err());
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), before);
        assert_eq!(log[0], vec!["set-source-volume", "builtin mic", "40.000%", "20.000%"]);
        assert_eq!(log[1], vec!["set-source-mute", "usb mic", "1"]);
        assert_eq!(log[2], vec!["set-default-source", "usb mic"]);
        assert_eq!(log[3], vec!["--format=json", "list", "source-outputs"]);
        assert_eq!(log[4], vec!["move-source-output", "10", "usb mic"]);
        assert_eq!(log.len(), 5);
    }
    #[test]
    fn zero_passage_retains_balance_only_for_matching_device_and_channels() {
        let snap = |json: &str| Audio {
            sinks: parse_sinks(json).unwrap(),
            default: "dev".into(),
            ..Audio::default()
        };
        let s1 = snap(r#"[{"index":5,"name":"dev","volume":{"l":{"value":65536},"r":{"value":32768}}}]"#);
        assert_eq!(s1.sinks[0].balance_channels, vec![65536, 32768]);

        let mut s2 = snap(r#"[{"index":5,"name":"dev","volume":{"l":{"value":0},"r":{"value":0}}}]"#);
        assert!(s2.sinks[0].balance_channels.is_empty());
        s2.retain_zero_balances(&s1);
        assert_eq!(s2.sinks[0].volume(), 0.0);
        assert_eq!(s2.sinks[0].balance_channels, vec![65536, 32768]);

        let mut s3 = snap(r#"[{"index":5,"name":"dev","volume":{"l":{"value":0},"r":{"value":0}}}]"#);
        s3.retain_zero_balances(&s2);
        assert_eq!(volumes(&s3.sinks[0], 0.5), vec!["50.000%", "25.000%"]);
        let input_previous = Audio {sources:s1.sinks.clone(),..Audio::default()};
        let mut input_zero = Audio {sources:parse_sources(r#"[{"index":5,"name":"dev","volume":{"l":{"value":0},"r":{"value":0}}}]"#).unwrap(),..Audio::default()};
        input_zero.retain_zero_balances(&input_previous);
        assert_eq!(volumes(&input_zero.sources[0],0.5),["50.000%","25.000%"]);

        let mut new_idx = snap(r#"[{"index":6,"name":"dev","volume":{"l":{"value":0},"r":{"value":0}}}]"#);
        new_idx.retain_zero_balances(&s3);
        assert_eq!(volumes(&new_idx.sinks[0], 0.5), vec!["50.000%", "50.000%"]);

        let mut mono = snap(r#"[{"index":5,"name":"dev","volume":{"m":{"value":0}}}]"#);
        mono.retain_zero_balances(&s3);
        assert_eq!(volumes(&mono.sinks[0], 0.5), vec!["50.000%"]);

        let mut gone = Audio::default();
        gone.retain_zero_balances(&s3);
        let mut back = snap(r#"[{"index":5,"name":"dev","volume":{"l":{"value":0},"r":{"value":0}}}]"#);
        back.retain_zero_balances(&gone);
        assert_eq!(volumes(&back.sinks[0], 0.5), vec!["50.000%", "50.000%"]);
    }
    #[test]
    fn cached_input_snapshot_skips_source_queries_until_full_read() {
        let out1 = r#"[{"index":1,"name":"spk","volume":{"mono":{"value":32768}}}]"#;
        let out2 = r#"[{"index":1,"name":"spk","volume":{"mono":{"value":65536}}}]"#;
        let mic1 = r#"[{"index":2,"name":"mic1","volume":{"mono":{"value":16384}}}]"#;
        let mic2 = r#"[{"index":3,"name":"mic2","volume":{"mono":{"value":65536}}}]"#;
        let initial = audio_snapshot_with(&|args| match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["--format=json", "list", "sinks"] => Ok(out1.into()),
            ["get-default-sink"] => Ok("spk\n".into()),
            ["--format=json", "list", "sources"] => Ok(mic1.into()),
            ["get-default-source"] => Ok("mic1\n".into()),
            _ => panic!("unexpected query: {args:?}"),
        });
        let fast = audio_snapshot_with_cached_inputs(
            &|args| match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["--format=json", "list", "sinks"] => Ok(out2.into()),
                ["get-default-sink"] => Ok("spk\n".into()),
                _ => panic!("source query must not run on cached snapshot: {args:?}"),
            },
            Some(&initial),
        );
        assert_eq!(fast.current().unwrap().volume(), 1.0);
        assert_eq!(fast.default_source, "mic1");
        assert_eq!(fast.sources, initial.sources);

        let mut with_err = initial.clone();
        with_err.input_error = Some("Input error preserved".into());
        let fast_err = audio_snapshot_with_cached_inputs(
            &|args| match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["--format=json", "list", "sinks"] => Ok(out2.into()),
                ["get-default-sink"] => Ok("spk\n".into()),
                _ => panic!("source query must not run on cached snapshot: {args:?}"),
            },
            Some(&with_err),
        );
        assert_eq!(fast_err.input_error.as_deref(), Some("Input error preserved"));

        let refreshed = audio_snapshot_with_cached_inputs(
            &|args| match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["--format=json", "list", "sinks"] => Ok(out2.into()),
                ["get-default-sink"] => Ok("spk\n".into()),
                ["--format=json", "list", "sources"] => Ok(mic2.into()),
                ["get-default-source"] => Ok("mic2\n".into()),
                _ => panic!("unexpected query: {args:?}"),
            },
            None,
        );
        assert_eq!(refreshed.default_source, "mic2");
        assert_eq!(refreshed.current_source().unwrap().volume(), 1.0);
    }
    #[test]
    #[ignore = "Explicit read-only live service probe; never changes volume/network"]
    fn read_live_services() {
        let audio = audio_snapshot(None);
        assert!(audio.error.is_none(), "{:?}", audio.error);
        assert!(audio.input_error.is_none(), "{:?}", audio.input_error);
        println!(
            "audio: {} outputs, default_present={}, volume={:.3}, muted={}",
            audio.sinks.len(),
            audio.current().is_some(),
            audio.current().map_or(0.0, |s| s.volume()),
            audio.current().is_some_and(|s| s.muted)
        );
        future::block_on(async {
            let conn = zbus::connection::Builder::system()
                .unwrap()
                .method_timeout(Duration::from_secs(3))
                .build()
                .await
                .unwrap();
            // Background snapshot reads only the active AP; does not request a scan.
            let state = network::read_only_snapshot(&conn).await.unwrap();
            println!(
                "network: wifi_adapter={}, enabled={}, hardware={}",
                state.available, state.enabled, state.hardware
            );
        });
    }
    #[test]
    fn freshly_connected_headphones_take_over_once() {
        let sink = |name: &str, headphones: bool| Sink { name: name.into(), label: name.into(), channels: vec![30000], headphones, ..Default::default() };
        let wired = Audio { sinks: vec![sink("speakers", false), sink("hdmi", false)], default: "speakers".into(), ..Default::default() };
        let mut with_buds = wired.clone();
        with_buds.sinks.push(sink("bluez_output.AA", true));
        let mut vanished = std::collections::HashMap::new();
        let t = Instant::now();
        // Startup listing: nothing «arrived»
        assert_eq!(arrived_headphones(&Audio::default(), &with_buds, &mut vanished, t), None);
        // Connected later: switch to them
        assert_eq!(arrived_headphones(&wired, &with_buds, &mut vanished, t), Some("bluez_output.AA".into()));
        // Already the default, or a plain new speaker: nothing to do
        let mut chosen = with_buds.clone();
        chosen.default = "bluez_output.AA".into();
        assert_eq!(arrived_headphones(&wired, &chosen, &mut vanished, t), None);
        let mut usb = wired.clone();
        usb.sinks.push(sink("usb-speaker", false));
        assert_eq!(arrived_headphones(&wired, &usb, &mut vanished, t), None);
        // A profile change drops and recreates the sink within seconds: the user's pick stays
        assert_eq!(arrived_headphones(&with_buds, &wired, &mut vanished, t), None);
        assert_eq!(arrived_headphones(&wired, &with_buds, &mut vanished, t + Duration::from_secs(2)), None);
        // A real reconnect later switches again
        assert_eq!(arrived_headphones(&wired, &with_buds, &mut vanished, t + Duration::from_secs(30)), Some("bluez_output.AA".into()));
        // pactl JSON: Bluetooth bus or headset form factor mark headphones
        let json = r#"[{"name":"bluez_output.X","index":1,"volume":{"front-left":{"value":30000}},"properties":{"device.bus":"bluetooth","device.form_factor":"speaker"}},
            {"name":"usb","index":2,"volume":{"mono":{"value":30000}},"properties":{"device.bus":"usb","device.form_factor":"headset"}},
            {"name":"pci","index":3,"volume":{"mono":{"value":30000}},"properties":{"device.bus":"pci","device.form_factor":"internal"}}]"#;
        let sinks = parse_sinks(json).unwrap();
        assert_eq!(sinks.iter().map(|s| s.headphones).collect::<Vec<_>>(), [true, true, false]);
    }
    #[test]
    fn streams_parse_filter_and_route_per_app() {
        let sinks = vec![
            Sink { name: "speakers".into(), label: "Динамики".into(), index: 1, channels: vec![65536, 65536], ..Default::default() },
            Sink { name: "usb_dac".into(), label: "USB ЦАП".into(), index: 7, channels: vec![65536, 65536], ..Default::default() },
        ];
        let json = r#"[
            {"index":42,"sink":7,"mute":false,"volume":{"front-left":{"value":32768},"front-right":{"value":65536}},
             "properties":{"application.name":"Google Chrome","media.name":"YouTube — Lo-Fi","application.process.binary":"chrome"}},
            {"index":11,"sink":1,"mute":true,"volume":{"mono":{"value":49152}},
             "properties":{"application.name":"Telegram Desktop","media.name":"Playback","application.process.binary":"Telegram"}},
            {"index":99,"sink":1,"mute":false,"volume":{"mono":{"value":65536}},
             "properties":{"application.name":"System Bell","media.role":"event"}},
            {"index":100,"sink":1,"mute":false,"volume":{"mono":{"value":65536}},
             "properties":{"application.id":"org.PulseAudio.pavucontrol","media.name":"Peak detect"}}
        ]"#;
        let streams = parse_streams(json, &sinks).unwrap();
        assert_eq!(streams.len(), 2);
        // Sorted deterministically by stream index
        assert_eq!(streams[0].index, 11);
        assert_eq!(streams[0].name, "Telegram Desktop");
        assert_eq!(streams[0].subtitle, "");
        assert_eq!(streams[0].sink_name, "speakers");
        assert!(streams[0].muted);
        assert_eq!(streams[1].index, 42);
        assert_eq!(streams[1].name, "Google Chrome");
        assert_eq!(streams[1].subtitle, "YouTube — Lo-Fi");
        assert_eq!(streams[1].sink_name, "usb_dac");
        assert_eq!(streams[1].volume(), 1.0);

        let state = Audio { sinks, default: "speakers".into(), streams, ..Default::default() };
        let calls = std::cell::RefCell::new(Vec::<Vec<String>>::new());
        let run = |args: &[String]| {
            calls.borrow_mut().push(args.to_vec());
            Ok(String::new())
        };
        audio_action_with(Control::StreamVolume { index: 42, value: 0.5 }, &state, &run).unwrap();
        audio_action_with(Control::StreamMute { index: 11, muted: false }, &state, &run).unwrap();
        audio_action_with(Control::StreamOutput { index: 11, sink: "usb_dac".into() }, &state, &run).unwrap();
        let recorded = calls.into_inner();
        assert_eq!(recorded[0], ["set-sink-input-volume", "42", "25.000%", "50.000%"]);
        assert_eq!(recorded[1], ["set-sink-input-mute", "11", "0"]);
        assert_eq!(recorded[2], ["move-sink-input", "11", "usb_dac"]);
    }
}
