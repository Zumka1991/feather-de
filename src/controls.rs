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
}
impl Sink {
    pub fn volume(&self) -> f32 {
        self.channels.iter().copied().max().unwrap_or(0) as f32 / 65536.0
    }
}
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Audio {
    pub sinks: Vec<Sink>,
    pub default: String,
    /// Microphones and line inputs; monitors of outputs are left out.
    pub sources: Vec<Sink>,
    pub default_source: String,
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
    RefreshAudio,
    Scan,
    WatchWifi(bool),
    Wifi(bool),
    Connect {
        network: network::Network,
        password: Option<Secret>,
    },
    Disconnect(String),
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
    AudioKey(AudioKey),
}
impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Refresh => "Refresh",
            Self::RefreshAudio => "RefreshAudio",
            Self::Scan => "Scan",
            Self::WatchWifi(_) => "WatchWifi",
            Self::Wifi(_) => "Wifi",
            Self::Connect { .. } => "Connect(redacted)",
            Self::Disconnect(_) => "Disconnect",
            Self::Volume { .. } => "Volume",
            Self::Mute { .. } => "Mute",
            Self::Output(_) => "Output",
            Self::InputVolume { .. } => "InputVolume",
            Self::InputMute { .. } => "InputMute",
            Self::Input(_) => "Input",
            Self::AudioKey(_) => "AudioKey",
        })
    }
}
pub enum Update {
    Wifi(network::Wifi),
    Audio(Audio),
    AudioFeedback(Audio),
    Error(&'static str, String),
}

pub struct Controls {
    wifi: async_channel::Sender<Control>,
    audio: async_channel::Sender<Control>,
    wifi_open: bool,
    audio_open: bool,
    detailed: bool,
}
impl Controls {
    pub fn new(
        handle: &LoopHandle<'static, crate::FeatherCompositor>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (updates, source) = channel::sync_channel(4);
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(update) = event {
                if let Update::AudioFeedback(ref audio) = update {
                    state.show_audio_feedback(audio);
                }
                let sound_page = state.system_settings.page == crate::system_settings::Page::Sound
                    && state.windows.iter().any(|w| w.app_id == "builtin.settings" && w.is_visible());
                let audio_update = matches!(update, Update::Audio(_) | Update::AudioFeedback(_));
                if sound_page {
                    if let Update::Error("Звук", ref error) = update {
                        state.system_settings.error = Some(error.clone());
                        state.mark_settings_dirty();
                        state.needs_redraw = true;
                        return;
                    }
                }
                let previous = if sound_page && audio_update { Some(state.shell.cc.audio.clone()) } else { None };
                if state.shell.install_controls(update) {
                    state.needs_redraw = true;
                }
                if previous.as_ref().is_some_and(|old| *old != state.shell.cc.audio) {
                    if state.system_settings.audio_drag.as_ref().is_some_and(|(_, command)| match command {
                        Control::InputVolume { source, .. } => state.shell.cc.audio.current_source().is_none_or(|s| s.name != *source),
                        Control::Volume { sink, .. } => state.shell.cc.audio.current().is_none_or(|s| s.name != *sink),
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
        let (audio, rx) = async_channel::bounded(16);
        std::thread::Builder::new()
            .name("feather-audio".into())
            .stack_size(512 * 1024)
            .spawn(move || audio_run(rx, updates))?;
        Ok(Self {
            wifi,
            audio,
            wifi_open: false,
            audio_open: false,
            detailed: false,
        })
    }
    pub fn sync(&mut self, open: bool, detailed: bool) {
        if open && !self.wifi_open && self.wifi.try_send(Control::Refresh).is_ok() {
            self.wifi_open = true;
        }
        if open && !self.audio_open && self.audio.try_send(Control::Refresh).is_ok() {
            self.audio_open = true;
        }
        if !open {
            self.wifi_open = false;
            self.audio_open = false;
        }
        if detailed != self.detailed && self.send(Control::WatchWifi(detailed)).is_ok() {
            self.detailed = detailed;
        }
    }
    pub fn send(&self, command: Control) -> Result<(), &'static str> {
        match command {
            Control::Refresh => {
                let a = self.wifi.try_send(Control::Refresh);
                let b = self.audio.try_send(Control::Refresh);
                if a.is_err() || b.is_err() {
                    Err("Служба занята. Повторите действие.")
                } else {
                    Ok(())
                }
            }
            Control::Scan
            | Control::WatchWifi(_)
            | Control::Wifi(_)
            | Control::Connect { .. }
            | Control::Disconnect(_) => self
                .wifi
                .try_send(command)
                .map_err(|_| "Сеть занята. Повторите действие."),
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
        result.push(Sink {
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

fn volumes(sink: &Sink, value: f32) -> Vec<String> {
    let active = if sink.channels.iter().copied().max().unwrap_or(0) > 0 {
        &sink.channels
    } else if sink.balance_channels.len() == sink.channels.len()
        && sink.balance_channels.iter().copied().max().unwrap_or(0) > 0
    {
        &sink.balance_channels
    } else {
        &sink.channels
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
fn audio_run(commands: async_channel::Receiver<Control>, updates: channel::SyncSender<Update>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (wake, changes) = async_channel::bounded(1);
    let sources_dirty = Arc::new(AtomicBool::new(true));
    let mut watcher = subscribe(wake.clone(), sources_dirty.clone()).ok();
    let mut state = Audio::default();
    let mut command = Some(Control::Refresh);
    loop {
        let mut feedback = false;
        let mut queued = None;
        let mut refresh_inputs = sources_dirty.swap(false, Ordering::AcqRel);
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
            if matches!(c, Control::Refresh | Control::RefreshAudio)
                && watcher
                    .as_ref()
                    .is_none_or(|w| w.0.lock().unwrap().try_wait().ok().flatten().is_some())
            {
                watcher = subscribe(wake.clone(), sources_dirty.clone()).ok();
            }
            let result = if let Control::AudioKey(first) = c {
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
            } else { audio_action(c, &state) };
            if let Err(error) = result {
                refresh_inputs = true;
                if updates.send(Update::Error("Звук", error)).is_err() {
                    break;
                }
            }
        }
        refresh_inputs |= sources_dirty.swap(false, Ordering::AcqRel);
        let mut next = audio_snapshot(if refresh_inputs { None } else { Some(&state) });
        next.retain_zero_balances(&state);
        if feedback || next != state {
            state = next;
            let update = if feedback { Update::AudioFeedback(state.clone()) } else { Update::Audio(state.clone()) };
            if updates.send(update).is_err() { break; }
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
}
