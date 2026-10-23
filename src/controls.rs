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
    pub muted: bool,
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
    pub error: Option<String>,
}
impl Audio {
    pub fn current(&self) -> Option<&Sink> {
        self.sinks.iter().find(|s| s.name == self.default)
    }
}

// Password commands intentionally do not implement Debug.
#[derive(Clone)]
pub enum Control {
    Refresh,
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
}
impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Refresh => "Refresh",
            Self::Scan => "Scan",
            Self::WatchWifi(_) => "WatchWifi",
            Self::Wifi(_) => "Wifi",
            Self::Connect { .. } => "Connect(redacted)",
            Self::Disconnect(_) => "Disconnect",
            Self::Volume { .. } => "Volume",
            Self::Mute { .. } => "Mute",
            Self::Output(_) => "Output",
        })
    }
}
pub enum Update {
    Wifi(network::Wifi),
    Audio(Audio),
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
                if state.shell.install_controls(update) {
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
fn pactl(args: &[String]) -> Result<String, String> {
    pactl_until(args, Instant::now() + Duration::from_secs(3))
}
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
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "Некорректный список устройств звука")?;
    let list = value
        .as_array()
        .ok_or("Некорректный список устройств звука")?;
    let mut result = Vec::new();
    for s in list.iter().take(128) {
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
            muted: s["mute"].as_bool().unwrap_or(false),
        });
    }
    Ok(result)
}
fn audio_snapshot() -> Result<Audio, String> {
    let default = pactl(&strings(&["get-default-sink"]))?.trim().to_string();
    let sinks = parse_sinks(&pactl(&strings(&["--format=json", "list", "sinks"]))?)?;
    Ok(Audio {
        sinks,
        default,
        error: None,
    })
}
fn volumes(sink: &Sink, value: f32) -> Vec<String> {
    let peak = sink.channels.iter().copied().max().unwrap_or(0) as f32;
    let target = value.clamp(0.0, 1.0);
    sink.channels
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
        Control::Output(name) => {
            if !state.sinks.iter().any(|s| s.name == name) {
                return Err("Устройство отключено".into());
            }
            run(&vec!["set-default-sink".into(), name.clone()])?;
            // Existing streams on the old default follow an explicit output selection.
            let inputs: serde_json::Value =
                serde_json::from_str(&run(&strings(&["--format=json", "list", "sink-inputs"]))?)
                    .map_err(|_| "Некорректный список потоков")?;
            let old = state.current().map(|s| s.index as u64);
            let deadline = Instant::now() + Duration::from_secs(3);
            for input in inputs
                .as_array()
                .ok_or("Некорректный список потоков")?
                .iter()
                .take(64)
            {
                if Instant::now() >= deadline {
                    return Err("Вывод выбран; не все потоки успели перейти".into());
                }
                if old.is_some() && input["sink"].as_u64() == old {
                    if let Some(id) = input["index"].as_u64() {
                        let _ = run(&vec![
                            "move-sink-input".into(),
                            id.to_string(),
                            name.clone(),
                        ]);
                    }
                }
            }
        }
        _ => {}
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
fn subscribe(wake: async_channel::Sender<()>) -> Result<Subscription, String> {
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
                            if line.contains(" on sink #") || line.contains(" on server #") {
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
            let _ = wake.try_send(());
        })
        .map_err(|_| "Не удалось читать события звука")?;
    Ok(guard)
}
fn audio_run(commands: async_channel::Receiver<Control>, updates: channel::SyncSender<Update>) {
    let (wake, changes) = async_channel::bounded(1);
    let mut watcher = subscribe(wake.clone()).ok();
    let mut state = Audio::default();
    let mut command = Some(Control::Refresh);
    loop {
        if let Some(c) = command.take() {
            if matches!(c, Control::Refresh)
                && watcher
                    .as_ref()
                    .is_none_or(|w| w.0.lock().unwrap().try_wait().ok().flatten().is_some())
            {
                watcher = subscribe(wake.clone()).ok();
            }
            if let Err(error) = audio_action(c, &state) {
                if updates.send(Update::Error("Звук", error)).is_err() {
                    break;
                }
            }
        }
        let next = match audio_snapshot() {
            Ok(s) => s,
            Err(e) => Audio {
                error: Some(e),
                ..Audio::default()
            },
        };
        if next != state {
            state = next;
            if updates.send(Update::Audio(state.clone())).is_err() {
                break;
            }
        }
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
            error: None,
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
    #[ignore = "Explicit read-only live service probe; never changes volume/network"]
    fn read_live_services() {
        let audio = audio_snapshot().unwrap();
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
