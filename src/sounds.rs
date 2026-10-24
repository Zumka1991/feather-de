//! Short shell sounds. The event loop only tries a bounded send; synthesis,
//! player discovery, memfd I/O and process lifetime belong to one sleeping worker.
use smithay::reexports::calloop::{channel, LoopHandle};
use std::{
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Feather,
    Glass,
    Drop,
}
impl Tone {
    pub fn id(self) -> &'static str {
        match self {
            Self::Feather => "feather",
            Self::Glass => "glass",
            Self::Drop => "drop",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Feather => "Пёрышко",
            Self::Glass => "Стекло",
            Self::Drop => "Капля",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Feather, Self::Glass, Self::Drop]
            .into_iter()
            .find(|v| v.id() == s)
    }
    pub fn next(self) -> Self {
        match self {
            Self::Feather => Self::Glass,
            Self::Glass => Self::Drop,
            Self::Drop => Self::Feather,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub alerts: bool,
    pub tone: Tone,
    pub volume: u8,
    pub interface: bool,
    pub volume_feedback: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            alerts: true,
            tone: Tone::Feather,
            volume: 50,
            interface: true,
            volume_feedback: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Alert(Tone),
    Screenshot,
    Volume,
    Notification,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sound {
    File(PathBuf),
    Effect(Effect),
}
impl From<&str> for Sound {
    fn from(s: &str) -> Self {
        Self::File(s.into())
    }
}
impl Preferences {
    pub fn permits(self, effect: Effect, quiet: bool, locked: bool, preview: bool) -> bool {
        !locked
            && self.volume > 0
            && (preview
                || (!quiet
                    && match effect {
                        Effect::Alert(_) => self.alerts,
                        Effect::Screenshot => self.interface,
                        Effect::Volume => self.volume_feedback,
                        Effect::Notification => true,
                    }))
    }
}
struct Request {
    sound: Sound,
    volume: u8,
}
pub struct Sounds {
    tx: mpsc::SyncSender<Request>,
    busy: Arc<AtomicBool>,
}
impl Sounds {
    pub fn new(
        handle: &LoopHandle<'static, crate::FeatherCompositor>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (updates, source) = channel::sync_channel(1);
        handle.insert_source(source, |event, _, s| {
            if let channel::Event::Msg(error) = event {
                tracing::warn!("Системные звуки: {error}");
                if s.system_settings.page == crate::system_settings::Page::Sound {
                    s.system_settings.error = Some(error);
                    s.mark_settings_dirty();
                    s.needs_redraw = true;
                }
            }
        })?;
        Self::worker(None, move |error| {
            let _ = updates.try_send(error);
        })
        .map_err(Into::into)
    }
    fn worker(
        player: Option<PathBuf>,
        mut report: impl FnMut(String) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Request>(1);
        let busy = Arc::new(AtomicBool::new(false));
        let flag = busy.clone();
        std::thread::Builder::new()
            .name("feather-sound".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut player = player;
                while let Ok(request) = rx.recv() {
                    if player.is_none() {
                        player = find_player();
                    }
                    let result = player
                        .as_ref()
                        .ok_or_else(|| {
                            "Не найден проигрыватель: установите pw-play или paplay".to_string()
                        })
                        .and_then(|p| {
                            play(p, &request)
                                .map_err(|e| format!("Не удалось воспроизвести звук: {e}"))
                        });
                    if let Err(e) = result {
                        report(e);
                    }
                    flag.store(false, Ordering::Release);
                }
            })?;
        Ok(Self { tx, busy })
    }
    /// Reserve before waking the worker: even a same-turn burst cannot queue sounds.
    pub fn play(&self, sound: Sound, volume: u8) -> bool {
        if volume == 0
            || self
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        if self
            .tx
            .try_send(Request {
                sound,
                volume: volume.min(100),
            })
            .is_err()
        {
            self.busy.store(false, Ordering::Release);
            return false;
        }
        true
    }
}
fn find_player() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    ["pw-play", "paplay", "canberra-gtk-play"]
        .into_iter()
        .find_map(|name| {
            std::env::split_paths(&path)
                .map(|d| d.join(name))
                .find(|p| {
                    p.metadata()
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
        })
}
fn play(player: &std::path::Path, request: &Request) -> Result<(), Box<dyn std::error::Error>> {
    let mut memory = None;
    let file = match &request.sound {
        Sound::File(path) => path.clone(),
        Sound::Effect(effect) => {
            // Anonymous, automatically reclaimed file; no cache or user data on disk.
            let fd = unsafe { libc::memfd_create(c"feather-sound".as_ptr(), libc::MFD_CLOEXEC) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
            f.write_all(&wave(*effect, request.volume))?;
            let path = PathBuf::from(format!("/proc/{}/fd/{}", std::process::id(), f.as_raw_fd()));
            memory = Some(f);
            path
        }
    };
    let mut command = Command::new(player);
    if player.ends_with("canberra-gtk-play") {
        command.arg("-f");
    }
    let mut child = command
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = wait_player(&mut child, Duration::from_secs(4));
    drop(memory);
    result.map_err(Into::into)
}
fn wait_player(child: &mut std::process::Child, timeout: Duration) -> std::io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(std::io::Error::other("проигрыватель завершился с ошибкой")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result.err().unwrap_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "проигрыватель не отвечает")
                }));
            }
        }
    }
}
/// Original, deterministic mono PCM tones, <= 0.32 s / 31 KiB. Volume is baked
/// into PCM, so all three players obey it without changing the user's sink level.
fn wave(effect: Effect, volume: u8) -> Vec<u8> {
    let rate = 48_000u32;
    let (seconds, f1, f2, decay) = match effect {
        Effect::Alert(Tone::Feather) => (0.28, 880.0, 1320.0, 5.0),
        Effect::Alert(Tone::Glass) => (0.32, 1174.66, 1760.0, 7.0),
        Effect::Alert(Tone::Drop) => (0.20, 660.0, 990.0, 10.0),
        Effect::Screenshot => (0.09, 1800.0, 2700.0, 35.0),
        Effect::Volume => (0.065, 600.0, 900.0, 28.0),
        Effect::Notification => (0.22, 1046.5, 1568.0, 8.0),
    };
    let samples = (seconds * rate as f64) as u32;
    let bytes = samples * 2;
    let mut out = Vec::with_capacity(44 + bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&bytes.to_le_bytes());
    for i in 0..samples {
        let t = i as f64 / rate as f64;
        let end = (seconds - t) / 0.015;
        let envelope = (t / 0.004).min(1.0) * end.clamp(0.0, 1.0) * (-decay * t).exp();
        let phase = std::f64::consts::TAU * t;
        let sample = ((f1 * phase).sin() * 0.72 + (f2 * phase).sin() * 0.28)
            * envelope
            * 0.3
            * volume.min(100) as f64
            / 100.0;
        out.extend_from_slice(&((sample * 32767.0) as i16).to_le_bytes());
    }
    out
}
impl crate::FeatherCompositor {
    pub fn play_effect(&self, effect: Effect, preview: bool) -> bool {
        if !self.sound_preferences.permits(
            effect,
            self.shell.dnd_active(),
            self.lock.active,
            preview,
        ) {
            return false;
        }
        self.sounds
            .as_ref()
            .is_some_and(|s| s.play(Sound::Effect(effect), self.sound_preferences.volume))
    }
    pub fn system_bell(
        &self,
        _surface: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    ) {
        self.play_effect(Effect::Alert(self.sound_preferences.tone), false);
    }
}
impl smithay::wayland::xdg_system_bell::XdgSystemBellHandler for crate::FeatherCompositor {
    fn ring(
        &mut self,
        surface: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    ) {
        self.system_bell(surface);
    }
}
smithay::delegate_xdg_system_bell!(crate::FeatherCompositor);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_is_bounded_fades_and_scales_without_clipping() {
        for effect in [
            Effect::Alert(Tone::Feather),
            Effect::Alert(Tone::Glass),
            Effect::Alert(Tone::Drop),
            Effect::Screenshot,
            Effect::Volume,
            Effect::Notification,
        ] {
            let w = wave(effect, 100);
            assert_eq!(&w[..4], b"RIFF");
            assert!(w.len() <= 31 * 1024);
            assert_eq!(
                u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize,
                w.len() - 44
            );
            let samples: Vec<_> = w[44..]
                .chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]]))
                .collect();
            assert_eq!(samples[0], 0);
            assert!(samples.last().unwrap().abs() < 20);
            assert!(samples.iter().any(|s| s.abs() > 1000));
            assert!(samples.iter().all(|s| s.abs() < 10000));
            assert!(wave(effect, 0)[44..].iter().all(|b| *b == 0));
            let half = wave(effect, 50);
            for (a, b) in w[44..].chunks_exact(2).zip(half[44..].chunks_exact(2)) {
                assert!(
                    (i16::from_le_bytes(a.try_into().unwrap()) / 2
                        - i16::from_le_bytes(b.try_into().unwrap()))
                    .abs()
                        <= 1
                );
            }
        }
    }
    #[test]
    fn policy_respects_quiet_lock_and_independent_switches() {
        let mut p = Preferences::default();
        let a = Effect::Alert(p.tone);
        assert!(p.permits(a, false, false, false));
        assert!(!p.permits(Effect::Volume, false, false, false));
        p.alerts = false;
        assert!(!p.permits(a, false, false, false));
        assert!(p.permits(a, true, false, true));
        assert!(!p.permits(a, false, true, true));
        assert!(!p.permits(Effect::Screenshot, true, false, false));
        p.interface = false;
        assert!(!p.permits(Effect::Screenshot, false, false, false));
        p.volume = 0;
        assert!(!p.permits(a, false, false, true));
    }
    #[test]
    fn shared_player_drops_bursts_reads_memfd_and_recovers_after_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("feather-system-sound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let player = dir.join("pw-play");
        let log = dir.join("pcm.wav");
        let gate = dir.join("ready");
        std::fs::write(
            &player,
            format!(
                "#!/bin/sh\ncat \"$1\" > '{}'\ntouch '{}'\nsleep 0.1\nexit 1\n",
                log.display(),
                gate.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, rx) = mpsc::channel();
        let sounds = Sounds::worker(Some(player), move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        let effect = Sound::Effect(Effect::Screenshot);
        assert!(sounds.play(effect.clone(), 50));
        assert!(!sounds.play(effect.clone(), 50));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gate.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(std::fs::read(&log).unwrap(), wave(Effect::Screenshot, 50));
        assert!(!sounds.play(effect.clone(), 50));
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        while sounds.busy.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(sounds.play(effect, 50));
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(sounds);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn hung_player_is_reaped() {
        let mut child = Command::new("sleep").arg("10").spawn().unwrap();
        assert_eq!(
            wait_player(&mut child, Duration::from_millis(30))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
        assert!(child.try_wait().unwrap().is_some());
    }
}
